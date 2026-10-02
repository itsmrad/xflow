use anyhow::{anyhow, bail, Result};
use xflow_core::{AudioClip, MAX_UPLOAD_FRAMES};

pub(crate) const RATE: u32 = 16_000;
/// Downmix, low-pass and resample. Polyphase windowed sinc avoids per-sample trig.
/// ponytail: a fixed 96-tap Blackman filter targets dictation (passband <7 kHz);
/// increase taps or use a DSP library if music-quality conversion is required.
pub(crate) fn mono16(audio: &AudioClip) -> Result<Vec<f32>> {
    let channels = usize::from(audio.channels);
    if channels == 0
        || channels > 32
        || !(8000..=384000).contains(&audio.sample_rate)
        || audio.samples.is_empty()
        || !audio.samples.len().is_multiple_of(channels)
    {
        bail!("invalid or empty PCM audio");
    }
    let frames = audio.samples.len() / channels;
    let count = (frames as u64 * u64::from(RATE)).div_ceil(u64::from(audio.sample_rate)) as usize;
    if count > MAX_UPLOAD_FRAMES || audio.samples.len() > 32_000_000 {
        bail!("audio exceeds provider upload limit");
    }
    if audio.samples.iter().any(|s| !s.is_finite()) {
        bail!("audio contains non-finite samples");
    }
    let mono: Vec<f32> = audio
        .samples
        .chunks_exact(channels)
        .map(|f| f.iter().map(|s| s.clamp(-1.0, 1.0)).sum::<f32>() / channels as f32)
        .collect();
    if audio.sample_rate == RATE {
        return Ok(mono);
    }
    fn gcd(mut a: u32, mut b: u32) -> u32 {
        while b != 0 {
            let r = a % b;
            a = b;
            b = r;
        }
        a
    }
    let divisor = gcd(RATE, audio.sample_rate);
    let phases = RATE / divisor;
    let step = audio.sample_rate / divisor;
    let cutoff = 0.45 * f64::from(RATE.min(audio.sample_rate)) / f64::from(audio.sample_rate);
    const HALF: i64 = 48;
    let filters: Vec<Vec<f32>> = (0..phases)
        .map(|p| {
            let frac = f64::from(p) / f64::from(phases);
            (-HALF + 1..=HALF)
                .map(|tap| {
                    let x = tap as f64 - frac;
                    let sinc = if x.abs() < 1e-10 {
                        2.0 * cutoff
                    } else {
                        (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
                    };
                    let window = 0.42
                        + 0.5 * (std::f64::consts::PI * x / HALF as f64).cos()
                        + 0.08 * (2.0 * std::f64::consts::PI * x / HALF as f64).cos();
                    (sinc * window) as f32
                })
                .collect()
        })
        .collect();
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let position = i as u64 * u64::from(step);
        let center = (position / u64::from(phases)) as i64;
        let filter = &filters[(position % u64::from(phases)) as usize];
        let mut sum = 0.0;
        let mut weight = 0.0;
        for (j, &coefficient) in filter.iter().enumerate() {
            let source = center + j as i64 - HALF + 1;
            if let Some(&sample) = usize::try_from(source).ok().and_then(|n| mono.get(n)) {
                sum += sample * coefficient;
                weight += coefficient;
            }
        }
        out.push((sum / weight).clamp(-1.0, 1.0));
    }
    Ok(out)
}
/// Encode 16 kHz mono PCM16 WAV after proper anti-alias filtering.
pub fn encode_wav(audio: &AudioClip) -> Result<Vec<u8>> {
    wav(&mono16(audio)?)
}
/// Compression is lossless after PCM16 conversion. Tiny or incompressible clips keep WAV.
pub(crate) fn encode_upload(audio: &AudioClip, flac: bool) -> Result<(Vec<u8>, &'static str)> {
    let samples = mono16(audio)?;
    let wav = wav(&samples)?;
    if !flac || wav.len() < 4096 {
        return Ok((wav, "wav"));
    }
    use flacenc::{component::BitRepr, error::Verify};
    let pcm: Vec<i32> = samples.iter().map(|&s| pcm16(s) as i32).collect();
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|_| anyhow!("invalid FLAC encoder configuration"))?;
    let source = flacenc::source::MemSource::from_samples(&pcm, 1, 16, RATE as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|_| anyhow!("FLAC audio encoding failed"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|_| anyhow!("FLAC serialization failed"))?;
    if sink.as_slice().len() < wav.len() * 9 / 10 {
        Ok((sink.as_slice().to_vec(), "flac"))
    } else {
        Ok((wav, "wav"))
    }
}
fn pcm16(sample: f32) -> i16 {
    (sample * if sample < 0.0 { 32768.0 } else { 32767.0 }).round() as i16
}
fn wav(samples: &[f32]) -> Result<Vec<u8>> {
    if samples.len() > MAX_UPLOAD_FRAMES {
        bail!("audio exceeds provider upload limit");
    }
    let size = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(size as usize + 44);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(size + 36).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&size.to_le_bytes());
    for &sample in samples {
        let pcm = pcm16(sample);
        bytes.extend_from_slice(&pcm.to_le_bytes());
    }
    Ok(bytes)
}
pub(crate) fn decode_wav(bytes: &[u8]) -> Result<AudioClip> {
    let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes))
        .map_err(|_| anyhow!("invalid WAV file"))?;
    let spec = reader.spec();
    if reader.len() as usize > 32_000_000 {
        bail!("WAV exceeds PCM size limit");
    }
    let samples = match spec.sample_format {
        hound::SampleFormat::Float if spec.bits_per_sample == 32 => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>(
        ),
        hound::SampleFormat::Int if (1..=32).contains(&spec.bits_per_sample) => {
            let scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|r| r.map(|s| s as f32 / scale))
                .collect()
        }
        _ => bail!("unsupported WAV sample format"),
    }
    .map_err(|_| anyhow!("invalid WAV samples"))?;
    Ok(AudioClip {
        samples,
        sample_rate: spec.sample_rate,
        channels: spec.channels,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tone(rate: u32, hz: f32) -> AudioClip {
        AudioClip {
            sample_rate: rate,
            channels: 2,
            samples: (0..rate)
                .flat_map(|i| {
                    let s = 0.5 * (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin();
                    [s, s]
                })
                .collect(),
        }
    }
    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }
    #[test]
    fn length_amplitude_and_alias_rejection() {
        for rate in [44100, 48000, 96000] {
            let low = mono16(&tone(rate, 1000.0)).unwrap();
            let high = mono16(&tone(rate, 12000.0)).unwrap();
            assert_eq!(low.len(), 16000);
            assert!((rms(&low[100..15900]) - 0.35355).abs() < 0.005);
            assert!(rms(&high[100..15900]) < 0.003, "alias at {rate}");
            let dc = AudioClip {
                samples: vec![0.25; rate as usize * 2],
                sample_rate: rate,
                channels: 2,
            };
            assert!(mono16(&dc)
                .unwrap()
                .iter()
                .all(|s| (s - 0.25).abs() < 0.00001));
        }
    }
    #[test]
    fn flac_is_smaller_and_pcm_lossless() {
        let clip = tone(48000, 1000.0);
        let (bytes, format) = encode_upload(&clip, true).unwrap();
        assert_eq!(format, "flac");
        assert!(bytes.len() < 32044 * 9 / 10);
        let mut reader = claxon::FlacReader::new(std::io::Cursor::new(&bytes)).unwrap();
        let decoded: Vec<i32> = reader
            .samples()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        let expected: Vec<i32> = mono16(&clip)
            .unwrap()
            .iter()
            .map(|&s| pcm16(s) as i32)
            .collect();
        assert_eq!(decoded, expected);
        assert_eq!(encode_upload(&clip, false).unwrap().1, "wav");
    }
    #[test]
    fn wav_roundtrip_and_short_upsampling() {
        let clip = tone(48000, 1000.0);
        let bytes = encode_wav(&clip).unwrap();
        assert_eq!(bytes.len(), 32044);
        let decoded = decode_wav(&bytes).unwrap();
        assert_eq!(decoded.sample_rate, 16000);
        assert_eq!(decoded.samples.len(), 16000);
        assert_eq!(
            mono16(&AudioClip {
                samples: vec![0.5],
                sample_rate: 8000,
                channels: 1
            })
            .unwrap()
            .len(),
            2
        );
        assert!(decode_wav(b"broken").is_err());
    }
}
