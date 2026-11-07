// Meeting consent, Stop indicator and app-audio tap. Linked only into `xflow`
// and run only as `xflow meeting-native`, a child of the recorder: never in xflowd.
//
// stdout, to the recorder: frames of [kind u8][length u32 LE][body].
//   'C' consent JSON  'D' declined  'E' error text  'R' tap running, rate u32
//   'A' audio: rate u32, frames lost before u64, mono f32 samples
//   'S' stop asked here (button, sleep, app quit), reason text  'X' finished
// stdin, from the recorder: 's' starts the tap; 'q' or EOF stops everything.
#import <AppKit/AppKit.h>
#import <CoreAudio/CoreAudio.h>
#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/CATapDescription.h>
#import <libproc.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <dlfcn.h>
#include <unistd.h>

// AppKit is opened at run time and its classes looked up by name, so no other
// xflow command loads it (the same rule as the dictation pill).
#define CLS(name) ((Class)NSClassFromString(@#name))
static NSApplication *App;

#define SLOTS 64
#define FRAMES 4096
typedef struct { unsigned frames; float pcm[FRAMES]; } Slot;
static Slot slots[SLOTS];
static atomic_uint_fast64_t written, taken, lost, callbacks;
static atomic_bool running, audioDone;
static double rate;
static pid_t parentPID;
static NSRunningApplication *selected;
// A process that is not an app (by PID): tapped on its own.
static pid_t targetPID;
static NSString *targetName;
static NSString *appRoot;
static NSArray<NSNumber *> *targets;
static NSTextField *label;
static NSString *what;
static NSDate *began;
static id sleepObserver;
static pthread_mutex_t outLock = PTHREAD_MUTEX_INITIALIZER;

static void wakeApp(void) {
    [App stop:nil];
    [App postEvent:[CLS(NSEvent) otherEventWithType:NSEventTypeApplicationDefined location:NSZeroPoint modifierFlags:0 timestamp:0 windowNumber:0 context:nil subtype:0 data1:0 data2:0] atStart:YES];
}

static void frame(char kind, const void *body, uint32_t length) {
    pthread_mutex_lock(&outLock);
    uint8_t head[5] = {(uint8_t)kind, length, length >> 8, length >> 16, length >> 24};
    if (fwrite(head, 1, 5, stdout) != 5 || (length && fwrite(body, 1, length, stdout) != length) || fflush(stdout)) _exit(3);
    pthread_mutex_unlock(&outLock);
}
static void text(char kind, NSString *s) { NSData *d = [s dataUsingEncoding:NSUTF8StringEncoding]; frame(kind, d.bytes, (uint32_t)d.length); }
static void json(char kind, NSDictionary *o) { NSData *d = [NSJSONSerialization dataWithJSONObject:o options:0 error:nil]; frame(kind, d.bytes, (uint32_t)d.length); }

static void stopHere(NSString *reason) {
    if (atomic_exchange(&running, false)) text('S', reason);
}
static AudioObjectPropertyAddress address(AudioObjectPropertySelector s) {
    return (AudioObjectPropertyAddress){s, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
}
// Every Core Audio process inside the chosen app's bundle: browsers and call
// apps play through helper processes.
static NSArray<NSNumber *> *family(void) API_AVAILABLE(macos(14.2)) {
    AudioObjectPropertyAddress a = address(kAudioHardwarePropertyProcessObjectList);
    UInt32 n = 0;
    if (AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &a, 0, NULL, &n) || n > 65536) return nil;
    AudioObjectID ids[16384];
    if (AudioObjectGetPropertyData(kAudioObjectSystemObject, &a, 0, NULL, &n, ids)) return nil;
    NSMutableArray *out = [NSMutableArray new];
    for (unsigned i = 0; i < n / sizeof(AudioObjectID); i++) {
        pid_t pid = 0; UInt32 size = sizeof(pid); a = address(kAudioProcessPropertyPID);
        if (AudioObjectGetPropertyData(ids[i], &a, 0, NULL, &size, &pid) || pid <= 0 || pid == getpid() || pid == parentPID) continue;
        char path[PROC_PIDPATHINFO_MAXSIZE] = {0};
        if (proc_pidpath(pid, path, sizeof(path)) <= 0) continue;
        NSString *p = @(path);
        if ((appRoot && [p hasPrefix:[appRoot stringByAppendingString:@"/"]]) || (selected && pid == selected.processIdentifier) || (targetPID && pid == targetPID)) [out addObject:@(ids[i])];
    }
    return [out sortedArrayUsingSelector:@selector(compare:)];
}
// Real-time thread: copy and mix down, never block or allocate.
static OSStatus callback(AudioObjectID device, const AudioTimeStamp *now, const AudioBufferList *input, const AudioTimeStamp *inTime, AudioBufferList *output, const AudioTimeStamp *outTime, void *context) {
    atomic_fetch_add_explicit(&callbacks, 1, memory_order_relaxed);
    if (!atomic_load_explicit(&running, memory_order_relaxed)) return 0;
    unsigned channels = 0, frames = 0;
    for (unsigned b = 0; b < input->mNumberBuffers; b++) {
        unsigned c = input->mBuffers[b].mNumberChannels;
        channels += c;
        if (c && input->mBuffers[b].mData) frames = input->mBuffers[b].mDataByteSize / (4 * c);
    }
    if (!channels || !frames) return 0;
    if (frames > FRAMES) { atomic_fetch_add(&lost, frames); return 0; }
    uint64_t w = atomic_load_explicit(&written, memory_order_relaxed), r = atomic_load_explicit(&taken, memory_order_acquire);
    if (w - r == SLOTS) { atomic_fetch_add(&lost, frames); return 0; }
    Slot *s = &slots[w % SLOTS];
    s->frames = frames;
    memset(s->pcm, 0, frames * sizeof(float));
    for (unsigned b = 0; b < input->mNumberBuffers; b++) {
        const AudioBuffer *buffer = &input->mBuffers[b];
        const float *p = buffer->mData;
        if (!p) continue;
        for (unsigned f = 0; f < frames; f++)
            for (unsigned c = 0; c < buffer->mNumberChannels; c++) s->pcm[f] += p[f * buffer->mNumberChannels + c];
    }
    for (unsigned f = 0; f < frames; f++) { float v = s->pcm[f] / channels; s->pcm[f] = isfinite(v) ? v : 0; }
    atomic_store_explicit(&written, w + 1, memory_order_release);
    return 0;
}
static void tap(int mute) API_AVAILABLE(macos(14.2)) {
    @autoreleasepool {
        AudioObjectID tapID = 0, aggregate = 0; AudioDeviceIOProcID io = NULL; OSStatus status = 0;
        CATapDescription *d = [[CATapDescription alloc] initStereoMixdownOfProcesses:targets];
        d.privateTap = YES;
        // Muting is for automated tests only: the user always hears the call.
        d.muteBehavior = mute ? CATapMutedWhenTapped : CATapUnmuted;
        status = AudioHardwareCreateProcessTap(d, &tapID);
        if (!status) {
            AudioStreamBasicDescription format = {0}; UInt32 n = sizeof(format); AudioObjectPropertyAddress a = address(kAudioTapPropertyFormat);
            status = AudioObjectGetPropertyData(tapID, &a, 0, NULL, &n, &format);
            if (!status && (format.mFormatID != kAudioFormatLinearPCM || !(format.mFormatFlags & kAudioFormatFlagIsFloat) || format.mBitsPerChannel != 32 || format.mSampleRate < 8000 || format.mSampleRate > 192000)) status = -3;
            rate = format.mSampleRate;
        }
        if (!status) {
            NSDictionary *config = @{@kAudioAggregateDeviceNameKey: @"Xflow meeting audio", @kAudioAggregateDeviceUIDKey: NSUUID.UUID.UUIDString, @kAudioAggregateDeviceIsPrivateKey: @YES, @kAudioAggregateDeviceTapAutoStartKey: @YES, @kAudioAggregateDeviceTapListKey: @[@{@kAudioSubTapUIDKey: d.UUID.UUIDString, @kAudioSubTapDriftCompensationKey: @YES}]};
            status = AudioHardwareCreateAggregateDevice((__bridge CFDictionaryRef)config, &aggregate);
        }
        if (!status) status = AudioDeviceCreateIOProcID(aggregate, callback, NULL, &io);
        if (!status && atomic_load(&running)) status = AudioDeviceStart(aggregate, io);
        if (!status) {
            uint32_t r = (uint32_t)rate;
            frame('R', &r, 4);
        } else if (atomic_load(&running)) {
            text('E', [NSString stringWithFormat:@"Could not capture the app's audio (Core Audio %d). Allow Xflow under System Settings > Privacy & Security > Screen & System Audio Recording.", (int)status]);
            atomic_store(&running, false);
        }
        uint64_t checked = 0;
        while (atomic_load(&running)) {
            usleep(20000);
            if (++checked % 50) continue;
            // New helper processes of the app join the tap; exited ones leave.
            NSArray *current = family();
            if (current.count && ![current isEqual:targets]) {
                d.processes = current;
                AudioObjectPropertyAddress a = address(kAudioTapPropertyDescription);
                CFTypeRef ref = (__bridge CFTypeRef)d;
                if (!AudioObjectSetPropertyData(tapID, &a, 0, NULL, sizeof(ref), &ref)) targets = current;
            }
        }
        // After Stop returns no callback runs, so the writer can drain everything.
        if (io) { AudioDeviceStop(aggregate, io); AudioDeviceDestroyIOProcID(aggregate, io); }
        if (aggregate) AudioHardwareDestroyAggregateDevice(aggregate);
        if (tapID) AudioHardwareDestroyProcessTap(tapID);
        atomic_store(&audioDone, true);
    }
}
static void drain(void) {
    static float out[FRAMES * 8];
    uint64_t r = atomic_load(&taken), w = atomic_load_explicit(&written, memory_order_acquire);
    while (r < w) {
        unsigned n = 0;
        while (r < w && n + slots[r % SLOTS].frames <= FRAMES * 8) {
            memcpy(out + n, slots[r % SLOTS].pcm, slots[r % SLOTS].frames * sizeof(float));
            n += slots[r % SLOTS].frames;
            atomic_store_explicit(&taken, ++r, memory_order_release);
        }
        static uint8_t body[12 + sizeof(out)];
        uint32_t rr = (uint32_t)rate; uint64_t l = atomic_exchange(&lost, 0);
        memcpy(body, &rr, 4); memcpy(body + 4, &l, 8); memcpy(body + 12, out, n * sizeof(float));
        frame('A', body, 12 + n * (uint32_t)sizeof(float));
    }
}
static void writer(void) {
    while (true) {
        if (getppid() != parentPID) atomic_store(&running, false);
        if ((selected && selected.terminated) || (targetPID && kill(targetPID, 0) != 0)) stopHere(@"the meeting app quit");
        drain();
        if (!atomic_load(&running) && atomic_load(&audioDone)) { drain(); break; }
        usleep(20000);
    }
    frame('X', NULL, 0);
    exit(0);
}
@interface MeetingActions : NSObject <NSWindowDelegate>
@end
@implementation MeetingActions
- (void)stop:(id)sender { stopHere(@"stopped from the Xflow indicator"); label.stringValue = @"Stopping. Saved audio is kept."; }
- (BOOL)windowShouldClose:(NSWindow *)sender { [self stop:nil]; return NO; }
- (void)tick:(NSTimer *)timer {
    if (!atomic_load(&running)) return;
    long s = (long)-[began timeIntervalSinceNow];
    label.stringValue = [NSString stringWithFormat:@"Recording %@ · %ld:%02ld:%02ld", what, s / 3600, s / 60 % 60, s % 60];
}
@end

@interface MeetingConsent : NSObject <NSWindowDelegate>
@property NSInteger answer;
- (void)record:(id)sender;
- (void)decline:(id)sender;
@end
@implementation MeetingConsent
- (void)record:(id)sender { self.answer = 1; wakeApp(); }
- (void)decline:(id)sender { self.answer = -1; wakeApp(); }
- (BOOL)windowShouldClose:(NSWindow *)sender { [self decline:nil]; return NO; }
@end

int xflow_meeting_native(const char *app, int mic, int yes, int mute) {
    @autoreleasepool {
        parentPID = getppid();
        // The recorder decides when to stop (stdin), so a terminal's Ctrl-C or
        // hangup cannot cut off the last of the app's audio.
        signal(SIGINT, SIG_IGN);
        signal(SIGHUP, SIG_IGN);
        if (@available(macOS 14.2, *)) {} else { text('E', @"Meeting recording needs macOS 14.2 or newer. Dictation is unaffected."); return 2; }
        if (!dlopen("/System/Library/Frameworks/AppKit.framework/AppKit", RTLD_NOW | RTLD_GLOBAL)) { text('E', @"Could not load AppKit for the recording window."); return 2; }
        App = [CLS(NSApplication) sharedApplication];
        [App setActivationPolicy:NSApplicationActivationPolicyAccessory];
        __block bool quit = false;
        // The recorder's 'q' or EOF ends everything, also while the dialog is open.
        [NSThread detachNewThreadWithBlock:^{
            char c = 0;
            while (read(STDIN_FILENO, &c, 1) == 1 && c != 'q') {
                if (c == 's') dispatch_async(dispatch_get_main_queue(), ^{ wakeApp(); });
            }
            atomic_store(&running, false);
            dispatch_async(dispatch_get_main_queue(), ^{ quit = true; wakeApp(); });
        }];
        NSMutableArray *visible = [NSMutableArray new];
        for (NSRunningApplication *a in [CLS(NSWorkspace) sharedWorkspace].runningApplications)
            if (a.activationPolicy == NSApplicationActivationPolicyRegular && a.processIdentifier != getpid() && a.processIdentifier != parentPID && a.bundleURL && a.localizedName) [visible addObject:a];
        [visible sortUsingComparator:^NSComparisonResult(NSRunningApplication *a, NSRunningApplication *b) { return [a.localizedName localizedCaseInsensitiveCompare:b.localizedName]; }];
        NSString *choice = app ? @(app) : @"";
        if (choice.length) {
            NSDictionary *aliases = @{@"zoom": @"us.zoom.xos", @"teams": @"com.microsoft.teams2", @"chrome": @"com.google.Chrome", @"safari": @"com.apple.Safari", @"firefox": @"org.mozilla.firefox", @"chime": @"com.amazon.Amazon-Chime", @"edge": @"com.microsoft.edgemac", @"arc": @"company.thebrowser.Browser"};
            NSString *bundle = aliases[choice.lowercaseString] ?: choice;
            NSMutableArray *matches = [NSMutableArray new];
            for (NSRunningApplication *a in [CLS(NSWorkspace) sharedWorkspace].runningApplications)
                if ((a.bundleIdentifier && [a.bundleIdentifier caseInsensitiveCompare:bundle] == NSOrderedSame) || (a.localizedName && [a.localizedName caseInsensitiveCompare:choice] == NSOrderedSame) || (choice.intValue > 0 && a.processIdentifier == choice.intValue)) [matches addObject:a];
            char path[PROC_PIDPATHINFO_MAXSIZE] = {0};
            if (!matches.count && choice.intValue > 0 && choice.intValue != getpid() && choice.intValue != parentPID && proc_pidpath(choice.intValue, path, sizeof(path)) > 0) {
                targetPID = choice.intValue;
                targetName = @(path).lastPathComponent;
            } else if (matches.count != 1) { text('E', [NSString stringWithFormat:@"%@ is not running, or matches more than one app. Pass its bundle ID or PID.", choice]); return 2; }
            else selected = matches[0];
        }
        if (!yes) {
            NSRunningApplication *previous = [CLS(NSWorkspace) sharedWorkspace].frontmostApplication;
            NSAlert *alert = [CLS(NSAlert) new];
            alert.messageText = @"Record this meeting?";
            alert.informativeText = [NSString stringWithFormat:@"Xflow records %@ the app you choose, including its helper processes. A browser records all of its tabs. Audio stays private on this Mac until it is sent to your configured transcription provider. Make sure everyone in the meeting agrees. Stop any time from the Xflow indicator. Nothing is recorded until you choose Record.", mic ? @"your microphone and" : @""];
            [alert addButtonWithTitle:@"Record"];
            [alert addButtonWithTitle:@"Cancel"];
            NSPopUpButton *picker = [[CLS(NSPopUpButton) alloc] initWithFrame:NSMakeRect(0, 0, 360, 28) pullsDown:NO];
            if (mic) [picker addItemWithTitle:@"Microphone only (no app audio)"];
            for (NSRunningApplication *a in visible) [picker addItemWithTitle:[NSString stringWithFormat:@"%@ (%@)", a.localizedName, a.bundleIdentifier ?: @"app"]];
            NSUInteger at = selected ? [visible indexOfObject:selected] : NSNotFound;
            if (at != NSNotFound) [picker selectItemAtIndex:at + (mic ? 1 : 0)];
            picker.accessibilityLabel = @"App to record";
            alert.accessoryView = picker;
            [alert layout];
            NSWindow *window = alert.window;
            window.title = @"Record this meeting?";
            MeetingConsent *consent = [MeetingConsent new];
            window.delegate = consent;
            NSButton *record = alert.buttons[0], *cancel = alert.buttons[1];
            record.target = cancel.target = consent;
            record.action = @selector(record:); cancel.action = @selector(decline:);
            record.keyEquivalent = @""; cancel.keyEquivalent = @"\e";
            window.defaultButtonCell = nil;
            window.initialFirstResponder = cancel;
            // Return must never become consent, even with Record keyboard-focused.
            id keys = [CLS(NSEvent) addLocalMonitorForEventsMatchingMask:NSEventMaskKeyDown handler:^NSEvent *(NSEvent *event) {
                if (event.window == window && (event.keyCode == 36 || event.keyCode == 76)) return nil;
                return event;
            }];
            [window orderFrontRegardless];
            // runModal before App.run never finishes launching the AX server.
            [App run];
            [CLS(NSEvent) removeMonitor:keys];
            [window orderOut:nil];
            if (App.active && previous && !previous.terminated && previous.processIdentifier != getpid()) {
                if (@available(macOS 14.0, *)) [App yieldActivationToApplication:previous];
                [previous activateWithOptions:0];
            }
            if (quit || consent.answer != 1) { frame('D', NULL, 0); return 0; }
            NSInteger index = picker.indexOfSelectedItem - (mic ? 1 : 0);
            selected = index >= 0 && index < (NSInteger)visible.count ? visible[index] : nil;
        }
        if (!selected && !targetPID && !mic) { text('E', @"Choose an app to record, or record the microphone."); return 2; }
        if (selected || targetPID) {
            appRoot = selected.bundleURL.path;
            if (@available(macOS 14.2, *)) targets = family();
            if (!targetName) targetName = selected.localizedName;
            if (!targets.count) { text('E', [NSString stringWithFormat:@"%@ has no audio yet. Join the call first, then start recording.", targetName]); return 2; }
        }
        what = targetName ? (mic ? [NSString stringWithFormat:@"microphone + %@", targetName] : targetName) : @"microphone";
        atomic_store(&running, true);
        json('C', @{@"app": targetName ?: @"", @"bundle": selected.bundleIdentifier ?: @"", @"pid": @(selected ? selected.processIdentifier : targetPID)});
        NSPanel *panel = [[CLS(NSPanel) alloc] initWithContentRect:NSMakeRect(0, 0, 340, 64) styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskNonactivatingPanel backing:NSBackingStoreBuffered defer:NO];
        panel.title = @"Xflow meeting";
        panel.level = NSFloatingWindowLevel;
        panel.hidesOnDeactivate = NO;
        panel.collectionBehavior = NSWindowCollectionBehaviorCanJoinAllSpaces | NSWindowCollectionBehaviorFullScreenAuxiliary;
        label = [CLS(NSTextField) labelWithString:[NSString stringWithFormat:@"Starting %@…", what]];
        label.frame = NSMakeRect(12, 22, 200, 30);
        label.lineBreakMode = NSLineBreakByTruncatingTail;
        MeetingActions *actions = [MeetingActions new];
        NSButton *stop = [CLS(NSButton) buttonWithTitle:@"Stop recording" target:actions action:@selector(stop:)];
        stop.frame = NSMakeRect(214, 16, 116, 32);
        stop.keyEquivalent = @"\e";
        [panel.contentView addSubview:label];
        [panel.contentView addSubview:stop];
        panel.delegate = actions;
        NSRect screen = [CLS(NSScreen) mainScreen].visibleFrame;
        [panel setFrameTopLeftPoint:NSMakePoint(NSMaxX(screen) - 360, NSMaxY(screen) - 12)];
        [panel orderFrontRegardless];
        void (*announce)(id, NSString *, NSDictionary *) = dlsym(RTLD_DEFAULT, "NSAccessibilityPostNotificationWithUserInfo");
        if (announce) announce(panel, @"AXAnnouncementRequested", @{@"AXAnnouncementKey": @"Xflow meeting recording is starting"});
        // The center does not retain block observers: keep the token alive.
        sleepObserver = [[CLS(NSWorkspace) sharedWorkspace].notificationCenter addObserverForName:@"NSWorkspaceWillSleepNotification" object:nil queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *n) { stopHere(@"the Mac went to sleep"); }];
        // Wait for the recorder's 's': it has the microphone and the journal open.
        [App run];
        if (quit || !atomic_load(&running)) { [panel close]; frame('X', NULL, 0); return 0; }
        began = [NSDate date];
        [NSTimer scheduledTimerWithTimeInterval:1 target:actions selector:@selector(tick:) userInfo:nil repeats:YES];
        [actions tick:nil];
        if (targetName) [NSThread detachNewThreadWithBlock:^{ if (@available(macOS 14.2, *)) tap(mute); }];
        else atomic_store(&audioDone, true);
        [NSThread detachNewThreadWithBlock:^{ writer(); }];
        [App run];
        // Kept on screen until the writer exits the process with 'X'.
        while (true) [NSThread sleepForTimeInterval:1];
    }
}
