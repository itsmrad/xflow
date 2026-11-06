//! Timestamp-only gesture logic (Wispr-style). The adapter sleeps only for a pending tap.
use xflow_core::{config::HotkeyConfig, HotkeyAction};

/// Another key pressed this soon after the hotkey means it was used as a modifier.
const CHORD_CANCEL_MS: u64 = 1000;
/// A press while apparently held longer than this means the release was lost.
const STUCK_MS: u64 = 10_000;
/// Capture starts on press; the cue and pill wait this long so a lone tap or a
/// Fn+key shortcut shows nothing.
const REVEAL_MS: u64 = 150;
/// When the ask chord contains the dictation key, wait this long before `Start`
/// so the second modifier can turn the gesture into ask instead.
pub const ASK_GRACE_MS: u64 = 75;

/// The ask chord, already parsed. `overlaps` means it contains the dictation key.
#[derive(Clone, Copy, Debug)]
pub struct AskGesture {
    pub overlaps: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum KeyEvent {
    Press,
    Release,
    /// Some other key went down while the hotkey was held.
    Other,
    Escape,
    Timeout,
    /// The tap was disabled and the held key is physically up: its key-up was missed.
    Lost,
    /// One side of the ask chord, not the dictation key itself. `slot` is 0 or 1.
    Ask {
        slot: u8,
        down: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Idle,
    /// Pressed at, and whether feedback was revealed.
    Held(u64, bool),
    TapPending(u64),
    Latched(u64),
    /// Dictation key is down and the ask chord contains it. `Start` waits `ASK_GRACE_MS`.
    Arming(u64),
    /// Ask chord is held. Release stops; there is no hands-free ask.
    Asking,
}

pub struct HotkeyMachine {
    config: HotkeyConfig,
    ask: Option<AskGesture>,
    phase: Phase,
    ask_down: [bool; 2],
}
impl HotkeyMachine {
    pub fn new(config: HotkeyConfig) -> Self {
        Self::with_ask(config, None)
    }
    pub fn with_ask(config: HotkeyConfig, ask: Option<AskGesture>) -> Self {
        Self {
            config,
            ask,
            phase: Phase::Idle,
            ask_down: [false; 2],
        }
    }
    pub fn reset(&mut self) {
        self.phase = Phase::Idle;
        self.ask_down = [false; 2];
    }
    pub fn idle(&self) -> bool {
        self.phase == Phase::Idle
    }
    /// When the adapter must deliver a `Timeout`: a pending tap or an unrevealed hold.
    pub fn deadline_ms(&self) -> Option<u64> {
        match self.phase {
            Phase::TapPending(deadline) => Some(deadline),
            Phase::Held(t0, false) => Some(t0.saturating_add(REVEAL_MS)),
            Phase::Arming(t0) => Some(t0.saturating_add(ASK_GRACE_MS)),
            _ => None,
        }
    }
    fn overlaps(&self) -> bool {
        self.ask.is_some_and(|ask| ask.overlaps)
    }
    fn dictation_open(&self) -> bool {
        matches!(
            self.phase,
            Phase::Held(_, _) | Phase::TapPending(_) | Phase::Latched(_)
        )
    }
    pub fn event(&mut self, event: KeyEvent, now_ms: u64) -> Vec<HotkeyAction> {
        use HotkeyAction::*;
        if let KeyEvent::Ask { slot, down } = event {
            if let Some(bit) = self.ask_down.get_mut(usize::from(slot)) {
                *bit = down;
            }
        }
        let mut actions = Vec::new();
        if self.deadline_ms().is_some_and(|deadline| now_ms > deadline) {
            match self.phase {
                Phase::Held(t0, _) => {
                    self.phase = Phase::Held(t0, true);
                    actions.push(Reveal);
                }
                Phase::Arming(t0) => {
                    self.phase = Phase::Held(t0, false);
                    actions.push(Start);
                }
                _ => {
                    self.phase = Phase::Idle;
                    actions.push(Cancel);
                }
            }
        }
        let overlaps = self.overlaps();
        let ask_on = self.ask.is_some();
        let both_ask = self.ask_down[0] && self.ask_down[1];
        let (phase, action) = match (self.phase, event) {
            // The other ask modifier is already down: this press completes ask, not dictation.
            (Phase::Idle, KeyEvent::Press) if overlaps && self.ask_down.iter().any(|d| *d) => {
                (Phase::Asking, Some(AskStart))
            }
            (Phase::Idle, KeyEvent::Press) if overlaps => (Phase::Arming(now_ms), None),
            (Phase::Idle, KeyEvent::Press) => (Phase::Held(now_ms, false), Some(Start)),
            (Phase::Arming(_), KeyEvent::Ask { down: true, .. }) if ask_on => {
                (Phase::Asking, Some(AskStart))
            }
            (Phase::Arming(t0), KeyEvent::Timeout) => (Phase::Held(t0, false), Some(Start)),
            (Phase::Arming(_), KeyEvent::Release) => (
                Phase::TapPending(now_ms.saturating_add(self.config.double_tap_ms)),
                Some(Start),
            ),
            (Phase::Arming(_), KeyEvent::Other | KeyEvent::Lost) => (Phase::Idle, None),
            (Phase::Held(t0, _), KeyEvent::Press) if now_ms.saturating_sub(t0) > STUCK_MS => {
                (Phase::Idle, Some(Stop))
            }
            (Phase::Held(t0, _), KeyEvent::Release)
                if now_ms.saturating_sub(t0) >= self.config.hold_threshold_ms =>
            {
                (Phase::Idle, Some(Stop))
            }
            (Phase::Held(..), KeyEvent::Release) => (
                Phase::TapPending(now_ms.saturating_add(self.config.double_tap_ms)),
                None,
            ),
            (Phase::Held(t0, _), KeyEvent::Other)
                if now_ms.saturating_sub(t0) < CHORD_CANCEL_MS =>
            {
                (Phase::Idle, Some(Cancel))
            }
            // Past the grace window the extra modifier is today's cancel, not a flip into ask.
            (Phase::Held(t0, _), KeyEvent::Ask { down: true, .. })
                if overlaps && now_ms.saturating_sub(t0) < CHORD_CANCEL_MS =>
            {
                (Phase::Idle, Some(Cancel))
            }
            (Phase::Held(..), KeyEvent::Lost) => (Phase::Idle, Some(Cancel)),
            (Phase::TapPending(_), KeyEvent::Press) => (Phase::Latched(now_ms), Some(HandsFree)),
            // A third rapid press discards instead of stopping.
            (Phase::Latched(t), KeyEvent::Press)
                if now_ms.saturating_sub(t) <= self.config.double_tap_ms =>
            {
                (Phase::Idle, Some(Cancel))
            }
            (Phase::Latched(_), KeyEvent::Press) => (Phase::Idle, Some(Stop)),
            // Ask is hold-only. A dictation press while it is open does not start dictation.
            (Phase::Asking, KeyEvent::Press) => (Phase::Asking, None),
            (Phase::Asking, KeyEvent::Release) if overlaps => (Phase::Idle, Some(AskStop)),
            (Phase::Asking, KeyEvent::Ask { down: false, .. }) => (Phase::Idle, Some(AskStop)),
            (Phase::Asking, KeyEvent::Lost) => (Phase::Idle, Some(Cancel)),
            // Disjoint chords: both modifiers down, and only while nothing else is open.
            (Phase::Idle, KeyEvent::Ask { down: true, .. })
                if ask_on && !overlaps && both_ask && !self.dictation_open() =>
            {
                (Phase::Asking, Some(AskStart))
            }
            // Idle Esc only hides an answer. A live gesture still cancels, and the tap stays listen-only.
            (Phase::Idle, KeyEvent::Escape) => (Phase::Idle, Some(Dismiss)),
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
            // Hold to talk: feedback after 150 ms; key-repeat and duplicate releases are noise.
            &[
                (Press, 0, &[Start]),
                (Press, 40, &[]),
                (Timeout, 151, &[Reveal]),
                (Release, 500, &[Stop]),
                (Release, 520, &[]),
            ],
            // Lone tap cancels silently when the window expires.
            &[
                (Press, 0, &[Start]),
                (Release, 60, &[]),
                (Timeout, 561, &[Cancel]),
            ],
            // Double tap latches (revealing); a third rapid press cancels.
            &[
                (Press, 0, &[Start]),
                (Release, 60, &[]),
                (Press, 560, &[HandsFree]),
                (Press, 600, &[Cancel]),
            ],
            // Hands-free: repeats ignored; the next press stops.
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
                (Release, 1100, &[Reveal, Stop]),
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
                (Release, 1700, &[Reveal, Stop]),
            ],
            // Fn used as a modifier (e.g. Fn+arrow) right away cancels silently; later it is ignored.
            &[
                (Press, 0, &[Start]),
                (Other, 100, &[Cancel]),
                (Release, 400, &[]),
                (Press, 2000, &[Start]),
                (Timeout, 2151, &[Reveal]),
                (Other, 3500, &[]),
                (Release, 4000, &[Stop]),
            ],
            // A key-up missed while the tap was disabled discards the hold.
            &[
                (Press, 0, &[Start]),
                (Timeout, 151, &[Reveal]),
                (Lost, 300, &[Cancel]),
                (Release, 320, &[]),
            ],
            // A lost release: the next press after 10 s ends the stuck session.
            &[
                (Press, 0, &[Start]),
                (Press, 10_001, &[Reveal, Stop]),
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

    fn ask_machine(overlaps: bool) -> HotkeyMachine {
        HotkeyMachine::with_ask(
            HotkeyConfig {
                hold_threshold_ms: 500,
                double_tap_ms: 500,
                ..HotkeyConfig::default()
            },
            Some(AskGesture { overlaps }),
        )
    }

    fn play(overlaps: bool, trace: &[(KeyEvent, u64, &[HotkeyAction])]) {
        let mut machine = ask_machine(overlaps);
        for &(event, now, expected) in trace {
            assert_eq!(machine.event(event, now), expected, "{trace:?} at {now}");
        }
    }

    #[test]
    fn ask_chords_delay_when_overlapping_and_dictation_stays_immediate_when_disjoint() {
        // Overlap: dictation Start waits 75 ms. Both keys inside that window are only Ask.
        play(
            true,
            &[
                (Press, 0, &[]),
                (
                    Ask {
                        slot: 0,
                        down: true,
                    },
                    40,
                    &[AskStart],
                ),
                (Release, 200, &[AskStop]),
            ],
        );
        // The window expiring emits dictation Start, not Ask. A later modifier cancels.
        play(
            true,
            &[
                (Press, 0, &[]),
                (Timeout, 75, &[Start]),
                (
                    Ask {
                        slot: 0,
                        down: true,
                    },
                    120,
                    &[Cancel],
                ),
            ],
        );
        // Disjoint chords: dictation Start is immediate, on the press.
        // Disjoint: Start is the press itself. Reveal still waits, as it does today.
        play(
            false,
            &[(Press, 0, &[Start]), (Release, 500, &[Reveal, Stop])],
        );
        play(
            false,
            &[
                (
                    Ask {
                        slot: 0,
                        down: true,
                    },
                    0,
                    &[],
                ),
                (
                    Ask {
                        slot: 1,
                        down: true,
                    },
                    10,
                    &[AskStart],
                ),
                (
                    Ask {
                        slot: 0,
                        down: false,
                    },
                    80,
                    &[AskStop],
                ),
            ],
        );
        // Ask does not start while dictation is open, and dictation does not start while ask is.
        play(
            false,
            &[
                (Press, 0, &[Start]),
                (
                    Ask {
                        slot: 0,
                        down: true,
                    },
                    30,
                    &[],
                ),
                (
                    Ask {
                        slot: 1,
                        down: true,
                    },
                    40,
                    &[],
                ),
                (Release, 500, &[Reveal, Stop]),
            ],
        );
        play(
            false,
            &[
                (
                    Ask {
                        slot: 0,
                        down: true,
                    },
                    0,
                    &[],
                ),
                (
                    Ask {
                        slot: 1,
                        down: true,
                    },
                    10,
                    &[AskStart],
                ),
                (Press, 20, &[]),
                (Escape, 30, &[Cancel]),
            ],
        );
    }

    #[test]
    fn escape_while_idle_dismisses_and_dictation_still_starts_immediately() {
        let mut machine = HotkeyMachine::new(HotkeyConfig::default());
        assert_eq!(machine.event(Escape, 0), &[Dismiss]);
        assert_eq!(machine.event(Press, 10), &[Start]);
        assert_eq!(machine.event(Escape, 20), &[Cancel]);
    }
}
