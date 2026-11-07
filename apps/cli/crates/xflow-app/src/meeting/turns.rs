//! Meeting-only pause boundaries. No language hints and no work outside capture.
use super::journal::RATE;
use xflow_core::AudioClip;

const FRAME: usize = RATE as usize / 50;
const PAUSE_FRAMES: usize = 20; // 400 ms; shorter gaps stay in the same request.

/// First real pause after speech, or the hard cap for continuous speech.
/// Use the existing detector's soft edge floor, preserving quiet word endings.
pub(super) fn cut(samples: &[f32], capped: bool, finished: bool) -> Option<(usize, bool)> {
    let mut voiced = false;
    let mut quiet = 0;
    for (i, frame) in samples.as_chunks::<FRAME>().0.iter().enumerate() {
        let audio = AudioClip {
            samples: frame.to_vec(),
            sample_rate: RATE as u32,
            channels: 1,
        };
        if !crate::speech::activity(&audio, 0.0025).voiced.is_empty() {
            voiced = true;
            quiet = 0;
        } else if voiced {
            quiet += 1;
            if quiet == PAUSE_FRAMES {
                return Some(((i + 1) * FRAME, true));
            }
        }
    }
    (capped || finished).then_some((samples.len(), false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pauses_split_but_short_gaps_soft_endings_and_continuous_speech_survive() {
        let mut samples = vec![0.1; 16_000];
        samples.extend(vec![0.0; 320 * 19]);
        assert_eq!(cut(&samples, false, false), None);
        samples.extend(vec![0.002; 320]); // quiet speech over the existing soft floor
        samples.extend(vec![0.0; 320 * 20]);
        assert_eq!(cut(&samples, false, false), Some((samples.len(), true)));
        samples.extend(vec![0.2; 16_000]);
        assert_eq!(cut(&samples, false, false), Some((28_800, true)));
        assert_eq!(
            cut(&vec![0.1; 480_000], true, false),
            Some((480_000, false))
        );
        assert_eq!(cut(&[0.1; 100], false, true), Some((100, false)));
    }
}
