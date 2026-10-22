//! Wispr-style "Flow bar": a dark capsule with a live waveform, drawn with
//! CALayers. All Objective-C objects stay on the helper's main thread.
use super::{read_event, PillEvent};
use crate::gsettings::GSettings;
use anyhow::{bail, Result};
use std::{
    ffi::{c_char, c_void, CStr},
    io::BufRead,
    sync::mpsc,
    time::Instant,
};
use xflow_core::{ipc::Request, State};

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
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorCreateSRGB(red: f64, green: f64, blue: f64, alpha: f64) -> Id;
    fn CGColorRelease(color: Id);
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
#[repr(C)]
#[derive(Clone, Copy)]
struct Affine {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
}

impl Rect {
    fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            origin: Point { x, y },
            size: Size { width, height },
        }
    }
    fn contains(self, p: Point) -> bool {
        p.x >= self.origin.x
            && p.y >= self.origin.y
            && p.x < self.origin.x + self.size.width
            && p.y < self.origin.y + self.size.height
    }
}

// Geometry in points before the `scale` setting. The window is sized for the
// widest (hands-free) capsule so width changes never move the window.
const WIN_W: f64 = 158.0;
const WIN_H: f64 = 38.0;
const PILL_H: f64 = 34.0;
const LISTEN_W: f64 = 110.0;
const HANDS_FREE_W: f64 = 150.0;
const BAR_W: f64 = 2.5;
const BAR_GAP: f64 = 2.75;
const BAR_MAX: f64 = 18.0;
const BUTTON: f64 = 22.0;
const BARS: usize = 9;

/// Bar heights (0..1) for the waveform: per-sample targets are centre-weighted
/// with jitter so it reads as a voice; quick attack, slower release.
pub(super) struct Wave {
    values: [f64; BARS],
    targets: [f64; BARS],
    seed: u32,
}
impl Wave {
    pub(super) fn new() -> Self {
        Self {
            values: [0.0; BARS],
            targets: [0.0; BARS],
            seed: 0x9e37_79b9,
        }
    }
    pub(super) fn set_level(&mut self, level: f32) {
        // Speech RMS sits far below full scale: -52 dBFS -> 0, -14 dBFS -> 1.
        let amplitude = if level > 0.0 {
            ((20.0 * f64::from(level).log10() + 52.0) / 38.0).clamp(0.0, 1.0)
        } else {
            0.0
        };
        for i in 0..BARS {
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 17;
            self.seed ^= self.seed << 5;
            let jitter = 0.62 + 0.38 * f64::from(self.seed) / f64::from(u32::MAX);
            let d = (i as f64 - (BARS - 1) as f64 / 2.0).abs() / ((BARS - 1) as f64 / 2.0);
            self.targets[i] = amplitude * (1.0 - 0.72 * d * d) * jitter;
        }
    }
    /// Heights and opacities after `dt_ms`; processing collapses the bars to
    /// dots with a travelling shimmer.
    pub(super) fn step(&mut self, dt_ms: f64, now_ms: f64, processing: bool) -> [(f64, f64); BARS] {
        let dt = dt_ms.clamp(0.0, 100.0);
        let relax = (-dt / 260.0).exp();
        let attack = 1.0 - (-dt / 45.0).exp();
        let decay = 1.0 - (-dt / 140.0).exp();
        let shimmer = -2.0 + (now_ms % 1150.0) / 1150.0 * (BARS as f64 + 3.0);
        let mut out = [(0.0, 1.0); BARS];
        for (i, bar) in out.iter_mut().enumerate() {
            self.targets[i] = if processing {
                0.0
            } else {
                self.targets[i] * relax
            };
            let (target, value) = (self.targets[i], self.values[i]);
            self.values[i] = value + (target - value) * if target > value { attack } else { decay };
            *bar = if processing {
                let glow = (-(i as f64 - shimmer).powi(2) / 3.2).exp();
                (self.values[i], 0.35 + 0.65 * glow)
            } else {
                // A faint ripple keeps a quiet room looking alive.
                let ripple = 0.04 * (1.0 + (now_ms / 280.0 + i as f64 * 0.8).sin());
                (self.values[i].max(ripple), 1.0)
            };
        }
        out
    }
}

unsafe fn class(name: &CStr) -> Id {
    objc_getClass(name.as_ptr())
}
unsafe fn string(text: &CStr) -> Id {
    msg!(class(c"NSString"), c"stringWithUTF8String:", text.as_ptr() => *const c_char; Id)
}
unsafe fn rect_of(object: Id, selector: &CStr) -> Rect {
    #[cfg(target_arch = "x86_64")]
    {
        // x86_64 returns NSRect through objc_msgSend_stret; arm64 uses msgSend.
        let call: unsafe extern "C" fn(*mut Rect, Id, Sel) =
            std::mem::transmute(objc_msgSend_stret as *const ());
        let mut rect = Rect::new(0.0, 0.0, 0.0, 0.0);
        call(&mut rect, object, sel_registerName(selector.as_ptr()));
        rect
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        msg!(object, selector; Rect)
    }
}
unsafe fn layer(parent: Id, color: Id, radius: f64) -> Id {
    let layer = msg!(class(c"CALayer"), c"layer"; Id);
    msg!(layer, c"setBackgroundColor:", color => Id; ());
    msg!(layer, c"setCornerRadius:", radius => f64; ());
    msg!(parent, c"addSublayer:", layer => Id; ());
    layer
}
unsafe fn place(layer: Id, x: f64, y: f64, width: f64, height: f64) {
    msg!(layer, c"setBounds:", Rect::new(0.0, 0.0, width, height) => Rect; ());
    msg!(layer, c"setPosition:", Point { x, y } => Point; ());
}
unsafe fn transform(layer: Id, scale: f64, angle: f64, tx: f64) {
    let (sin, cos) = angle.sin_cos();
    let t = Affine {
        a: scale * cos,
        b: scale * sin,
        c: -scale * sin,
        d: scale * cos,
        tx,
        ty: 0.0,
    };
    msg!(layer, c"setAffineTransform:", t => Affine; ());
}

/// The screen under the mouse, so the pill follows the user across monitors.
unsafe fn active_screen() -> Id {
    let mouse = msg!(class(c"NSEvent"), c"mouseLocation"; Point);
    let screens = msg!(class(c"NSScreen"), c"screens"; Id);
    for i in 0..msg!(screens, c"count"; usize) {
        let screen = msg!(screens, c"objectAtIndex:", i => usize; Id);
        if rect_of(screen, c"frame").contains(mouse) {
            return screen;
        }
    }
    msg!(class(c"NSScreen"), c"mainScreen"; Id)
}

fn frame(screen: Rect, position: &str, scale: f64) -> Rect {
    let width = (WIN_W * scale).min(screen.size.width);
    let height = (WIN_H * scale).min(screen.size.height);
    // The capsule sits ~10 pt above the Dock/bottom edge (window padding included).
    let margin = (10.0 - (WIN_H - PILL_H) / 2.0) * scale;
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

fn ease_out(p: f64) -> f64 {
    1.0 - (1.0 - p.clamp(0.0, 1.0)).powi(3)
}

/// Clicks on the hands-free ✕/■ go to the daemon like `xflow cancel`/`stop`.
fn send(request: Request) {
    let _ = std::thread::Builder::new().spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        if let Ok(runtime) = runtime {
            let _ = runtime.block_on(crate::transport::request(&request));
        }
    });
}

pub(super) fn run(
    first: PillEvent,
    mut input: impl BufRead + Send + 'static,
    settings: GSettings,
) -> Result<()> {
    let s: f64 = settings.get("scale")?.parse()?;
    let opacity: f64 = settings.get("opacity")?.parse()?;
    let position = settings.get("position")?;
    let (events_tx, events) = mpsc::channel();
    std::thread::Builder::new()
        .name("pill-input".into())
        .spawn(move || {
            loop {
                match read_event(&mut input) {
                    Ok(Some(event)) if event.active() => {
                        let _ = events_tx.send(event);
                    }
                    // A final success/error frame, or EOF/garbage meaning idle.
                    other => {
                        let state = match other {
                            Ok(Some(event)) => event.state,
                            _ => State::Idle,
                        };
                        let _ = events_tx.send(PillEvent {
                            state,
                            level: 0.0,
                            hands_free: false,
                        });
                        break;
                    }
                }
            }
        })?;
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
        let workspace = msg!(class(c"NSWorkspace"), c"sharedWorkspace"; Id);
        let reduce_motion = msg!(workspace, c"accessibilityDisplayShouldReduceMotion"; i8) != 0;
        let screen = active_screen();
        if screen.is_null() {
            bail!("no screen is available for the recording pill");
        }
        let window = frame(
            rect_of(screen, c"visibleFrame"),
            position.trim_matches('\''),
            s,
        );
        let panel = msg!(class(c"NSPanel"), c"alloc"; Id);
        // Borderless | non-activating panel: never takes focus.
        let panel = msg!(panel, c"initWithContentRect:styleMask:backing:defer:",
            window => Rect, 1usize << 7 => usize, 2usize => usize, 0i8 => i8; Id);
        if panel.is_null() {
            bail!("cannot create the recording pill");
        }
        msg!(panel, c"setReleasedWhenClosed:", 0i8 => i8; ());
        msg!(panel, c"setLevel:", 25isize => isize; ()); // NSStatusWindowLevel
        msg!(panel, c"setFloatingPanel:", 1i8 => i8; ());
        msg!(panel, c"setBecomesKeyOnlyIfNeeded:", 1i8 => i8; ());
        msg!(panel, c"setHidesOnDeactivate:", 0i8 => i8; ());
        msg!(panel, c"setIgnoresMouseEvents:", 1i8 => i8; ());
        msg!(panel, c"setAccessibilityLabel:", string(c"XFlow recording status") => Id; ());
        // All Spaces, stationary, outside window cycling, over full-screen apps.
        msg!(panel, c"setCollectionBehavior:", 1usize | (1 << 4) | (1 << 6) | (1 << 8) => usize; ());
        msg!(panel, c"setOpaque:", 0i8 => i8; ());
        msg!(panel, c"setHasShadow:", 0i8 => i8; ());
        let clear = msg!(class(c"NSColor"), c"clearColor"; Id);
        msg!(panel, c"setBackgroundColor:", clear => Id; ());
        let view = msg!(panel, c"contentView"; Id);
        msg!(view, c"setWantsLayer:", 1i8 => i8; ());
        let root = msg!(view, c"layer"; Id);

        let mut colors = Vec::new(); // layers retain what they use; released at exit
        let mut color = |r, g, b, a| {
            let color = CGColorCreateSRGB(r, g, b, a);
            colors.push(color);
            color
        };
        let dark = color(0.0, 0.0, 0.0, 0.85);
        let alarm = color(0.62, 0.09, 0.09, 0.92);
        let white = color(1.0, 1.0, 1.0, 1.0);
        let capsule = layer(root, dark, PILL_H * s / 2.0);
        msg!(capsule, c"setBorderWidth:", 0.75 * s => f64; ());
        msg!(capsule, c"setBorderColor:", color(1.0, 1.0, 1.0, 0.14) => Id; ());
        let bars: Vec<Id> = (0..BARS)
            .map(|_| layer(capsule, white, BAR_W * s / 2.0))
            .collect();
        let cancel = layer(capsule, color(1.0, 1.0, 1.0, 0.16), BUTTON * s / 2.0);
        for angle in [std::f64::consts::FRAC_PI_4, -std::f64::consts::FRAC_PI_4] {
            let stroke = layer(cancel, white, 0.8 * s);
            place(
                stroke,
                BUTTON * s / 2.0,
                BUTTON * s / 2.0,
                1.6 * s,
                10.0 * s,
            );
            transform(stroke, 1.0, angle, 0.0);
        }
        let stop = layer(capsule, color(1.0, 0.23, 0.19, 1.0), BUTTON * s / 2.0);
        let square = layer(stop, white, 1.5 * s);
        place(square, BUTTON * s / 2.0, BUTTON * s / 2.0, 8.0 * s, 8.0 * s);

        msg!(app, c"finishLaunching"; ());
        msg!(panel, c"orderFrontRegardless"; ());

        let mut wave = Wave::new();
        let mut state = first;
        wave.set_level(first.level);
        let mut width = if reduce_motion { LISTEN_W } else { PILL_H }; // grow from a stub
        let mut buttons = 0.0;
        let mut clickable = false;
        let shown = Instant::now();
        let mut last = shown;
        let mut closing: Option<(Instant, State)> = None;
        loop {
            let tick_pool = msg!(class(c"NSAutoreleasePool"), c"new"; Id);
            while let Ok(event) = events.try_recv() {
                if event.active() {
                    if event.state == State::Listening {
                        wave.set_level(event.level);
                    }
                    state = event;
                } else {
                    closing.get_or_insert((Instant::now(), event.state));
                }
            }
            let now = Instant::now();
            let dt = now.duration_since(last).as_secs_f64() * 1000.0;
            last = now;
            let hands_free =
                state.hands_free && state.state == State::Listening && closing.is_none();
            if hands_free != clickable {
                clickable = hands_free;
                msg!(panel, c"setIgnoresMouseEvents:", i8::from(!clickable) => i8; ());
            }
            let follow = if reduce_motion {
                1.0
            } else {
                1.0 - (-dt / 70.0).exp()
            };
            width += ((if hands_free { HANDS_FREE_W } else { LISTEN_W }) - width) * follow;
            buttons += (f64::from(u8::from(hands_free)) - buttons) * follow;
            let appear = if reduce_motion {
                1.0
            } else {
                ease_out(shown.elapsed().as_secs_f64() / 0.18)
            };
            let (mut alpha, mut scale, mut shake, mut error) =
                (appear, 0.6 + 0.4 * appear, 0.0, false);
            if let Some((at, final_state)) = closing {
                let e = at.elapsed().as_secs_f64();
                error = final_state == State::Error;
                let fade_from = if error { 0.45 } else { 0.0 };
                let fade = if reduce_motion { 0.0 } else { 0.15 };
                if e >= fade_from + fade {
                    msg!(tick_pool, c"drain"; ());
                    break;
                }
                if error && !reduce_motion && e < 0.3 {
                    shake = 4.0 * s * (e * std::f64::consts::TAU * 12.0).sin() * (1.0 - e / 0.3);
                }
                let q = if fade > 0.0 {
                    ((e - fade_from) / fade).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                alpha *= 1.0 - q;
                scale *= 1.0 - 0.08 * q;
            }

            let heights = wave.step(
                dt,
                shown.elapsed().as_secs_f64() * 1000.0,
                state.state == State::Processing,
            );
            msg!(class(c"CATransaction"), c"begin"; ());
            msg!(class(c"CATransaction"), c"setDisableActions:", 1i8 => i8; ());
            let (w, h) = (width * s, PILL_H * s);
            place(capsule, WIN_W * s / 2.0, WIN_H * s / 2.0, w, h);
            transform(capsule, scale, 0.0, shake);
            msg!(capsule, c"setOpacity:", (alpha * opacity) as f32 => f32; ());
            msg!(capsule, c"setBackgroundColor:", if error { alarm } else { dark } => Id; ());
            let pitch = (BAR_W + BAR_GAP) * s;
            let first_x = w / 2.0 - pitch * (BARS - 1) as f64 / 2.0;
            for (i, (&bar, (height, bar_alpha))) in bars.iter().zip(heights).enumerate() {
                let bar_h = BAR_W * s + (BAR_MAX - BAR_W) * s * height;
                place(bar, first_x + i as f64 * pitch, h / 2.0, BAR_W * s, bar_h);
                msg!(bar, c"setOpacity:", bar_alpha as f32 => f32; ());
            }
            let inset = (PILL_H - BUTTON) / 2.0 * s + BUTTON * s / 2.0;
            place(cancel, inset, h / 2.0, BUTTON * s, BUTTON * s);
            place(stop, w - inset, h / 2.0, BUTTON * s, BUTTON * s);
            for button in [cancel, stop] {
                msg!(button, c"setOpacity:", buttons as f32 => f32; ());
            }
            msg!(class(c"CATransaction"), c"commit"; ());

            // ~60 fps only while visible; the helper exits when the session ends.
            let deadline =
                msg!(class(c"NSDate"), c"dateWithTimeIntervalSinceNow:", 1.0f64 / 60.0 => f64; Id);
            let event = msg!(app, c"nextEventMatchingMask:untilDate:inMode:dequeue:",
                usize::MAX => usize, deadline => Id, string(c"kCFRunLoopDefaultMode") => Id, 1i8 => i8; Id);
            if !event.is_null() {
                if clickable
                    && msg!(event, c"type"; usize) == 1
                    && msg!(event, c"window"; Id) == panel
                {
                    let p = msg!(event, c"locationInWindow"; Point);
                    let (centre, reach) = (WIN_W * s / 2.0, BUTTON * s / 2.0 + 2.0);
                    let hit =
                        |x: f64| (p.x - x).abs() <= reach && (p.y - WIN_H * s / 2.0).abs() <= reach;
                    if hit(centre - w / 2.0 + inset) {
                        send(Request::Cancel);
                    } else if hit(centre + w / 2.0 - inset) {
                        send(Request::Stop);
                    }
                }
                msg!(app, c"sendEvent:", event => Id; ());
            }
            msg!(tick_pool, c"drain"; ());
        }
        msg!(panel, c"orderOut:", std::ptr::null_mut::<c_void>() => Id; ());
        msg!(panel, c"close"; ());
        msg!(panel, c"release"; ());
        for color in colors {
            CGColorRelease(color);
        }
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
        // Capsule bottom edge = window y + padding = 10 pt above the visible area.
        assert_eq!(
            frame(screen, "bottom", 1.0).origin.y + (WIN_H - PILL_H) / 2.0,
            50.0
        );
    }

    #[test]
    fn waveform_is_centre_weighted_decays_and_shimmers_when_processing() {
        let mut wave = Wave::new();
        wave.set_level(0.1);
        let mut bars = wave.step(16.0, 0.0, false);
        for _ in 0..5 {
            bars = wave.step(16.0, 0.0, false);
        }
        assert!(bars
            .iter()
            .all(|&(h, a)| (0.0..=1.0).contains(&h) && a == 1.0));
        assert!(bars[BARS / 2].0 > bars[0].0, "centre-weighted: {bars:?}");
        for _ in 0..200 {
            bars = wave.step(16.0, 0.0, false);
        }
        assert!(
            bars.iter().all(|&(h, _)| h <= 0.08),
            "silence settles: {bars:?}"
        );
        let processing = wave.step(16.0, 500.0, true);
        let alphas: Vec<f64> = processing.iter().map(|b| b.1).collect();
        assert!(alphas.iter().any(|&a| a > 0.9) && alphas.iter().any(|&a| a < 0.5));
    }
}
