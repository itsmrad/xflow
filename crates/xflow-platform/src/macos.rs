//! Accessibility and CGEvent delivery without NSApplication or an AppKit run loop.
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::{
    ffi::{c_char, c_void, CStr},
    ptr,
    sync::{Once, OnceLock},
    time::Duration,
};
use xflow_core::{
    config::{InjectionConfig, InjectionMethod},
    AppContext, Desktop, InjectionOutcome,
};

const MAX_TEXT: usize = 64 * 1024;
type Ref = *const c_void;
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request: u32) -> u32;
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
    fn CFDataGetBytePtr(data: Ref) -> *const u8;
    static kCFBooleanTrue: Ref;
    fn CFDictionaryGetValue(dict: Ref, key: Ref) -> Ref;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: Ref) -> bool;
}
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn GetFrontProcess(psn: *mut ProcessSerialNumber) -> i16;
    fn GetProcessPID(psn: *const ProcessSerialNumber, pid: *mut i32) -> i32;
    fn ProcessInformationCopyDictionary(psn: *const ProcessSerialNumber, mask: u32) -> Ref;
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementCreateSystemWide() -> Ref;
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
    // Process Manager avoids loading AppKit just to identify the frontmost app.
    if unsafe { GetFrontProcess(&mut psn) != 0 || GetProcessPID(&psn, &mut pid) != 0 } {
        return focus;
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
    if !unsafe { AXIsProcessTrusted() } {
        return focus;
    }
    // The system-wide timeout bounds every AX call, so a stalled app cannot block delivery.
    TIMEOUT.call_once(|| {
        if let Some(system) = Owned::new(unsafe { AXUIElementCreateSystemWide() }) {
            unsafe { AXUIElementSetMessagingTimeout(system.0, 0.2) };
        }
    });
    let Some(app) = Owned::new(unsafe { AXUIElementCreateApplication(pid) }) else {
        return focus;
    };
    let role_of = |e: &Option<Owned>| e.as_ref().and_then(|e| e.text("AXRole"));
    let mut element = app.attribute("AXFocusedUIElement");
    let mut role = role_of(&element);
    // Electron and Chromium build their AX tree only once an assistive client asks.
    if role.as_deref().is_none_or(is_container) && app.enable("AXManualAccessibility") {
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
    focus.editable = if matches!(role.as_str(), "AXTextField" | "AXTextArea" | "AXComboBox") {
        Some(
            !focus.secure
                // NSTextView (TextEdit) omits AXEnabled; only an explicit false disables.
                && element.flag("AXEnabled") != Some(false)
                && (element.settable("AXValue")
                    || element.settable("AXSelectedText")
                    || element.flag("AXEditable") == Some(true)
                    || is_terminal(focus.context.app_id.as_deref())),
        )
    } else if is_container(&role) {
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
    // Unknown editability (web content without AX) pastes blind, like Wispr Flow;
    // a changed window or a known non-editable element copies instead.
    if target.app_id.as_deref().is_none_or(str::is_empty)
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
    } else {
        Decision::Paste
    }
}
fn send_keys(text: &str, typed: bool) -> Result<()> {
    if !unsafe { CGPreflightPostEventAccess() } {
        bail!("Accessibility permission is required for text delivery; run xflow doctor");
    }
    let key = if typed { 0 } else { paste_keycode() };
    // Allocate both events before posting so failure cannot leave a key down.
    let down = Owned::new(unsafe { CGEventCreateKeyboardEvent(ptr::null(), key, true) })
        .context("cannot create keyboard event")?;
    let up = Owned::new(unsafe { CGEventCreateKeyboardEvent(ptr::null(), key, false) })
        .context("cannot create keyboard event")?;
    for event in [&down, &up] {
        unsafe {
            CGEventSetFlags(event.0, if typed { 0 } else { 1 << 20 });
        }
    }
    if typed {
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
        unsafe {
            CGEventPost(0, down.0);
            CGEventPost(0, up.0);
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
}
impl MacDesktop {
    pub fn new(config: &InjectionConfig) -> Self {
        // Preflight only (never prompts); launchd logs make the fallback explainable.
        if !unsafe { AXIsProcessTrusted() } {
            eprintln!("xflowd [warn]: Accessibility is not granted to this executable; transcripts are copied instead of pasted (System Settings > Privacy & Security > Accessibility)");
        }
        Self {
            config: config.clone(),
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
        let previous = if action == Decision::Paste && self.config.restore_clipboard {
            super::desktop::output("/usr/bin/pbpaste", &[]).await.ok()
        } else {
            None
        };
        if action == Decision::Paste {
            self.copy(text).await?;
        }
        let target = target.clone();
        let text_owned = text.to_owned();
        // Re-check just before the keys: focus may have moved to another window or a secure field.
        let delivered = tokio::task::spawn_blocking(move || {
            match decision(&text_owned, method, &target, &focus()) {
                Decision::Refuse | Decision::Copy => Ok(false),
                _ => send_keys(&text_owned, method == InjectionMethod::Type).map(|()| true),
            }
        })
        .await??;
        if !delivered {
            if action == Decision::Type {
                self.copy(text).await?;
            }
            return Ok(InjectionOutcome::ClipboardOnly);
        }
        if let Some(previous) = previous {
            let injected = text.to_owned();
            let delay = self.config.restore_delay_ms;
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay.into())).await;
                if super::desktop::output("/usr/bin/pbpaste", &[])
                    .await
                    .is_ok_and(|s| s == injected)
                {
                    let _ = super::desktop::copy_with("/usr/bin/pbcopy", &[], &previous).await;
                }
            });
        }
        Ok(if action == Decision::Type {
            InjectionOutcome::Typed
        } else {
            InjectionOutcome::Pasted
        })
    }
}

/// Nonprompting preflights; called inside xflowd for its IPC diagnostics.
pub fn permissions() -> xflow_core::ipc::Permissions {
    xflow_core::ipc::Permissions {
        accessibility: unsafe { AXIsProcessTrusted() },
        // kIOHIDRequestTypeListenEvent = 1; kIOHIDAccessTypeGranted = 0.
        input_monitoring: unsafe { IOHIDCheckAccess(1) == 0 && CGPreflightListenEventAccess() },
        keyboard_events: None,
    }
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
        assert_eq!(decision("hello", P, &target, &current), Paste);
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
