//! A passive Quartz event tap: no key suppression, AppKit, or idle timer.
use crate::hotkey::{HotkeyMachine, KeyEvent};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::{
    collections::VecDeque,
    ffi::{c_char, c_void},
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tokio::sync::mpsc;
use xflow_core::{
    config::HotkeyConfig,
    replay::{self, Action, Key, Kind, Mods, ReplayIn},
    GlobalHotkeys, HotkeyAction,
};

type Ref = *mut c_void;
type Callback = unsafe extern "C" fn(Ref, u32, Ref, Ref) -> Ref;
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        mask: u64,
        callback: Callback,
        context: Ref,
    ) -> Ref;
    fn CGEventTapEnable(tap: Ref, enable: bool);
    fn CGEventGetFlags(event: Ref) -> u64;
    fn CGEventGetIntegerValueField(event: Ref, field: u32) -> i64;
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    fn CGEventGetTimestamp(event: Ref) -> u64;
    fn CGEventKeyboardGetUnicodeString(event: Ref, max: usize, len: *mut usize, out: *mut u16);
}
extern "C" {
    fn clock_gettime_nsec_np(clock: u32) -> u64;
}
/// CGEvent timestamps are CLOCK_UPTIME_RAW nanoseconds; gestures use them so
/// delivery latency under load cannot stretch a double-tap.
fn now_ms() -> u64 {
    unsafe { clock_gettime_nsec_np(8) / 1_000_000 } // CLOCK_UPTIME_RAW
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: Ref;
    fn CFMachPortCreateRunLoopSource(allocator: Ref, port: Ref, order: isize) -> Ref;
    fn CFMachPortInvalidate(port: Ref);
    fn CFRunLoopGetCurrent() -> Ref;
    fn CFRunLoopAddSource(runloop: Ref, source: Ref, mode: Ref);
    fn CFRunLoopRemoveSource(runloop: Ref, source: Ref, mode: Ref);
    fn CFRunLoopRun();
    fn CFRunLoopStop(runloop: Ref);
    fn CFRunLoopWakeUp(runloop: Ref);
    fn CFRetain(value: Ref) -> Ref;
    fn CFRelease(value: *const c_void); // matches macos.rs
}

const FLAGS_CHANGED: u32 = 12;
const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const LEFT_DOWN: u32 = 1;
const RIGHT_DOWN: u32 = 3;
const OTHER_DOWN: u32 = 25;
/// `CGEventField` values from CGEventTypes.h.
const TARGET_PID: u32 = 40;
const SOURCE_PID: u32 = 41;
/// `kCGEventSourceUserData`. Delivery stamps this so a watch skips its own keys.
pub(crate) const USER_DATA: u32 = 42;
pub(crate) const INJECTED: i64 = 0x5846_4c4f_5701;
const SHIFT: u64 = 1 << 17;
const CTRL: u64 = 1 << 18;
const ALT: u64 = 1 << 19;
const CMD: u64 = 1 << 20;
const FN: u64 = 1 << 23;
const MODIFIERS: u64 = SHIFT | CTRL | ALT | CMD;

struct Binding {
    code: u16,
    flag: u64,
    modifiers: u64,
}
impl Binding {
    fn parse(key: &str) -> Self {
        let (code, flag) = match key {
            "fn" => (63, FN),
            "left_option" => (58, 0x20),
            "right_option" => (61, 0x40),
            "right_command" => (54, 0x10),
            "right_control" => (62, 0x2000),
            "right_shift" => (60, 0x4),
            "caps_lock" => (57, 1 << 16),
            _ => (0, 0),
        };
        if flag != 0 {
            return Self {
                code,
                flag,
                modifiers: 0,
            };
        }
        let mut parts: Vec<_> = key.split('+').collect();
        let key = parts.pop().unwrap_or_default();
        let modifiers = parts.iter().fold(0, |flags, name| {
            flags
                | match *name {
                    "ctrl" => CTRL,
                    "alt" => ALT,
                    "shift" => SHIFT,
                    "cmd" => CMD,
                    _ => 0,
                }
        });
        // Virtual key codes are physical ANSI positions; named keys are layout-independent.
        let code = match key {
            "a" => 0,
            "s" => 1,
            "d" => 2,
            "f" => 3,
            "h" => 4,
            "g" => 5,
            "z" => 6,
            "x" => 7,
            "c" => 8,
            "v" => 9,
            "b" => 11,
            "q" => 12,
            "w" => 13,
            "e" => 14,
            "r" => 15,
            "y" => 16,
            "t" => 17,
            "1" => 18,
            "2" => 19,
            "3" => 20,
            "4" => 21,
            "6" => 22,
            "5" => 23,
            "9" => 25,
            "7" => 26,
            "8" => 28,
            "0" => 29,
            "o" => 31,
            "u" => 32,
            "i" => 34,
            "p" => 35,
            "enter" => 36,
            "l" => 37,
            "j" => 38,
            "k" => 40,
            "n" => 45,
            "m" => 46,
            "tab" => 48,
            "space" => 49,
            "backspace" => 51,
            "escape" => 53,
            "f1" => 122,
            "f2" => 120,
            "f3" => 99,
            "f4" => 118,
            "f5" => 96,
            "f6" => 97,
            "f7" => 98,
            "f8" => 100,
            "f9" => 101,
            "f10" => 109,
            "f11" => 103,
            "f12" => 111,
            "f13" => 105,
            "f14" => 107,
            "f15" => 113,
            "f16" => 106,
            "f17" => 64,
            "f18" => 79,
            "f19" => 80,
            "f20" => 90,
            _ => unreachable!("validated hotkey"),
        };
        Self {
            code,
            flag: 0,
            modifiers,
        }
    }
}

struct Input {
    event: KeyEvent,
    at_ms: u64,
}
struct TapContext {
    binding: Binding,
    down: bool,
    tap: Ref,
    at_ms: u64,
    events: mpsc::Sender<Input>,
    overflow: Arc<AtomicBool>,
    received: Arc<AtomicBool>,
}
impl TapContext {
    fn send(&self, event: KeyEvent) {
        let at_ms = self.at_ms;
        if self.events.try_send(Input { event, at_ms }).is_err() {
            self.overflow.store(true, Ordering::Release);
        }
    }
}
/// Set once the hotkey tap is running. Replay cannot listen without it.
static TAP_UP: AtomicBool = AtomicBool::new(false);
static SELF_PID: AtomicI32 = AtomicI32::new(0);
struct ReplayBus {
    pid: AtomicI32,
    tx: Mutex<Option<mpsc::Sender<RawEvent>>>,
    missed: AtomicBool,
}
static BUS: ReplayBus = ReplayBus {
    pid: AtomicI32::new(0),
    tx: Mutex::new(None),
    missed: AtomicBool::new(false),
};
const RAW_KEY: u8 = 0;
const RAW_MOUSE: u8 = 1;
const RAW_OTHER: u8 = 2;
const RAW_SECURE: u8 = 3;
const RAW_ABSTAIN: u8 = 4;
#[derive(Clone, Copy)]
pub(crate) struct RawEvent {
    class: u8,
    keycode: u16,
    flags: u64,
    units: [u16; 8],
    len: u8,
}
pub(crate) struct KeyFeed {
    pub rx: mpsc::Receiver<RawEvent>,
}
impl KeyFeed {
    pub(crate) fn missed(&self) -> bool {
        BUS.missed.swap(false, Ordering::AcqRel)
    }
    pub(crate) fn stop(&self) {
        disarm();
    }
}
impl Drop for KeyFeed {
    fn drop(&mut self) {
        disarm();
    }
}
/// `IsSecureEventInputEnabled`, loaded on first use. Carbon stays out of the
/// idle daemon, same as paste. None when the symbol cannot be resolved: a
/// watch must not run without this check.
fn secure_fn() -> Option<unsafe extern "C" fn() -> u8> {
    static SLOT: AtomicUsize = AtomicUsize::new(0);
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let ptr = unsafe {
            extern "C" {
                fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
                fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
            }
            let lib = dlopen(
                c"/System/Library/Frameworks/Carbon.framework/Carbon".as_ptr(),
                1,
            );
            if lib.is_null() {
                0
            } else {
                dlsym(lib, c"IsSecureEventInputEnabled".as_ptr()) as usize
            }
        };
        // 1 means looked up and missing. A real address is larger.
        SLOT.store(if ptr == 0 { 1 } else { ptr }, Ordering::Release);
    });
    match SLOT.load(Ordering::Acquire) {
        ptr if ptr > 1 => Some(unsafe { std::mem::transmute(ptr) }),
        _ => None,
    }
}
pub(crate) fn secure_input() -> bool {
    secure_fn().is_some_and(|is_on| unsafe { is_on() != 0 })
}
/// Listen for keys aimed at `pid` until [`disarm`]. None when the tap is down
/// or a watch is already listening.
pub(crate) fn subscribe(pid: i32) -> Option<KeyFeed> {
    if pid <= 0 || !TAP_UP.load(Ordering::Acquire) || secure_fn().is_none() {
        return None;
    }
    let (tx, rx) = mpsc::channel(64);
    let mut slot = BUS.tx.lock().unwrap_or_else(|poison| poison.into_inner());
    if BUS.pid.load(Ordering::Acquire) != 0 {
        return None;
    }
    *slot = Some(tx);
    BUS.missed.store(false, Ordering::Release);
    SELF_PID.store(std::process::id() as i32, Ordering::Relaxed);
    BUS.pid.store(pid, Ordering::Release);
    drop(slot);
    Some(KeyFeed { rx })
}
fn disarm() {
    let mut slot = BUS.tx.lock().unwrap_or_else(|poison| poison.into_inner());
    BUS.pid.store(0, Ordering::Release);
    *slot = None;
    BUS.missed.store(false, Ordering::Release);
}
fn enqueue(raw: RawEvent) {
    let Ok(slot) = BUS.tx.try_lock() else {
        if BUS.pid.load(Ordering::Acquire) != 0 {
            BUS.missed.store(true, Ordering::Release);
        }
        return;
    };
    if BUS.pid.load(Ordering::Acquire) == 0 {
        return;
    }
    let Some(tx) = slot.as_ref() else {
        return;
    };
    if tx.try_send(raw).is_err() {
        BUS.missed.store(true, Ordering::Release);
    }
}
/// Copy what the watch needs and return. Never blocks: a contended slot or a
/// full queue marks the watch missed, which discards the model.
fn note_replay(kind: u32, event: Ref) {
    if BUS.pid.load(Ordering::Acquire) == 0 {
        return;
    }
    // Delivery posts its own keys. They are not the user's edit.
    if unsafe { CGEventGetIntegerValueField(event, USER_DATA) } == INJECTED {
        return;
    }
    let source = unsafe { CGEventGetIntegerValueField(event, SOURCE_PID) } as i32;
    if source > 0 && source == SELF_PID.load(Ordering::Relaxed) {
        return;
    }
    let mut raw = RawEvent {
        class: RAW_KEY,
        keycode: 0,
        flags: 0,
        units: [0; 8],
        len: 0,
    };
    if secure_input() {
        raw.class = RAW_SECURE;
        enqueue(raw);
        return;
    }
    let target = unsafe { CGEventGetIntegerValueField(event, TARGET_PID) } as i32;
    let mouse = matches!(kind, LEFT_DOWN | RIGHT_DOWN | OTHER_DOWN);
    if target != BUS.pid.load(Ordering::Acquire) {
        if mouse || kind == KEY_DOWN {
            raw.class = RAW_OTHER;
            enqueue(raw);
        }
        return;
    }
    if mouse {
        raw.class = RAW_MOUSE;
        enqueue(raw);
        return;
    }
    if kind != KEY_DOWN {
        return;
    }
    raw.keycode = unsafe { CGEventGetIntegerValueField(event, 9) } as u16;
    raw.flags = unsafe { CGEventGetFlags(event) };
    let mut len = 0usize;
    unsafe {
        CGEventKeyboardGetUnicodeString(event, 8, &mut len, raw.units.as_mut_ptr());
    }
    if len > 8 {
        raw.class = RAW_ABSTAIN;
    } else {
        raw.len = len as u8;
    }
    enqueue(raw);
}
pub(crate) fn decode(raw: RawEvent) -> ReplayIn {
    match raw.class {
        RAW_MOUSE | RAW_ABSTAIN => return ReplayIn::Key(Key::Abstain),
        RAW_OTHER => return ReplayIn::Leave,
        RAW_SECURE => return ReplayIn::Secure,
        _ => {}
    }
    let text = String::from_utf16(&raw.units[..raw.len as usize]).unwrap_or_default();
    if raw.len > 0 && text.is_empty() {
        return ReplayIn::Key(Key::Abstain);
    }
    match replay::classify(kind_of(raw.keycode), &text, mods_of(raw.flags)) {
        Action::Leave => ReplayIn::Leave,
        Action::Edit(key) => ReplayIn::Key(key),
    }
}
fn mods_of(flags: u64) -> Mods {
    Mods {
        shift: flags & SHIFT != 0,
        ctrl: flags & CTRL != 0,
        alt: flags & ALT != 0,
        cmd: flags & CMD != 0,
    }
}
/// ANSI virtual key codes, the same positions `Binding::parse` uses.
fn kind_of(code: u16) -> Kind {
    match code {
        36 | 76 => Kind::Return,
        48 => Kind::Tab,
        50 => Kind::Grave,
        51 => Kind::Backspace,
        53 => Kind::Escape,
        115 => Kind::Home,
        117 => Kind::Delete,
        119 => Kind::End,
        123 => Kind::Left,
        124 => Kind::Right,
        125 => Kind::Down,
        126 => Kind::Up,
        0 => Kind::Letter('a'),
        1 => Kind::Letter('s'),
        2 => Kind::Letter('d'),
        3 => Kind::Letter('f'),
        4 => Kind::Letter('h'),
        5 => Kind::Letter('g'),
        6 => Kind::Letter('z'),
        7 => Kind::Letter('x'),
        8 => Kind::Letter('c'),
        9 => Kind::Letter('v'),
        11 => Kind::Letter('b'),
        12 => Kind::Letter('q'),
        13 => Kind::Letter('w'),
        14 => Kind::Letter('e'),
        15 => Kind::Letter('r'),
        16 => Kind::Letter('y'),
        17 => Kind::Letter('t'),
        31 => Kind::Letter('o'),
        32 => Kind::Letter('u'),
        34 => Kind::Letter('i'),
        35 => Kind::Letter('p'),
        37 => Kind::Letter('l'),
        38 => Kind::Letter('j'),
        40 => Kind::Letter('k'),
        45 => Kind::Letter('n'),
        46 => Kind::Letter('m'),
        _ => Kind::Other,
    }
}
unsafe extern "C" fn callback(_proxy: Ref, kind: u32, event: Ref, context: Ref) -> Ref {
    let context = &mut *(context as *mut TapContext);
    if kind == u32::MAX || kind == u32::MAX - 1 {
        // Disabled by timeout/user input: re-enable. A key still physically held
        // keeps its hold; only a key-up we never saw resets the gesture.
        CGEventTapEnable(context.tap, true);
        if context.down && !CGEventSourceKeyState(1, context.binding.code) {
            context.down = false;
            context.at_ms = now_ms();
            context.send(KeyEvent::Lost);
        }
        return event;
    }
    // One atomic load when no terminal watch is armed. Mouse downs are not keys.
    if matches!(kind, LEFT_DOWN | RIGHT_DOWN | OTHER_DOWN | KEY_DOWN) {
        note_replay(kind, event);
    }
    if matches!(kind, LEFT_DOWN | RIGHT_DOWN | OTHER_DOWN) {
        return event;
    }
    context.received.store(true, Ordering::Relaxed);
    context.at_ms = match CGEventGetTimestamp(event) {
        0 => now_ms(),
        ns => ns / 1_000_000,
    };
    let code = CGEventGetIntegerValueField(event, 9) as u16;
    if kind == KEY_DOWN && code != context.binding.code {
        if code == 53 {
            context.send(KeyEvent::Escape);
        } else if context.down && CGEventGetIntegerValueField(event, 8) == 0 {
            context.send(KeyEvent::Other);
        }
    }
    let flags = CGEventGetFlags(event);
    let binding = &context.binding;
    let down = if binding.flag != 0 {
        if kind != FLAGS_CHANGED || code != binding.code {
            return event;
        }
        if binding.code == 57 {
            CGEventSourceKeyState(1, 57)
        } else {
            flags & binding.flag != 0
        }
    } else if kind == KEY_DOWN && code == binding.code {
        if CGEventGetIntegerValueField(event, 8) != 0 {
            return event;
        }
        flags & MODIFIERS == binding.modifiers
    } else if kind == KEY_UP && code == binding.code
        || kind == FLAGS_CHANGED && flags & MODIFIERS != binding.modifiers
    {
        false
    } else {
        return event;
    };
    // Presses always reach the machine so it can clear a stuck (lost key-up) state.
    if down || down != context.down {
        context.down = down;
        context.send(if down {
            KeyEvent::Press
        } else {
            KeyEvent::Release
        });
    }
    event
}

/// Frees the callback context once the tap thread is done with it.
struct ContextGuard(*mut TapContext);
impl Drop for ContextGuard {
    fn drop(&mut self) {
        unsafe { drop(Box::from_raw(self.0)) }
    }
}

pub struct MacHotkeys {
    events: mpsc::Receiver<Input>,
    machine: HotkeyMachine,
    pending: VecDeque<HotkeyAction>,
    overflow: Arc<AtomicBool>,
    received: Arc<AtomicBool>,
    tap: usize,
    runloop: usize,
    thread: Option<thread::JoinHandle<()>>,
}
impl MacHotkeys {
    pub fn new(config: HotkeyConfig) -> Result<Self> {
        config.validate()?;
        let (events_tx, events) = mpsc::channel(32);
        let (ready_tx, ready) = std::sync::mpsc::sync_channel(1);
        let overflow = Arc::new(AtomicBool::new(false));
        let received = Arc::new(AtomicBool::new(false));
        let thread_received = received.clone();
        let binding = Binding::parse(&config.key);
        let thread_overflow = overflow.clone();
        // The context is only touched through this raw pointer, on its run-loop thread.
        let thread = thread::Builder::new()
            .name("xflow-hotkey".into())
            .stack_size(256 * 1024)
            .spawn(move || unsafe {
                let context = Box::into_raw(Box::new(TapContext {
                    binding,
                    down: false,
                    tap: ptr::null_mut(),
                    at_ms: 0,
                    events: events_tx,
                    overflow: thread_overflow,
                    received: thread_received,
                }));
                let mask = (1 << FLAGS_CHANGED)
                    | (1 << KEY_DOWN)
                    | (1 << KEY_UP)
                    | (1 << LEFT_DOWN)
                    | (1 << RIGHT_DOWN)
                    | (1 << OTHER_DOWN);
                let tap = CGEventTapCreate(1, 0, 1, mask, callback, context as Ref);
                (*context).tap = tap;
                let _context = ContextGuard(context); // freed after the tap is invalidated
                if tap.is_null() {
                    let _ = ready_tx.send(Err(anyhow::anyhow!(xflow_core::ipc::ACCESSIBILITY_FIX)));
                    return;
                }
                let source = CFMachPortCreateRunLoopSource(ptr::null_mut(), tap, 0);
                if source.is_null() {
                    CFRelease(tap);
                    let _ = ready_tx.send(Err(anyhow::anyhow!(
                        "cannot create the global hotkey run-loop source"
                    )));
                    return;
                }
                let runloop = CFRunLoopGetCurrent();
                CFRunLoopAddSource(runloop, source, kCFRunLoopDefaultMode);
                CGEventTapEnable(tap, true);
                TAP_UP.store(true, Ordering::Release);
                let handles = (CFRetain(tap) as usize, CFRetain(runloop) as usize);
                if ready_tx.send(Ok(handles)).is_ok() {
                    CFRunLoopRun();
                } else {
                    CFRelease(handles.0 as Ref);
                    CFRelease(handles.1 as Ref);
                }
                TAP_UP.store(false, Ordering::Release);
                disarm();
                CFMachPortInvalidate(tap);
                CFRunLoopRemoveSource(runloop, source, kCFRunLoopDefaultMode);
                CFRelease(source);
                CFRelease(tap);
            })
            .context("cannot start the global hotkey thread")?;
        // Bounded: a wedged WindowServer must not hang daemon startup or reload.
        let handles = ready
            .recv_timeout(Duration::from_secs(2))
            .context("global hotkey event tap did not start")?;
        match handles {
            Ok((tap, runloop)) => Ok(Self {
                events,
                machine: HotkeyMachine::new(config),
                pending: VecDeque::new(),
                overflow,
                received,
                tap,
                runloop,
                thread: Some(thread),
            }),
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }
}

#[async_trait]
impl GlobalHotkeys for MacHotkeys {
    fn received_events(&self) -> Option<bool> {
        Some(self.received.load(Ordering::Relaxed))
    }
    async fn next_action(&mut self) -> Result<HotkeyAction> {
        loop {
            if self.overflow.swap(false, Ordering::AcqRel) {
                // Events were dropped: abandon only a gesture in progress, never
                // a session the machine no longer owns (e.g. processing).
                let busy = !self.machine.idle();
                self.reset();
                while self.events.try_recv().is_ok() {}
                if busy {
                    return Ok(HotkeyAction::Cancel);
                }
            }
            if let Some(action) = self.pending.pop_front() {
                return Ok(action);
            }
            // Drain already queued physical events before expiring their gesture.
            let input = match self.events.try_recv() {
                Ok(input) => input,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    bail!("global hotkey event tap stopped; restart xflowd")
                }
                Err(mpsc::error::TryRecvError::Empty) => {
                    let wait = self.machine.deadline_ms().map(|ms| {
                        Duration::from_millis(ms.saturating_add(1).saturating_sub(now_ms()))
                    });
                    tokio::select! { biased;
                        input = self.events.recv() => input.context("global hotkey event tap stopped; restart xflowd")?,
                        _ = async { if let Some(wait) = wait { tokio::time::sleep(wait).await; } }, if wait.is_some() => Input { event: KeyEvent::Timeout, at_ms: now_ms() },
                    }
                }
            };
            self.pending
                .extend(self.machine.event(input.event, input.at_ms));
        }
    }
    fn reset(&mut self) {
        self.machine.reset();
        self.pending.clear();
    }
}
impl Drop for MacHotkeys {
    fn drop(&mut self) {
        TAP_UP.store(false, Ordering::Release);
        disarm();
        unsafe {
            CFMachPortInvalidate(self.tap as Ref);
            CFRunLoopStop(self.runloop as Ref);
            CFRunLoopWakeUp(self.runloop as Ref);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        unsafe {
            CFRelease(self.tap as Ref);
            CFRelease(self.runloop as Ref);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_and_chord_bindings_are_distinct() {
        assert_eq!(Binding::parse("fn").flag, FN);
        assert_eq!(Binding::parse("right_option").flag, 0x40);
        let binding = Binding::parse("ctrl+alt+space");
        assert_eq!(
            (binding.code, binding.flag, binding.modifiers),
            (49, 0, CTRL | ALT)
        );
        for key in xflow_core::config::HOTKEY_KEYS {
            Binding::parse(key);
        }
    }
}
