//! Timestamp-only gesture logic (Wispr-style). The adapter sleeps only for a pending tap.
use xflow_core::{config::HotkeyConfig, HotkeyAction};

/// Another key pressed this soon after the hotkey means it was used as a modifier.
const CHORD_CANCEL_MS: u64 = 1000;
/// A press while apparently held longer than this means the release was lost.
const STUCK_MS: u64 = 10_000;

#[derive(Clone, Copy, Debug)]
pub enum KeyEvent {
    Press,
    Release,
    /// Some other key went down while the hotkey was held.
    Other,
    Escape,
    Timeout,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Idle,
    Held(u64),
    TapPending(u64),
    Latched(u64),
}

pub struct HotkeyMachine {
    config: HotkeyConfig,
    phase: Phase,
}
impl HotkeyMachine {
    pub fn new(config: HotkeyConfig) -> Self {
        Self {
            config,
            phase: Phase::Idle,
        }
    }
    pub fn reset(&mut self) {
        self.phase = Phase::Idle;
    }
    pub fn deadline_ms(&self) -> Option<u64> {
        match self.phase {
            Phase::TapPending(deadline) => Some(deadline),
            _ => None,
        }
    }
    pub fn event(&mut self, event: KeyEvent, now_ms: u64) -> Vec<HotkeyAction> {
        use HotkeyAction::*;
        let mut actions = Vec::new();
        if self.deadline_ms().is_some_and(|deadline| now_ms > deadline) {
            self.phase = Phase::Idle;
            actions.push(Cancel);
        }
        let (phase, action) = match (self.phase, event) {
            (Phase::Idle, KeyEvent::Press) => (Phase::Held(now_ms), Some(Start)),
            (Phase::Held(t0), KeyEvent::Press) if now_ms.saturating_sub(t0) > STUCK_MS => {
                (Phase::Idle, Some(Stop))
            }
            (Phase::Held(t0), KeyEvent::Release)
                if now_ms.saturating_sub(t0) >= self.config.hold_threshold_ms =>
            {
                (Phase::Idle, Some(Stop))
            }
            (Phase::Held(_), KeyEvent::Release) => (
                Phase::TapPending(now_ms.saturating_add(self.config.double_tap_ms)),
                None,
            ),
            (Phase::Held(t0), KeyEvent::Other) if now_ms.saturating_sub(t0) < CHORD_CANCEL_MS => {
                (Phase::Idle, Some(Cancel))
            }
            (Phase::TapPending(_), KeyEvent::Press) => (Phase::Latched(now_ms), Some(HandsFree)),
            // A third rapid press discards instead of stopping.
            (Phase::Latched(t), KeyEvent::Press)
                if now_ms.saturating_sub(t) <= self.config.double_tap_ms =>
            {
                (Phase::Idle, Some(Cancel))
            }
            (Phase::Latched(_), KeyEvent::Press) => (Phase::Idle, Some(Stop)),
            (phase, KeyEvent::Escape) if phase != Phase::Idle => (Phase::Idle, Some(Cancel)),
            (phase, _) => (phase, None),
        };
        self.phase = phase;
        actions.extend(action);
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use HotkeyAction::*;
    use KeyEvent::*;

    #[test]
    fn hold_double_tap_cancel_and_repeat_traces() {
        // Defaults: hold_threshold_ms = 500, double_tap_ms = 500 (window starts at release).
        let traces: &[&[(KeyEvent, u64, &[HotkeyAction])]] = &[
            // Hold to talk; key-repeat and duplicate releases are noise.
            &[
                (Press, 0, &[Start]),
                (Press, 40, &[]),
                (Release, 500, &[Stop]),
                (Release, 520, &[]),
            ],
            // Lone tap cancels when the window expires.
            &[
                (Press, 0, &[Start]),
                (Release, 60, &[]),
                (Timeout, 561, &[Cancel]),
            ],
            // Double tap latches; repeats ignored; next press stops.
            &[
                (Press, 0, &[Start]),
                (Release, 60, &[]),
                (Press, 560, &[HandsFree]),
                (Press, 600, &[Cancel]),
            ],
            &[
                (Press, 0, &[Start]),
                (Release, 60, &[]),
                (Press, 200, &[HandsFree]),
                (Release, 260, &[]),
                (Press, 1200, &[Stop]),
                (Release, 1250, &[]),
            ],
            // Late second press: the tap is discarded and a new session starts.
            &[
                (Press, 0, &[Start]),
                (Release, 60, &[]),
                (Press, 561, &[Cancel, Start]),
                (Release, 1100, &[Stop]),
            ],
            // Escape cancels while held or latched.
            &[
                (Press, 0, &[Start]),
                (Escape, 50, &[Cancel]),
                (Release, 60, &[]),
            ],
            &[
                (Press, 0, &[Start]),
                (Release, 60, &[]),
                (Press, 150, &[HandsFree]),
                (Release, 180, &[]),
                (Escape, 900, &[Cancel]),
                (Press, 1000, &[Start]),
                (Release, 1700, &[Stop]),
            ],
            // Fn used as a modifier (e.g. Fn+arrow) soon after start cancels; later it is ignored.
            &[
                (Press, 0, &[Start]),
                (Other, 300, &[Cancel]),
                (Release, 400, &[]),
                (Press, 2000, &[Start]),
                (Other, 3500, &[]),
                (Release, 4000, &[Stop]),
            ],
            // A lost release: the next press after 10 s ends the stuck session.
            &[
                (Press, 0, &[Start]),
                (Press, 10_001, &[Stop]),
                (Release, 10_050, &[]),
            ],
        ];
        let config = HotkeyConfig {
            hold_threshold_ms: 500,
            double_tap_ms: 500,
            ..HotkeyConfig::default()
        };
        for trace in traces {
            let mut machine = HotkeyMachine::new(config.clone());
            for &(event, now, expected) in *trace {
                assert_eq!(machine.event(event, now), expected, "{trace:?} at {now}");
            }
            assert!(machine.deadline_ms().is_none());
        }
    }
}
