// macOS UI regression; run on an isolated desktop, with stdin/stdout owned here.
// clang -fobjc-arc -O2 scripts/test-meeting-consent.m -framework AppKit -framework CoreAudio -o /tmp/meeting-consent-test
// /tmp/meeting-consent-test [escape]
#ifndef NATIVE_SOURCE
#define NATIVE_SOURCE "../apps/cli/crates/xflow-app/src/cli/meeting_native.m"
#endif
#include NATIVE_SOURCE
#include <assert.h>

static int control;
static NSButton *button(NSView *view, NSString *title) {
    if ([view isKindOfClass:NSButton.class] && [((NSButton *)view).title isEqual:title]) return (NSButton *)view;
    for (NSView *child in view.subviews) { NSButton *found = button(child, title); if (found) return found; }
    return nil;
}
static void key(NSWindow *window, unsigned short code, NSString *characters) {
    NSEvent *event = [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint modifierFlags:0 timestamp:0 windowNumber:window.windowNumber context:nil characters:characters charactersIgnoringModifiers:characters isARepeat:NO keyCode:code];
    [App postEvent:event atStart:NO];
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        int pipefd[2]; assert(pipe(pipefd) == 0);
        assert(dup2(pipefd[0], STDIN_FILENO) >= 0); close(pipefd[0]); control = pipefd[1];
        bool escape = argc > 1;
        __block bool completed = false;
        // The old pre-launch modal loop never services this notification/main queue.
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC), dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0), ^{
            fprintf(stderr, "FAIL: consent did not service the application run loop\n"); _exit(1);
        });
        id observer = [NSNotificationCenter.defaultCenter addObserverForName:NSApplicationDidFinishLaunchingNotification object:nil queue:nil usingBlock:^(NSNotification *notification) {
            dispatch_async(dispatch_get_main_queue(), ^{
                NSWindow *window = nil;
                for (NSWindow *candidate in App.windows)
                    if (candidate.visible && [candidate.title isEqual:@"Record this meeting?"]) window = candidate;
                assert(window && window.visible && !atomic_load(&running));
                assert(window.defaultButtonCell == nil);
                NSButton *record = button(window.contentView, @"Record"), *cancel = button(window.contentView, @"Cancel");
                assert(record && cancel && record.keyEquivalent.length == 0);
                assert([cancel.keyEquivalent isEqual:@"\e"]);
                assert([window makeFirstResponder:record]);
                key(window, 36, @"\r"); key(window, 76, @"\003");
                dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 100 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{
                    assert(!atomic_load(&running) && window.visible);
                    if (escape) { completed = true; key(window, 53, @"\e"); return; }
                    // AXPress and Space use the same standard NSButton action.
                    [record performClick:nil];
                    dispatch_async(dispatch_get_main_queue(), ^{
                        assert(atomic_load(&running) && !window.visible);
                        NSWindow *panel = nil;
                        for (NSWindow *candidate in App.windows)
                            if ([candidate.title isEqual:@"Xflow meeting"]) panel = candidate;
                        assert(panel && panel.visible);
                        NSButton *stop = button(panel.contentView, @"Stop recording");
                        assert(stop && [stop.keyEquivalent isEqual:@"\e"]);
                        [stop performClick:nil];
                        assert(!atomic_load(&running));
                        completed = true;
                        assert(write(control, "q", 1) == 1);
                    });
                });
            });
        }];
        int result = xflow_meeting_native(NULL, 1, 0, 1);
        [NSNotificationCenter.defaultCenter removeObserver:observer];
        assert(completed && result == 0 && !atomic_load(&running));
        fprintf(stderr, "PASS: responsive consent, Return refused, %s\n", escape ? "Esc declined" : "explicit Record and Stop actions");
        return 0;
    }
}
