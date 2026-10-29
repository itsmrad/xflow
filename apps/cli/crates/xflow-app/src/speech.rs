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

#[cfg(test)]
mod tests {
    use super::*;

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
