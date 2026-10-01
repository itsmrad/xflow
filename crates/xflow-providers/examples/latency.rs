//! Isolated stop-to-text comparison: 40 ms connection setup + 1 MB/s upload.
//! No provider service, keys, microphone, clipboard or daemon are used.
use anyhow::{Context, Result};
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::{Duration, Instant},
};
use xflow_core::{config::SttConfig, AudioClip};

fn original_rate_wav(audio: &AudioClip) -> Vec<u8> {
    let mut wav = xflow_providers::encode_wav(&AudioClip {
        samples: vec![0.0],
        sample_rate: 16000,
        channels: 1,
    })
    .unwrap();
    wav.truncate(44);
    let bytes = (audio.samples.len() / usize::from(audio.channels) * 2) as u32;
    wav[4..8].copy_from_slice(&(bytes + 36).to_le_bytes());
    wav[24..28].copy_from_slice(&audio.sample_rate.to_le_bytes());
    wav[28..32].copy_from_slice(&(audio.sample_rate * 2).to_le_bytes());
    wav[40..44].copy_from_slice(&bytes.to_le_bytes());
    for frame in audio.samples.chunks_exact(usize::from(audio.channels)) {
        let s = frame.iter().map(|s| s.clamp(-1.0, 1.0)).sum::<f32>() / f32::from(audio.channels);
        let pcm = (s * if s < 0.0 { 32768.0 } else { 32767.0 }).round() as i16;
        wav.extend_from_slice(&pcm.to_le_bytes());
    }
    wav
}
fn mock() -> Result<(String, std::thread::JoinHandle<Result<Vec<usize>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("http://{}/v1/audio/transcriptions", listener.local_addr()?);
    let worker = std::thread::spawn(move || {
        let mut sizes = Vec::new();
        for connection in 0..2 {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(20)))?;
            std::thread::sleep(Duration::from_millis(40));
            for request in 0..if connection == 0 { 1 } else { 2 } {
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut b = [0];
                    stream.read_exact(&mut b)?;
                    header.push(b[0]);
                    anyhow::ensure!(header.len() < 8192, "oversized mock request headers");
                }
                let text = String::from_utf8_lossy(&header).to_ascii_lowercase();
                let len = text
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap_or("0")
                    .parse::<usize>()?;
                let mut left = len;
                while left > 0 {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer[..left.min(4096)])?;
                    anyhow::ensure!(n > 0, "truncated mock request");
                    left -= n;
                    std::thread::sleep(Duration::from_secs_f64(n as f64 / 1_000_000.0));
                }
                sizes.push(len);
                let body = br#"{"text":"mock transcript"}"#;
                let close = connection == 0 || request == 1;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: {}\r\n\r\n",
                    body.len(),
                    if close { "close" } else { "keep-alive" }
                )?;
                stream.write_all(body)?;
            }
        }
        Ok(sizes)
    });
    Ok((endpoint, worker))
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let seconds = args
        .get(2)
        .map(|s| s.parse::<usize>())
        .transpose()?
        .unwrap_or(2);
    anyhow::ensure!(
        (1..=120).contains(&seconds),
        "duration must be 1..120 seconds"
    );
    let mut clip = if let Some(path) = args.get(1) {
        let mut reader = hound::WavReader::open(path)?;
        let spec = reader.spec();
        anyhow::ensure!(
            spec.bits_per_sample == 16 && spec.sample_format == hound::SampleFormat::Int,
            "fixture must be PCM16 WAV"
        );
        AudioClip {
            samples: reader
                .samples::<i16>()
                .map(|r| r.map(|s| f32::from(s) / 32768.0))
                .collect::<std::result::Result<_, _>>()?,
            sample_rate: spec.sample_rate,
            channels: spec.channels,
        }
    } else {
        AudioClip {
            samples: (0..48000 * seconds)
                .map(|i| 0.5 * (i as f32 * 0.1).sin())
                .collect(),
            sample_rate: 48000,
            channels: 1,
        }
    };
    clip.samples
        .truncate(seconds * clip.sample_rate as usize * usize::from(clip.channels));
    for run in 1..=3 {
        let (url, worker) = mock()?;
        let baseline = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(20))
            .build()?;
        let t = Instant::now();
        let wav = original_rate_wav(&clip);
        let form = reqwest::multipart::Form::new().text("model", "test").part(
            "file",
            reqwest::multipart::Part::bytes(wav).file_name("dictation.wav"),
        );
        baseline
            .post(&url)
            .multipart(form)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let before = t.elapsed();
        let config = SttConfig {
            provider: "openai".into(),
            endpoint: Some(url),
            ..Default::default()
        };
        let provider = xflow_providers::build_stt(&config, true)?;
        let t = Instant::now();
        provider.warm().await?;
        let warm = t.elapsed();
        let t = Instant::now();
        provider
            .transcribe(clip.clone(), Default::default())
            .await?;
        let after = t.elapsed();
        let sizes = tokio::task::spawn_blocking(move || worker.join().expect("mock thread failed"))
            .await
            .context("mock join failed")??;
        println!("run={run} seconds={seconds} before_ms={:.2} after_ms={:.2} warm_ms={:.2} before_wire_bytes={} after_wire_bytes={}",before.as_secs_f64()*1000.0,after.as_secs_f64()*1000.0,warm.as_secs_f64()*1000.0,sizes[0],sizes[2]);
    }
    Ok(())
}
