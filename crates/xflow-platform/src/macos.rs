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
    AppContext, Desktop, InjectionOutcome,
};

const MAX_TEXT: usize = 64 * 1024;
type Ref = *const c_void;
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
    fn CFDataGetBytePtr(data: Ref) -> *const u8;
    static kCFBooleanTrue: Ref;
    fn CFDictionaryGetValue(dict: Ref, key: Ref) -> Ref;
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
    fn enable(&self, name: &str) -> bool {
        let name = Self::string(name);
        unsafe { AXUIElementSetAttributeValue(self.0, name.0, kCFBooleanTrue) == 0 }
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
    // Electron and Chromium build their AX tree only once an assistive client asks.
    if role.as_deref().is_none_or(is_container) && app.enable("AXManualAccessibility") {
        // Chromium builds the tree asynchronously after accepting the request.
        std::thread::sleep(Duration::from_millis(100));
        element = app.attribute("AXFocusedUIElement");
        role = role_of(&element);
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
/// What changes in a field when text lands in it.
fn snapshot(field: &Owned) -> (Option<i64>, Option<String>) {
    (field.number("AXNumberOfCharacters"), field.text("AXValue"))
}
/// Restore only after the field contains the insertion, never on a timer alone (#53).
fn landed(
    field: &Owned,
    before: &(Option<i64>, Option<String>),
    text: &str,
    delay: Duration,
) -> bool {
    if *before == (None, None) {
        return false;
    }
    std::thread::sleep(delay);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let after = snapshot(field);
        if after.1.as_ref().is_some_and(|s| {
            s.matches(text).count() > before.1.as_deref().map_or(0, |s| s.matches(text).count())
        }) || (before.1.is_none()
            && before
                .0
                .zip(after.0)
                .is_some_and(|(a, b)| b == a + text.encode_utf16().count() as i64))
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
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
    delivery: tokio::sync::Mutex<()>,
}
impl MacDesktop {
    pub fn new(config: &InjectionConfig) -> Self {
        // Preflight only (never prompts); launchd logs make the fallback explainable.
        if !unsafe { AXIsProcessTrusted() } {
            eprintln!("xflowd [warn]: Accessibility is not granted to this executable; transcripts are copied instead of pasted (System Settings > Privacy & Security > Accessibility)");
        }
        Self {
            config: config.clone(),
            delivery: tokio::sync::Mutex::new(()),
        }
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
    async fn focused_text(&self) -> Result<(AppContext, Option<String>)> {
        tokio::task::spawn_blocking(|| {
            let focus = focus();
            let text =
                if focus.editable != Some(true) || is_terminal(focus.context.app_id.as_deref()) {
                    None
                } else {
                    focus.element.and_then(|e| e.text("AXValue"))
                };
            Ok((focus.context, text))
        })
        .await?
    }
    async fn copy(&self, text: &str) -> Result<()> {
        if text.len() > MAX_TEXT {
            bail!("injection text exceeds 64 KiB");
        }
        super::desktop::copy_with("/usr/bin/pbcopy", &[], text).await
    }
    async fn inject(&self, text: &str, target: &AppContext) -> Result<InjectionOutcome> {
        if text.len() > MAX_TEXT {
            bail!("injection text exceeds 64 KiB");
        }
        let _delivery = self.delivery.lock().await;
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
                self.copy(text).await?;
                return Ok(InjectionOutcome::ClipboardOnly);
            }
            _ => {}
        }
        // shortcut: restores text only; preserve native pasteboard formats in #27.
        let previous = if action == Decision::Paste && self.config.restore_clipboard {
            super::desktop::output("/usr/bin/pbpaste", &[]).await.ok()
        } else {
            None
        };
        if action != Decision::Type {
            self.copy(text).await?;
            // Cmd+V only while the pasteboard holds the transcript, never another writer's text.
            if super::desktop::output("/usr/bin/pbpaste", &[]).await? != text {
                self.copy(text).await?;
                return Ok(InjectionOutcome::ClipboardOnly);
            }
        }
        let target = target.clone();
        let text_owned = text.to_owned();
        let delay = Duration::from_millis(self.config.restore_delay_ms.into());
        let restore = previous.is_some();
        // Re-check focus just before the keys, then keep this delivery serialized through restore.
        let (action, confirmed) = tokio::task::spawn_blocking(move || {
            let current = focus();
            let action = decision(&text_owned, method, &target, &current);
            if matches!(action, Decision::Refuse | Decision::Copy) {
                return Ok::<_, anyhow::Error>((Decision::Copy, false));
            }
            let before = current.element.as_ref().map(snapshot);
            send_keys(&text_owned, action == Decision::Type)?;
            let confirmed = action == Decision::Paste
                && restore
                && current
                    .element
                    .as_ref()
                    .zip(before.as_ref())
                    .is_some_and(|(field, before)| landed(field, before, &text_owned, delay));
            Ok((action, confirmed))
        })
        .await??;
        if action == Decision::Copy {
            self.copy(text).await?;
            return Ok(InjectionOutcome::ClipboardOnly);
        }
        if confirmed {
            if let Some(previous) = previous {
                if super::desktop::output("/usr/bin/pbpaste", &[])
                    .await
                    .is_ok_and(|s| s == text)
                {
                    super::desktop::copy_with("/usr/bin/pbcopy", &[], &previous).await?;
                }
            }
        }
        if action == Decision::PasteAndCopy
            || (action == Decision::Paste && self.config.restore_clipboard && !confirmed)
        {
            return Ok(InjectionOutcome::ClipboardOnly);
        }

        Ok(if action == Decision::Type {
            InjectionOutcome::Typed
        } else {
            InjectionOutcome::Pasted
        })
    }
}

/// Nonprompting permission preflights for the calling process (doctor).
pub fn permissions() -> (bool, bool) {
    unsafe { (AXIsProcessTrusted(), CGPreflightListenEventAccess()) }
}

/// Read AVFoundation authorization without opening an input device or prompting.
/// Load it only for doctor; the daemon's idle image stays free of AVFoundation.
pub fn microphone_permission() -> &'static str {
    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Ref;
        fn sel_registerName(name: *const c_char) -> Ref;
        fn objc_msgSend();
    }
    extern "C" {
        fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> i32;
    }
    unsafe {
        let library = dlopen(
            c"/System/Library/Frameworks/AVFoundation.framework/AVFoundation".as_ptr(),
            1,
        );
        if library.is_null() {
            return "unavailable";
        }
        let class = objc_getClass(c"AVCaptureDevice".as_ptr());
        if class.is_null() {
            dlclose(library);
            return "unavailable";
        }
        let send: unsafe extern "C" fn(Ref, Ref, Ref) -> isize =
            std::mem::transmute(objc_msgSend as *const ());
        let audio = Owned::string("soun");
        let status = send(
            class,
            sel_registerName(c"authorizationStatusForMediaType:".as_ptr()),
            audio.0,
        );
        dlclose(library);
        match status {
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
        let previous = super::super::desktop::output("/usr/bin/pbpaste", &[])
            .await
            .unwrap();
        let context = desktop.context().await.unwrap();
        assert_eq!(
            context.app_id.as_deref(),
            Some("com.apple.TextEdit"),
            "focus a scratch TextEdit document first"
        );
        assert!(
            desktop.focused_text().await.unwrap().1.is_some(),
            "focused element must be editable"
        );
        assert_eq!(
            desktop
                .inject("xflow macOS delivery check", &context)
                .await
                .unwrap(),
            InjectionOutcome::Pasted
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(desktop
            .focused_text()
            .await
            .unwrap()
            .1
            .unwrap()
            .contains("xflow macOS delivery check"));
        assert!(
            super::super::desktop::output("/usr/bin/pbpaste", &[])
                .await
                .unwrap()
                == previous
        );
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
