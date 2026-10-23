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
    let mut levels: Vec<(f32, usize)> = audio
        .samples
        .chunks(frame_samples)
        .map(|frame| {
            let rms = (frame.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>()
                / frame.len() as f64)
                .sqrt() as f32;
            (rms, frame.len())
        })
        .collect();
    if levels.is_empty() || levels.iter().any(|(rms, _)| !rms.is_finite()) {
        return Activity {
            speech: false,
            low_energy: true,
        };
    }
    levels.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    // #51's quiet-room median was 0.00064–0.00154, with peaks near 0.03.
    // Adapt to steady noise; cap its estimate so sustained loud speech still passes.
    // shortcut: energy cannot identify speech in changing noise; use a speech classifier if hardware trials require it.
    let floor = levels[levels.len() / 5].0.min(0.03);
    let cutoff = threshold.max(floor * 2.5);
    let voiced_samples: usize = levels
        .iter()
        .filter(|(rms, _)| *rms > cutoff)
        .map(|(_, len)| len)
        .sum();
    let voiced_ms =
        voiced_samples as u64 * 1000 / u64::from(audio.sample_rate) / u64::from(audio.channels);
    Activity {
        speech: voiced_ms >= 200,
        low_energy: voiced_ms < 300
            || voiced_samples < audio.samples.len() / 10
            || levels[levels.len() - 1].0 < cutoff * 2.0,
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
