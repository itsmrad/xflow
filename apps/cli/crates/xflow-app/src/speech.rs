use xflow_core::{config::Config, AudioClip};

pub const NOT_CAUGHT: &str = "Didn't catch anything";

pub struct Activity {
    pub speech: bool,
    pub low_energy: bool,
}

/// Analyze the captured buffer once, outside the audio callback; keep all pauses.
pub fn activity(audio: &AudioClip, threshold: f32) -> Activity {
    if audio.sample_rate == 0 || audio.channels == 0 {
        return Activity {
            speech: false,
            low_energy: true,
        };
    }
    let frame_samples = (audio.sample_rate as usize / 50).max(1) * usize::from(audio.channels);
    let (mut voiced_samples, mut peak) = (0, 0f32);
    for frame in audio.samples.chunks(frame_samples) {
        let rms = (frame.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / frame.len() as f64)
            .sqrt() as f32;
        if rms > threshold {
            voiced_samples += frame.len();
        }
        peak = peak.max(rms);
    }
    let voiced_ms =
        voiced_samples as u64 * 1000 / u64::from(audio.sample_rate) / u64::from(audio.channels);
    // Losing a short "yes" is worse than sending a click to STT: 80 ms passes it.
    // #51's silent sessions peaked at 0.025–0.030 frame RMS (key thumps), under 8x the default.
    // shortcut: energy cannot identify speech in changing noise; use a speech classifier if hardware trials require it.
    Activity {
        speech: voiced_ms >= 80,
        low_energy: peak < threshold * 8.0,
    }
}

pub fn suspect(text: &str, config: &Config) -> bool {
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let text = words(text);
    let hint_words: Vec<_> = config
        .stt
        .vocabulary
        .iter()
        .chain(&config.dictionary.words)
        .flat_map(|hint| words(hint))
        .collect();
    text.is_empty()
        || text.len() == 1 && text[0].chars().count() <= 2
        || matches!(
            text.join(" ").as_str(),
            "thank you"
                | "thanks for watching"
                | "hi there"
                | "fox jumps over the lazy dog"
                | "the quick brown fox jumps over the lazy dog"
        )
        || text.iter().all(|word| hint_words.contains(word))
}

/// Long dictations are transcribed in segments while recording continues. A
/// segment ends at the quietest moment between these bounds, so words are never
/// split, providers get enough context, and only the last segment (< 60 s) is
/// left to transcribe after stop.
pub const SEGMENT_MIN_SECS: usize = 30;
pub const SEGMENT_MAX_SECS: usize = 60;

/// Sample offset (whole frames) where the next segment of a growing recording
/// ends: the centre of the quietest 250 ms between 30 s and 60 s. `None` until
/// 60 s are buffered.
pub fn split_point(audio: &AudioClip) -> Option<usize> {
    let (rate, channels) = (audio.sample_rate as usize, usize::from(audio.channels));
    if rate == 0 || channels == 0 || audio.samples.len() / channels < rate * SEGMENT_MAX_SECS {
        return None;
    }
    let hop = (rate / 100).max(1); // 10 ms frames
    const WINDOW: usize = 25;
    let first = rate * SEGMENT_MIN_SECS;
    let energy: Vec<f64> = audio.samples[first * channels..rate * SEGMENT_MAX_SECS * channels]
        .chunks(hop * channels)
        .map(|frame| frame.iter().map(|s| f64::from(*s).powi(2)).sum())
        .collect();
    let mut sum: f64 = energy[..WINDOW].iter().sum();
    let (mut quietest, mut start) = (sum, 0);
    for i in WINDOW..energy.len() {
        sum += energy[i] - energy[i - WINDOW];
        if sum < quietest {
            (quietest, start) = (sum, i + 1 - WINDOW);
        }
    }
    Some((first + (start + WINDOW / 2) * hop) * channels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speech-like bursts with a 400 ms pause at each second in `pauses`.
    fn speech(rate: u32, channels: u16, seconds: usize, pauses: &[usize]) -> AudioClip {
        let rate_ = rate as usize;
        let samples = (0..rate_ * seconds)
            .flat_map(|i| {
                let paused = pauses
                    .iter()
                    .any(|p| (p * rate_..p * rate_ + rate_ * 2 / 5).contains(&i));
                let s = if paused {
                    0.001
                } else {
                    0.3 * (i as f32 * 0.07).sin()
                };
                std::iter::repeat_n(s, usize::from(channels))
            })
            .collect();
        AudioClip {
            samples,
            sample_rate: rate,
            channels,
        }
    }

    #[test]
    fn waits_for_a_full_segment() {
        assert_eq!(split_point(&speech(16_000, 1, 59, &[40])), None);
        assert!(split_point(&speech(16_000, 1, 60, &[40])).is_some());
        let empty = AudioClip {
            samples: vec![],
            sample_rate: 0,
            channels: 1,
        };
        assert_eq!(split_point(&empty), None);
    }

    #[test]
    fn cuts_inside_the_pause_not_inside_a_word() {
        for (rate, channels) in [(16_000, 1), (48_000, 1), (44_100, 2)] {
            let audio = speech(rate, channels, 75, &[12, 47, 70]);
            let cut = split_point(&audio).unwrap();
            assert_eq!(cut % usize::from(channels), 0, "whole frames only");
            let second = cut as f64 / f64::from(rate) / f64::from(channels);
            // The 47.0–47.4 s pause; outside [30, 60] pauses are ignored.
            assert!(
                (47.0..47.4).contains(&second),
                "{rate}/{channels}: {second}"
            );
        }
    }

    #[test]
    fn continuous_speech_still_cuts_within_bounds() {
        let audio = speech(16_000, 1, 90, &[]);
        let cut = split_point(&audio).unwrap() / 16_000;
        assert!((SEGMENT_MIN_SECS..SEGMENT_MAX_SECS).contains(&cut));
    }

    #[test]
    fn repeated_splits_keep_every_sample_in_order() {
        // 12 minutes at 8 kHz with a pause every 7 s: segments tile the recording.
        let pauses: Vec<usize> = (3..720).step_by(7).collect();
        let mut tail = speech(8_000, 1, 720, &pauses);
        let original = tail.samples.clone();
        let mut rebuilt = Vec::new();
        let mut segments = 0;
        while let Some(cut) = split_point(&tail) {
            let rest = tail.samples.split_off(cut);
            let head = std::mem::replace(&mut tail.samples, rest);
            assert!((SEGMENT_MIN_SECS * 8_000..SEGMENT_MAX_SECS * 8_000).contains(&head.len()));
            rebuilt.extend(head);
            segments += 1;
        }
        assert!(tail.samples.len() < SEGMENT_MAX_SECS * 8_000);
        rebuilt.extend(&tail.samples);
        assert_eq!(rebuilt, original);
        assert!((12..=24).contains(&segments), "{segments} segments");
    }

    // #63: the larger hint prompt must not let a key thump through as dictionary words.
    // Groq outputs for #51's thump clip, both models, every prompt variant.
    #[test]
    fn larger_prompt_thump_outputs_stay_suspect() {
        let live: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../benchmarks/devterms/silence.json"
        ))
        .unwrap();
        let mut config = Config::default();
        config.dictionary.words = serde_json::from_value(live["dictionary"].clone()).unwrap();
        let thumps: Vec<_> = live["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| case["voice"] == "thump")
            .collect();
        assert_eq!(thumps.len(), 6);
        for case in thumps {
            assert!(suspect(case["text"].as_str().unwrap(), &config), "{case}");
        }
        assert!(suspect("OpenResty, ZenML.", &config));
        assert!(!suspect("OpenResty is ready", &config));
    }
}
