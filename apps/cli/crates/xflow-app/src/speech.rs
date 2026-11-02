use std::ops::Range;
use xflow_core::{config::Config, AudioClip};

pub const NOT_CAUGHT: &str = "Didn't catch anything";

pub struct Activity {
    pub speech: bool,
    pub low_energy: bool,
    /// Samples from the first frame over half the threshold to the end of the last;
    /// empty without one. The lower bar keeps soft first words and trailing endings.
    pub voiced: Range<usize>,
}

/// Analyze the captured buffer once, outside the audio callback; keep all pauses.
pub fn activity(audio: &AudioClip, threshold: f32) -> Activity {
    if audio.sample_rate == 0 || audio.channels == 0 {
        return Activity {
            speech: false,
            low_energy: true,
            voiced: 0..0,
        };
    }
    let frame_samples = (audio.sample_rate as usize / 50).max(1) * usize::from(audio.channels);
    let (mut voiced_samples, mut peak, mut voiced) = (0, 0f32, None::<Range<usize>>);
    for (i, frame) in audio.samples.chunks(frame_samples).enumerate() {
        let rms = (frame.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / frame.len() as f64)
            .sqrt() as f32;
        if rms > threshold {
            voiced_samples += frame.len();
        }
        if rms > threshold / 2.0 {
            let start = i * frame_samples;
            voiced = Some(voiced.map_or(start, |v| v.start)..start + frame.len());
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
        voiced: voiced.unwrap_or(0..0),
    }
}

/// Silence kept beside the voiced frames for onsets ("f", "h") and decays that are
/// quieter still; faster-whisper's default speech pad.
pub const TRIM_PAD_MS: usize = 400;

/// The samples to upload: edge silence beyond `TRIM_PAD_MS` cut where `lead` /
/// `tail` allow, never shorter than `min_ms`. Pauses between voiced frames stay.
pub fn trim(
    audio: &AudioClip,
    voiced: &Range<usize>,
    lead: bool,
    tail: bool,
    min_ms: u32,
) -> Range<usize> {
    let len = audio.samples.len();
    if voiced.is_empty() {
        return 0..len;
    }
    let samples = |ms: usize| audio.sample_rate as usize * ms / 1000 * usize::from(audio.channels);
    let mut start = if lead {
        voiced.start.saturating_sub(samples(TRIM_PAD_MS))
    } else {
        0
    };
    let mut end = if tail {
        (voiced.end + samples(TRIM_PAD_MS)).min(len)
    } else {
        len
    };
    let floor = samples(min_ms as usize);
    end = (start + floor).clamp(end, len);
    start = start.min(end.saturating_sub(floor));
    start..end
}

/// `domain`: the shipped terms offered with the upload, which Whisper can echo
/// too. They count only whole ("Claude Code", "Next.js"): a near-silent "Next."
/// or "Code" stays unless the user's own hints contain that word.
pub fn suspect(text: &str, config: &Config, domain: &[&str]) -> bool {
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
        .map(String::as_str)
        .chain(config.dictionary.words.iter().map(String::as_str))
        .chain(xflow_providers::language_lead(&config.stt))
        .flat_map(words)
        .collect();
    let terms: Vec<Vec<String>> = domain.iter().map(|term| words(term)).collect();
    // echo[i]: the first i words are all user hint words or whole shipped terms.
    let mut echo = vec![false; text.len() + 1];
    echo[0] = true;
    for i in 0..text.len() {
        if echo[i] {
            echo[i + 1] |= hint_words.contains(&text[i]);
            for term in terms.iter().filter(|t| text[i..].starts_with(t)) {
                echo[i + term.len()] = true;
            }
        }
    }
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
        || echo[text.len()]
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

    /// Sine at 220 Hz from (milliseconds, amplitude) parts.
    fn tone(rate: u32, channels: u16, parts: &[(usize, f32)]) -> AudioClip {
        let samples = parts
            .iter()
            .flat_map(|&(ms, a)| std::iter::repeat_n(a, rate as usize * ms / 1000))
            .enumerate()
            .flat_map(|(i, a)| {
                let s = a * (i as f32 * std::f32::consts::TAU * 220.0 / rate as f32).sin();
                std::iter::repeat_n(s, usize::from(channels))
            })
            .collect();
        AudioClip {
            samples,
            sample_rate: rate,
            channels,
        }
    }
    /// Milliseconds of `audio` before `range` and inside it.
    fn kept_ms(audio: &AudioClip, range: &Range<usize>) -> (usize, usize) {
        let ms = |n: usize| n * 1000 / audio.sample_rate as usize / usize::from(audio.channels);
        (ms(range.start), ms(range.len()))
    }

    #[test]
    fn silence_has_no_speech_and_nothing_to_trim() {
        let audio = tone(16_000, 1, &[(2000, 0.0)]);
        let found = activity(&audio, 0.005);
        assert!(!found.speech && found.voiced.is_empty());
        assert_eq!(trim(&audio, &found.voiced, true, true, 300), 0..32_000);
    }

    #[test]
    fn edge_silence_is_trimmed_to_the_pad_in_whole_frames() {
        // #68: 10 s of speech with 3 s of silence each side uploads 10.8 s (the issue's
        // 10.6 s assumed a 250 ms pad; review raised it to 400 ms for soft speech).
        for (rate, channels) in [(16_000, 1), (48_000, 1), (44_100, 2)] {
            let audio = tone(rate, channels, &[(3000, 0.0), (10_000, 0.2), (3000, 0.0)]);
            let found = activity(&audio, 0.005);
            let kept = trim(&audio, &found.voiced, true, true, 300);
            assert_eq!(kept.start % usize::from(channels), 0);
            assert_eq!(kept.end % usize::from(channels), 0);
            let (start, len) = kept_ms(&audio, &kept);
            assert!((2590..=2600).contains(&start), "{rate}/{channels}: {start}");
            assert!((10_780..=10_820).contains(&len), "{rate}/{channels}: {len}");
        }
    }

    #[test]
    fn quiet_onsets_and_decays_under_the_threshold_are_kept() {
        // A soft "f" before the vowel and a fading tail, both below the threshold.
        let audio = tone(
            16_000,
            1,
            &[
                (1000, 0.0),
                (180, 0.006),
                (400, 0.2),
                (180, 0.006),
                (1000, 0.0),
            ],
        );
        let found = activity(&audio, 0.005);
        assert_eq!(
            found.voiced,
            16_000..28_160,
            "edges count from half the threshold"
        );
        let kept = trim(&audio, &found.voiced, true, true, 0);
        assert_eq!(kept, 9_600..34_560);
    }

    #[test]
    fn soft_first_words_and_endings_are_never_clipped() {
        // Review of #116: sines at -50 / -48 dBFS RMS, under the 0.005 (-46 dBFS)
        // threshold, beside a loud word; each was clipped at the first 250 ms pad.
        let amplitude = |dbfs: f32| 10f32.powf(dbfs / 20.0) * std::f32::consts::SQRT_2;
        for (dbfs, ms, lead) in [
            (-50.0, 300, false),
            (-50.0, 500, false),
            (-50.0, 800, false),
            (-50.0, 600, true),
            (-48.0, 400, true),
        ] {
            let soft = (ms, amplitude(dbfs));
            let parts = if lead {
                [(2000, 0.0), soft, (500, 0.2), (2000, 0.0)]
            } else {
                [(2000, 0.0), (500, 0.2), soft, (2000, 0.0)]
            };
            let audio = tone(16_000, 1, &parts);
            let found = activity(&audio, 0.005);
            let kept = trim(&audio, &found.voiced, true, true, 0);
            let speech = 32_000..(32_000 + (500 + ms) * 16);
            assert!(
                kept.start <= speech.start && kept.end >= speech.end,
                "{dbfs} dBFS {ms} ms lead={lead}: {kept:?}"
            );
        }
        // Even quieter (-56 dBFS) endings are not detected, but keep the whole pad.
        let audio = tone(
            16_000,
            1,
            &[(500, 0.2), (600, amplitude(-56.0)), (2000, 0.0)],
        );
        let found = activity(&audio, 0.005);
        assert_eq!(trim(&audio, &found.voiced, true, true, 0), 0..14_400);
        // Only the edges use the lower bar: soft sound alone is still not speech.
        let murmur = tone(16_000, 1, &[(2000, amplitude(-50.0))]);
        let found = activity(&murmur, 0.005);
        assert!(!found.speech && !found.voiced.is_empty());
    }

    #[test]
    fn pauses_and_segment_boundaries_are_never_trimmed() {
        let audio = tone(
            16_000,
            1,
            &[
                (1000, 0.0),
                (500, 0.2),
                (3000, 0.0),
                (500, 0.2),
                (1000, 0.0),
            ],
        );
        let voiced = activity(&audio, 0.005).voiced;
        // The 3 s pause between words stays.
        assert_eq!(trim(&audio, &voiced, true, true, 0), 9_600..86_400);
        // A segment cut mid-dictation keeps its edge where the other segment continues.
        assert_eq!(trim(&audio, &voiced, false, true, 0), 0..86_400);
        assert_eq!(trim(&audio, &voiced, true, false, 0), 9_600..96_000);
        assert_eq!(trim(&audio, &voiced, false, false, 0), 0..96_000);
    }

    #[test]
    fn trimming_never_goes_below_min_ms() {
        // 100 ms word: padded to 900 ms, or grown to min_ms (end first, then start).
        let audio = tone(16_000, 1, &[(2000, 0.0), (100, 0.2), (2000, 0.0)]);
        let voiced = activity(&audio, 0.005).voiced;
        assert_eq!(trim(&audio, &voiced, true, true, 300), 25_600..40_000);
        assert_eq!(trim(&audio, &voiced, true, true, 1000), 25_600..41_600);
        let near_end = tone(16_000, 1, &[(2000, 0.0), (100, 0.2), (100, 0.0)]);
        let voiced = activity(&near_end, 0.005).voiced;
        assert_eq!(trim(&near_end, &voiced, true, true, 1000), 19_200..35_200);
        assert_eq!(trim(&near_end, &voiced, true, true, 5000), 0..35_200);
    }

    #[test]
    fn trimming_a_long_clip_costs_no_extra_pass() {
        // 60 s of 48 kHz stereo, the largest segment. Trim reads only the range the
        // existing activity pass found; the bound is generous so slow CI never flakes.
        let audio = tone(48_000, 2, &[(3000, 0.0), (54_000, 0.2), (3000, 0.0)]);
        let started = std::time::Instant::now();
        let found = activity(&audio, 0.005);
        let kept = trim(&audio, &found.voiced, true, true, 300);
        let elapsed = started.elapsed();
        assert!(elapsed < std::time::Duration::from_secs(10), "{elapsed:?}");
        assert_eq!(kept_ms(&audio, &kept), (2600, 54_800));
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
            assert!(
                suspect(case["text"].as_str().unwrap(), &config, &[]),
                "{case}"
            );
        }
        assert!(suspect("OpenResty, ZenML.", &config, &[]));
        assert!(!suspect("OpenResty is ready", &config, &[]));
        // Shipped domain hints are echoes too.
        let domain = xflow_core::hints::select(
            &Config::default(),
            &xflow_core::hints::Signals {
                terminal: true,
                ..Default::default()
            },
        );
        assert!(!suspect("Supabase, Vercel.", &config, &[]));
        assert!(suspect("Supabase, Vercel.", &config, &domain));
        assert!(!suspect("Supabase is ready", &config, &domain));
        assert!(suspect("Claude Code, Next.js, Supabase.", &config, &domain));
        // The Hindi + English prompt lead is echoed on silence too (#40).
        let echo = "हाँ, मैं अभी check करके बताता हूँ।";
        assert!(!suspect(echo, &config, &[]));
        config.stt.languages = vec!["hi".into(), "en".into()];
        assert!(suspect(echo, &config, &[]));
        assert!(suspect("मैं अभी check करके", &config, &[]));
        assert!(suspect("अभी अभी अभी अभी अभी बताता हूँ।.", &config, &[]));
        assert!(!suspect("कल API deploy करना है", &config, &[]));
        // Everyday words inside shipped terms are not echoes on their own.
        for heard in ["Next.", "Code", "Monday", "Teams", "Power", "Microsoft"] {
            assert!(!suspect(heard, &config, &domain), "{heard}");
        }
    }
}
