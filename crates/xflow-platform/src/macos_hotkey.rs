//! A passive Quartz event tap: no key suppression, AppKit, or idle timer.
use crate::hotkey::{HotkeyMachine, KeyEvent};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::{
    collections::VecDeque,
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
use xflow_core::{config::HotkeyConfig, GlobalHotkeys, HotkeyAction};

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
}
impl TapContext {
    fn send(&self, event: KeyEvent) {
        let at_ms = self.at_ms;
        if self.events.try_send(Input { event, at_ms }).is_err() {
            self.overflow.store(true, Ordering::Release);
        }
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
        let binding = Binding::parse(&config.key);
        let thread_overflow = overflow.clone();
        // The context is only touched through this raw pointer, on its run-loop thread.
        let thread = thread::Builder::new().name("xflow-hotkey".into()).stack_size(256 * 1024).spawn(move || unsafe {
            let context = Box::into_raw(Box::new(TapContext { binding, down: false, tap: ptr::null_mut(), at_ms: 0,
                events: events_tx, overflow: thread_overflow }));
            let mask = (1 << FLAGS_CHANGED) | (1 << KEY_DOWN) | (1 << KEY_UP);
            let tap = CGEventTapCreate(1, 0, 1, mask, callback, context as Ref);
            (*context).tap = tap;
            let _context = ContextGuard(context); // freed after the tap is invalidated
            if tap.is_null() {
                let _ = ready_tx.send(Err(anyhow::anyhow!("Global hotkey unavailable: allow xflowd (or its terminal) in System Settings → Privacy & Security → Input Monitoring, then restart xflowd or run xflow config set hotkey.enabled true")));
                return;
            }
            let source = CFMachPortCreateRunLoopSource(ptr::null_mut(), tap, 0);
            if source.is_null() {
                CFRelease(tap);
                let _ = ready_tx.send(Err(anyhow::anyhow!("cannot create the global hotkey run-loop source")));
                return;
            }
            let runloop = CFRunLoopGetCurrent();
            CFRunLoopAddSource(runloop, source, kCFRunLoopDefaultMode);
            CGEventTapEnable(tap, true);
            let handles = (CFRetain(tap) as usize, CFRetain(runloop) as usize);
            if ready_tx.send(Ok(handles)).is_ok() { CFRunLoopRun(); }
            else { CFRelease(handles.0 as Ref); CFRelease(handles.1 as Ref); }
            CFMachPortInvalidate(tap);
                CFRunLoopRemoveSource(runloop, source, kCFRunLoopDefaultMode);
            CFRelease(source);
            CFRelease(tap);
        }).context("cannot start the global hotkey thread")?;
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
