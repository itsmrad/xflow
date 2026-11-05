//! Watches the field a dictation was pasted into, for learning corrections.
//! An AXObserver on a run-loop thread that exists only while a watch does, so
//! the idle daemon has no thread, timer or AX traffic. Text is read only on
//! request and never leaves the daemon. Also reads the few characters around
//! the caret that smart spacing needs.
use super::{
    cf_text, focus_waiting, search_or_address, text_reads_allowed, AXUIElementCreateApplication,
    AXUIElementCreateSystemWide, AXUIElementGetPid, AXUIElementSetMessagingTimeout,
    AccessibilityGuard, CFGetTypeID, CFRelease, Focus, Owned, Ref,
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
    fn AXValueCreate(kind: u32, value: *const c_void) -> Ref;
    fn AXUIElementCopyParameterizedAttributeValue(
        element: Ref,
        attribute: Ref,
        parameter: Ref,
        value: *mut Ref,
    ) -> i32;
}
/// 4 = kAXValueTypeCFRange
const CF_RANGE: u32 = 4;
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

#[derive(Default)]
struct State {
    stop: AtomicBool,
    invalid: AtomicBool,
    overflow: AtomicBool,
    generation: std::sync::atomic::AtomicU64,
    ready: AtomicBool,
}
struct Reader {
    pid: i32,
    app: Shared,
    element: Shared,
    window: Shared,
    state: Arc<State>,
    terminal: bool,
}
pub(super) struct Watch {
    reader: Arc<Reader>,
    events: mpsc::Receiver<FieldEvent>,
    observer: Option<thread::JoinHandle<()>>,
    inflight: Option<tokio::task::JoinHandle<Option<xflow_core::FieldSnapshot>>>,
}
fn retain(value: &Owned) -> Owned {
    Owned(unsafe { CFRetain(value.0 as MutRef) } as Ref)
}
/// Prepare without waiting for observer readiness. Injection checks `ready` before keys.
pub(super) fn prepare(focus: &Focus) -> Option<Watch> {
    if !readable(focus) {
        return None;
    }
    let element = focus.element.as_ref()?;
    let window = focus.window.as_ref()?;
    let mut pid = 0;
    if unsafe { AXUIElementGetPid(element.0, &mut pid) } != 0 {
        return None;
    }
    let app = Owned::new(unsafe { AXUIElementCreateApplication(pid) })?;
    for e in [&app, element, window] {
        unsafe { AXUIElementSetMessagingTimeout(e.0, 0.1) };
    }
    let reader = Arc::new(Reader {
        pid,
        app: Shared(app),
        element: Shared(retain(element)),
        window: Shared(retain(window)),
        state: Arc::new(State::default()),
        terminal: super::terminal_context(&focus.context),
    });
    let (tx, events) = mpsc::channel(32);
    let r = reader.clone();
    let observer = thread::Builder::new()
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
            let mut context = Box::new(Context {
                events: tx,
                reader: r.clone(),
            });
            let ptr = (&mut *context as *mut Context).cast();
            let add = |element: Ref, name: &str| {
                let name = Owned::string(name);
                AXObserverAddNotification(observer.0, element, name.0, ptr) == 0
            };
            // App-wide value notifications cannot prove the emitting field's identity.
            let capable = add(r.element.0 .0, "AXValueChanged")
                && add(r.element.0 .0, "AXUIElementDestroyed")
                && add(r.app.0 .0, "AXFocusedUIElementChanged")
                && add(r.app.0 .0, "AXFocusedWindowChanged")
                && add(r.app.0 .0, "AXApplicationDeactivated");
            if !capable {
                return;
            }
            let source = AXObserverGetRunLoopSource(observer.0) as MutRef;
            let runloop = CFRunLoopGetCurrent();
            CFRunLoopAddSource(runloop, source, kCFRunLoopDefaultMode);
            r.state.ready.store(true, Ordering::Release);
            while !r.state.stop.load(Ordering::Acquire)
                && CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.02, true) != 1
            {}
            CFRunLoopRemoveSource(runloop, source, kCFRunLoopDefaultMode);
        })
        .ok();
    Some(Watch {
        reader,
        events,
        observer,
        inflight: None,
    })
}
pub(super) fn watch(target: &AppContext) -> Option<Watch> {
    let f = focus_waiting(false);
    if f.context.app_id != target.app_id || f.context.window_id != target.window_id {
        return None;
    }
    prepare(&f)
}
fn readable(focus: &Focus) -> bool {
    focus.editable == Some(true) && text_reads_allowed(focus)
}
struct Context {
    events: mpsc::Sender<FieldEvent>,
    reader: Arc<Reader>,
}
#[cfg(test)]
const SAME_EDIT: Duration = Duration::from_millis(5);
#[cfg(test)]
fn new_edit(last: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= SAME_EDIT)
}
unsafe extern "C" fn callback(_: Ref, element: Ref, notification: Ref, context: *mut c_void) {
    let context = &*(context as *const Context);
    let r = &context.reader;
    let event = if cf_text(notification).as_deref() == Some("AXValueChanged")
        && CFEqual(element, r.element.0 .0)
    {
        r.state.generation.fetch_add(1, Ordering::AcqRel);
        FieldEvent::Changed
    } else {
        // A focus journey is terminal even if focus returns before the next read.
        r.state.invalid.store(true, Ordering::Release);
        FieldEvent::Invalidated
    };
    if context.events.try_send(event).is_err() {
        r.state.overflow.store(true, Ordering::Release);
        r.state.invalid.store(true, Ordering::Release);
    }
}
/// Unsupported optional metadata is distinct from a failed privacy query.
fn optional_attribute(element: &Owned, name: &str) -> Option<Option<Owned>> {
    let name = Owned::string(name);
    let mut value = ptr::null();
    match unsafe { super::AXUIElementCopyAttributeValue(element.0, name.0, &mut value) } {
        0 => Some(Some(Owned::new(value)?)),
        -25205 => Some(None), // kAXErrorAttributeUnsupported
        _ => None,
    }
}
impl Reader {
    fn valid(&self) -> bool {
        !self.state.stop.load(Ordering::Acquire)
            && !self.state.invalid.load(Ordering::Acquire)
            && !self.state.overflow.load(Ordering::Acquire)
    }
    fn guard(&self) -> Option<()> {
        if !self.valid() || self.terminal {
            return None;
        }
        let e = &self.element.0;
        let role = e.text("AXRole")?;
        let subrole = match optional_attribute(e, "AXSubrole")? {
            Some(v) => Some(cf_text(v.0)?),
            None => None,
        };
        let protected = optional_attribute(e, "AXProtectedContent")?;
        if let Some(p) = protected {
            if unsafe {
                CFGetTypeID(p.0) != super::CFBooleanGetTypeID() || super::CFBooleanGetValue(p.0)
            } {
                return None;
            }
        }
        if role == "AXSecureTextField"
            || subrole.as_deref() == Some("AXSecureTextField")
            || e.flag("AXEnabled") != Some(true)
            || e.flag("AXEditable") == Some(false)
            || !(e.flag("AXEditable") == Some(true) || e.settable("AXSelectedText"))
            || search_or_address(e)
        {
            return None;
        }
        for name in ["AXTitle", "AXDescription", "AXIdentifier"] {
            if e.text(name)
                .is_some_and(|s| super::terminal_input_label(&s))
            {
                return None;
            }
        }
        if e.attribute("AXDOMClassList")
            .is_some_and(|v| super::dom_class(&v, "xterm-helper-textarea"))
            || !focused(self.pid, &self.app.0, e)
            || !e
                .attribute("AXWindow")
                .is_some_and(|w| unsafe { CFEqual(w.0, self.window.0 .0) })
        {
            return None;
        }
        self.valid().then_some(())
    }
    fn snapshot(&self) -> Option<xflow_core::FieldSnapshot> {
        let generation = self.state.generation.load(Ordering::Acquire);
        self.guard()?;
        let selected = selection(&self.element.0)?;
        self.guard()?;
        let count = usize::try_from(self.element.0.number("AXNumberOfCharacters")?).ok()?;
        if count > xflow_core::MAX_FIELD_UTF16 {
            return None;
        }
        let end = (selected.location as usize).checked_add(selected.length as usize)?;
        if end > count {
            return None;
        }
        self.guard()?;
        let value = string_range(&self.element.0, 0, count)?;
        self.guard()?;
        if self.element.0.number("AXNumberOfCharacters")? != count as i64
            || generation != self.state.generation.load(Ordering::Acquire)
        {
            return None;
        }
        self.guard()?;
        let after = selection(&self.element.0)?;
        self.guard()?;
        if after.location != selected.location
            || after.length != selected.length
            || generation != self.state.generation.load(Ordering::Acquire)
        {
            return None;
        }
        Some(xflow_core::FieldSnapshot {
            text: FieldText {
                value,
                caret: Some(end),
            },
            selection: Some((selected.location as usize, selected.length as usize)),
            generation,
            range_start: 0,
        })
    }
}
/// A fixed request bound applies even if the provider grows the field mid-call.
fn string_range(element: &Owned, start: usize, len: usize) -> Option<String> {
    if len > xflow_core::MAX_FIELD_UTF16 {
        return None;
    }
    let range = CFRange {
        location: isize::try_from(start).ok()?,
        length: isize::try_from(len).ok()?,
    };
    let parameter =
        Owned::new(unsafe { AXValueCreate(CF_RANGE, (&range as *const CFRange).cast()) })?;
    let name = Owned::string("AXStringForRange");
    let mut value = ptr::null();
    if unsafe {
        AXUIElementCopyParameterizedAttributeValue(element.0, name.0, parameter.0, &mut value)
    } != 0
    {
        return None;
    }
    let value = cf_text(Owned::new(value)?.0)?;
    (value.encode_utf16().count() == len).then_some(value)
}
pub(super) struct DeliveryReader(Arc<Reader>);
impl DeliveryReader {
    pub(super) fn current(&self) -> Option<xflow_core::FieldSnapshot> {
        self.0.snapshot()
    }
}
impl Watch {
    pub(super) fn delivery_reader(&self) -> DeliveryReader {
        DeliveryReader(self.reader.clone())
    }
    pub(super) fn ready(&self) -> bool {
        self.reader.state.ready.load(Ordering::Acquire) && self.reader.valid()
    }
    pub(super) fn current(&self) -> Option<xflow_core::FieldSnapshot> {
        self.reader.snapshot()
    }
    pub(super) fn same(&self, f: &Focus) -> bool {
        f.element
            .as_ref()
            .is_some_and(|e| unsafe { CFEqual(e.0, self.reader.element.0 .0) })
            && f.window
                .as_ref()
                .is_some_and(|w| unsafe { CFEqual(w.0, self.reader.window.0 .0) })
            && self.reader.valid()
    }
}
fn selection(element: &Owned) -> Option<CFRange> {
    let range = element.attribute("AXSelectedTextRange")?;
    let mut r = CFRange {
        location: 0,
        length: 0,
    };
    unsafe {
        (CFGetTypeID(range.0) == AXValueGetTypeID()
            && AXValueGetValue(range.0, CF_RANGE, (&mut r as *mut CFRange).cast())
            && r.location >= 0
            && r.length >= 0)
            .then_some(r)
    }
}
/// UTF-16 units read before the caret and after it.
const BEFORE: isize = 32;
const AFTER: isize = 16;
/// The focused editable field's text around the caret. None for secure,
/// terminal and unknown fields and on any AX error. Blocking: after one look at
/// focus, two AX calls with a selection, three without, each bounded at 100 ms.
pub(super) fn neighbors() -> Option<(String, String)> {
    // No waiting for a Chromium tree to appear: the whole read has 100 ms.
    let focus = focus_waiting(false);
    if !readable(&focus) {
        return None;
    }
    let watch = prepare(&focus)?;
    let r = &watch.reader;
    r.guard()?;
    let selected = selection(&r.element.0)?;
    let caret = selected.location;
    let start = (caret - BEFORE).max(0);
    r.guard()?;
    let stop = if selected.length == 0 {
        (caret + AFTER).min(isize::try_from(r.element.0.number("AXNumberOfCharacters")?).ok()?)
    } else {
        caret
    };
    if stop < caret {
        return None;
    }
    r.guard()?;
    let text = string_range(&r.element.0, start as usize, (stop - start) as usize)?;
    r.guard()?;
    split(&text, (stop - start) as usize, (caret - start) as usize)
}
/// `text`, `len` UTF-16 units long, cut at unit `at`. None when the length is
/// off or the cut splits a surrogate pair.
fn split(text: &str, len: usize, at: usize) -> Option<(String, String)> {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() != len {
        return None;
    }
    Some((
        String::from_utf16(units.get(..at)?).ok()?,
        String::from_utf16(&units[at..]).ok()?,
    ))
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
        if self.reader.state.overflow.load(Ordering::Acquire) {
            return Some(FieldEvent::Overflow);
        }
        if !self.reader.valid() {
            return Some(FieldEvent::Invalidated);
        }
        self.events.recv().await
    }
    fn try_next(&mut self) -> Option<FieldEvent> {
        if self.reader.state.overflow.swap(false, Ordering::AcqRel) {
            return Some(FieldEvent::Overflow);
        }
        // Return terminal state once; the caller stops instead of draining forever.
        self.events.try_recv().ok()
    }
    async fn read(&mut self) -> Option<FieldText> {
        self.snapshot().await.map(|s| s.text)
    }
    async fn snapshot(&mut self) -> Option<xflow_core::FieldSnapshot> {
        if !self.reader.valid() {
            return None;
        }
        if self.inflight.is_none() {
            let reader = self.reader.clone();
            self.inflight = Some(tokio::task::spawn_blocking(move || reader.snapshot()));
        }
        let result = self.inflight.as_mut()?.await.ok().flatten();
        self.inflight = None;
        self.reader.valid().then_some(result).flatten()
    }
    async fn focused(&mut self) -> bool {
        let r = self.reader.clone();
        tokio::task::spawn_blocking(move || r.guard().is_some())
            .await
            .unwrap_or(false)
    }
    async fn stop(&mut self) {
        self.reader.state.stop.store(true, Ordering::Release);
        if let Some(read) = self.inflight.take() {
            let _ = read.await;
        }
        if let Some(observer) = self.observer.take() {
            let _ = tokio::task::spawn_blocking(move || observer.join()).await;
        }
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.reader.state.stop.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn secure_terminal_and_unknown_fields_are_never_read() {
        let field = |app: &str, editable, secure| Focus {
            context: AppContext {
                app_id: Some(app.into()),
                ..AppContext::default()
            },
            editable,
            secure,
            ..Focus::default()
        };
        assert!(readable(&field("com.apple.TextEdit", Some(true), false)));
        assert!(!readable(&field("org.tabby", Some(true), false)));
        assert!(readable(&field("com.microsoft.VSCode", Some(true), false)));
        let mut integrated = field("org.example.Editor", Some(true), false);
        integrated.context.terminal_input = true;
        assert!(!readable(&integrated));
        assert!(!readable(&field("com.apple.TextEdit", Some(true), true)));
        assert!(!readable(&field("com.apple.TextEdit", None, false)));
        assert!(!readable(&field("com.apple.TextEdit", Some(false), false)));
        assert!(!readable(&field("com.apple.Terminal", Some(true), false)));
        assert!(!readable(&field(
            "com.mitchellh.ghostty",
            Some(true),
            false
        )));
    }
    #[test]
    fn vscode_buffer_keeps_selection_spacing_and_receipt_reads() {
        let mut focus = Focus {
            context: AppContext {
                app_id: Some("com.microsoft.VSCode".into()),
                terminal_input: false,
                ..Default::default()
            },
            editable: Some(true),
            ..Default::default()
        };
        // Selection and receipt paths share this guard; caret/learning additionally need editability.
        assert!(text_reads_allowed(&focus));
        assert!(readable(&focus));
        focus.context.terminal_input = true;
        assert!(!text_reads_allowed(&focus));
        assert!(!readable(&focus));
        focus.context.terminal_input = false;
        focus.secure = true;
        assert!(!text_reads_allowed(&focus));
        assert!(!readable(&focus));
    }
    #[test]
    fn neighbors_split_on_utf16_offsets() {
        let s = |a: &str, b: &str| Some((a.to_owned(), b.to_owned()));
        // The emoji is two units.
        assert_eq!(split("a😀bc", 5, 4), s("a😀b", "c"));
        assert_eq!(split("ab", 2, 0), s("", "ab"));
        assert_eq!(split("ab", 2, 2), s("ab", ""));
        // A cut inside the surrogate pair, a short read or a cut past the end: unknown.
        assert_eq!(split("a😀b", 4, 2), None);
        assert_eq!(split("ab", 3, 1), None);
        assert_eq!(split("ab", 2, 3), None);
    }
    #[test]
    fn duplicate_value_changes_within_5_ms_are_one_edit() {
        let t = Instant::now();
        assert!(new_edit(None, t));
        assert!(!new_edit(Some(t), t + Duration::from_millis(1)));
        assert!(!new_edit(Some(t), t + Duration::from_micros(4_999)));
        assert!(new_edit(Some(t), t + SAME_EDIT));
        assert!(new_edit(Some(t), t + Duration::from_millis(80)));
    }
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
        let _accessibility = super::super::AccessibilityGuard::enable(
            Owned::new(unsafe { AXUIElementCreateApplication(pid) }).unwrap(),
        );
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
        let mut watch = observe(pid, focused(), None).unwrap();
        println!(
            "role={:?} observer={} observe_ms={} threads idle={idle} watching={}",
            editor.text("AXRole"),
            watch.reader.state.ready.load(Ordering::Acquire),
            started.elapsed().as_millis(),
            threads()
        );
        let before = watch.read().await.expect("unreadable");
        let marker = "xflowmark";
        let text = Owned::string(marker);
        let name = Owned::string("AXSelectedText");
        let edited = Instant::now();
        let set = unsafe { super::super::AXUIElementSetAttributeValue(editor.0, name.0, text.0) };
        // Every notification for this one edit over a full second: more than
        // one value change (after SAME_EDIT coalescing) means this app would
        // never learn, since several unread changes end a watch as unsettled.
        let (mut changed_ms, mut changes, mut focus_events, mut merged) = (None, 0, 0, 0);
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(event) = watch.next().await {
                match event {
                    FieldEvent::Changed => {
                        changed_ms.get_or_insert(edited.elapsed().as_millis());
                        changes += 1;
                    }
                    FieldEvent::Focus => focus_events += 1,
                    FieldEvent::Merged => merged += 1,
                }
            }
        })
        .await;
        let read_at = Instant::now();
        let after = watch.read().await.expect("unreadable");
        println!(
            "set={set} changed_ms={changed_ms:?} value_changes={changes} merged={merged} focus_events={focus_events} read_ms={} len {}->{} caret {:?}->{:?} marker={}",
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
        let watch = observe(pid, app, None).unwrap();
        assert!(
            watch.reader.state.ready.load(Ordering::Acquire),
            "observer not created"
        );
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
