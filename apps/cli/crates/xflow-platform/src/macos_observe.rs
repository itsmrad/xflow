//! Watches the field a dictation was pasted into, for learning corrections.
//! An AXObserver on a run-loop thread that exists only while a watch does, so
//! the idle daemon has no thread, timer or AX traffic. Text is read only on
//! request and never leaves the daemon.
use super::{
    cf_text, focus, is_terminal, secure, AXUIElementCreateApplication, AXUIElementCreateSystemWide,
    AXUIElementGetPid, CFGetTypeID, CFRelease, Owned, Ref,
};
use async_trait::async_trait;
use std::{
    ffi::c_void,
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
use tokio::sync::mpsc;
use xflow_core::{AppContext, FieldEvent, FieldText, FieldWatch};

type Callback = unsafe extern "C" fn(Ref, Ref, Ref, *mut c_void);
#[repr(C)]
struct CFRange {
    location: isize,
    length: isize,
}
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXObserverCreate(pid: i32, callback: Callback, observer: *mut Ref) -> i32;
    fn AXObserverAddNotification(
        observer: Ref,
        element: Ref,
        notification: Ref,
        context: *mut c_void,
    ) -> i32;
    fn AXObserverGetRunLoopSource(observer: Ref) -> Ref;
    fn AXValueGetTypeID() -> usize;
    fn AXValueGetValue(value: Ref, kind: u32, out: *mut c_void) -> bool;
}
/// Run-loop calls take mutable refs, matching their declarations in macos_hotkey.rs.
type MutRef = *mut c_void;
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: MutRef;
    fn CFEqual(a: Ref, b: Ref) -> bool;
    fn CFRetain(value: MutRef) -> MutRef;
    fn CFRunLoopGetCurrent() -> MutRef;
    fn CFRunLoopAddSource(runloop: MutRef, source: MutRef, mode: MutRef);
    fn CFRunLoopRemoveSource(runloop: MutRef, source: MutRef, mode: MutRef);
    fn CFRunLoopRunInMode(mode: MutRef, seconds: f64, return_after_source: bool) -> i32;
    fn CFRunLoopStop(runloop: MutRef);
    fn CFRunLoopWakeUp(runloop: MutRef);
}

/// CF and AX references are thread-safe; Owned is only !Send for its raw pointer.
struct Shared(Owned);
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

pub(super) struct Watch {
    pid: i32,
    app: Arc<Shared>,
    element: Arc<Shared>,
    events: mpsc::Receiver<FieldEvent>,
    stop: Arc<AtomicBool>,
    /// Retained run loop and observer source; None when the app refused an observer.
    runloop: Option<(usize, usize)>,
}

/// Pin the focused editable element if focus is still in `target`. Blocking.
pub(super) fn watch(target: &AppContext) -> Option<Watch> {
    let focus = focus();
    if focus.context.app_id != target.app_id
        || focus.context.window_id != target.window_id
        || focus.editable != Some(true)
        || focus.secure
        || is_terminal(focus.context.app_id.as_deref())
    {
        return None;
    }
    let element = focus.element?;
    let mut pid = 0;
    if unsafe { AXUIElementGetPid(element.0, &mut pid) } != 0 {
        return None;
    }
    observe(pid, element)
}
/// Observe `element` of app `pid`; a watch without notifications when the app
/// refuses an observer, so reads still work.
fn observe(pid: i32, element: Owned) -> Option<Watch> {
    let element = Arc::new(Shared(element));
    let app = Arc::new(Shared(Owned::new(unsafe {
        AXUIElementCreateApplication(pid)
    })?));
    let (tx, events) = mpsc::channel(32);
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready) = std::sync::mpsc::sync_channel(1);
    let (thread_app, thread_element, thread_stop) = (app.clone(), element.clone(), stop.clone());
    let spawned = thread::Builder::new()
        .name("xflow-observe".into())
        .stack_size(256 * 1024)
        .spawn(move || unsafe {
            let mut observer = ptr::null();
            if AXObserverCreate(pid, callback, &mut observer) != 0 {
                return;
            }
            let Some(observer) = Owned::new(observer) else {
                return;
            };
            // Freed on this thread after the run loop stops, so no callback outlives it.
            let context = Box::into_raw(Box::new(tx));
            let add = |element: Ref, name: &str| {
                let name = Owned::string(name);
                AXObserverAddNotification(observer.0, element, name.0, context.cast()) == 0
            };
            // Some apps only post value changes for the whole application.
            if !add(thread_element.0 .0, "AXValueChanged") {
                add(thread_app.0 .0, "AXValueChanged");
            }
            add(thread_element.0 .0, "AXUIElementDestroyed");
            for name in [
                "AXFocusedUIElementChanged",
                "AXFocusedWindowChanged",
                "AXApplicationDeactivated",
            ] {
                add(thread_app.0 .0, name);
            }
            let source = AXObserverGetRunLoopSource(observer.0) as MutRef;
            let runloop = CFRunLoopGetCurrent();
            CFRunLoopAddSource(runloop, source, kCFRunLoopDefaultMode);
            let handles = (CFRetain(runloop) as usize, CFRetain(source) as usize);
            if let Err(e) = ready_tx.send(handles) {
                CFRelease(e.0 .0 as Ref);
                CFRelease(e.0 .1 as Ref);
            } else {
                // A stop that lands before the run loop starts is seen within a second.
                // 1 = kCFRunLoopRunFinished: the source was removed.
                while !thread_stop.load(Ordering::Acquire)
                    && CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, false) != 1
                {}
            }
            CFRunLoopRemoveSource(runloop, source, kCFRunLoopDefaultMode);
            drop(observer);
            drop(Box::from_raw(context));
        });
    let runloop = spawned
        .ok()
        .and_then(|_| ready.recv_timeout(Duration::from_millis(500)).ok());
    Some(Watch {
        pid,
        app,
        element,
        events,
        stop,
        runloop,
    })
}
unsafe extern "C" fn callback(_: Ref, _: Ref, notification: Ref, context: *mut c_void) {
    let events = &*(context as *const mpsc::Sender<FieldEvent>);
    let event = if cf_text(notification).as_deref() == Some("AXValueChanged") {
        FieldEvent::Changed
    } else {
        FieldEvent::Focus
    };
    // A full queue already holds a pending change for the reader.
    let _ = events.try_send(event);
}
/// A field that turned secure since it was pinned (a page swapping in a
/// password input) is no longer read. Fields over MAX_FIELD_UTF16 are skipped
/// before their value is copied across processes.
fn read(element: &Owned) -> Option<FieldText> {
    let large = |n: usize| n > xflow_core::MAX_FIELD_UTF16;
    if secure(element)
        || element
            .number("AXNumberOfCharacters")
            .is_some_and(|n| large(n as usize))
    {
        return None;
    }
    let value = element
        .text("AXValue")
        .filter(|v| !large(v.encode_utf16().count()))?;
    let caret = element.attribute("AXSelectedTextRange").and_then(|range| {
        let mut r = CFRange {
            location: 0,
            length: 0,
        };
        // 4 = kAXValueTypeCFRange
        unsafe {
            (CFGetTypeID(range.0) == AXValueGetTypeID()
                && AXValueGetValue(range.0, 4, (&mut r as *mut CFRange).cast())
                && r.location >= 0
                && r.length >= 0)
                .then(|| (r.location + r.length) as usize)
        }
    });
    Some(FieldText { value, caret })
}
fn focused(pid: i32, app: &Owned, element: &Owned) -> bool {
    let Some(system) = Owned::new(unsafe { AXUIElementCreateSystemWide() }) else {
        return false;
    };
    let mut front = 0;
    system
        .attribute("AXFocusedApplication")
        .is_some_and(|a| unsafe { AXUIElementGetPid(a.0, &mut front) == 0 } && front == pid)
        && app
            .attribute("AXFocusedUIElement")
            .is_some_and(|e| unsafe { CFEqual(e.0, element.0) })
}
#[async_trait]
impl FieldWatch for Watch {
    async fn next(&mut self) -> Option<FieldEvent> {
        if self.runloop.is_none() {
            return std::future::pending().await;
        }
        self.events.recv().await
    }
    async fn read(&mut self) -> Option<FieldText> {
        let element = self.element.clone();
        tokio::task::spawn_blocking(move || read(&element.0))
            .await
            .ok()
            .flatten()
    }
    async fn focused(&mut self) -> bool {
        let (pid, app, element) = (self.pid, self.app.clone(), self.element.clone());
        tokio::task::spawn_blocking(move || focused(pid, &app.0, &element.0))
            .await
            .unwrap_or(false)
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some((runloop, source)) = self.runloop.take() {
            unsafe {
                CFRunLoopRemoveSource(runloop as MutRef, source as MutRef, kCFRunLoopDefaultMode);
                CFRunLoopStop(runloop as MutRef);
                CFRunLoopWakeUp(runloop as MutRef);
                CFRelease(source as Ref);
                CFRelease(runloop as Ref);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn threads() -> usize {
        let out = std::process::Command::new("/bin/ps")
            .args(["-M", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).lines().count() - 1
    }
    /// App matrix without keyboard or focus (works with the screen locked):
    /// pins the focused text element of XFLOW_OBSERVE_PID, inserts a marker at
    /// its caret through AXSelectedText as typing does, and reports whether the
    /// app posts AXValueChanged and the read shows the edit. Refuses unless the
    /// element's window title contains XFLOW_OBSERVE_WINDOW. Prints no field text.
    #[tokio::test]
    #[ignore = "edits the focused field of XFLOW_OBSERVE_PID; run explicitly on a scratch document"]
    async fn app_matrix() {
        let pid: i32 = std::env::var("XFLOW_OBSERVE_PID").unwrap().parse().unwrap();
        let app = Owned::new(unsafe { AXUIElementCreateApplication(pid) }).unwrap();
        app.enable("AXManualAccessibility");
        std::thread::sleep(Duration::from_millis(500));
        let focused = || {
            app.attribute("AXFocusedUIElement")
                .expect("no focused element")
        };
        let editor = focused();
        let window = editor
            .attribute("AXWindow")
            .and_then(|w| w.text("AXTitle"))
            .unwrap_or_default();
        let expected = std::env::var("XFLOW_OBSERVE_WINDOW").unwrap();
        assert!(window.contains(&expected), "focused window is {window:?}");
        let idle = threads();
        let started = Instant::now();
        let mut watch = observe(pid, focused()).unwrap();
        println!(
            "role={:?} observer={} observe_ms={} threads idle={idle} watching={}",
            editor.text("AXRole"),
            watch.runloop.is_some(),
            started.elapsed().as_millis(),
            threads()
        );
        let before = watch.read().await.expect("unreadable");
        let marker = "xflowmark";
        let text = Owned::string(marker);
        let name = Owned::string("AXSelectedText");
        let edited = Instant::now();
        let set = unsafe { super::super::AXUIElementSetAttributeValue(editor.0, name.0, text.0) };
        // Wait for a value change specifically; focus notifications are counted apart.
        let (mut changed_ms, mut focus_events) = (None, 0);
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(event) = watch.next().await {
                match event {
                    FieldEvent::Changed => {
                        changed_ms = Some(edited.elapsed().as_millis());
                        break;
                    }
                    FieldEvent::Focus => focus_events += 1,
                }
            }
        })
        .await;
        let read_at = Instant::now();
        let after = watch.read().await.expect("unreadable");
        println!(
            "set={set} changed_ms={changed_ms:?} focus_events={focus_events} read_ms={} len {}->{} caret {:?}->{:?} marker={}",
            read_at.elapsed().as_millis(),
            before.value.encode_utf16().count(),
            after.value.encode_utf16().count(),
            before.caret,
            after.caret,
            after.value.contains(marker)
        );
        assert_eq!(set, 0, "the app refused the edit");
        assert!(
            after.value.contains(marker),
            "the read does not show the edit"
        );
        drop(watch);
        std::thread::sleep(Duration::from_millis(1200));
        println!("threads after drop={}", threads());
    }
    /// Cost of a watch with no edits: CPU time and RSS over 20 s, and that its
    /// thread is gone after drop. Pins XFLOW_OBSERVE_PID's application element.
    #[tokio::test]
    #[ignore = "measures for 20 s; run explicitly on macOS"]
    async fn watch_idle_cost() {
        fn usage() -> (Duration, u64) {
            let out = std::process::Command::new("/bin/ps")
                .args(["-o", "rss=,time=", "-p", &std::process::id().to_string()])
                .output()
                .unwrap();
            let out = String::from_utf8_lossy(&out.stdout).to_string();
            let mut fields = out.split_whitespace();
            let rss = fields.next().unwrap().parse().unwrap();
            let time = fields.next().unwrap().replace('.', ":");
            let parts: Vec<u64> = time.split(':').map(|p| p.parse().unwrap()).collect();
            let [m, s, cs] = parts[..] else {
                panic!("{time}")
            };
            (Duration::from_millis((m * 60 + s) * 1000 + cs * 10), rss)
        }
        let pid: i32 = std::env::var("XFLOW_OBSERVE_PID").unwrap().parse().unwrap();
        let app = Owned::new(unsafe { AXUIElementCreateApplication(pid) }).unwrap();
        let idle = threads();
        let (cpu0, rss0) = usage();
        let watch = observe(pid, app).unwrap();
        assert!(watch.runloop.is_some(), "observer not created");
        let watching = threads();
        std::thread::sleep(Duration::from_secs(20));
        let (cpu1, rss1) = usage();
        let named = || {
            let out = std::process::Command::new("/usr/bin/sample")
                .args([&std::process::id().to_string(), "1"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).contains("xflow-observe")
        };
        assert!(named(), "sample cannot see the observer thread");
        drop(watch);
        std::thread::sleep(Duration::from_millis(1200));
        println!(
            "20 s watch: cpu={}ms rss {}->{} KiB threads idle={idle} watching={watching} after_drop={} (other threads are AX/libdispatch workers)",
            (cpu1 - cpu0).as_millis(),
            rss0,
            rss1,
            threads()
        );
        assert!(!named(), "observer thread outlived its watch");
    }
}
