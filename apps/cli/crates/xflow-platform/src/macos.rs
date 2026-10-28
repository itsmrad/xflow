//! Accessibility and CGEvent delivery without NSApplication or an AppKit run loop.
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::{
    ffi::{c_char, c_void, CStr},
    ptr,
    sync::{Once, OnceLock},
    time::{Duration, Instant},
};
use xflow_core::{
    config::{InjectionConfig, InjectionMethod},
    AppContext, Desktop, FieldWatch, InjectionOutcome,
};

const MAX_TEXT: usize = 64 * 1024;
// A child module so the observer reuses this file's private AX helpers.
#[path = "macos_observe.rs"]
mod observe;
type Ref = *const c_void;
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request: u32) -> u32;
    fn IOHIDRequestAccess(request: u32) -> bool;
}
#[repr(C)]
#[derive(Default)]
struct ProcessSerialNumber {
    high: u32,
    low: u32,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: Ref);
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFHash(value: Ref) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringCreateWithBytes(
        allocator: Ref,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        external: bool,
    ) -> Ref;
    fn CFStringGetCString(value: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> bool;
    fn CFStringGetLength(value: Ref) -> isize;
    fn CFStringGetMaximumSizeForEncoding(len: isize, encoding: u32) -> isize;
    fn CFDataCreate(allocator: Ref, bytes: *const u8, length: isize) -> Ref;
    fn CFDataGetBytePtr(data: Ref) -> *const u8;
    fn CFDataGetLength(data: Ref) -> isize;
    static kCFBooleanTrue: Ref;
    fn CFDictionaryGetValue(dict: Ref, key: Ref) -> Ref;
    fn CFDictionaryCreate(
        allocator: Ref,
        keys: *const Ref,
        values: *const Ref,
        count: isize,
        key_callbacks: Ref,
        value_callbacks: Ref,
    ) -> Ref;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: Ref) -> bool;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(number: Ref, kind: isize, value: *mut c_void) -> bool;
}
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn GetFrontProcess(psn: *mut ProcessSerialNumber) -> i16;
    fn GetProcessPID(psn: *const ProcessSerialNumber, pid: *mut i32) -> i32;
    fn GetProcessForPID(pid: i32, psn: *mut ProcessSerialNumber) -> i32;
    fn ProcessInformationCopyDictionary(psn: *const ProcessSerialNumber, mask: u32) -> Ref;
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: Ref) -> bool;
    static kAXTrustedCheckOptionPrompt: Ref;
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementCreateSystemWide() -> Ref;
    fn AXUIElementGetPid(element: Ref, pid: *mut i32) -> i32;
    fn AXUIElementSetAttributeValue(element: Ref, attribute: Ref, value: Ref) -> i32;
    fn AXUIElementCopyAttributeValue(element: Ref, attribute: Ref, value: *mut Ref) -> i32;
    fn AXUIElementIsAttributeSettable(element: Ref, attribute: Ref, settable: *mut bool) -> i32;
    fn AXUIElementSetMessagingTimeout(element: Ref, timeout: f32) -> i32;
    fn CGPreflightListenEventAccess() -> bool;
    fn CGPreflightPostEventAccess() -> bool;
    fn CGEventCreateKeyboardEvent(source: Ref, key: u16, down: bool) -> Ref;
    fn CGEventSetFlags(event: Ref, flags: u64);
    fn CGEventKeyboardSetUnicodeString(event: Ref, len: usize, text: *const u16);
    fn CGEventPost(tap: u32, event: Ref);
    // HIServices Pasteboard Manager (part of ApplicationServices).
    fn PasteboardCreate(name: Ref, out: *mut Ref) -> i32;
    fn PasteboardClear(pb: Ref) -> i32;
    fn PasteboardSynchronize(pb: Ref) -> u32;
    fn PasteboardPutItemFlavor(
        pb: Ref,
        item: *const c_void,
        flavor: Ref,
        data: Ref,
        flags: u32,
    ) -> i32;
    fn PasteboardCopyItemFlavorData(
        pb: Ref,
        item: *const c_void,
        flavor: Ref,
        out_data: *mut Ref,
    ) -> i32;
    fn PasteboardGetItemCount(pb: Ref, count: *mut usize) -> i32;
    fn PasteboardGetItemIdentifier(pb: Ref, index: isize, item: *mut *const c_void) -> i32;
}

struct Owned(Ref);
impl Owned {
    fn new(value: Ref) -> Option<Self> {
        (!value.is_null()).then_some(Self(value))
    }
    fn string(value: &str) -> Self {
        Self(unsafe {
            CFStringCreateWithBytes(
                ptr::null(),
                value.as_ptr(),
                value.len() as isize,
                0x08000100,
                false,
            )
        })
    }
    fn attribute(&self, name: &str) -> Option<Self> {
        let name = Self::string(name);
        let mut value = ptr::null();
        (unsafe { AXUIElementCopyAttributeValue(self.0, name.0, &mut value) } == 0)
            .then(|| Self::new(value))
            .flatten()
    }
    fn text(&self, name: &str) -> Option<String> {
        cf_text(self.attribute(name)?.0)
    }
    fn settable(&self, name: &str) -> bool {
        let name = Self::string(name);
        let mut settable = false;
        unsafe { AXUIElementIsAttributeSettable(self.0, name.0, &mut settable) == 0 && settable }
    }
    fn enable(&self, name: &str) {
        let name = Self::string(name);
        unsafe { AXUIElementSetAttributeValue(self.0, name.0, kCFBooleanTrue) };
    }
    fn flag(&self, name: &str) -> Option<bool> {
        let value = self.attribute(name)?;
        unsafe {
            (CFGetTypeID(value.0) == CFBooleanGetTypeID()).then(|| CFBooleanGetValue(value.0))
        }
    }
    fn number(&self, name: &str) -> Option<i64> {
        let value = self.attribute(name)?;
        let mut number = 0_i64;
        // kCFNumberSInt64Type
        unsafe {
            (CFGetTypeID(value.0) == CFNumberGetTypeID()
                && CFNumberGetValue(value.0, 4, (&mut number as *mut i64).cast()))
            .then_some(number)
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}
fn cf_text(value: Ref) -> Option<String> {
    if value.is_null() || unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        return None;
    }
    let len = unsafe { CFStringGetLength(value) };
    if !(0..=MAX_TEXT as isize).contains(&len) {
        return None;
    }
    let mut bytes =
        vec![0_u8; unsafe { CFStringGetMaximumSizeForEncoding(len, 0x08000100) } as usize + 1];
    if !unsafe {
        CFStringGetCString(
            value,
            bytes.as_mut_ptr().cast(),
            bytes.len() as isize,
            0x08000100,
        )
    } {
        return None;
    }
    Some(
        CStr::from_bytes_until_nul(&bytes)
            .ok()?
            .to_str()
            .ok()?
            .to_owned(),
    )
}

/// HIServices Pasteboard Manager: in-process clipboard access without AppKit
/// or a pbcopy/pbpaste child per call.
struct Pasteboard(Ref);
// SAFETY: a PasteboardRef may move between threads; HIServices documents its
// calls as not thread safe, so shared use goes through clipboard()'s Mutex.
unsafe impl Send for Pasteboard {}
/// kPasteboardClipboard
const CLIPBOARD: &str = "com.apple.pasteboard.clipboard";
/// badPasteboardFlavorErr: the item has no such flavor.
const NO_FLAVOR: i32 = -25133;
impl Pasteboard {
    fn open(name: &str) -> Result<Self> {
        let name = Owned::string(name);
        let mut pasteboard = ptr::null();
        let status = unsafe { PasteboardCreate(name.0, &mut pasteboard) };
        if status != 0 || pasteboard.is_null() {
            bail!("cannot open the pasteboard (OSStatus {status})");
        }
        Ok(Self(pasteboard))
    }
    /// True until another process writes: the change-count check, without reading back.
    fn owned(&self) -> bool {
        // kPasteboardClientIsOwner
        let flags = unsafe { PasteboardSynchronize(self.0) };
        flags & 2 != 0
    }
    /// The first item's text as pbpaste reads it: UTF-8, else UTF-16, "" when it
    /// has neither. None on any failure, so a failed read never restores "".
    fn read(&self) -> Option<String> {
        let mut count = 0;
        let mut item = ptr::null();
        unsafe {
            PasteboardSynchronize(self.0);
            if PasteboardGetItemCount(self.0, &mut count) != 0 {
                return None;
            }
            if count == 0 {
                return Some(String::new());
            }
            if PasteboardGetItemIdentifier(self.0, 1, &mut item) != 0 {
                return None;
            }
        }
        let text = match self.bytes(item, "public.utf8-plain-text") {
            Ok(bytes) => String::from_utf8(bytes).ok()?,
            // The Pasteboard Manager does not translate UTF-16 to UTF-8.
            Err(NO_FLAVOR) => match self.bytes(item, "public.utf16-plain-text") {
                Ok(bytes) if bytes.len() % 2 == 0 => {
                    let units: Vec<u16> = bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|&unit| u16::from_ne_bytes(unit))
                        .collect();
                    let text = String::from_utf16(&units).ok()?;
                    text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned()
                }
                Err(NO_FLAVOR) => String::new(),
                _ => return None,
            },
            Err(_) => return None,
        };
        (text.len() <= MAX_TEXT).then_some(text)
    }
    /// A flavor's bytes (at most 2 × MAX_TEXT), or the OSStatus that refused them.
    fn bytes(&self, item: Ref, flavor: &str) -> Result<Vec<u8>, i32> {
        let flavor = Owned::string(flavor);
        let mut data = ptr::null();
        let status = unsafe { PasteboardCopyItemFlavorData(self.0, item, flavor.0, &mut data) };
        let data = Owned::new(data).filter(|_| status == 0).ok_or(status)?;
        let len = unsafe { CFDataGetLength(data.0) } as usize;
        if len > 2 * MAX_TEXT {
            return Err(status);
        }
        if len == 0 {
            return Ok(Vec::new());
        }
        Ok(unsafe { std::slice::from_raw_parts(CFDataGetBytePtr(data.0), len) }.to_vec())
    }
    fn write(&self, text: &str) -> Result<()> {
        let status = unsafe { PasteboardClear(self.0) };
        if status != 0 {
            bail!("cannot clear the pasteboard (OSStatus {status})");
        }
        // Clearing takes ownership; synchronize before adding the item.
        unsafe { PasteboardSynchronize(self.0) };
        let flavor = Owned::string("public.utf8-plain-text");
        let data =
            Owned::new(unsafe { CFDataCreate(ptr::null(), text.as_ptr(), text.len() as isize) })
                .context("cannot allocate pasteboard data")?;
        let status = unsafe { PasteboardPutItemFlavor(self.0, 1 as Ref, flavor.0, data.0, 0) };
        if status != 0 {
            bail!("cannot write the pasteboard (OSStatus {status})");
        }
        Ok(())
    }
    /// Put the user's text back only while the pasteboard still holds ours, so
    /// another app's newer copy is never clobbered.
    fn restore(&self, previous: &str) {
        if self.owned() {
            let _ = self.write(previous);
        }
    }
}
impl Drop for Pasteboard {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}
fn clipboard() -> Result<std::sync::MutexGuard<'static, Pasteboard>> {
    static SHARED: OnceLock<std::sync::Mutex<Pasteboard>> = OnceLock::new();
    if SHARED.get().is_none() {
        let opened = Pasteboard::open(CLIPBOARD)?;
        let _ = SHARED.set(std::sync::Mutex::new(opened));
    }
    let shared = SHARED.get().context("clipboard not initialized")?;
    Ok(shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}
/// Read the user's text, bounded like the 2 s pbpaste it replaces: an owner
/// that promised its data and is busy blocks the read until it answers. A fresh
/// handle on a detached thread, so an abandoned read never holds the shared
/// handle, a runtime thread, or the daemon's exit.
// ponytail: an abandoned read resumes whenever the hung owner answers and may
// then overlap shared-handle calls, which HIServices does not promise are safe.
// Rare (hung owner + overlap); a single pasteboard thread with a cancellable
// read queue is the upgrade if it ever shows up in crash logs.
async fn read_previous(name: String) -> Option<String> {
    let (reply, read) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = reply.send(Pasteboard::open(&name).ok().and_then(|p| p.read()));
    });
    tokio::time::timeout(Duration::from_secs(2), read)
        .await
        .ok()?
        .ok()?
}

#[derive(Default)]
struct Focus {
    context: AppContext,
    /// None: the app exposes no usable focused element (e.g. web content).
    editable: Option<bool>,
    secure: bool,
    element: Option<Owned>,
}
fn focus() -> Focus {
    static TIMEOUT: Once = Once::new();
    let mut focus = Focus::default();
    let mut psn = ProcessSerialNumber::default();
    let mut pid = 0;
    // Process Manager avoids loading AppKit just to identify the frontmost app;
    // GetProcessPID also connects the services system-wide AX focus queries need.
    if unsafe { GetFrontProcess(&mut psn) != 0 || GetProcessPID(&psn, &mut pid) != 0 } {
        return focus;
    }
    let trusted = unsafe { AXIsProcessTrusted() };
    if trusted {
        if let Some(system) = Owned::new(unsafe { AXUIElementCreateSystemWide() }) {
            // The system-wide timeout bounds every AX call, so a stalled app cannot block delivery.
            TIMEOUT.call_once(|| unsafe {
                AXUIElementSetMessagingTimeout(system.0, 0.2);
            });
            // Keyboard focus, not the front process: Spotlight/Raycast-style panels
            // own the key window while the app behind them stays frontmost.
            if let Some(app) = system.attribute("AXFocusedApplication") {
                let (mut key_pid, mut key_psn) = (0, ProcessSerialNumber::default());
                if unsafe {
                    AXUIElementGetPid(app.0, &mut key_pid) == 0
                        && GetProcessForPID(key_pid, &mut key_psn) == 0
                } {
                    (pid, psn) = (key_pid, key_psn);
                }
            }
        }
    }
    if let Some(info) = Owned::new(unsafe { ProcessInformationCopyDictionary(&psn, u32::MAX) }) {
        for key in ["CFBundleIdentifier", "CFBundleName"] {
            let key = Owned::string(key);
            if let Some(app) = cf_text(unsafe { CFDictionaryGetValue(info.0, key.0) }) {
                focus.context.app_id = Some(app);
                break;
            }
        }
    }
    if focus.context.app_id.as_deref() == Some("com.apple.finder") {
        focus.editable = Some(false);
    }
    if !trusted {
        return focus;
    }
    let Some(app) = Owned::new(unsafe { AXUIElementCreateApplication(pid) }) else {
        return focus;
    };
    let role_of = |e: &Option<Owned>| e.as_ref().and_then(|e| e.text("AXRole"));
    let mut element = app.attribute("AXFocusedUIElement");
    let mut role = role_of(&element);
    // Electron and Chromium build their AX tree only once an assistive client asks;
    // Chrome builds it asynchronously even while reporting the attribute unsupported.
    if role.as_deref().is_none_or(is_container) {
        app.enable("AXManualAccessibility");
        for _ in 0..6 {
            std::thread::sleep(Duration::from_millis(50));
            element = app.attribute("AXFocusedUIElement");
            role = role_of(&element);
            if !role.as_deref().is_none_or(is_container) {
                break;
            }
        }
    }
    let window = element
        .as_ref()
        .and_then(|e| e.attribute("AXWindow"))
        .or_else(|| app.attribute("AXFocusedWindow"));
    focus.context.window_id = Some(format!(
        "{pid}:{}",
        window.as_ref().map_or(0, |w| unsafe { CFHash(w.0) })
    ));
    let Some(element) = element else {
        return focus;
    };
    let role = role.unwrap_or_default();
    focus.secure = role == "AXSecureTextField"
        || element.text("AXSubrole").as_deref() == Some("AXSecureTextField")
        || element.flag("AXProtectedContent") == Some(true);
    focus.editable = if focus.secure
        || element.flag("AXEnabled") == Some(false)
        || element.flag("AXEditable") == Some(false)
    {
        Some(false)
    } else if element.flag("AXEditable") == Some(true)
        || element.settable("AXSelectedText")
        || ((is_container(&role)
            || matches!(
                role.as_str(),
                "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField"
            ))
            && (element.settable("AXValue") || is_terminal(focus.context.app_id.as_deref())))
    {
        Some(true)
    } else if is_container(&role) && focus.context.app_id.as_deref() != Some("com.apple.finder") {
        None
    } else {
        Some(false)
    };
    focus.element = Some(element);
    focus
}
fn is_container(role: &str) -> bool {
    matches!(
        role,
        "" | "AXWebArea" | "AXGroup" | "AXWindow" | "AXScrollArea" | "AXUnknown" | "AXApplication"
    )
}
fn is_terminal(app: Option<&str>) -> bool {
    let app = app.unwrap_or_default().to_ascii_lowercase();
    [
        "terminal",
        "iterm",
        "ghostty",
        "warp",
        "kitty",
        "alacritty",
        "wezterm",
        "hyper",
    ]
    .iter()
    .any(|t| app.contains(t))
}
#[derive(Debug, PartialEq)]
enum Decision {
    /// An editable field: paste, then restore the user's clipboard.
    Paste,
    /// Editability unknown (canvas apps, games): paste and keep the text copied.
    PasteAndCopy,
    Type,
    Copy,
    Refuse,
}
fn decision(text: &str, method: InjectionMethod, target: &AppContext, current: &Focus) -> Decision {
    if method == InjectionMethod::Clipboard {
        return Decision::Copy;
    }
    if current.secure {
        return Decision::Refuse;
    }
    // Unknown editability pastes blind, like Wispr Flow; a changed window or a
    // known non-editable element copies instead.
    if target.app_id.as_deref().is_none_or(str::is_empty)
        || current.context.app_id.as_deref() == Some("com.apple.loginwindow")
        || target.window_id.is_none()
        || target.app_id != current.context.app_id
        || target.window_id != current.context.window_id
        || current.editable == Some(false)
        // Terminal control characters can submit commands; leave them on the clipboard.
        || (is_terminal(current.context.app_id.as_deref()) && text.chars().any(char::is_control))
    {
        return Decision::Copy;
    }
    if method == InjectionMethod::Type {
        Decision::Type
    } else if current.editable.is_none() {
        Decision::PasteAndCopy
    } else {
        Decision::Paste
    }
}
/// A field's character count, value and selected-text length (UTF-16 units).
type Snapshot = (Option<i64>, Option<String>, i64);
fn snapshot(field: &Owned) -> Snapshot {
    let selected = field
        .text("AXSelectedText")
        .map_or(0, |s| s.encode_utf16().count() as i64);
    (
        field.number("AXNumberOfCharacters"),
        field.text("AXValue"),
        selected,
    )
}
fn content_len(s: &Snapshot) -> Option<i64> {
    s.1.as_deref()
        .map(|v| v.encode_utf16().count() as i64)
        .or(s.0)
}
/// The field shows the paste: one more occurrence of the text, or the selection
/// replaced by it. Restore only on this receipt, never on a timer alone (#53).
fn inserted(before: &Snapshot, after: &Snapshot, text: &str) -> bool {
    let replaced = content_len(before)
        .zip(content_len(after))
        .is_some_and(|(b, a)| a == b - before.2 + text.encode_utf16().count() as i64);
    match (&before.1, &after.1) {
        (Some(b), Some(a)) => {
            a.matches(text).count() > b.matches(text).count() || (replaced && a.contains(text))
        }
        (None, _) => replaced,
        (Some(_), None) => false,
    }
}
/// After Cmd+V the delivery lock is held until the receipt or this bound, so no
/// later clipboard write replaces text a slow app has yet to read.
const PENDING_PASTE: Duration = Duration::from_secs(5);
/// How long a reply waits for the receipt. Counted from the request's start,
/// including any wait for an earlier pending paste, so any accepted
/// restore_delay_ms replies inside the 10 s IPC request timeout.
fn receipt_budget(restore_delay_ms: u32, started: Instant) -> Duration {
    Duration::from_millis(u64::from(restore_delay_ms) + 2000)
        .min(Duration::from_secs(4))
        .min((started + Duration::from_secs(8)).saturating_duration_since(Instant::now()))
}
/// Shared by every MacDesktop: a config reload must not drop a pending paste's lock.
static DELIVERY: std::sync::LazyLock<std::sync::Arc<tokio::sync::Mutex<()>>> =
    std::sync::LazyLock::new(Default::default);
fn send_keys(text: &str, typed: bool) -> Result<()> {
    if !unsafe { CGPreflightPostEventAccess() } {
        bail!("Accessibility permission is required for text delivery; run xflow doctor");
    }
    const COMMAND: u64 = 1 << 20;
    let key = |code, down, flags| {
        let event = Owned::new(unsafe { CGEventCreateKeyboardEvent(ptr::null(), code, down) })
            .context("cannot create keyboard event")?;
        unsafe { CGEventSetFlags(event.0, flags) };
        Ok::<_, anyhow::Error>(event)
    };
    // Allocate every event before posting so failure cannot leave a key down.
    if typed {
        let (down, up) = (key(0, true, 0)?, key(0, false, 0)?);
        for ch in text.chars() {
            let mut buf = [0; 2];
            let utf16 = ch.encode_utf16(&mut buf);
            for event in [&down, &up] {
                unsafe {
                    CGEventKeyboardSetUnicodeString(event.0, utf16.len(), utf16.as_ptr());
                    CGEventPost(0, event.0);
                }
            }
        }
    } else {
        // A real ⌘ press and release: a V that only carries the ⌘ flag can leave ⌘
        // held in the HID state, so later pastes misfire and keys become shortcuts.
        let v = paste_keycode();
        let events = [
            key(55, true, COMMAND)?,
            key(v, true, COMMAND)?,
            key(v, false, COMMAND)?,
            key(55, false, 0)?,
        ];
        for event in &events {
            unsafe { CGEventPost(0, event.0) };
        }
    }
    Ok(())
}

/// Virtual key that yields "v" with Command held in the current layout (Dvorak,
/// AZERTY, "… QWERTY ⌘"); kVK_ANSI_V when the layout has none (e.g. Cyrillic).
/// Carbon is loaded on first paste only, keeping it out of the idle daemon.
fn paste_keycode() -> u16 {
    type CopySource = unsafe extern "C" fn() -> Ref;
    type GetProperty = unsafe extern "C" fn(Ref, Ref) -> Ref;
    type KbdType = unsafe extern "C" fn() -> u8;
    type Translate = unsafe extern "C" fn(
        *const u8,
        u16,
        u16,
        u32,
        u32,
        u32,
        *mut u32,
        usize,
        *mut usize,
        *mut u16,
    ) -> i32;
    struct Carbon(CopySource, GetProperty, KbdType, Translate, Ref);
    unsafe impl Send for Carbon {}
    unsafe impl Sync for Carbon {}
    extern "C" {
        fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    const ANSI_V: u16 = 9;
    static CARBON: OnceLock<Option<Carbon>> = OnceLock::new();
    let carbon = CARBON.get_or_init(|| unsafe {
        let lib = dlopen(
            c"/System/Library/Frameworks/Carbon.framework/Carbon".as_ptr(),
            1,
        );
        let sym = |name: &CStr| {
            (!lib.is_null())
                .then(|| dlsym(lib, name.as_ptr()))
                .filter(|p| !p.is_null())
        };
        let key = sym(c"kTISPropertyUnicodeKeyLayoutData")?;
        Some(Carbon(
            std::mem::transmute::<*mut c_void, CopySource>(sym(
                c"TISCopyCurrentKeyboardLayoutInputSource",
            )?),
            std::mem::transmute::<*mut c_void, GetProperty>(sym(c"TISGetInputSourceProperty")?),
            std::mem::transmute::<*mut c_void, KbdType>(sym(c"LMGetKbdType")?),
            std::mem::transmute::<*mut c_void, Translate>(sym(c"UCKeyTranslate")?),
            *(key as *const Ref),
        ))
    });
    let Some(Carbon(copy_source, property, kbd_type, translate, layout_key)) = carbon else {
        return ANSI_V;
    };
    unsafe {
        let Some(source) = Owned::new(copy_source()) else {
            return ANSI_V;
        };
        let data = property(source.0, *layout_key);
        if data.is_null() {
            return ANSI_V;
        }
        let layout = CFDataGetBytePtr(data);
        let translates_to_v = |key: u16| {
            let (mut dead, mut len, mut out) = (0, 0, [0_u16; 4]);
            // cmdKey >> 8; kUCKeyActionDown; kUCKeyTranslateNoDeadKeysMask.
            translate(
                layout,
                key,
                0,
                1,
                kbd_type().into(),
                1,
                &mut dead,
                4,
                &mut len,
                out.as_mut_ptr(),
            ) == 0
                && out[..len] == [u16::from(b'v')]
        };
        std::iter::once(ANSI_V)
            .chain(0..128)
            .find(|&key| translates_to_v(key))
            .unwrap_or(ANSI_V)
    }
}

pub struct MacDesktop {
    config: InjectionConfig,
    /// Serializes clipboard writes, including a pending paste (see PENDING_PASTE).
    delivery: std::sync::Arc<tokio::sync::Mutex<()>>,
}
impl MacDesktop {
    pub fn new(config: &InjectionConfig) -> Self {
        // Preflight only (never prompts); launchd logs make the fallback explainable.
        if !unsafe { AXIsProcessTrusted() } {
            eprintln!("xflowd [warn]: Accessibility is not granted to this executable; transcripts are copied instead of pasted (System Settings > Privacy & Security > Accessibility)");
        }
        Self {
            config: config.clone(),
            delivery: DELIVERY.clone(),
        }
    }
    fn write(&self, text: &str) -> Result<()> {
        if text.len() > MAX_TEXT {
            bail!("injection text exceeds 64 KiB");
        }
        clipboard()?.write(text)
    }
}
#[async_trait]
impl Desktop for MacDesktop {
    async fn context(&self) -> Result<AppContext> {
        tokio::task::spawn_blocking(|| Ok(focus().context)).await?
    }
    async fn selection(&self) -> Result<Option<String>> {
        tokio::task::spawn_blocking(|| {
            let focus = focus();
            Ok(if focus.secure {
                None
            } else {
                focus.element.and_then(|e| e.text("AXSelectedText"))
            })
        })
        .await?
    }
    async fn watch_field(&self, target: &AppContext) -> Option<Box<dyn FieldWatch>> {
        let target = target.clone();
        let watch = tokio::task::spawn_blocking(move || observe::watch(&target))
            .await
            .ok()??;
        Some(Box::new(watch))
    }
    async fn copy(&self, text: &str) -> Result<()> {
        let _delivery = self.delivery.lock().await;
        self.write(text)
    }
    async fn inject(&self, text: &str, target: &AppContext) -> Result<InjectionOutcome> {
        if text.len() > MAX_TEXT {
            bail!("injection text exceeds 64 KiB");
        }
        let started = Instant::now();
        let delivery = self.delivery.clone().lock_owned().await;
        let method = self.config.effective_method();
        let target_owned = target.clone();
        let text_owned = text.to_owned();
        let action = tokio::task::spawn_blocking(move || {
            decision(&text_owned, method, &target_owned, &focus())
        })
        .await?;
        match action {
            Decision::Refuse => bail!("refusing delivery into a secure field"),
            Decision::Copy => {
                self.write(text)?;
                return Ok(InjectionOutcome::ClipboardOnly);
            }
            _ => {}
        }
        // shortcut: restores text only; preserve native pasteboard formats in #27.
        let previous = if action == Decision::Paste && self.config.restore_clipboard {
            read_previous(CLIPBOARD.into()).await
        } else {
            None
        };
        if action != Decision::Type {
            self.write(text)?;
            // Cmd+V only while the pasteboard holds the transcript, never another writer's text.
            if !clipboard()?.owned() {
                self.write(text)?;
                return Ok(InjectionOutcome::ClipboardOnly);
            }
        }
        let target = target.clone();
        let text_owned = text.to_owned();
        let restore = previous.is_some();
        let restore_delay_ms = self.config.restore_delay_ms;
        let (reply, outcome) = tokio::sync::oneshot::channel();
        // Re-check focus just before the keys; the lock then stays with the pending paste.
        tokio::task::spawn_blocking(move || {
            let _delivery = delivery;
            let current = focus();
            let action = decision(&text_owned, method, &target, &current);
            if matches!(action, Decision::Refuse | Decision::Copy) {
                let copied = clipboard().and_then(|c| c.write(&text_owned));
                return drop(reply.send(copied.map(|()| (Decision::Copy, false))));
            }
            let field = current
                .element
                .as_ref()
                .map(|f| (f, snapshot(f)))
                .filter(|(_, before)| before.0.is_some() || before.1.is_some());
            if let Err(e) = send_keys(&text_owned, action == Decision::Type) {
                return drop(reply.send(Err(e)));
            }
            if action == Decision::Type {
                return drop(reply.send(Ok((action, false))));
            }
            // A known-editable field showing no content may never show the paste
            // (VS Code exposes an empty AXValue): report pasted at once, restore on
            // receipt or at the PENDING_PASTE bound.
            let opaque = field
                .as_ref()
                .is_none_or(|(_, before)| content_len(before).unwrap_or(0) == 0);
            let restore_previous = || {
                if let (Some(previous), Ok(clipboard)) = (&previous, clipboard()) {
                    clipboard.restore(previous);
                }
            };
            let mut restored = false;
            await_paste(
                || {
                    field
                        .as_ref()
                        .is_some_and(|(f, before)| inserted(before, &snapshot(f), &text_owned))
                },
                if opaque || !restore {
                    Duration::ZERO
                } else {
                    receipt_budget(restore_delay_ms, started)
                },
                |confirmed| {
                    if confirmed {
                        restore_previous();
                        restored = true;
                    }
                    let _ = reply.send(Ok((action, confirmed || opaque)));
                },
            );
            if opaque && !restored {
                restore_previous();
            }
        });
        let (action, confirmed) = outcome.await.context("delivery task stopped")??;
        Ok(match action {
            Decision::Type => InjectionOutcome::Typed,
            Decision::Paste if confirmed || !restore => InjectionOutcome::Pasted,
            _ => InjectionOutcome::ClipboardOnly,
        })
    }
}

/// Poll `landed` while the caller holds the delivery lock. `report` answers the
/// request once, on receipt or after `budget`; returning releases the lock, on
/// receipt or after PENDING_PASTE.
fn await_paste(mut landed: impl FnMut() -> bool, budget: Duration, report: impl FnOnce(bool)) {
    let start = Instant::now();
    let mut report = Some(report);
    loop {
        let done = landed();
        if done || start.elapsed() >= budget {
            if let Some(report) = report.take() {
                report(done);
            }
        }
        if done || start.elapsed() >= PENDING_PASTE {
            return;
        }
        // Wake at the next deadline rather than up to a poll late.
        let deadline = if report.is_some() {
            budget
        } else {
            PENDING_PASTE
        };
        std::thread::sleep(Duration::from_millis(25).min(deadline.saturating_sub(start.elapsed())));
    }
}

/// Nonprompting preflights; called inside xflowd for its IPC diagnostics.
pub fn permissions() -> xflow_core::ipc::Permissions {
    xflow_core::ipc::Permissions {
        accessibility: unsafe { AXIsProcessTrusted() },
        // kIOHIDRequestTypeListenEvent = 1; kIOHIDAccessTypeGranted = 0.
        input_monitoring: unsafe { IOHIDCheckAccess(1) == 0 && CGPreflightListenEventAccess() },
        keyboard_events: None,
        microphone: None,
    }
}

/// Ask macOS for Input Monitoring or Accessibility: this lists the executable in
/// System Settings and shows the system prompt while undecided. Call off the runtime.
pub fn request_access(grant: xflow_core::ipc::Grant) {
    use xflow_core::ipc::Grant;
    match grant {
        // kIOHIDRequestTypeListenEvent = 1.
        Grant::InputMonitoring => unsafe {
            IOHIDRequestAccess(1);
        },
        Grant::Accessibility => unsafe {
            let options = CFDictionaryCreate(
                ptr::null(),
                &kAXTrustedCheckOptionPrompt,
                &kCFBooleanTrue,
                1,
                &kCFTypeDictionaryKeyCallBacks as *const u8 as Ref,
                &kCFTypeDictionaryValueCallBacks as *const u8 as Ref,
            );
            AXIsProcessTrustedWithOptions(options);
            if !options.is_null() {
                CFRelease(options);
            }
        },
        Grant::Microphone => {
            microphone_permission(true);
        }
    }
}

/// AVFoundation microphone authorization; `request` asks while undecided and waits
/// up to a minute for the answer. AVFoundation is heavy: xflowd runs this in a
/// short-lived child (`xflowd --microphone`) so its idle image stays free of it.
pub fn microphone_permission(request: bool) -> &'static str {
    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Ref;
        fn sel_registerName(name: *const c_char) -> Ref;
        fn objc_msgSend();
    }
    extern "C" {
        fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
        static _NSConcreteGlobalBlock: u8;
    }
    // A global block literal (Clang block ABI) for the completion handler; the
    // answer is read back through authorizationStatus instead.
    #[repr(C)]
    struct Block {
        isa: *const u8,
        flags: i32,
        reserved: i32,
        invoke: unsafe extern "C" fn(*const Block, bool),
        descriptor: *const [usize; 2],
    }
    unsafe extern "C" fn answered(_block: *const Block, _granted: bool) {}
    static DESCRIPTOR: [usize; 2] = [0, std::mem::size_of::<Block>()];
    unsafe {
        // Never unloaded: AVFoundation cannot be safely dlclosed once its classes load.
        let library = dlopen(
            c"/System/Library/Frameworks/AVFoundation.framework/AVFoundation".as_ptr(),
            1,
        );
        let class = objc_getClass(c"AVCaptureDevice".as_ptr());
        if library.is_null() || class.is_null() {
            return "unavailable";
        }
        let status: unsafe extern "C" fn(Ref, Ref, Ref) -> isize =
            std::mem::transmute(objc_msgSend as *const ());
        let audio = Owned::string("soun");
        let read = || {
            status(
                class,
                sel_registerName(c"authorizationStatusForMediaType:".as_ptr()),
                audio.0,
            )
        };
        if request && read() == 0 {
            let ask: unsafe extern "C" fn(Ref, Ref, Ref, *const Block) =
                std::mem::transmute(objc_msgSend as *const ());
            let block: &'static Block = Box::leak(Box::new(Block {
                isa: &_NSConcreteGlobalBlock,
                flags: 1 << 28, // BLOCK_IS_GLOBAL
                reserved: 0,
                invoke: answered,
                descriptor: &DESCRIPTOR,
            }));
            ask(
                class,
                sel_registerName(c"requestAccessForMediaType:completionHandler:".as_ptr()),
                audio.0,
                block,
            );
            let deadline = Instant::now() + Duration::from_secs(60);
            while read() == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        match read() {
            0 => "not determined",
            1 => "restricted",
            2 => "denied",
            3 => "authorized",
            _ => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "types a marker into the frontmost TextEdit document; run explicitly on macOS"]
    async fn native_textedit_paste() {
        let desktop = MacDesktop::new(&InjectionConfig::default());
        let previous = read_previous(CLIPBOARD.into()).await.unwrap();
        let context = desktop.context().await.unwrap();
        assert_eq!(
            context.app_id.as_deref(),
            Some("com.apple.TextEdit"),
            "focus a scratch TextEdit document first"
        );
        let mut field = desktop
            .watch_field(&context)
            .await
            .expect("focused element must be editable");
        assert_eq!(
            desktop
                .inject("xflow macOS delivery check", &context)
                .await
                .unwrap(),
            InjectionOutcome::Pasted
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(field
            .read()
            .await
            .unwrap()
            .value
            .contains("xflow macOS delivery check"));
        assert_eq!(read_previous(CLIPBOARD.into()).await.unwrap(), previous);
    }
    /// A private named pasteboard, so tests never touch the user's clipboard.
    fn scratch(test: &str) -> (String, Pasteboard, std::sync::MutexGuard<'static, ()>) {
        // HIServices state is process-wide and not thread safe: one test at a time.
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let serial = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let name = format!("com.xflow.test.{}.{test}", std::process::id());
        let pasteboard = Pasteboard::open(&name).unwrap();
        (name, pasteboard, serial)
    }
    extern "C" {
        fn PasteboardCopyItemFlavors(pasteboard: Ref, item: Ref, flavors: *mut Ref) -> i32;
        fn CFArrayGetCount(array: Ref) -> isize;
    }
    /// Whether the first item lists any flavor, without fetching (and so
    /// without resolving) a promise.
    fn published(pasteboard: &Pasteboard) -> bool {
        let (mut count, mut item, mut flavors) = (0, ptr::null(), ptr::null());
        unsafe {
            PasteboardSynchronize(pasteboard.0);
            PasteboardGetItemCount(pasteboard.0, &mut count) == 0
                && count > 0
                && PasteboardGetItemIdentifier(pasteboard.0, 1, &mut item) == 0
                && PasteboardCopyItemFlavors(pasteboard.0, item, &mut flavors) == 0
                && Owned::new(flavors).is_some_and(|flavors| CFArrayGetCount(flavors.0) > 0)
        }
    }
    /// Run JXA as another (AppKit) app against pasteboard `name`, bound to `p`.
    fn appkit(name: &str, script: &str) -> std::process::Command {
        let mut command = std::process::Command::new("/usr/bin/osascript");
        command
            .args(["-l", "JavaScript", "-e"])
            .arg(format!(
                "ObjC.import('AppKit'); var p = $.NSPasteboard.pasteboardWithName('{name}'); {script}"
            ))
            .stdout(std::process::Stdio::null());
        command
    }
    #[test]
    fn pasteboard_round_trips_text_in_process() {
        let (_, pasteboard, _serial) = scratch("round-trip");
        for text in ["", "fix `cargo` — naïve 🚀\n  indented\r\n"] {
            pasteboard.write(text).unwrap();
            assert!(pasteboard.owned());
            assert_eq!(pasteboard.read().as_deref(), Some(text));
        }
        pasteboard.write(&"x".repeat(MAX_TEXT + 1)).unwrap();
        assert_eq!(pasteboard.read(), None, "oversized text is never restored");
        pasteboard.write("").unwrap();
    }
    #[test]
    fn read_matches_pbpaste_for_utf16_and_textless_clipboards() {
        let (name, pasteboard, _serial) = scratch("flavors");
        for (script, expected) in [
            (
                "p.setDataForType($('héllo utf16').dataUsingEncoding($.NSUTF16StringEncoding), \
                 'public.utf16-plain-text')",
                "héllo utf16",
            ),
            ("p.setDataForType($.NSData.data, 'public.tiff')", ""),
        ] {
            let status = appkit(&name, &format!("p.clearContents; {script}"))
                .status()
                .unwrap();
            assert!(status.success());
            assert_eq!(pasteboard.read().as_deref(), Some(expected), "{script}");
        }
        pasteboard.write("").unwrap();
    }
    #[test]
    fn restore_never_clobbers_another_apps_newer_copy() {
        let (name, pasteboard, _serial) = scratch("restore");
        pasteboard.write("transcript").unwrap();
        pasteboard.restore("previous");
        assert_eq!(pasteboard.read().as_deref(), Some("previous"));

        pasteboard.write("transcript").unwrap();
        // A real AppKit app in another process copies while the paste is pending.
        let script = "p.clearContents; p.setStringForType('newer copy', $.NSPasteboardTypeString)";
        assert!(appkit(&name, script).status().unwrap().success());
        assert!(!pasteboard.owned());
        pasteboard.restore("previous");
        assert_eq!(pasteboard.read().as_deref(), Some("newer copy"));
    }
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // the guard serializes HIServices tests, not tasks
    async fn a_busy_owners_promise_bounds_the_read_and_never_stalls_writes() {
        let (name, pasteboard, _serial) = scratch("promise");
        pasteboard.write("ours").unwrap();
        // The owner promises text, then hangs (SIGSTOP) once pboard publishes the
        // promise: declareTypes reaches pboard asynchronously, and a live owner's
        // run loop would answer the request with nothing.
        struct Reap(std::process::Child);
        impl Drop for Reap {
            // SIGKILL also ends a stopped owner, even when an assertion fails.
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let owner = Reap(
            appkit(
                &name,
                "p.declareTypesOwner($(['public.utf8-plain-text']), $.NSObject.alloc.init); \
                 delay(10)",
            )
            .spawn()
            .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while pasteboard.owned() || !published(&pasteboard) {
            assert!(
                Instant::now() < deadline,
                "the owner never published its promise"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let stopped = std::process::Command::new("/bin/kill")
            .args(["-STOP", &owner.0.id().to_string()])
            .status()
            .unwrap();
        assert!(stopped.success());
        let started = Instant::now();
        let read = read_previous(name).await;
        let took = started.elapsed();
        assert_eq!(read, None, "a stuck read restores nothing");
        assert!(
            (Duration::from_millis(1900)..Duration::from_millis(2500)).contains(&took),
            "None must come from the 2 s bound: {took:?}"
        );
        // The paste proceeds on the shared handle while the abandoned read is stuck.
        let write = Instant::now();
        pasteboard.write("transcript").unwrap();
        assert!(pasteboard.owned());
        assert!(write.elapsed() < Duration::from_millis(500));
        drop(owner);
        // Let the abandoned read return before the next test touches HIServices.
        std::thread::sleep(Duration::from_millis(300));
        let _ = pasteboard.write("");
    }
    #[test]
    fn editability_selects_paste_copy_or_both() {
        let target = AppContext {
            app_id: Some("app".into()),
            window_id: Some("1:2".into()),
            selected_text: None,
        };
        for (editable, expected) in [
            (Some(true), Decision::Paste),
            (Some(false), Decision::Copy),
            (None, Decision::PasteAndCopy),
        ] {
            let current = Focus {
                context: target.clone(),
                editable,
                ..Default::default()
            };
            assert_eq!(
                decision("hello", InjectionMethod::Paste, &target, &current),
                expected
            );
        }
    }
    #[test]
    fn receipt_accepts_insertion_and_selection_replacement() {
        let s = |chars: Option<i64>, value: Option<&str>, selected| {
            (chars, value.map(str::to_owned), selected)
        };
        let world = s(Some(11), Some("hello world"), 11);
        assert!(inserted(&world, &s(Some(5), Some("hello"), 0), "hello"));
        assert!(inserted(
            &s(None, Some("hello world"), 0),
            &s(None, Some("hello worldhello"), 0),
            "hello"
        ));
        assert!(!inserted(&world, &world, "hello"));
        // Unreadable value: the character count must account for the replaced selection.
        assert!(inserted(
            &s(Some(11), None, 11),
            &s(Some(5), None, 0),
            "hello"
        ));
        assert!(!inserted(
            &s(Some(11), None, 0),
            &s(Some(5), None, 0),
            "hello"
        ));
    }
    #[test]
    fn pending_paste_holds_the_clipboard_for_a_delayed_reader() {
        use std::sync::{mpsc, Arc, Mutex};
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        let clipboard = Arc::new(Mutex::new("A"));
        let pasted = Arc::new(Mutex::new(None));
        // The app reads the clipboard after the receipt budget has expired.
        let app = {
            let (clipboard, pasted) = (clipboard.clone(), pasted.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(250));
                *pasted.lock().unwrap() = Some(*clipboard.lock().unwrap());
            })
        };
        let (reply, replied) = mpsc::channel();
        let guard = lock.clone().try_lock_owned().unwrap();
        let landed = pasted.clone();
        let a = std::thread::spawn(move || {
            let _guard = guard;
            await_paste(
                || landed.lock().unwrap().is_some(),
                Duration::from_millis(50),
                |confirmed| reply.send(confirmed).unwrap(),
            );
        });
        assert!(!replied.recv().unwrap(), "unconfirmed within the budget");
        // A second injection writes the clipboard only once it holds the lock.
        *clipboard.lock().unwrap() = {
            let _b = lock.blocking_lock();
            "B"
        };
        app.join().unwrap();
        a.join().unwrap();
        assert_eq!(*pasted.lock().unwrap(), Some("A"));
    }
    #[test]
    fn unconfirmable_paste_replies_within_ipc_timeout_and_holds_until_bound() {
        let mut config = xflow_core::config::Config::default();
        config.injection.restore_delay_ms = 10_000;
        config.validate().unwrap();
        let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let guard = lock.clone().try_lock_owned().unwrap();
        let start = Instant::now();
        let (reply, replied) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _guard = guard;
            await_paste(
                || false,
                receipt_budget(config.injection.restore_delay_ms, start),
                |confirmed| reply.send((confirmed, start.elapsed())).unwrap(),
            );
        });
        let (confirmed, at) = replied.recv().unwrap();
        // Leaves the 10 s IPC timeout room for focus checks and clipboard commands.
        assert!(!confirmed && at <= Duration::from_millis(4100), "{at:?}");
        drop(lock.blocking_lock());
        assert!(start.elapsed() >= PENDING_PASTE);
    }
    #[test]
    fn delivery_lock_survives_reload_and_budget_counts_lock_wait() {
        let config = InjectionConfig::default();
        // A config reload builds a new desktop while a paste may still be pending.
        assert!(std::sync::Arc::ptr_eq(
            &MacDesktop::new(&config).delivery,
            &MacDesktop::new(&config).delivery
        ));
        // 5 s spent waiting for an earlier pending paste leaves at most 3 s.
        let waited = Instant::now() - Duration::from_secs(5);
        assert!(receipt_budget(10_000, waited) <= Duration::from_secs(3));
        let waited = Instant::now() - Duration::from_secs(9);
        assert_eq!(receipt_budget(10_000, waited), Duration::ZERO);
    }
    #[test]
    fn delivery_requires_same_window_nonsecure_focus_and_safe_terminal_input() {
        use Decision::*;
        use InjectionMethod::{Clipboard, Paste as P, Type as T};
        let target = AppContext {
            app_id: Some("com.apple.TextEdit".into()),
            window_id: Some("1:2".into()),
            selected_text: None,
        };
        let mut current = Focus {
            context: target.clone(),
            editable: Some(true),
            ..Default::default()
        };
        assert_eq!(decision("hello", P, &target, &current), Paste);
        assert_eq!(decision("hé🦀", T, &target, &current), Type);
        assert_eq!(decision("hello", Clipboard, &target, &current), Copy);
        current.secure = true;
        assert_eq!(decision("secret", P, &target, &current), Refuse);
        assert_eq!(decision("secret", Clipboard, &target, &current), Copy);
        current.secure = false;
        current.editable = Some(false);
        assert_eq!(decision("hello", P, &target, &current), Copy);
        // Electron/web content without an AX tree: paste blind into the same window.
        current.editable = None;
        assert_eq!(decision("hello", P, &target, &current), PasteAndCopy);
        current.context.window_id = Some("1:3".into());
        assert_eq!(decision("hello", P, &target, &current), Copy);
        current.context.app_id = Some("com.googlecode.iterm2".into());
        let terminal = current.context.clone();
        for text in ["echo hi\n", "echo hi\r", "\u{1b}"] {
            assert_eq!(decision(text, P, &terminal, &current), Copy);
        }
        assert_eq!(decision("echo hi", P, &terminal, &current), PasteAndCopy);
        assert_eq!(decision("hello", P, &AppContext::default(), &current), Copy);
    }
}
