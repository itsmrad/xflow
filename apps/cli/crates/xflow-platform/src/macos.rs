//! Accessibility and CGEvent delivery without NSApplication or an AppKit run loop.
use crate::desktop::{is_terminal, terminal_context, terminal_controls};
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
#[path = "macos_meeting.rs"]
pub(crate) mod meeting;
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
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: Ref) -> isize;
    fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
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
    static kCFBooleanFalse: Ref;
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
    fn CGEventSetIntegerValueField(event: Ref, field: u32, value: i64);
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
    fn set_flag(&self, name: &str, value: bool) -> bool {
        let name = Self::string(name);
        unsafe {
            AXUIElementSetAttributeValue(
                self.0,
                name.0,
                if value {
                    kCFBooleanTrue
                } else {
                    kCFBooleanFalse
                },
            ) == 0
        }
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
    window: Option<Owned>,
    _accessibility: Option<AccessibilityGuard<Owned>>,
    /// Front app that owns the keyboard. 0 when it could not be resolved.
    pid: i32,
}

trait AccessibilityFlag {
    fn read(&self) -> Option<bool>;
    fn write(&self, value: bool) -> bool;
}
impl AccessibilityFlag for Owned {
    fn read(&self) -> Option<bool> {
        self.flag("AXManualAccessibility")
    }
    fn write(&self, value: bool) -> bool {
        self.set_flag("AXManualAccessibility", value)
    }
}
/// Keep Chromium's AX tree alive until the focus/receipt query finishes.
struct AccessibilityGuard<A: AccessibilityFlag> {
    app: A,
    previous: bool,
}
impl<A: AccessibilityFlag> AccessibilityGuard<A> {
    fn enable(app: A) -> Option<Self> {
        // Never change an attribute whose original value cannot be restored.
        let previous = app.read()?;
        if previous {
            return None;
        }
        let guard = Self { app, previous };
        // Retain the guard even on failure: the app may have applied the write.
        guard.app.write(true);
        Some(guard)
    }
}
// SAFETY: AX element references are thread-safe CF objects; the guard only makes AX IPC calls.
unsafe impl Send for AccessibilityGuard<Owned> {}
impl<A: AccessibilityFlag> Drop for AccessibilityGuard<A> {
    fn drop(&mut self) {
        self.app.write(self.previous);
        if self.app.read() != Some(self.previous) {
            eprintln!("xflowd [warn]: could not verify AXManualAccessibility restoration");
        }
    }
}
fn is_chromium(app: Option<&str>) -> bool {
    matches!(
        app,
        Some(
            "com.google.Chrome"
                | "com.google.Chrome.beta"
                | "com.google.Chrome.dev"
                | "com.google.Chrome.canary"
                | "org.chromium.Chromium"
                | "com.microsoft.VSCode"
                | "com.microsoft.VSCodeInsiders"
                | "com.todesktop.230313mzl4w4u92"
                | "com.tinyspeck.slackmacgap"
                | "com.hnc.Discord"
                | "notion.id"
                | "company.thebrowser.Browser"
                | "com.linear"
        )
    )
}
/// Unlisted Electron/Chromium apps (Edge, Brave, Obsidian, Teams, Postman…) by their bundle.
fn chromium_bundle(bundle: &str) -> bool {
    static LAST: std::sync::Mutex<Option<(String, bool)>> = std::sync::Mutex::new(None);
    let path = std::path::Path::new(bundle);
    let home = std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join("Applications"));
    // shortcut: only /Applications and ~/Applications are read; elsewhere (e.g. Downloads) a read
    // can raise a folder-access prompt. Add a fast-path ID for apps commonly run from there.
    if !(path.starts_with("/Applications") || home.is_some_and(|h| path.starts_with(h))) {
        return false;
    }
    let mut last = LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, chromium)) = last.as_ref().filter(|(cached, _)| cached == bundle) {
        return *chromium;
    }
    let chromium = has_chromium_framework(path);
    *last = Some((bundle.to_owned(), chromium));
    chromium
}
/// Electron Framework and every Chromium browser framework ship Chromium's resource pak.
fn has_chromium_framework(bundle: &std::path::Path) -> bool {
    bundle
        .join("Contents/Frameworks")
        .read_dir()
        .into_iter()
        .flatten()
        .flatten()
        .any(|f| f.path().join("Resources/chrome_100_percent.pak").is_file())
}
fn focus() -> Focus {
    focus_waiting(true)
}
/// The front process, which AX focus queries fall back on.
fn front_pid() -> Option<i32> {
    let mut psn = ProcessSerialNumber::default();
    let mut pid = 0;
    (unsafe { GetFrontProcess(&mut psn) == 0 && GetProcessPID(&psn, &mut pid) == 0 }).then_some(pid)
}
/// `wait`: give Electron and Chromium up to 300 ms to build their AX tree.
fn focus_waiting(wait: bool) -> Focus {
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
    focus.pid = pid;
    let mut bundle = None;
    if let Some(info) = Owned::new(unsafe { ProcessInformationCopyDictionary(&psn, u32::MAX) }) {
        let key = Owned::string("BundlePath");
        bundle = cf_text(unsafe { CFDictionaryGetValue(info.0, key.0) });
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
    let chromium = is_chromium(focus.context.app_id.as_deref())
        || bundle.as_deref().is_some_and(chromium_bundle);
    if chromium {
        // Chromium activates native accessibility when an assistive client reads its app role.
        let _ = app.text("AXRole");
    }
    let role_of = |e: &Option<Owned>| e.as_ref().and_then(|e| e.text("AXRole"));
    let mut element = app.attribute("AXFocusedUIElement");
    let mut role = role_of(&element);
    // Electron and Chromium build their AX tree only once an assistive client asks;
    // Poll asynchronous tree creation without Chrome's window-affecting AXEnhancedUserInterface.
    if wait && role.as_deref().is_none_or(is_container) && chromium {
        let enabling_app = Owned::new(unsafe { AXUIElementCreateApplication(pid) });
        focus._accessibility = enabling_app.and_then(AccessibilityGuard::enable);
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
    focus.context.window_id = window
        .as_ref()
        .map(|w| format!("{pid}:{}", unsafe { CFHash(w.0) }));
    focus.window = window;
    let Some(element) = element else {
        return focus;
    };
    let role = role.unwrap_or_default();
    focus.secure = secure(&element);
    focus.context.terminal_input = ["AXTitle", "AXDescription", "AXIdentifier"]
        .iter()
        .filter_map(|name| element.text(name))
        .any(|label| terminal_input_label(&label))
        || element
            .attribute("AXDOMClassList")
            .is_some_and(|classes| dom_class(&classes, "xterm-helper-textarea"));
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
/// A password or otherwise protected field, whose text xflow never reads or pastes into.
fn secure(element: &Owned) -> bool {
    is_secure(
        element.text("AXRole").as_deref(),
        element.text("AXSubrole").as_deref(),
        element.flag("AXProtectedContent"),
    )
}
fn is_secure(role: Option<&str>, subrole: Option<&str>, protected: Option<bool>) -> bool {
    role == Some("AXSecureTextField")
        || subrole == Some("AXSecureTextField")
        || protected == Some(true)
}
/// The selection a hold-to-talk may rewrite: plain editable text only, never a
/// secure, terminal, search or address field.
fn rewrite_selection(focus: &Focus) -> Option<String> {
    let element = focus.element.as_ref()?;
    if !text_reads_allowed(focus) || focus.editable != Some(true) {
        return None;
    }
    let text = element.text("AXSelectedText").filter(|s| !s.is_empty())?;
    let labels = [
        "AXTitle",
        "AXDescription",
        "AXIdentifier",
        "AXDOMIdentifier",
        "AXPlaceholderValue",
    ]
    .iter()
    .filter_map(|name| element.text(name));
    let role = element.text("AXRole");
    let subrole = element.text("AXSubrole");
    // Chromium's omnibox view class names Chrome, Edge and Brave address bars in any language.
    let omnibox = element
        .attribute("AXDOMClassList")
        .is_some_and(|classes| dom_class(&classes, "OmniboxViewViews"));
    (!omnibox && !search_field(role.as_deref(), subrole.as_deref(), labels)).then_some(text)
}
/// Search boxes and browser address bars: a rewrite there would run a query.
fn search_field(
    role: Option<&str>,
    subrole: Option<&str>,
    mut labels: impl Iterator<Item = String>,
) -> bool {
    role == Some("AXSearchField")
        || subrole == Some("AXSearchField")
        || labels.any(|label| {
            let label = label.to_ascii_lowercase();
            ["search", "address", "urlbar"]
                .iter()
                .any(|word| label.contains(word))
        })
}
/// Search boxes and address bars are not watched for corrections (an edit there
/// is a query, not a fix of the dictation): by role, subrole, Chromium's omnibox
/// class, or a label naming one as a whole word, so a Slack channel called
/// #research ("Message #research") is still watched.
fn search_or_address(element: &Owned) -> bool {
    let labels = [
        "AXTitle",
        "AXDescription",
        "AXIdentifier",
        "AXDOMIdentifier",
        "AXPlaceholderValue",
    ]
    .iter()
    .filter_map(|name| element.text(name));
    let omnibox = element
        .attribute("AXDOMClassList")
        .is_some_and(|classes| dom_class(&classes, "OmniboxViewViews"));
    omnibox
        || search_label(
            element.text("AXRole").as_deref(),
            element.text("AXSubrole").as_deref(),
            labels,
        )
}
fn search_label(
    role: Option<&str>,
    subrole: Option<&str>,
    mut labels: impl Iterator<Item = String>,
) -> bool {
    role == Some("AXSearchField")
        || subrole == Some("AXSearchField")
        || labels.any(|label| {
            label
                .to_ascii_lowercase()
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|word| matches!(word, "search" | "address" | "urlbar"))
        })
}
/// Labels, never AXValue: terminal contents may include secrets.
fn terminal_input_label(label: &str) -> bool {
    let label = label.trim().to_ascii_lowercase();
    label == "terminal input" || label == "terminal-input" || label == "xterm-helper-textarea"
}
/// Chromium exposes CSS (and native view) classes as an array, independently of localized labels.
fn dom_class(classes: &Owned, name: &str) -> bool {
    if unsafe { CFGetTypeID(classes.0) != CFArrayGetTypeID() } {
        return false;
    }
    let count = unsafe { CFArrayGetCount(classes.0) };
    (0..=128).contains(&count)
        && (0..count).any(|i| {
            cf_text(unsafe { CFArrayGetValueAtIndex(classes.0, i) }).as_deref() == Some(name)
        })
}
/// Shared by selection, paste receipts and the caret/correction reader.
fn text_reads_allowed(focus: &Focus) -> bool {
    !focus.secure && !terminal_context(&focus.context)
}
fn is_container(role: &str) -> bool {
    matches!(
        role,
        "" | "AXWebArea" | "AXGroup" | "AXWindow" | "AXScrollArea" | "AXUnknown" | "AXApplication"
    )
}
#[derive(Debug, PartialEq)]
enum Decision {
    /// An editable field: paste, then restore the user's clipboard.
    Paste,
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
    // Unknown focus cannot rule out a secure field; leave the transcript copied.
    if target.app_id.as_deref().is_none_or(str::is_empty)
        || current.context.app_id.as_deref() == Some("com.apple.loginwindow")
        || target.window_id.is_none()
        || target.app_id != current.context.app_id
        || target.window_id != current.context.window_id
        || current.editable != Some(true)
        // Terminal control characters can submit commands; leave them on the clipboard.
        || terminal_controls(text, &current.context)
        || target.terminal_input != current.context.terminal_input
    {
        return Decision::Copy;
    }
    if method == InjectionMethod::Type {
        Decision::Type
    } else {
        Decision::Paste
    }
}
/// A field's character count, value and selected-text length (UTF-16 units).
type Snapshot = (Option<i64>, Option<String>, i64);
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
    let equivalent = |s: &str| s.replace('\u{a0}', " ");
    let text = equivalent(text);
    match (&before.1, &after.1) {
        (Some(b), Some(a)) => {
            let (b, a) = (equivalent(b), equivalent(a));
            a.matches(&text).count() > b.matches(&text).count() || (replaced && a.contains(&text))
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
/// A field exposing nothing can show no receipt: reply Uncertain at once. An empty value may be
/// a fresh field or one that never updates (VS Code), so it gets only restore_delay_ms. The
/// delivery lock is still held to PENDING_PASTE either way.
fn reply_budget(before: Option<&Snapshot>, restore_delay_ms: u32, started: Instant) -> Duration {
    let budget = receipt_budget(restore_delay_ms, started);
    match before.and_then(content_len) {
        None => Duration::ZERO,
        Some(0) => budget.min(Duration::from_millis(restore_delay_ms.into())),
        Some(_) => budget,
    }
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
        unsafe {
            CGEventSetFlags(event.0, flags);
            CGEventSetIntegerValueField(
                event.0,
                crate::macos_hotkey::USER_DATA,
                crate::macos_hotkey::INJECTED,
            );
        };
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
/// Read fresh on every paste (~10 µs), so layout switches need no invalidation.
fn paste_keycode() -> u16 {
    carbon()
        .and_then(|carbon| {
            let source = Owned::new(unsafe { (carbon.0)() })?;
            unsafe { source_v_key(carbon, source.0) }
        })
        .unwrap_or(ANSI_V)
}

const ANSI_V: u16 = 9;

/// kVK_ANSI_V when it gives "v", else the first key that does, else kVK_ANSI_V.
fn v_key(is_v: impl Fn(u16) -> bool) -> u16 {
    std::iter::once(ANSI_V)
        .chain(0..128)
        .find(|&key| is_v(key))
        .unwrap_or(ANSI_V)
}

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

/// Carbon is loaded on first paste only, keeping it out of the idle daemon.
fn carbon() -> Option<&'static Carbon> {
    extern "C" {
        fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    static CARBON: OnceLock<Option<Carbon>> = OnceLock::new();
    CARBON
        .get_or_init(|| unsafe {
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
        })
        .as_ref()
}

/// The paste key for a TIS input source; None when it has no Unicode layout data.
unsafe fn source_v_key(carbon: &Carbon, source: Ref) -> Option<u16> {
    let Carbon(_, property, kbd_type, translate, layout_key) = carbon;
    let data = property(source, *layout_key);
    if data.is_null() {
        return None;
    }
    let layout = CFDataGetBytePtr(data);
    Some(v_key(|key| {
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
    }))
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
    async fn context_with_selection(&self) -> Result<AppContext> {
        tokio::task::spawn_blocking(|| {
            let mut focus = focus();
            focus.context.selected_text = rewrite_selection(&focus);
            Ok(focus.context)
        })
        .await?
    }
    async fn selection(&self) -> Result<Option<String>> {
        tokio::task::spawn_blocking(|| {
            let focus = focus();
            Ok(if !text_reads_allowed(&focus) {
                None
            } else {
                focus.element.and_then(|e| e.text("AXSelectedText"))
            })
        })
        .await?
    }
    async fn watch_field(&self, target: &AppContext) -> Option<Box<dyn FieldWatch>> {
        let target = target.clone();
        let watch = tokio::task::spawn_blocking(move || {
            let focus = focus_waiting(false);
            (focus.context.app_id == target.app_id && focus.context.window_id == target.window_id)
                .then(|| observe::Reader::new(&focus))
                .flatten()
                .map(observe::arm)
        })
        .await
        .ok()??;
        Some(Box::new(watch))
    }
    async fn neighbors(&self) -> Option<(String, String)> {
        tokio::task::spawn_blocking(observe::neighbors)
            .await
            .ok()
            .flatten()
    }
    async fn window_title(&self, target: &AppContext) -> Option<String> {
        let target = target.clone();
        tokio::task::spawn_blocking(move || {
            // No waiting for a Chromium tree: the window title is native.
            let focus = focus_waiting(false);
            let same = target.window_id.is_some()
                && focus.context.window_id == target.window_id
                && focus.context.app_id == target.app_id;
            if focus.secure || !same {
                return None;
            }
            focus.window?.text("AXTitle")
        })
        .await
        .ok()
        .flatten()
    }
    async fn copy(&self, text: &str) -> Result<()> {
        let _delivery = self.delivery.lock().await;
        self.write(text)
    }
    async fn inject(&self, text: &str, target: &AppContext) -> Result<InjectionOutcome> {
        Ok(self.deliver(text, target, false).await?.outcome)
    }
    async fn inject_observed(
        &self,
        text: &str,
        target: &AppContext,
    ) -> Result<xflow_core::ObservedDelivery> {
        self.deliver(text, target, true).await
    }
}
impl MacDesktop {
    /// `observe`: also hand back a watch on the exact field, armed before the keys
    /// so an edit right after the paste is still announced. Nobody else starts one.
    async fn deliver(
        &self,
        text: &str,
        target: &AppContext,
        observe: bool,
    ) -> Result<xflow_core::ObservedDelivery> {
        let copied = xflow_core::ObservedDelivery {
            outcome: InjectionOutcome::ClipboardOnly,
            ticket: None,
        };
        if text.len() > MAX_TEXT {
            bail!("injection text exceeds 64 KiB");
        }
        let started = Instant::now();
        let delivery = self.delivery.clone().lock_owned().await;
        let method = self.config.effective_method();
        let target_owned = target.clone();
        let text_owned = text.to_owned();
        // One ManualAccessibility guard spans the pre-check, keys and receipt. Refuse/Copy
        // restore the flag here, off the async runtime.
        // shortcut: a clipboard error (`?`) or cancellation before the worker restores it on the
        // async thread (two AX calls, each capped by the 0.2 s messaging timeout); fine while rare.
        let (action, accessibility, reader, watch) = tokio::task::spawn_blocking(move || {
            let mut focus = focus();
            let action = decision(&text_owned, method, &target_owned, &focus);
            let keys = matches!(action, Decision::Paste | Decision::Type);
            let reader = keys.then(|| observe::Reader::new(&focus)).flatten();
            let watch = reader.clone().filter(|_| observe).map(observe::arm);
            (
                action,
                focus._accessibility.take().filter(|_| keys),
                reader,
                watch,
            )
        })
        .await?;
        match action {
            Decision::Refuse => bail!("refusing delivery into a secure field"),
            Decision::Copy => {
                self.write(text)?;
                return Ok(copied);
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
                // Restore off the runtime, still under the delivery lock.
                let _ = tokio::task::spawn_blocking(move || {
                    drop(watch);
                    drop(accessibility)
                })
                .await;
                return Ok(copied);
            }
        }
        let target = target.clone();
        let text_owned = text.to_owned();
        let restore_delay_ms = self.config.restore_delay_ms;
        let (reply, outcome) = tokio::sync::oneshot::channel();
        // Re-check focus just before the keys; the lock then stays with the pending paste.
        tokio::task::spawn_blocking(move || {
            let _delivery = delivery;
            // Dropped before the lock, so the next paste sees the original flag.
            let mut accessibility = accessibility;
            let current = focus();
            let action = decision(&text_owned, method, &target, &current);
            if action == Decision::Refuse {
                return drop(reply.send(Err(anyhow::anyhow!(
                    "refusing delivery into a secure field"
                ))));
            }
            if action == Decision::Copy {
                let copied = clipboard().and_then(|c| c.write(&text_owned));
                return drop(reply.send(copied.map(|()| (Decision::Copy, false, None))));
            }
            // The exact element and window, or nothing is read and nothing is learned.
            // Receipts have their own gate: they outlive a dropped watch.
            let reader = reader.filter(|r| r.same(&current));
            let gate = xflow_core::ReadGate::default();
            let read = |r: &std::sync::Arc<observe::Reader>| -> Option<Snapshot> {
                let bounded = xflow_core::read_field(&**r, &gate).map(|s| {
                    let selected = s.selection.map_or(0, |(_, len)| len as i64);
                    (
                        Some(s.text.value.encode_utf16().count() as i64),
                        Some(s.text.value),
                        selected,
                    )
                });
                bounded.or_else(|| {
                    r.counts()
                        .map(|(count, selected)| (Some(count as i64), None, selected as i64))
                })
            };
            let before = reader
                .as_ref()
                .and_then(|r| xflow_core::read_field(&**r, &gate));
            let ticket = watch
                .filter(|_| reader.is_some())
                .zip(before.as_ref())
                .and_then(|(watch, before)| {
                    let receipt = xflow_core::learn::Receipt::delivery(
                        &before.text.value,
                        before.selection?,
                        &text_owned,
                    )?;
                    watch.hold(accessibility.take());
                    Some(xflow_core::DeliveryTicket {
                        field: Box::new(watch),
                        receipt,
                        keys: None,
                    })
                });
            let field: Option<Snapshot> = match &before {
                Some(s) => Some((
                    Some(s.text.value.encode_utf16().count() as i64),
                    Some(s.text.value.clone()),
                    s.selection.map_or(0, |(_, len)| len as i64),
                )),
                None => reader.as_ref().and_then(|r| {
                    r.counts()
                        .map(|(count, selected)| (Some(count as i64), None, selected as i64))
                }),
            };
            // Field text stays only in the ticket's receipt from here on.
            drop(before);
            // Arm before posting: stamped delivery keys are ignored, user edits are not lost.
            let ticket = ticket.or_else(|| {
                observe
                    .then(|| terminal_keys(&current, &text_owned))
                    .flatten()
            });
            if let Err(e) = send_keys(&text_owned, action == Decision::Type) {
                return drop(reply.send(Err(e)));
            }
            if action == Decision::Type {
                return drop(reply.send(Ok((action, false, ticket))));
            }
            let restore_previous = || {
                if let (Some(previous), Ok(clipboard)) = (&previous, clipboard()) {
                    clipboard.restore(previous);
                }
            };
            await_paste(
                || {
                    field
                        .as_ref()
                        .zip(reader.as_ref())
                        .is_some_and(|(before, r)| {
                            read(r).is_some_and(|after| inserted(before, &after, &text_owned))
                        })
                },
                reply_budget(field.as_ref(), restore_delay_ms, started),
                |confirmed| {
                    if confirmed {
                        restore_previous();
                    }
                    let _ = reply.send(Ok((action, confirmed, ticket)));
                },
            );
        });
        let (action, confirmed, ticket) = outcome.await.context("delivery task stopped")??;
        Ok(xflow_core::ObservedDelivery {
            outcome: injection_outcome(action, confirmed),
            ticket,
        })
    }
}
/// A terminal paste has no field text. Replay keys over the paste instead.
/// None when the hotkey tap is down, input is secure, or this is not a terminal:
/// the watch then does not start, same as before.
fn terminal_keys(focus: &Focus, text: &str) -> Option<xflow_core::DeliveryTicket> {
    if focus.pid <= 0
        || focus.secure
        || !matches!(
            focus.context.app_id.as_deref(),
            Some("com.apple.Terminal" | "com.googlecode.iterm2" | "com.mitchellh.ghostty")
        )
        || crate::macos_hotkey::secure_input()
    {
        return None;
    }
    let screen = observe::Reader::terminal(focus)?;
    let feed = crate::macos_hotkey::subscribe(focus.pid)?;
    let departure = Some(observe::departure(focus.pid, focus.window.as_ref()?)?);
    let receipt = xflow_core::learn::Receipt::new(text, text, Some(text.encode_utf16().count()))?;
    Some(xflow_core::DeliveryTicket {
        field: Box::new(QuietField),
        receipt,
        keys: Some(Box::new(TerminalKeys {
            feed,
            departure,
            screen,
        })),
    })
}
struct QuietField;
#[async_trait]
impl FieldWatch for QuietField {
    async fn next(&mut self) -> Option<xflow_core::FieldEvent> {
        std::future::pending().await
    }
    fn try_next(&mut self) -> Option<xflow_core::FieldEvent> {
        None
    }
    async fn read(&mut self) -> Option<xflow_core::FieldText> {
        None
    }
    async fn focused(&mut self) -> bool {
        false
    }
}
struct TerminalKeys {
    feed: crate::macos_hotkey::KeyFeed,
    departure: Option<observe::Departure>,
    screen: std::sync::Arc<observe::Reader>,
}
#[async_trait]
impl xflow_core::replay::KeyReplay for TerminalKeys {
    async fn next(&mut self) -> Option<xflow_core::replay::ReplayIn> {
        use xflow_core::replay::ReplayIn;
        loop {
            if crate::macos_hotkey::secure_input() {
                return Some(ReplayIn::Secure);
            }
            if self.feed.missed() {
                return Some(ReplayIn::Missed);
            }
            let TerminalKeys {
                feed, departure, ..
            } = self;
            let raw = tokio::select! {
                biased;
                _ = tokio::time::sleep(Duration::from_millis(50)) => continue,
                leave = async {
                    match departure.as_mut() {
                        Some(departure) => departure.events.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if leave.is_some() {
                        return Some(ReplayIn::Leave);
                    }
                    // Lost coverage cannot prove that this is still the same window.
                    return Some(ReplayIn::Missed);
                }
                raw = feed.rx.recv() => raw,
            };
            if feed.missed() {
                return Some(ReplayIn::Missed);
            }
            return raw.map(crate::macos_hotkey::decode);
        }
    }
    async fn confirm(&mut self, original: &str, corrected: &str) -> Result<(), &'static str> {
        if crate::macos_hotkey::secure_input() {
            return Err("secure");
        }
        if !self.feed.settled() {
            return Err("abstain");
        }
        let (screen, original, corrected) = (
            self.screen.clone(),
            original.to_owned(),
            corrected.to_owned(),
        );
        let confirmed = tokio::task::spawn_blocking(move || {
            xflow_core::replay::confirm(&*screen, &original, &corrected)
        })
        .await
        .unwrap_or(Err("visible-read"));
        if crate::macos_hotkey::secure_input() {
            return Err("secure");
        }
        if !self.feed.settled() {
            return Err("abstain");
        }
        confirmed
    }
    fn stop(&mut self) {
        self.feed.stop();
        if let Some(departure) = &self.departure {
            departure.signal();
        }
    }
}
fn injection_outcome(action: Decision, confirmed: bool) -> InjectionOutcome {
    match action {
        Decision::Type => InjectionOutcome::Typed,
        Decision::Paste if confirmed => InjectionOutcome::Pasted,
        Decision::Paste => InjectionOutcome::Uncertain,
        _ => InjectionOutcome::ClipboardOnly,
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
    #[test]
    fn paste_key_follows_command_layer_of_layout() {
        // Sparse ⌘-layer fixtures: (virtual key, character it types with ⌘ held).
        type Layout<'a> = &'a [(u16, &'a str)];
        let pick = |layout: Layout| v_key(|key| layout.contains(&(key, "v")));
        let cases: [(&str, Layout, u16); 12] = [
            ("US", &[(9, "v"), (47, "."), (11, "b")], 9),
            ("Dvorak", &[(9, "k"), (47, "v"), (11, "x")], 47),
            ("Dvorak - QWERTY ⌘", &[(9, "v"), (47, "."), (40, "k")], 9),
            ("Dvorak - Right", &[(9, "j"), (43, "v")], 43),
            ("AZERTY", &[(9, "v"), (12, "a"), (13, "z")], 9),
            ("QWERTZ", &[(9, "v"), (6, "y"), (16, "z")], 9),
            ("Colemak", &[(9, "v"), (2, "s"), (40, "e")], 9),
            ("Colemak Mod-DH", &[(9, "d"), (11, "v")], 11),
            ("Russian, Latin ⌘ layer", &[(9, "v"), (47, "ю")], 9),
            ("Cyrillic only: physical V", &[(9, "м"), (47, "ю")], 9),
            ("no v anywhere", &[(9, "ž"), (11, "V")], 9),
            ("two v keys: lowest", &[(9, "c"), (3, "v"), (47, "v")], 3),
        ];
        for (name, layout, key) in cases {
            assert_eq!(pick(layout), key, "{name}");
        }
        assert_eq!(pick(&[(9, "v"), (2, "v")]), 9, "kVK_ANSI_V wins a tie");
    }
    #[test]
    #[ignore = "reads every installed macOS keyboard layout (read-only; no switching or typing)"]
    fn native_installed_layout_paste_keys() {
        #[link(name = "Carbon", kind = "framework")]
        extern "C" {
            fn TISCreateInputSourceList(properties: Ref, all_installed: bool) -> Ref;
            static kTISPropertyInputSourceID: Ref;
        }
        let carbon = carbon().expect("Carbon TIS/UCKeyTranslate symbols");
        let list = Owned::new(unsafe { TISCreateInputSourceList(ptr::null(), true) }).unwrap();
        let mut keys = std::collections::BTreeMap::new();
        for i in 0..unsafe { CFArrayGetCount(list.0) } {
            let source = unsafe { CFArrayGetValueAtIndex(list.0, i) };
            let id = cf_text(unsafe { (carbon.1)(source, kTISPropertyInputSourceID) });
            if let (Some(id), Some(key)) = (id, unsafe { source_v_key(carbon, source) }) {
                keys.insert(id, key);
            }
        }
        let moved: Vec<_> = keys.iter().filter(|(_, &key)| key != ANSI_V).collect();
        println!("{} layouts; off kVK_ANSI_V: {moved:?}", keys.len());
        let current = paste_keycode();
        println!("current layout paste key: {current}");
        for (id, key) in [
            ("US", 9),
            ("Dvorak", 47),
            ("DVORAK-QWERTYCMD", 9),
            ("French", 9),
            ("German", 9),
            ("Colemak", 9),
            ("Russian", 9),
            ("Devanagari-QWERTY", 9),
        ] {
            let id = format!("com.apple.keylayout.{id}");
            assert_eq!(keys.get(&id), Some(&key), "{id}");
        }
    }
    #[tokio::test]
    #[ignore = "pastes 20 markers into an explicitly named scratch Chrome/VS Code window"]
    async fn native_chromium_paste_matrix() {
        assert!(
            unsafe { AXIsProcessTrusted() && CGPreflightPostEventAccess() },
            "test executable lacks an existing Accessibility grant; do not request one"
        );
        let expected = std::env::var("XFLOW_PASTE_APP").unwrap();
        assert!(matches!(
            expected.as_str(),
            "com.google.Chrome" | "com.microsoft.VSCode"
        ));
        let title = "XFlow delivery scratch 25";
        let mut psn = ProcessSerialNumber::default();
        let mut pid = 0;
        assert_eq!(unsafe { GetFrontProcess(&mut psn) }, 0);
        assert_eq!(unsafe { GetProcessPID(&psn, &mut pid) }, 0);
        let app = Owned::new(unsafe { AXUIElementCreateApplication(pid) }).unwrap();
        let info = Owned::new(unsafe { ProcessInformationCopyDictionary(&psn, u32::MAX) }).unwrap();
        let key = Owned::string("BundlePath");
        let bundle = cf_text(unsafe { CFDictionaryGetValue(info.0, key.0) }).unwrap();
        assert!(
            chromium_bundle(&bundle),
            "generic detection missed {bundle}"
        );
        let trials = std::env::var("XFLOW_PASTE_TRIALS").map_or(20, |n| n.parse().unwrap());
        let original = [
            app.flag("AXManualAccessibility"),
            app.flag("AXEnhancedUserInterface"),
        ];
        let desktop = MacDesktop::new(&InjectionConfig::default());
        let target = desktop.context().await.unwrap();
        assert_eq!(target.app_id.as_deref(), Some(expected.as_str()));
        let previous = read_previous(CLIPBOARD.into()).await;
        let mut confirmed = 0;
        let mut uncertain = 0;
        let mut copied = 0;
        for trial in 0..trials {
            let current = focus();
            assert_eq!(current.context, target, "focus changed; stop the matrix");
            assert!(
                app.attribute("AXFocusedWindow")
                    .and_then(|w| w.text("AXTitle"))
                    .is_some_and(|t| t.contains(title)),
                "only an explicitly titled scratch window is allowed"
            );
            assert!(!current.secure);
            drop(current);
            let marker = format!("xflow25-{trial:02} ");
            let started = Instant::now();
            let outcome = desktop.inject(&marker, &target).await.unwrap();
            println!("trial {trial}: {outcome:?} after {:?}", started.elapsed());
            match outcome {
                InjectionOutcome::Pasted => confirmed += 1,
                InjectionOutcome::Uncertain => uncertain += 1,
                InjectionOutcome::ClipboardOnly => copied += 1,
                other => panic!("unexpected outcome {other:?}"),
            }
            // Wait for a pending receipt worker to release its AX guard too.
            let _delivery = desktop.delivery.lock().await;
            assert_eq!(
                [
                    app.flag("AXManualAccessibility"),
                    app.flag("AXEnhancedUserInterface")
                ],
                original,
                "attribute restoration trial {trial}"
            );
        }
        if let (Some(previous), Ok(clipboard)) = (previous, clipboard()) {
            clipboard.restore(&previous);
        }
        println!("{expected}: confirmed={confirmed}/{trials} uncertain={uncertain}/{trials} copied={copied}/{trials}; AX flags original={original:?}, restored={trials}/{trials}");
    }
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
    fn search_and_address_fields_are_never_rewritten() {
        let labels = |l: &[&str]| {
            l.iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .into_iter()
        };
        assert!(search_field(Some("AXSearchField"), None, labels(&[])));
        assert!(search_field(
            Some("AXTextField"),
            Some("AXSearchField"),
            labels(&[])
        ));
        // Chrome, Safari and Firefox address bars.
        for label in [
            "Address and search bar",
            "WEB_BROWSER_ADDRESS_AND_SEARCH_FIELD",
            "urlbar-input",
        ] {
            assert!(
                search_field(Some("AXTextField"), None, labels(&[label])),
                "{label}"
            );
        }
        assert!(!search_field(
            Some("AXTextArea"),
            None,
            labels(&["Message body", "compose"])
        ));
    }
    #[test]
    fn watched_fields_skip_search_by_role_or_whole_word_only() {
        let labels = |l: &[&str]| {
            l.iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .into_iter()
        };
        assert!(search_label(Some("AXSearchField"), None, labels(&[])));
        assert!(search_label(None, Some("AXSearchField"), labels(&[])));
        for label in [
            "Search",
            "Address and search bar",
            "WEB_BROWSER_ADDRESS_AND_SEARCH_FIELD",
            "urlbar-input",
        ] {
            assert!(
                search_label(Some("AXTextField"), None, labels(&[label])),
                "{label}"
            );
        }
        // A word that merely contains "search" is not a search field.
        for label in [
            "Message #research",
            "Researchers channel",
            "addressee",
            "compose",
        ] {
            assert!(
                !search_label(Some("AXTextArea"), None, labels(&[label])),
                "{label}"
            );
        }
    }
    #[test]
    fn secure_role_subrole_or_protected_content_is_never_read() {
        assert!(is_secure(Some("AXSecureTextField"), None, None));
        assert!(is_secure(
            Some("AXTextField"),
            Some("AXSecureTextField"),
            None
        ));
        assert!(is_secure(Some("AXTextArea"), None, Some(true)));
        assert!(!is_secure(Some("AXTextArea"), None, Some(false)));
        assert!(!is_secure(Some("AXTextField"), None, None));
    }
    #[test]
    fn only_known_editability_selects_paste() {
        let target = AppContext {
            app_id: Some("app".into()),
            window_id: Some("1:2".into()),
            selected_text: None,
            terminal_input: false,
        };
        for (editable, expected) in [
            (Some(true), Decision::Paste),
            (Some(false), Decision::Copy),
            (None, Decision::Copy),
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
    fn manual_accessibility_restores_only_a_known_original_value() {
        use std::{cell::RefCell, rc::Rc};
        struct Flags {
            value: Option<bool>,
            writes: Vec<bool>,
            writes_succeed: bool,
        }
        #[derive(Clone)]
        struct App(Rc<RefCell<Flags>>);
        impl AccessibilityFlag for App {
            fn read(&self) -> Option<bool> {
                self.0.borrow().value
            }
            fn write(&self, value: bool) -> bool {
                let mut flags = self.0.borrow_mut();
                flags.writes.push(value);
                flags.value = Some(value);
                flags.writes_succeed
            }
        }
        for (original, writes_succeed) in [
            (Some(false), true),
            (Some(true), true),
            (None, true),
            (Some(false), false),
        ] {
            let app = App(Rc::new(RefCell::new(Flags {
                value: original,
                writes: vec![],
                writes_succeed,
            })));
            let attempt = || -> Result<()> {
                let _guard = AccessibilityGuard::enable(app.clone());
                if original == Some(false) {
                    assert_eq!(app.read(), Some(true));
                }
                bail!("delivery failed or was cancelled");
            };
            assert!(attempt().is_err());
            assert_eq!(app.read(), original);
            assert_eq!(
                app.0.borrow().writes,
                if original == Some(false) {
                    vec![true, false]
                } else {
                    vec![]
                }
            );
            let guard = AccessibilityGuard::enable(app.clone());
            drop(guard);
            assert_eq!(app.read(), original, "success also restores the original");
            let mut cancelled = Box::pin(async {
                let _guard = AccessibilityGuard::enable(app.clone());
                std::future::pending::<()>().await;
            });
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(std::future::Future::poll(cancelled.as_mut(), &mut context).is_pending());
            drop(cancelled);
            assert_eq!(
                app.read(),
                original,
                "cancellation also restores the original"
            );
        }
    }

    #[test]
    fn chromium_classification_does_not_enable_native_apps() {
        for app in [
            "com.google.Chrome",
            "org.chromium.Chromium",
            "com.microsoft.VSCode",
            "com.todesktop.230313mzl4w4u92",
            "com.tinyspeck.slackmacgap",
            "com.hnc.Discord",
            "notion.id",
            "company.thebrowser.Browser",
            "com.linear",
        ] {
            assert!(is_chromium(Some(app)), "{app}");
        }
        for app in [
            None,
            Some("com.apple.TextEdit"),
            Some("com.apple.finder"),
            Some("com.google.Chrome.fake"),
        ] {
            assert!(!is_chromium(app));
        }
        assert!(is_container("AXWebArea"));
        assert!(is_container("AXGroup"));
        assert!(!is_container("AXTextArea"));
        assert!(!is_container("AXSecureTextField"));
    }
    #[test]
    fn generic_chromium_classification_reads_bundle_frameworks() {
        let dir = tempfile::tempdir().unwrap();
        let app = |name: &str, framework: &str, pak: Option<&str>| {
            let framework = dir
                .path()
                .join(name)
                .join("Contents/Frameworks")
                .join(framework);
            let resources = framework.join("Versions/A/Resources");
            std::fs::create_dir_all(&resources).unwrap();
            // Real frameworks link Resources to Versions/Current/Resources.
            std::os::unix::fs::symlink("Versions/A/Resources", framework.join("Resources"))
                .unwrap();
            if let Some(pak) = pak {
                std::fs::write(resources.join(pak), b"").unwrap();
            }
            dir.path().join(name)
        };
        let pak = Some("chrome_100_percent.pak");
        let edge = app("Edge.app", "Microsoft Edge Framework.framework", pak);
        assert!(has_chromium_framework(&edge));
        assert!(has_chromium_framework(&app(
            "Obsidian.app",
            "Electron Framework.framework",
            pak
        )));
        let native = app("Native.app", "Sparkle.framework", Some("Sparkle.nib"));
        assert!(!has_chromium_framework(&native));
        assert!(!has_chromium_framework(&dir.path().join("Missing.app")));
        // Bundles outside the Applications folders are never read.
        assert!(!chromium_bundle(edge.to_str().unwrap()));
        assert!(!chromium_bundle("/Users/someone/Downloads/Edge.app"));
    }
    #[test]
    fn opaque_field_replies_uncertain_at_once_but_keeps_the_paste_lock() {
        let start = Instant::now();
        let empty: Snapshot = (Some(0), Some(String::new()), 0);
        let text: Snapshot = (Some(5), Some("hello".into()), 0);
        assert_eq!(reply_budget(None, 300, start), Duration::ZERO);
        assert_eq!(
            reply_budget(Some(&empty), 300, start),
            Duration::from_millis(300)
        );
        assert_eq!(
            reply_budget(Some(&text), 300, start),
            Duration::from_millis(2300)
        );
        // Count receipt polls instead of timing them, so a slow runner cannot race the test.
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let polls = std::sync::Arc::new(AtomicUsize::new(0));
        let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let guard = lock.clone().try_lock_owned().unwrap();
        let (reply, replied) = std::sync::mpsc::channel();
        let (land, landed) = std::sync::mpsc::channel();
        let counted = polls.clone();
        let worker = std::thread::spawn(move || {
            let _guard = guard;
            await_paste(
                || counted.fetch_add(1, SeqCst) > 0 && landed.try_recv().is_ok(),
                reply_budget(None, 300, start),
                |confirmed| reply.send((confirmed, counted.load(SeqCst))).unwrap(),
            );
        });
        let (confirmed, polls_at_reply) = replied.recv().unwrap();
        assert_eq!(polls_at_reply, 1, "replied after one look, without waiting");
        assert_eq!(
            injection_outcome(Decision::Paste, confirmed),
            InjectionOutcome::Uncertain
        );
        // The worker keeps the lock and keeps watching until a late receipt lands.
        let _ = land.send(());
        worker.join().unwrap();
        assert!(polls.load(SeqCst) > 1);
        drop(lock.blocking_lock());
    }
    #[test]
    fn opaque_paste_is_uncertain_and_unknown_focus_is_copy_only() {
        assert_eq!(
            injection_outcome(Decision::Paste, false),
            InjectionOutcome::Uncertain
        );
        assert_eq!(
            injection_outcome(Decision::Paste, true),
            InjectionOutcome::Pasted
        );
        assert_eq!(
            injection_outcome(Decision::Copy, false),
            InjectionOutcome::ClipboardOnly
        );
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
    fn receipt_accepts_nbsp_but_not_a_different_spelling() {
        let before = (Some(0), Some(String::new()), 0);
        assert!(inserted(
            &before,
            &(Some(9), Some("Priyanca\u{a0}".into()), 0),
            "Priyanca "
        ));
        assert!(!inserted(
            &before,
            &(Some(9), Some("Priyanka ".into()), 0),
            "Priyanca "
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
        let budget = receipt_budget(config.injection.restore_delay_ms, start);
        // Leaves the 10 s IPC timeout room for focus checks and clipboard commands.
        assert!(budget <= Duration::from_secs(4), "{budget:?}");
        let (reply, replied) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _guard = guard;
            await_paste(
                || false,
                budget,
                |confirmed| reply.send((confirmed, start.elapsed())).unwrap(),
            );
        });
        // Lower bounds only: a loaded CI runner may oversleep, never undersleep.
        let (confirmed, at) = replied.recv().unwrap();
        assert!(!confirmed && at >= budget, "{at:?}");
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
    fn terminal_dom_class_list_detects_localized_xterm_input() {
        #[link(name = "CoreFoundation", kind = "framework")]
        extern "C" {
            fn CFArrayCreate(
                allocator: Ref,
                values: *const Ref,
                count: isize,
                callbacks: Ref,
            ) -> Ref;
        }
        // Null callbacks: these strings stay alive until the borrowed array is dropped.
        let contains = |values: &[&str]| {
            let strings: Vec<_> = values.iter().map(|s| Owned::string(s)).collect();
            let refs: Vec<_> = strings.iter().map(|s| s.0).collect();
            let array = Owned::new(unsafe {
                CFArrayCreate(ptr::null(), refs.as_ptr(), refs.len() as isize, ptr::null())
            })
            .unwrap();
            dom_class(&array, "xterm-helper-textarea")
        };
        assert!(contains(&["input", "xterm-helper-textarea", "focused"]));
        assert!(contains(&["xterm-helper-textarea"]));
        assert!(!contains(&[]));
        assert!(!contains(&["editor", "xterm-helper-textarea-other"]));
        assert!(!contains(&["editor xterm-helper-textarea"]));
        assert!(!dom_class(
            &Owned::string("xterm-helper-textarea"),
            "xterm-helper-textarea"
        ));
    }
    #[test]
    fn terminal_input_labels_and_password_prompt_refusal() {
        for label in [
            "Terminal input",
            " Terminal Input ",
            "terminal-input",
            "xterm-helper-textarea",
        ] {
            assert!(terminal_input_label(label), "{label}");
        }
        for label in ["", "Editor", "terminal.rs", "claude", "codex", "aider"] {
            assert!(!terminal_input_label(label), "{label}");
        }
        let context = AppContext {
            app_id: Some("com.microsoft.VSCode".into()),
            window_id: Some("1:2".into()),
            terminal_input: true,
            ..Default::default()
        };
        let mut current = Focus {
            context: context.clone(),
            editable: Some(true),
            ..Default::default()
        };
        for method in [InjectionMethod::Paste, InjectionMethod::Type] {
            for text in [
                "line one\nline two",
                "\u{1b}[200~prompt\u{1b}[201~",
                "\r",
                "\0",
            ] {
                assert_eq!(decision(text, method, &context, &current), Decision::Copy);
            }
            current.secure = true;
            assert_eq!(
                decision("password", method, &context, &current),
                Decision::Refuse
            );
            current.secure = false;
        }
        current.context.terminal_input = false;
        assert_eq!(
            decision("prompt", InjectionMethod::Paste, &context, &current),
            Decision::Copy
        );
    }
    #[test]
    fn delivery_requires_same_window_nonsecure_focus_and_safe_terminal_input() {
        use Decision::*;
        use InjectionMethod::{Clipboard, Paste as P, Type as T};
        let target = AppContext {
            app_id: Some("com.apple.TextEdit".into()),
            window_id: Some("1:2".into()),
            selected_text: None,
            terminal_input: false,
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
        // An absent AX tree cannot establish that the field is safe to paste into.
        current.editable = None;
        assert_eq!(decision("hello", P, &target, &current), Copy);
        assert_eq!(decision("hello", T, &target, &current), Copy);
        current.editable = Some(true);
        current.context.window_id = Some("1:3".into());
        assert_eq!(decision("hello", P, &target, &current), Copy);
        current.context.app_id = Some("com.googlecode.iterm2".into());
        let terminal = current.context.clone();
        for text in ["echo hi\n", "echo hi\r", "\u{1b}"] {
            assert_eq!(decision(text, P, &terminal, &current), Copy);
        }
        assert_eq!(decision("echo hi", P, &terminal, &current), Paste);
        assert_eq!(decision("hello", P, &AppContext::default(), &current), Copy);
    }
}
