//! Wispr-style "Flow bar": a dark capsule with a live waveform, drawn with
//! CALayers. All Objective-C objects stay on the helper's main thread.
use super::{read_event, Alert, Notice, PillEvent};
use crate::gsettings::GSettings;
use anyhow::{bail, Result};
use std::{
    ffi::{c_char, c_void, CStr, CString},
    io::BufRead,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use xflow_core::{ipc::Request, State};

type Id = *mut c_void;
type Sel = *mut c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra: usize) -> Id;
    fn objc_registerClassPair(class: Id);
    fn class_addMethod(
        class: Id,
        selector: Sel,
        implementation: unsafe extern "C" fn(),
        types: *const c_char,
    ) -> i8;
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
const WIN_W: f64 = 300.0;
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
    sized_frame(screen, position, scale, WIN_W)
}
/// `frame` for a window `win_w` points wide.
fn sized_frame(screen: Rect, position: &str, scale: f64, win_w: f64) -> Rect {
    let width = (win_w * scale).min(screen.size.width);
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

/// `frame` moved one window height off the screen edge the pill sits on.
fn beside_pill(mut frame: Rect, position: &str, scale: f64) -> Rect {
    let step = WIN_H * scale;
    frame.origin.y += if position.starts_with("top") {
        -step
    } else {
        step
    };
    frame
}

/// The alert held after release, at most this long including the fade.
const ALERT_SECS: f64 = 2.0;

/// What the pill says: a failure alert after release, or a listening advisory.
fn label_text(event: &PillEvent) -> Option<&'static str> {
    // Only an error event carries a problem, and it always has an alert.
    if let Some(problem) = event.problem {
        return Some(problem.title());
    }
    Some(match (event.alert, event.ends_soon, event.no_signal) {
        (Some(Alert::Copied), ..) => "Copied — paste with ⌘V",
        (Some(Alert::NoSpeech), ..) => "No speech heard",
        (Some(Alert::Paste), ..) => "Couldn't paste",
        (Some(Alert::Transcribe), ..) => "Couldn't transcribe",
        (Some(Alert::Record), ..) => "Recording failed",
        (None, true, _) => "Recording stops in 1 min",
        (None, _, true) => "Can't hear you — check mic",
        (None, false, false) => return None,
    })
}

/// Static advisories, or the streaming answer in the same text layer.
fn caption(event: &PillEvent) -> Option<String> {
    event
        .text
        .as_ref()
        .filter(|text| !text.is_empty())
        .cloned()
        .or_else(|| label_text(event).map(str::to_owned))
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

/// Loads AppKit (confined to this CLI-only helper; xflowd never links it) and
/// starts an accessory app: no Dock icon. Returns the autorelease pool, the
/// app and the AppKit handle.
unsafe fn appkit() -> Result<(Id, Id, *mut c_void)> {
    let appkit = libc::dlopen(
        c"/System/Library/Frameworks/AppKit.framework/AppKit".as_ptr(),
        libc::RTLD_NOW | libc::RTLD_LOCAL,
    );
    if appkit.is_null() {
        bail!("cannot load AppKit");
    }
    let pool = msg!(class(c"NSAutoreleasePool"), c"new"; Id);
    let app = msg!(class(c"NSApplication"), c"sharedApplication"; Id);
    // Prohibited, not Accessory: launched while the user is idle, an accessory app
    // becomes the front application at `finishLaunching` whatever its window's
    // style says, and every key goes to it until it exits (FB01).
    msg!(app, c"setActivationPolicy:", 2isize => isize; i8);
    Ok((pool, app, appkit))
}

unsafe extern "C" fn never(_: Id, _: Sel) -> i8 {
    0
}
/// An NSPanel that can never be the key or main window, whatever is clicked: its
/// actions still fire, and no key is ever sent to it.
unsafe fn panel_class() -> Result<Id> {
    let name = c"XFlowStatusPanel";
    let mut panel = objc_getClass(name.as_ptr());
    if panel.is_null() {
        panel = objc_allocateClassPair(class(c"NSPanel"), name.as_ptr(), 0);
        let imp = std::mem::transmute::<unsafe extern "C" fn(Id, Sel) -> i8, unsafe extern "C" fn()>(
            never,
        );
        if panel.is_null()
            || class_addMethod(
                panel,
                sel_registerName(c"canBecomeKeyWindow".as_ptr()),
                imp,
                c"c@:".as_ptr(),
            ) == 0
            || class_addMethod(
                panel,
                sel_registerName(c"canBecomeMainWindow".as_ptr()),
                imp,
                c"c@:".as_ptr(),
            ) == 0
        {
            bail!("cannot create the status panel");
        }
        objc_registerClassPair(panel);
    }
    Ok(panel)
}

/// The borderless, non-activating status panel both modes draw in: it never
/// takes focus, so the user's app keeps its key window and caret.
unsafe fn status_panel(window: Rect, label: Id) -> Result<Id> {
    let panel = msg!(panel_class()?, c"alloc"; Id);
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
    msg!(panel, c"setAccessibilityLabel:", label => Id; ());
    // All Spaces, stationary, outside window cycling, over full-screen apps.
    msg!(panel, c"setCollectionBehavior:", 1usize | (1 << 4) | (1 << 6) | (1 << 8) => usize; ());
    msg!(panel, c"setOpaque:", 0i8 => i8; ());
    msg!(panel, c"setHasShadow:", 0i8 => i8; ());
    let clear = msg!(class(c"NSColor"), c"clearColor"; Id);
    msg!(panel, c"setBackgroundColor:", clear => Id; ());
    let view = msg!(panel, c"contentView"; Id);
    msg!(view, c"setWantsLayer:", 1i8 => i8; ());
    Ok(panel)
}

/// How long the learning notice stays, from its first frame: no hover hold.
/// Long enough to read the word and reach Undo with the mouse.
const NOTICE: Duration = Duration::from_millis(4000);
/// The notice window's width before `scale`; the capsule fits its text.
const NOTICE_W: f64 = 360.0;

static NOTICE_PRESSED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn press_notice(_: Id, _: Sel, _: Id) {
    NOTICE_PRESSED.store(true, Ordering::Relaxed);
}

/// Explicit action state: a window being hidden is never an undo request.
unsafe fn notice_target() -> Result<Id> {
    let name = c"XFlowNoticeUndoTarget";
    let mut target = objc_getClass(name.as_ptr());
    if target.is_null() {
        target = objc_allocateClassPair(class(c"NSObject"), name.as_ptr(), 0);
        if target.is_null()
            || class_addMethod(
                target,
                sel_registerName(c"undoNotice:".as_ptr()),
                std::mem::transmute::<unsafe extern "C" fn(Id, Sel, Id), unsafe extern "C" fn()>(
                    press_notice,
                ),
                c"v@:@".as_ptr(),
            ) == 0
        {
            bail!("cannot create the dictionary undo action");
        }
        objc_registerClassPair(target);
    }
    let action = msg!(target, c"new"; Id);
    if action.is_null() {
        bail!("cannot create the dictionary undo action");
    }
    Ok(action)
}

/// What the notice says: the exact saved word(s).
fn notice_text(notice: &Notice) -> String {
    let first = &notice.words[0];
    let more = match notice.words.len() {
        1 => String::new(),
        n => format!(" +{}", n - 1),
    };
    if notice.correction {
        format!("“{first}”{more} correction saved")
    } else {
        format!("“{first}”{more} saved to dictionary")
    }
}
/// The Undo button's accessibility label: every word it removes.
fn undo_label(notice: &Notice) -> String {
    let words: Vec<String> = notice.words.iter().map(|w| format!("“{w}”")).collect();
    format!("Undo dictionary changes for {}", words.join(", "))
}
fn cstring(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_default()
}

/// The undo request outlives the hidden panel by at most two seconds.
fn undo(op: u64) {
    let (done, finished) = mpsc::channel();
    let spawned = std::thread::Builder::new().spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        if let Ok(runtime) = runtime {
            let request = Request::UndoLearned { op: Some(op) };
            let _ = runtime.block_on(crate::transport::request(&request));
        }
        let _ = done.send(());
    });
    if spawned.is_ok() {
        let _ = finished.recv_timeout(Duration::from_secs(2));
    }
}

/// The learning notice: the saved word and an Undo (×) for four seconds, timed
/// from its first committed frame (the screen shows it up to a frame later),
/// then gone. No animation, no hover extension. The panel never activates, so
/// the caret stays in the user's app, and it sits one pill clear of where the
/// recording pill draws, so both can show. The × is a real momentary button
/// (VoiceOver: "Undo: remove … from dictionary", pressed through accessibility
/// like a click); the save is also announced, and `xflow dictionary undo` is the
/// durable route.
pub(super) fn notice(notice: Notice, settings: GSettings) -> Result<()> {
    let s: f64 = settings.get("scale")?.parse()?;
    let opacity: f64 = settings.get("opacity")?.parse()?;
    let position = settings.get("position")?;
    let text = cstring(&notice_text(&notice));
    let label = cstring(&undo_label(&notice));
    unsafe {
        let (pool, app, appkit) = appkit()?;
        let screen = active_screen();
        if screen.is_null() {
            bail!("no screen is available for the learning notice");
        }
        let mut colors = Vec::new();
        let mut color = |r, g, b, a| {
            let color = CGColorCreateSRGB(r, g, b, a);
            colors.push(color);
            color
        };
        let (dark, white) = (color(0.0, 0.0, 0.0, 0.85), color(1.0, 1.0, 1.0, 1.0));
        let faint = color(1.0, 1.0, 1.0, 0.16);
        let rim = color(1.0, 1.0, 1.0, 0.14);
        let words = msg!(class(c"CATextLayer"), c"layer"; Id);
        msg!(words, c"setString:", string(&text) => Id; ());
        msg!(words, c"setFontSize:", 12.0 * s => f64; ());
        msg!(words, c"setForegroundColor:", white => Id; ());
        msg!(words, c"setContentsScale:", msg!(screen, c"backingScaleFactor"; f64) => f64; ());
        msg!(words, c"setTruncationMode:", string(c"middle") => Id; ());
        // The capsule fits the text and the ×, within the window.
        let pad = (PILL_H - BUTTON) / 2.0 * s;
        let room = NOTICE_W * s - 4.0 * s - BUTTON * s - 3.0 * pad;
        let text_w = msg!(words, c"preferredFrameSize"; Size).width.min(room);
        let (w, h) = (text_w + BUTTON * s + 3.0 * pad + 6.0 * s, PILL_H * s);
        // The window is the capsule plus its padding: clicks beside it reach the app below.
        let win_w = w / s + (WIN_H - PILL_H);
        let window = beside_pill(
            sized_frame(
                rect_of(screen, c"visibleFrame"),
                position.trim_matches('\''),
                s,
                win_w,
            ),
            position.trim_matches('\''),
            s,
        );
        let panel = status_panel(window, string(&text))?;
        let view = msg!(panel, c"contentView"; Id);
        let root = msg!(view, c"layer"; Id);
        let (cx, cy) = (window.size.width / 2.0, window.size.height / 2.0);
        let capsule = layer(root, dark, h / 2.0);
        msg!(capsule, c"setBorderWidth:", 0.75 * s => f64; ());
        msg!(capsule, c"setBorderColor:", rim => Id; ());
        msg!(capsule, c"setOpacity:", opacity as f32 => f32; ());
        msg!(capsule, c"addSublayer:", words => Id; ());
        let close = layer(capsule, faint, BUTTON * s / 2.0);
        for angle in [std::f64::consts::FRAC_PI_4, -std::f64::consts::FRAC_PI_4] {
            let stroke = layer(close, white, 0.8 * s);
            place(
                stroke,
                BUTTON * s / 2.0,
                BUTTON * s / 2.0,
                1.6 * s,
                10.0 * s,
            );
            transform(stroke, 1.0, angle, 0.0);
        }
        msg!(class(c"CATransaction"), c"begin"; ());
        msg!(class(c"CATransaction"), c"setDisableActions:", 1i8 => i8; ());
        place(capsule, cx, cy, w, h);
        place(
            words,
            pad + 3.0 * s + text_w / 2.0,
            h / 2.0,
            text_w,
            16.0 * s,
        );
        let button_x = w - pad - BUTTON * s / 2.0;
        place(close, button_x, h / 2.0, BUTTON * s, BUTTON * s);
        msg!(class(c"CATransaction"), c"commit"; ());
        // In window coordinates, like mouse locations.
        let (bx, by) = (cx - w / 2.0 + button_x, cy);
        let reach = BUTTON * s / 2.0 + 2.0;
        // Both a mouse click and an accessibility press explicitly request undo.
        NOTICE_PRESSED.store(false, Ordering::Relaxed);
        let action = notice_target()?;
        let button = msg!(class(c"NSButton"), c"alloc"; Id);
        let button = msg!(button, c"initWithFrame:",
            Rect::new(bx - reach, by - reach, 2.0 * reach, 2.0 * reach) => Rect; Id);
        if !button.is_null() {
            msg!(button, c"setButtonType:", 7usize => usize; ()); // NSButtonTypeMomentaryPushIn
            msg!(button, c"setBordered:", 0i8 => i8; ());
            msg!(button, c"setTitle:", string(c"") => Id; ());
            msg!(button, c"setAccessibilityLabel:", string(&label) => Id; ());
            msg!(button, c"setTarget:", action => Id; ());
            msg!(button, c"setAction:", sel_registerName(c"undoNotice:".as_ptr()) => Sel; ());
            msg!(view, c"addSubview:", button => Id; ());
            msg!(button, c"release"; ());
        }
        msg!(panel, c"setIgnoresMouseEvents:", 0i8 => i8; ());
        msg!(app, c"finishLaunching"; ());
        msg!(panel, c"orderFrontRegardless"; ());
        msg!(class(c"CATransaction"), c"flush"; ());
        let shown = Instant::now();
        // VoiceOver hears the save even though the button is out of reach in time.
        let post = libc::dlsym(
            appkit,
            c"NSAccessibilityPostNotificationWithUserInfo".as_ptr(),
        );
        if !post.is_null() {
            let post: unsafe extern "C" fn(Id, Id, Id) = std::mem::transmute(post);
            let info = msg!(class(c"NSMutableDictionary"), c"dictionary"; Id);
            msg!(info, c"setObject:forKey:", string(&text) => Id, string(c"AXAnnouncementKey") => Id; ());
            let high = msg!(class(c"NSNumber"), c"numberWithInteger:", 90isize => isize; Id);
            msg!(info, c"setObject:forKey:", high => Id, string(c"AXPriorityKey") => Id; ());
            post(app, string(c"AXAnnouncementRequested"), info);
        }
        let mut pressed = false;
        while !pressed {
            let left = NOTICE.saturating_sub(shown.elapsed());
            if left.is_zero() {
                break;
            }
            let tick_pool = msg!(class(c"NSAutoreleasePool"), c"new"; Id);
            let wait = left.as_secs_f64().min(1.0 / 60.0);
            let deadline =
                msg!(class(c"NSDate"), c"dateWithTimeIntervalSinceNow:", wait => f64; Id);
            let event = msg!(app, c"nextEventMatchingMask:untilDate:inMode:dequeue:",
                usize::MAX => usize, deadline => Id, string(c"kCFRunLoopDefaultMode") => Id, 1i8 => i8; Id);
            if !event.is_null() {
                // The raw hit test also works where a non-key panel's button
                // would not take the first click.
                if msg!(event, c"type"; usize) == 1 && msg!(event, c"window"; Id) == panel {
                    let p = msg!(event, c"locationInWindow"; Point);
                    pressed = (p.x - bx).abs() <= reach && (p.y - by).abs() <= reach;
                }
                // A handled click is not passed on: the button's tracking loop
                // would hold the notice until the mouse is released.
                if !pressed {
                    msg!(app, c"sendEvent:", event => Id; ());
                }
            }
            pressed |= NOTICE_PRESSED.load(Ordering::Relaxed);
            msg!(tick_pool, c"drain"; ());
        }
        msg!(panel, c"orderOut:", std::ptr::null_mut::<c_void>() => Id; ());
        if std::env::var_os("XFLOW_PILL_TIMING").is_some() {
            eprintln!(
                "notice visible_ms={} pressed={pressed}",
                shown.elapsed().as_millis()
            );
        }
        if pressed {
            undo(notice.op);
        }
        msg!(panel, c"close"; ());
        msg!(panel, c"release"; ());
        msg!(action, c"release"; ());
        for color in colors {
            CGColorRelease(color);
        }
        msg!(pool, c"drain"; ());
    }
    Ok(())
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
                        let event = match other {
                            Ok(Some(event)) => event,
                            _ => PillEvent::default(),
                        };
                        let _ = events_tx.send(event);
                        break;
                    }
                }
            }
        })?;
    unsafe {
        let (pool, app, _) = appkit()?;
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
        let first_caption =
            cstring(&caption(&first).unwrap_or_else(|| "XFlow recording status".into()));
        let panel = status_panel(window, string(first_caption.as_c_str()))?;
        let view = msg!(panel, c"contentView"; Id);
        let root = msg!(view, c"layer"; Id);

        let mut colors = Vec::new(); // layers retain what they use; released at exit
        let mut color = |r, g, b, a| {
            let color = CGColorCreateSRGB(r, g, b, a);
            colors.push(color);
            color
        };
        let dark = color(0.0, 0.0, 0.0, 0.85);
        let alarm = color(0.62, 0.09, 0.09, 0.92);
        let amber = color(0.62, 0.38, 0.03, 0.92);
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

        let copied_label = msg!(class(c"CATextLayer"), c"layer"; Id);
        msg!(copied_label, c"setString:", string(first_caption.as_c_str()) => Id; ());
        msg!(copied_label, c"setFontSize:", 12.0 * s => f64; ());
        msg!(copied_label, c"setAlignmentMode:", string(c"center") => Id; ());
        msg!(copied_label, c"setForegroundColor:", white => Id; ());
        msg!(copied_label, c"setContentsScale:", msg!(screen, c"backingScaleFactor"; f64) => f64; ());
        msg!(capsule, c"addSublayer:", copied_label => Id; ());

        msg!(app, c"finishLaunching"; ());
        msg!(panel, c"orderFrontRegardless"; ());
        {
            use std::io::Write;
            // The daemon reads this into `Timings::pill_ms`; nobody may be listening.
            let _ = writeln!(std::io::stdout(), "shown {}", crate::daemon::monotonic_us());
        }

        let mut wave = Wave::new();
        // PillEvent is no longer Copy: read the first frame before moving it.
        let level = first.level;
        let mut closing = (!first.active()).then(|| (Instant::now(), first.state));
        let mut state = first;
        wave.set_level(level);
        let mut width = if reduce_motion { LISTEN_W } else { PILL_H }; // grow from a stub
        let mut buttons = 0.0;
        let mut clickable = false;
        let shown = Instant::now();
        let mut last = shown;
        loop {
            let tick_pool = msg!(class(c"NSAutoreleasePool"), c"new"; Id);
            while let Ok(event) = events.try_recv() {
                if closing.is_some() {
                    continue; // e.g. EOF after an alert frame
                }
                if event.state == State::Listening {
                    wave.set_level(event.level);
                }
                if !event.active() {
                    closing = Some((Instant::now(), event.state));
                }
                if caption(&event) != caption(&state) {
                    let spoken = caption(&event).unwrap_or_else(|| "XFlow recording status".into());
                    let owned = cstring(&spoken);
                    msg!(copied_label, c"setString:", string(owned.as_c_str()) => Id; ());
                    msg!(panel, c"setAccessibilityLabel:", string(owned.as_c_str()) => Id; ());
                }
                state = event;
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
            let alert = state.alert.is_some();
            let hold = state.text.as_ref().is_some_and(|text| !text.is_empty());
            let notice = caption(&state).is_some();
            width += ((if notice {
                250.0
            } else if hands_free {
                HANDS_FREE_W
            } else {
                LISTEN_W
            }) - width)
                * follow;
            buttons += (f64::from(u8::from(hands_free)) - buttons) * follow;
            let appear = if reduce_motion {
                1.0
            } else {
                ease_out(shown.elapsed().as_secs_f64() / 0.18)
            };
            let (mut alpha, mut scale, mut shake) = (appear, 0.6 + 0.4 * appear, 0.0);
            if let Some((at, _)) = closing {
                let e = at.elapsed().as_secs_f64();
                let fade = if reduce_motion { 0.0 } else { 0.15 };
                let fade_from = if hold {
                    super::answer_hold_secs(state.text.as_deref().unwrap_or("")) - fade
                } else if alert {
                    ALERT_SECS - fade
                } else {
                    0.0
                };
                if e >= fade_from + fade {
                    msg!(tick_pool, c"drain"; ());
                    break;
                }
                if alert && !reduce_motion && e < 0.3 {
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
            msg!(capsule, c"setBackgroundColor:", if alert { alarm } else if notice { amber } else { dark } => Id; ());
            place(copied_label, w / 2.0, h / 2.0, w - 16.0 * s, 16.0 * s);
            msg!(copied_label, c"setOpacity:", if notice { 1.0f32 } else { 0.0f32 } => f32; ());
            let pitch = (BAR_W + BAR_GAP) * s;
            let first_x = w / 2.0 - pitch * (BARS - 1) as f64 / 2.0;
            for (i, (&bar, (height, bar_alpha))) in bars.iter().zip(heights).enumerate() {
                let bar_h = BAR_W * s + (BAR_MAX - BAR_W) * s * height;
                place(bar, first_x + i as f64 * pitch, h / 2.0, BAR_W * s, bar_h);
                msg!(bar, c"setOpacity:", if notice { 0.0f32 } else { bar_alpha as f32 } => f32; ());
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
    fn the_notice_sits_clear_of_the_recording_pill() {
        let screen = Rect::new(0.0, 0.0, 1000.0, 800.0);
        for (position, scale) in [("bottom-center", 1.0), ("top-right", 1.5)] {
            let pill = sized_frame(screen, position, scale, 200.0);
            let notice = beside_pill(pill, position, scale);
            let (low, high) = if position.starts_with("top") {
                (notice, pill)
            } else {
                (pill, notice)
            };
            assert!(
                low.origin.y + low.size.height <= high.origin.y + 1e-6,
                "{position}"
            );
        }
    }

    #[test]
    fn label_text_matches_the_daemon_messages_and_alerts_win() {
        let text = label_text;
        let mut event = PillEvent::default();
        assert_eq!(text(&event), None);
        event.no_signal = true;
        assert_eq!(text(&event), Some(crate::daemon::NO_SIGNAL));
        event.ends_soon = true;
        assert_eq!(text(&event), Some(crate::daemon::ENDS_SOON));
        event.alert = Some(Alert::Copied);
        assert_eq!(text(&event), Some("Copied — paste with ⌘V"));
        event.alert = Some(Alert::Transcribe);
        assert_eq!(text(&event), Some("Couldn't transcribe"));
        event.problem = Some(xflow_core::problem::Problem::KeyRejected);
        assert_eq!(text(&event), Some("Key rejected"));
    }
    #[test]
    fn notice_names_the_saved_words_and_its_undo() {
        let mut notice = Notice {
            op: 1,
            words: vec!["claud".into()],
            correction: false,
        };
        assert_eq!(notice_text(&notice), "“claud” saved to dictionary");
        assert_eq!(undo_label(&notice), "Undo dictionary changes for “claud”");
        notice.words.push("Kubernetes".into());
        assert_eq!(notice_text(&notice), "“claud” +1 saved to dictionary");
        assert!(undo_label(&notice).contains("“Kubernetes”"));
        notice.words = vec!["Rimpa Sarkar".into()];
        notice.correction = true;
        assert_eq!(notice_text(&notice), "“Rimpa Sarkar” correction saved");
        assert_eq!(
            undo_label(&notice),
            "Undo dictionary changes for “Rimpa Sarkar”"
        );
        assert_eq!(cstring("a\0b").to_str().unwrap(), "ab");
    }

    /// Nothing clicked on the panel can make it receive keys.
    #[test]
    fn the_panel_can_never_take_key_status() {
        unsafe {
            let (_pool, _app, _appkit) = appkit().unwrap();
            let panel = msg!(panel_class().unwrap(), c"alloc"; Id);
            assert_eq!(msg!(panel, c"canBecomeKeyWindow"; i8), 0);
            assert_eq!(msg!(panel, c"canBecomeMainWindow"; i8), 0);
        }
    }
    #[test]
    fn notice_undo_requires_an_explicit_action() {
        NOTICE_PRESSED.store(false, Ordering::Relaxed);
        assert!(!NOTICE_PRESSED.load(Ordering::Relaxed));
        unsafe {
            press_notice(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert!(NOTICE_PRESSED.swap(false, Ordering::Relaxed));
    }
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
