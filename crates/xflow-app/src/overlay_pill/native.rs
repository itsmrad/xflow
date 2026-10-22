//! Small AppKit adapter. All Objective-C objects stay on the helper's main thread.
use super::{read_event, PillEvent};
use crate::gsettings::GSettings;
use anyhow::{bail, Result};
use std::{
    ffi::{c_char, c_void, CStr},
    io::BufRead,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

type Id = *mut c_void;
type Sel = *mut c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
}

// Each invocation states the exact native ABI, including CGFloat=f64 and NSInteger=isize.
macro_rules! msg {
    ($object:expr, $selector:expr $(, $arg:expr => $ty:ty)* ; $result:ty) => {{
        let call: unsafe extern "C" fn(Id, Sel $(, $ty)*) -> $result =
            std::mem::transmute(objc_msgSend as *const ());
        call($object, sel_registerName($selector.as_ptr()) $(, $arg)*)
    }};
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Point {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Size {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Rect {
    origin: Point,
    size: Size,
}

impl Rect {
    fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            origin: Point { x, y },
            size: Size { width, height },
        }
    }
}

unsafe fn class(name: &CStr) -> Id {
    objc_getClass(name.as_ptr())
}
unsafe fn string(text: &CStr) -> Id {
    msg!(class(c"NSString"), c"stringWithUTF8String:", text.as_ptr() => *const c_char; Id)
}

unsafe fn visible_frame(screen: Id) -> Rect {
    #[cfg(target_arch = "x86_64")]
    {
        // x86_64 returns NSRect through objc_msgSend_stret; arm64 uses msgSend.
        let call: unsafe extern "C" fn(*mut Rect, Id, Sel) =
            std::mem::transmute(objc_msgSend_stret as *const ());
        let mut rect = Rect::new(0.0, 0.0, 0.0, 0.0);
        call(
            &mut rect,
            screen,
            sel_registerName(c"visibleFrame".as_ptr()),
        );
        rect
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        msg!(screen, c"visibleFrame"; Rect)
    }
}

fn frame(screen: Rect, position: &str, scale: f64) -> Rect {
    let width = (220.0 * scale).min(screen.size.width);
    let height = (44.0 * scale).min(screen.size.height);
    let margin = 32.0 * scale;
    let x = if position.ends_with("left") {
        margin
    } else if position.ends_with("right") {
        screen.size.width - width - margin
    } else {
        (screen.size.width - width) / 2.0
    };
    let y = if position.starts_with("top") {
        screen.size.height - height - margin
    } else {
        margin
    };
    Rect::new(
        screen.origin.x + x.clamp(0.0, screen.size.width - width),
        screen.origin.y + y.clamp(0.0, screen.size.height - height),
        width,
        height,
    )
}

pub(super) fn run(
    first: PillEvent,
    mut input: impl BufRead + Send + 'static,
    settings: GSettings,
) -> Result<()> {
    let scale: f64 = settings.get("scale")?.parse()?;
    let opacity: f64 = settings.get("opacity")?.parse()?;
    let position = settings.get("position")?;
    let theme = settings.get("theme")?;
    let latest = Arc::new(Mutex::new(Some(first)));
    let closed = Arc::new(AtomicBool::new(false));
    let pending = latest.clone();
    let finished = closed.clone();
    // dlopen is confined to this CLI-only entry point; xflowd never links AppKit.
    unsafe {
        let appkit = libc::dlopen(
            c"/System/Library/Frameworks/AppKit.framework/AppKit".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        );
        if appkit.is_null() {
            bail!("cannot load AppKit");
        }
        let pool = msg!(class(c"NSAutoreleasePool"), c"new"; Id);
        let app = msg!(class(c"NSApplication"), c"sharedApplication"; Id);
        msg!(app, c"setActivationPolicy:", 1isize => isize; i8); // Accessory: no Dock icon.
        let screen = msg!(class(c"NSScreen"), c"mainScreen"; Id);
        if screen.is_null() {
            bail!("no screen is available for the recording pill");
        }
        // shortcut: use the main screen for each session, add monitor selection when requested.
        let panel_frame = frame(visible_frame(screen), position.trim_matches('\''), scale);
        let panel = msg!(class(c"NSPanel"), c"alloc"; Id);
        let panel = msg!(panel, c"initWithContentRect:styleMask:backing:defer:",
            panel_frame => Rect, 1usize << 7 => usize, 2usize => usize, 0i8 => i8; Id);
        if panel.is_null() {
            bail!("cannot create the recording pill");
        }
        msg!(panel, c"setReleasedWhenClosed:", 0i8 => i8; ());
        msg!(panel, c"setLevel:", 3isize => isize; ());
        msg!(panel, c"setFloatingPanel:", 1i8 => i8; ());
        msg!(panel, c"setBecomesKeyOnlyIfNeeded:", 1i8 => i8; ());
        msg!(panel, c"setHidesOnDeactivate:", 0i8 => i8; ());
        msg!(panel, c"setIgnoresMouseEvents:", 1i8 => i8; ());
        msg!(panel, c"setAccessibilityLabel:", string(c"XFlow recording status") => Id; ());
        msg!(panel, c"setCollectionBehavior:", 1usize | (1 << 6) | (1 << 8) => usize; ());
        msg!(panel, c"setOpaque:", 0i8 => i8; ());
        msg!(panel, c"setHasShadow:", 1i8 => i8; ());
        msg!(panel, c"setAlphaValue:", opacity => f64; ());
        let clear = msg!(class(c"NSColor"), c"clearColor"; Id);
        msg!(panel, c"setBackgroundColor:", clear => Id; ());
        let view = msg!(panel, c"contentView"; Id);
        msg!(view, c"setWantsLayer:", 1i8 => i8; ());
        let layer = msg!(view, c"layer"; Id);
        msg!(layer, c"setCornerRadius:", 22.0 * scale => f64; ());
        msg!(layer, c"setMasksToBounds:", 1i8 => i8; ());
        let dark = if theme == "'auto'" {
            let appearance = msg!(app, c"effectiveAppearance"; Id);
            let name = msg!(appearance, c"name"; Id);
            let text = msg!(name, c"UTF8String"; *const c_char);
            !text.is_null() && CStr::from_ptr(text).to_string_lossy().contains("Dark")
        } else {
            theme == "'dark'"
        };
        let shade = if dark { 0.12 } else { 0.96 };
        let background = msg!(class(c"NSColor"), c"colorWithSRGBRed:green:blue:alpha:",
            shade => f64, shade => f64, shade => f64, 1.0f64 => f64; Id);
        let color = msg!(background, c"CGColor"; Id);
        msg!(layer, c"setBackgroundColor:", color => Id; ());
        let label =
            msg!(class(c"NSTextField"), c"labelWithString:", string(c"Listening") => Id; Id);
        msg!(label, c"setFrame:", Rect::new(18.0*scale, 12.0*scale, 124.0*scale, 20.0*scale) => Rect; ());
        let font = msg!(class(c"NSFont"), c"systemFontOfSize:weight:", 14.0*scale => f64, 0.23f64 => f64; Id);
        msg!(label, c"setFont:", font => Id; ());
        let foreground = if dark {
            msg!(class(c"NSColor"), c"whiteColor"; Id)
        } else {
            msg!(class(c"NSColor"), c"blackColor"; Id)
        };
        msg!(label, c"setTextColor:", foreground => Id; ());
        msg!(view, c"addSubview:", label => Id; ());
        let meter = msg!(class(c"NSLevelIndicator"), c"alloc"; Id);
        let meter = msg!(meter, c"initWithFrame:", Rect::new(154.0*scale, 15.0*scale, 48.0*scale, 14.0*scale) => Rect; Id);
        msg!(meter, c"setLevelIndicatorStyle:", 1isize => isize; ());
        msg!(meter, c"setMinValue:", 0.0f64 => f64; ());
        msg!(meter, c"setMaxValue:", 1.0f64 => f64; ());
        msg!(meter, c"setWarningValue:", 1.0f64 => f64; ());
        msg!(meter, c"setCriticalValue:", 1.0f64 => f64; ());
        msg!(meter, c"setEditable:", 0i8 => i8; ());
        msg!(meter, c"setAccessibilityLabel:", string(c"Microphone level") => Id; ());
        msg!(view, c"addSubview:", meter => Id; ());
        msg!(meter, c"release"; ());
        msg!(app, c"finishLaunching"; ());
        msg!(panel, c"orderFrontRegardless"; ());
        let app_handle = app as usize;
        std::thread::Builder::new().name("pill-input".into()).spawn(move || {
            // NSApplication.postEvent is thread-safe and wakes its blocking event loop.
            let wake = || {
                let pool = msg!(class(c"NSAutoreleasePool"), c"new"; Id);
                let event = msg!(class(c"NSEvent"), c"otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:",
                    15usize => usize, Point { x: 0.0, y: 0.0 } => Point, 0usize => usize,
                    0.0f64 => f64, 0isize => isize, std::ptr::null_mut::<c_void>() => Id,
                    0i16 => i16, 0isize => isize, 0isize => isize; Id);
                msg!(app_handle as Id, c"postEvent:atStart:", event => Id, 0i8 => i8; ());
                msg!(pool, c"drain"; ());
            };
            while let Ok(Some(event)) = read_event(&mut input) {
                if !event.active() { break; }
                *pending.lock().unwrap() = Some(event);
                wake();
            }
            finished.store(true, Ordering::Release);
            wake();
        })?;
        while !closed.load(Ordering::Acquire) {
            let tick_pool = msg!(class(c"NSAutoreleasePool"), c"new"; Id);
            if let Some(event) = latest.lock().unwrap().take() {
                let text = if event.state == xflow_core::State::Listening {
                    c"Listening"
                } else {
                    c"Processing"
                };
                msg!(label, c"setStringValue:", string(text) => Id; ());
                let level = if event.state == xflow_core::State::Listening {
                    f64::from(event.level)
                } else {
                    0.0
                };
                msg!(meter, c"setDoubleValue:", level => f64; ());
            }
            let deadline = msg!(class(c"NSDate"), c"distantFuture"; Id);
            let event = msg!(app, c"nextEventMatchingMask:untilDate:inMode:dequeue:",
                usize::MAX => usize, deadline => Id, string(c"kCFRunLoopDefaultMode") => Id, 1i8 => i8; Id);
            if !event.is_null() {
                msg!(app, c"sendEvent:", event => Id; ());
            }
            msg!(app, c"updateWindows"; ());
            msg!(tick_pool, c"drain"; ());
        }
        msg!(panel, c"orderOut:", std::ptr::null_mut::<c_void>() => Id; ());
        msg!(panel, c"close"; ());
        msg!(panel, c"release"; ());
        msg!(pool, c"drain"; ());
        // AppKit remains loaded until this short-lived helper exits.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_respects_visible_area_and_position_aliases() {
        let screen = Rect::new(-1000.0, 40.0, 1000.0, 700.0);
        for scale in [0.5, 1.0, 3.0] {
            for position in [
                "bottom-center",
                "top-center",
                "bottom-left",
                "bottom-right",
                "top-left",
                "top-right",
                "top",
                "bottom",
            ] {
                let pill = frame(screen, position, scale);
                assert!(pill.origin.x >= screen.origin.x);
                assert!(pill.origin.y >= screen.origin.y);
                assert!(pill.origin.x + pill.size.width <= screen.origin.x + screen.size.width);
                assert!(pill.origin.y + pill.size.height <= screen.origin.y + screen.size.height);
            }
        }
        assert_eq!(
            frame(screen, "top", 1.0).origin.y,
            frame(screen, "top-center", 1.0).origin.y
        );
        assert_eq!(frame(screen, "bottom", 1.0).origin.y, 72.0);
    }
}
