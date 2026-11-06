//! Watches the field a dictation was pasted into, for learning corrections.
//! An AXObserver on a run-loop thread that exists only while a watch does, so
//! the idle daemon has no thread, timer or AX traffic. Text is read only on
//! request, in bounded ranges behind a guard (`xflow_core::read_field`), and
//! never leaves the daemon. Also reads the few characters around the caret that
//! smart spacing needs.
use super::{
    cf_text, focus_waiting, search_or_address, text_reads_allowed, AXUIElementCreateApplication,
    AXUIElementCreateSystemWide, AXUIElementGetPid, AXUIElementSetMessagingTimeout,
    AccessibilityGuard, CFGetTypeID, CFRelease, Focus, Owned, Ref,
};
use async_trait::async_trait;
use std::{
    cell::Cell,
    ffi::c_void,
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use xflow_core::{BoundedReader, FieldEvent, FieldSource, FieldText, FieldWatch, ReadGate};

type Callback = unsafe extern "C" fn(Ref, Ref, Ref, *mut c_void);
#[repr(C)]
struct CFRange {
    location: isize,
    length: isize,
}
#[repr(C)]
#[derive(Default)]
struct Point {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Default)]
struct Size {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Default)]
struct Rect {
    origin: Point,
    size: Size,
}
impl Rect {
    fn contains(&self, other: &Self) -> bool {
        let valid = |r: &Self| {
            [r.origin.x, r.origin.y, r.size.width, r.size.height]
                .iter()
                .all(|v| v.is_finite())
                && r.size.width > 0.0
                && r.size.height > 0.0
        };
        valid(self)
            && valid(other)
            && other.origin.x >= self.origin.x
            && other.origin.y >= self.origin.y
            && other.origin.x + other.size.width <= self.origin.x + self.size.width
            && other.origin.y + other.size.height <= self.origin.y + self.size.height
    }
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

/// The exact element and window a delivery targeted. Holds no text and starts
/// nothing: delivery receipts and caret reads need no observer.
pub(super) struct Reader {
    pid: i32,
    app: Shared,
    element: Shared,
    window: Shared,
}
fn retain(value: &Owned) -> Owned {
    Owned(unsafe { CFRetain(value.0 as MutRef) } as Ref)
}
impl Reader {
    /// None unless focus is a known-editable field whose text may be read.
    pub(super) fn new(focus: &Focus) -> Option<Arc<Self>> {
        if !readable(focus) {
            return None;
        }
        Self::pinned(focus.element.as_ref()?, focus.window.as_ref()?)
    }
    /// Terminal confirmation pins the delivery's screen, without making ordinary
    /// field readers eligible to read a terminal.
    pub(super) fn terminal(focus: &Focus) -> Option<Arc<Self>> {
        if focus.secure || !super::is_terminal(focus.context.app_id.as_deref()) {
            return None;
        }
        Self::pinned(focus.element.as_ref()?, focus.window.as_ref()?).filter(|r| r.pid == focus.pid)
    }
    fn pinned(element: &Owned, window: &Owned) -> Option<Arc<Self>> {
        let mut pid = 0;
        if unsafe { AXUIElementGetPid(element.0, &mut pid) } != 0 {
            return None;
        }
        let app = Owned::new(unsafe { AXUIElementCreateApplication(pid) })?;
        for e in [&app, element, window] {
            unsafe { AXUIElementSetMessagingTimeout(e.0, 0.1) };
        }
        Some(Arc::new(Self {
            pid,
            app: Shared(app),
            element: Shared(retain(element)),
            window: Shared(retain(window)),
        }))
    }
    pub(super) fn same(&self, f: &Focus) -> bool {
        f.element
            .as_ref()
            .is_some_and(|e| unsafe { CFEqual(e.0, self.element.0 .0) })
            && f.window
                .as_ref()
                .is_some_and(|w| unsafe { CFEqual(w.0, self.window.0 .0) })
    }
    /// Length and selection only, for a field whose text cannot be read within
    /// the bound: enough for a paste receipt, never for learning.
    pub(super) fn counts(&self) -> Option<(usize, usize)> {
        if !self.guard() {
            return None;
        }
        let selected = selection(&self.element.0)?;
        if !self.guard() {
            return None;
        }
        Some((self.count()?, selected.length as usize))
    }
}
impl xflow_core::replay::VisibleScreen for Reader {
    fn safe(&self) -> bool {
        !crate::macos_hotkey::secure_input()
            && FieldSource::guard(self)
            && self.element.0.flag("AXEnabled") != Some(false)
    }
    fn visible_range(&self) -> Option<(usize, usize)> {
        let r = named_range(&self.element.0, "AXVisibleCharacterRange")?;
        Some((r.location as usize, r.length as usize))
    }
    fn in_viewport(&self, start: usize, len: usize) -> bool {
        self.tail_in_viewport(start, len).is_some()
    }
    fn range(&self, start: usize, len: usize) -> Option<String> {
        string_range(&self.element.0, start, len)
    }
}
impl Reader {
    fn tail_in_viewport(&self, start: usize, len: usize) -> Option<()> {
        if len == 0 || len > xflow_core::replay::MAX_CONFIRM_UTF16 {
            return None;
        }
        let r = CFRange {
            location: isize::try_from(start).ok()?,
            length: isize::try_from(len).ok()?,
        };
        r.location.checked_add(r.length)?;
        // iTerm2 advertises scrollback as visible. Check the proposed tail itself
        // before requesting text; full-range or clipped bounds cannot widen it.
        let e = &self.element.0;
        let mut bounds = Rect::default();
        ax_value(&parameterized(e, "AXBoundsForRange", &r)?, 3, &mut bounds)?;
        // A document view can be as tall as scrollback; its scroll area's frame
        // is the viewport. Without that independent bound, do not request text.
        let mut parent = e.attribute("AXParent")?;
        let mut viewport = None;
        for _ in 0..4 {
            if parent.text("AXRole").as_deref() == Some("AXScrollArea") {
                viewport = Some(parent);
                break;
            }
            parent = parent.attribute("AXParent")?;
        }
        if !frame(e)?.contains(&bounds)
            || !frame(&viewport?)?.contains(&bounds)
            || !frame(&self.window.0)?.contains(&bounds)
        {
            return None;
        }
        Some(())
    }
}
fn frame(element: &Owned) -> Option<Rect> {
    let mut rect = Rect::default();
    ax_value(&element.attribute("AXPosition")?, 1, &mut rect.origin)?;
    ax_value(&element.attribute("AXSize")?, 2, &mut rect.size)?;
    Some(rect)
}
/// Unsupported optional metadata is distinct from a failed privacy query.
fn optional_attribute(element: &Owned, name: &str) -> Option<Option<Owned>> {
    let name = Owned::string(name);
    let mut value = ptr::null();
    match unsafe { super::AXUIElementCopyAttributeValue(element.0, name.0, &mut value) } {
        0 => Some(Some(Owned::new(value)?)),
        // kAXErrorAttributeUnsupported, kAXErrorNoValue: an answer, and it is no.
        -25205 | -25212 => Some(None),
        _ => None,
    }
}
/// Neither secure nor protected, as far as the element will say. An error or an
/// unknown answer is not safe; an attribute the element lacks says nothing.
fn secure_ok(e: &Owned) -> Option<()> {
    let role = e.text("AXRole")?;
    let subrole = match optional_attribute(e, "AXSubrole")? {
        Some(v) => Some(cf_text(v.0)?),
        None => None,
    };
    if let Some(p) = optional_attribute(e, "AXProtectedContent")? {
        if unsafe {
            CFGetTypeID(p.0) != super::CFBooleanGetTypeID() || super::CFBooleanGetValue(p.0)
        } {
            return None;
        }
    }
    (role != "AXSecureTextField" && subrole.as_deref() != Some("AXSecureTextField")).then_some(())
}
impl FieldSource for Reader {
    fn policy(&self) -> bool {
        let e = &self.element.0;
        e.flag("AXEnabled") != Some(false)
            && e.flag("AXEditable") != Some(false)
            && !search_or_address(e)
            && !["AXTitle", "AXDescription", "AXIdentifier"]
                .iter()
                .any(|name| {
                    e.text(name)
                        .is_some_and(|s| super::terminal_input_label(&s))
                })
            && !e
                .attribute("AXDOMClassList")
                .is_some_and(|v| super::dom_class(&v, "xterm-helper-textarea"))
    }
    fn guard(&self) -> bool {
        let e = &self.element.0;
        secure_ok(e).is_some()
            && focused(self.pid, &self.app.0, e)
            && e.attribute("AXWindow")
                .is_some_and(|w| unsafe { CFEqual(w.0, self.window.0 .0) })
    }
    fn selection(&self) -> Option<(usize, usize)> {
        let r = selection(&self.element.0)?;
        Some((r.location as usize, r.length as usize))
    }
    fn count(&self) -> Option<usize> {
        usize::try_from(self.element.0.number("AXNumberOfCharacters")?).ok()
    }
    fn range(&self, start: usize, len: usize) -> Option<String> {
        string_range(&self.element.0, start, len)
    }
}
pub(super) struct Watch {
    source: Arc<Reader>,
    reader: BoundedReader<Reader>,
    gate: Arc<ReadGate>,
    events: mpsc::Receiver<FieldEvent>,
    observer: Option<thread::JoinHandle<()>>,
    /// The observer's run loop while it runs, to wake it for a stop.
    runloop: Arc<AtomicUsize>,
    /// Chromium's accessibility stays on for the watch; restored on its thread.
    accessibility: Arc<Mutex<Option<AccessibilityGuard<Owned>>>>,
}
/// Start observing without waiting: registration takes AX round trips into the
/// target app. A late one only means the first read finds an unannounced edit,
/// which is a safe miss.
pub(super) fn arm(source: Arc<Reader>) -> Watch {
    let gate = Arc::new(ReadGate::default());
    let accessibility = Arc::new(Mutex::new(None));
    let (tx, events) = mpsc::channel(32);
    let runloop = Arc::new(AtomicUsize::new(0));
    let (r, g, kept, wake) = (
        source.clone(),
        gate.clone(),
        accessibility.clone(),
        runloop.clone(),
    );
    let observer = thread::Builder::new()
        .name("xflow-observe".into())
        .stack_size(256 * 1024)
        .spawn(move || unsafe {
            if !observing(&r, &g, tx, &wake) {
                g.unsupported.store(true, Ordering::Release);
            }
            let held: Option<AccessibilityGuard<Owned>> =
                kept.lock().ok().and_then(|mut kept| kept.take());
            drop(held);
        })
        .ok();
    if observer.is_none() {
        gate.unsupported.store(true, Ordering::Release);
    }
    Watch {
        reader: BoundedReader::new(source.clone(), gate.clone()),
        source,
        gate,
        events,
        observer,
        runloop,
        accessibility,
    }
}
/// Run until stopped. False when the app cannot report what the watch relies on.
unsafe fn observing(
    r: &Arc<Reader>,
    gate: &Arc<ReadGate>,
    events: mpsc::Sender<FieldEvent>,
    wake: &AtomicUsize,
) -> bool {
    let mut observer = ptr::null();
    if AXObserverCreate(r.pid, callback, &mut observer) != 0 {
        return false;
    }
    let Some(observer) = Owned::new(observer) else {
        return false;
    };
    let mut context = Box::new(Context {
        events,
        reader: r.clone(),
        gate: gate.clone(),
        app_scoped: false,
        last_change: Cell::new(None),
    });
    let ptr: *mut Context = &mut *context;
    let refcon: *mut c_void = ptr.cast();
    let add = |element: Ref, name: &str| {
        let name = Owned::string(name);
        AXObserverAddNotification(observer.0, element, name.0, refcon) == 0
    };
    // Some apps post value changes only on the application: then each is a hint
    // to reread the pinned field, never a reason to read anything else.
    if !add(r.element.0 .0, "AXValueChanged") {
        (*ptr).app_scoped = true;
        if !add(r.app.0 .0, "AXValueChanged") {
            return false;
        }
    }
    // Without this a recycled element's reads still fail the focus guard.
    add(r.element.0 .0, "AXUIElementDestroyed");
    if !(add(r.app.0 .0, "AXFocusedUIElementChanged")
        && add(r.app.0 .0, "AXFocusedWindowChanged")
        && add(r.app.0 .0, "AXApplicationDeactivated"))
    {
        return false;
    }
    let source = AXObserverGetRunLoopSource(observer.0) as MutRef;
    let runloop = CFRunLoopGetCurrent();
    CFRunLoopAddSource(runloop, source, kCFRunLoopDefaultMode);
    // No polling: asleep until a notification or `wake`. The long timeout only
    // bounds a stop that slipped in just before the run loop went to sleep.
    CFRetain(runloop);
    wake.store(runloop as usize, Ordering::Release);
    loop {
        if gate.stop.load(Ordering::Acquire)
            || CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, true) == 1
        {
            break;
        }
    }
    let retained = wake.swap(0, Ordering::AcqRel) as MutRef;
    if !retained.is_null() {
        CFRelease(retained as Ref);
    }
    CFRunLoopRemoveSource(runloop, source, kCFRunLoopDefaultMode);
    true
}
fn readable(focus: &Focus) -> bool {
    focus.editable == Some(true) && text_reads_allowed(focus)
}
struct Context {
    events: mpsc::Sender<FieldEvent>,
    reader: Arc<Reader>,
    gate: Arc<ReadGate>,
    app_scoped: bool,
    /// Only the observer thread touches this.
    last_change: Cell<Option<Instant>>,
}
/// Two value notifications this close are one edit (see `FieldEvent::Merged`).
const SAME_EDIT: Duration = Duration::from_millis(5);
fn new_edit(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= SAME_EDIT)
}
unsafe extern "C" fn callback(_: Ref, element: Ref, notification: Ref, context: *mut c_void) {
    let context = &*(context as *const Context);
    let (r, gate) = (&context.reader, &context.gate);
    let name = cf_text(notification);
    let event = match name.as_deref() {
        Some("AXValueChanged") => {
            if !context.app_scoped && !CFEqual(element, r.element.0 .0) {
                return;
            }
            gate.generation.fetch_add(1, Ordering::AcqRel);
            let now = Instant::now();
            if new_edit(context.last_change.replace(Some(now)), now) {
                FieldEvent::Changed
            } else {
                FieldEvent::Merged
            }
        }
        // A prompt to look, not proof of departure: apps repeat these with focus
        // unmoved (T17), some from another wrapper of the same element (Safari).
        Some("AXFocusedUIElementChanged" | "AXFocusedWindowChanged")
            if focused(r.pid, &r.app.0, &r.element.0) =>
        {
            return;
        }
        // A focus journey is terminal even if focus returns before the next read.
        _ => {
            gate.invalid.store(true, Ordering::Release);
            FieldEvent::Invalidated
        }
    };
    if context.events.try_send(event).is_err() {
        gate.overflow.store(true, Ordering::Release);
        gate.invalid.store(true, Ordering::Release);
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
    let value = parameterized(element, "AXStringForRange", &range)?;
    if unsafe {
        CFGetTypeID(value.0) != super::CFStringGetTypeID()
            || super::CFStringGetLength(value.0) != len as isize
    } {
        return None;
    }
    let value = cf_text(value.0)?;
    (value.encode_utf16().count() == len).then_some(value)
}
fn parameterized(element: &Owned, name: &str, range: &CFRange) -> Option<Owned> {
    let parameter =
        Owned::new(unsafe { AXValueCreate(CF_RANGE, (range as *const CFRange).cast()) })?;
    let name = Owned::string(name);
    let mut value = ptr::null();
    if unsafe {
        AXUIElementCopyParameterizedAttributeValue(element.0, name.0, parameter.0, &mut value)
    } != 0
    {
        return None;
    }
    Owned::new(value)
}
fn ax_value<T>(value: &Owned, kind: u32, out: &mut T) -> Option<()> {
    // All callers pair the AX kind with its C-layout struct (point, size, rect).
    unsafe {
        (CFGetTypeID(value.0) == AXValueGetTypeID()
            && AXValueGetValue(value.0, kind, (out as *mut T).cast()))
        .then_some(())
    }
}
impl Watch {
    /// Close the gate and wake the observer's run loop to notice.
    fn signal_stop(&self) {
        self.gate.stop.store(true, Ordering::Release);
        // Taken, so the thread's own release cannot race this use.
        let runloop = self.runloop.swap(0, Ordering::AcqRel) as MutRef;
        if !runloop.is_null() {
            unsafe {
                CFRunLoopStop(runloop);
                CFRunLoopWakeUp(runloop);
                CFRelease(runloop as Ref);
            }
        }
    }
    /// Keep Chromium's accessibility on until this watch ends.
    pub(super) fn hold(&self, accessibility: Option<AccessibilityGuard<Owned>>) {
        if let Ok(mut held) = self.accessibility.lock() {
            *held = accessibility;
        }
    }
}
fn selection(element: &Owned) -> Option<CFRange> {
    named_range(element, "AXSelectedTextRange")
}
fn named_range(element: &Owned, name: &str) -> Option<CFRange> {
    let range = element.attribute(name)?;
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
/// focus, two AX calls with a selection, three without, each bounded at 100 ms,
/// each behind a fresh security check.
pub(super) fn neighbors() -> Option<(String, String)> {
    // No waiting for a Chromium tree to appear: the whole read has 100 ms.
    let focus = focus_waiting(false);
    let r = Reader::new(&focus)?;
    let e = &r.element.0;
    secure_ok(e)?;
    let selected = selection(e)?;
    let caret = selected.location;
    let start = (caret - BEFORE).max(0);
    let stop = if selected.length == 0 {
        secure_ok(e)?;
        (caret + AFTER).min(isize::try_from(e.number("AXNumberOfCharacters")?).ok()?)
    } else {
        caret
    };
    if stop < caret {
        return None;
    }
    secure_ok(e)?;
    let text = string_range(e, start as usize, (stop - start) as usize)?;
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
/// `element` holds keyboard focus. The system-wide answer can be missing for
/// seconds in Chromium apps, as `focus_waiting` also finds; the front process
/// stands in only then. The app's own focused element is always required.
fn focused(pid: i32, app: &Owned, element: &Owned) -> bool {
    let Some(system) = Owned::new(unsafe { AXUIElementCreateSystemWide() }) else {
        return false;
    };
    let mut owner = 0;
    let owns = match system.attribute("AXFocusedApplication") {
        Some(a) => (unsafe { AXUIElementGetPid(a.0, &mut owner) }) == 0 && owner == pid,
        None => super::front_pid() == Some(pid),
    };
    owns && app
        .attribute("AXFocusedUIElement")
        .is_some_and(|e| unsafe { CFEqual(e.0, element.0) })
}
#[async_trait]
impl FieldWatch for Watch {
    async fn next(&mut self) -> Option<FieldEvent> {
        let flag = |g: &ReadGate| {
            if g.overflow.load(Ordering::Acquire) {
                Some(FieldEvent::Overflow)
            } else if g.unsupported.load(Ordering::Acquire) {
                Some(FieldEvent::Unsupported)
            } else if !g.open() {
                Some(FieldEvent::Invalidated)
            } else {
                None
            }
        };
        if let Some(event) = flag(&self.gate) {
            return Some(event);
        }
        match self.events.recv().await {
            None => flag(&self.gate),
            event => event,
        }
    }
    fn try_next(&mut self) -> Option<FieldEvent> {
        // Reported once: the caller stops instead of draining forever.
        if self.gate.overflow.swap(false, Ordering::AcqRel) {
            return Some(FieldEvent::Overflow);
        }
        self.events.try_recv().ok()
    }
    async fn read(&mut self) -> Option<FieldText> {
        self.snapshot().await.map(|s| s.text)
    }
    async fn snapshot(&mut self) -> Option<xflow_core::FieldSnapshot> {
        self.reader.snapshot().await
    }
    async fn focused(&mut self) -> bool {
        if !self.gate.open() {
            return false;
        }
        let r = self.source.clone();
        tokio::task::spawn_blocking(move || r.guard())
            .await
            .unwrap_or(false)
    }
    async fn stop(&mut self) {
        self.reader.stop().await;
        self.signal_stop();
        if let Some(observer) = self.observer.take() {
            let _ = tokio::task::spawn_blocking(move || observer.join()).await;
        }
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.signal_stop();
    }
}

/// App and window changes for a terminal replay. No text is read.
pub(super) struct Departure {
    pub(super) events: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
    runloop: Arc<AtomicUsize>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Departure {
    pub(super) fn signal(&self) {
        self.stop.store(true, Ordering::Release);
        let runloop = self.runloop.swap(0, Ordering::AcqRel) as MutRef;
        if !runloop.is_null() {
            unsafe {
                CFRunLoopStop(runloop);
                CFRunLoopWakeUp(runloop);
                CFRelease(runloop as Ref);
            }
        }
    }
}
impl Drop for Departure {
    fn drop(&mut self) {
        self.signal();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub(super) fn departure(pid: i32, window: &Owned) -> Option<Departure> {
    if pid <= 0 {
        return None;
    }
    let (tx, events) = mpsc::channel(4);
    let stop = Arc::new(AtomicBool::new(false));
    let runloop = Arc::new(AtomicUsize::new(0));
    let (flag, wake, kept) = (stop.clone(), runloop.clone(), Shared(retain(window)));
    let thread = thread::Builder::new()
        .name("xflow-replay-focus".into())
        .stack_size(256 * 1024)
        .spawn(move || unsafe { watch_departure(pid, kept, tx, &flag, &wake) })
        .ok()?;
    Some(Departure {
        events,
        stop,
        runloop,
        thread: Some(thread),
    })
}
unsafe fn watch_departure(
    pid: i32,
    window: Shared,
    events: mpsc::Sender<()>,
    stop: &AtomicBool,
    wake: &AtomicUsize,
) {
    let Some(app) = Owned::new(AXUIElementCreateApplication(pid)) else {
        return;
    };
    AXUIElementSetMessagingTimeout(app.0, 0.1);
    let mut observer = ptr::null();
    if AXObserverCreate(pid, leave_callback, &mut observer) != 0 {
        return;
    }
    let Some(observer) = Owned::new(observer) else {
        return;
    };
    let mut context = Box::new(LeaveCtx {
        events,
        window,
        app: Shared(app),
        sent: Cell::new(false),
    });
    let app_ref = (context.app.0).0;
    let refcon: *mut c_void = (&mut *context as *mut LeaveCtx).cast();
    let mut added = true;
    for name in ["AXApplicationDeactivated", "AXFocusedWindowChanged"] {
        let name = Owned::string(name);
        added &= AXObserverAddNotification(observer.0, app_ref, name.0, refcon) == 0;
    }
    if !added {
        return;
    }
    let source = AXObserverGetRunLoopSource(observer.0) as MutRef;
    let runloop = CFRunLoopGetCurrent();
    CFRunLoopAddSource(runloop, source, kCFRunLoopDefaultMode);
    CFRetain(runloop);
    wake.store(runloop as usize, Ordering::Release);
    loop {
        if stop.load(Ordering::Acquire) || CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, true) == 1
        {
            break;
        }
    }
    let retained = wake.swap(0, Ordering::AcqRel) as MutRef;
    if !retained.is_null() {
        CFRelease(retained as Ref);
    }
    CFRunLoopRemoveSource(runloop, source, kCFRunLoopDefaultMode);
}
struct LeaveCtx {
    events: mpsc::Sender<()>,
    window: Shared,
    app: Shared,
    sent: Cell<bool>,
}
unsafe extern "C" fn leave_callback(_: Ref, _: Ref, notification: Ref, context: *mut c_void) {
    let context = &*(context as *const LeaveCtx);
    if context.sent.get() {
        return;
    }
    let leave = match cf_text(notification).as_deref() {
        Some("AXApplicationDeactivated") => true,
        // A failed read is not a window change. A different window is.
        Some("AXFocusedWindowChanged") => context
            .app
            .0
            .attribute("AXFocusedWindow")
            .is_some_and(|w| !CFEqual(w.0, (context.window.0).0)),
        _ => false,
    };
    if leave && context.events.try_send(()).is_ok() {
        context.sent.set(true);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visible_bounds_reject_scrollback_and_unknown_geometry() {
        let frame = Rect {
            origin: Point { x: 20.0, y: 30.0 },
            size: Size {
                width: 800.0,
                height: 600.0,
            },
        };
        let visible = Rect {
            origin: Point { x: 20.0, y: 30.0 },
            size: Size {
                width: 800.0,
                height: 500.0,
            },
        };
        assert!(frame.contains(&visible));
        let scrollback = Rect {
            origin: Point {
                x: 20.0,
                y: -1000.0,
            },
            size: Size {
                width: 800.0,
                height: 1500.0,
            },
        };
        assert!(!frame.contains(&scrollback));
        assert!(!frame.contains(&Rect::default()));
        assert!(!frame.contains(&Rect {
            origin: Point {
                x: f64::NAN,
                y: 30.0
            },
            size: Size {
                width: 10.0,
                height: 10.0
            }
        }));
    }
    use xflow_core::AppContext;

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
        let focused_window = editor.attribute("AXWindow").unwrap();
        let mut watch = arm(Reader::pinned(&focused(), &focused_window).unwrap());
        std::thread::sleep(Duration::from_millis(100));
        println!(
            "role={:?} observe_ms={} threads idle={idle} watching={}",
            editor.text("AXRole"),
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
                    FieldEvent::Invalidated | FieldEvent::Overflow | FieldEvent::Unsupported => {
                        break
                    }
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
        watch.stop().await;
        println!(
            "threads after stop={} reads started={} finished={}",
            threads(),
            watch.gate.started.load(Ordering::Acquire),
            watch.gate.finished.load(Ordering::Acquire)
        );
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
        let mut watch = arm(Reader::pinned(&app, &app).unwrap());
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !watch.gate.unsupported.load(Ordering::Acquire),
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
        watch.stop().await;
        println!(
            "20 s watch: cpu={}ms rss {}->{} KiB threads idle={idle} watching={watching} after_stop={} (other threads are AX/libdispatch workers)",
            (cpu1 - cpu0).as_millis(),
            rss0,
            rss1,
            threads()
        );
        assert!(!named(), "observer thread outlived its watch");
    }
}
