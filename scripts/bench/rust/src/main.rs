//! Standalone micro-benchmarks for xflow hot paths, driven by scripts/benchmark.py.
//!
//! Usage: xflow-bench <suite> [--samples N] [--hosts a,b] [--i-consent-to-open-the-microphone]
//! Suites: wav stt dbus dbus-serve spawn tui json keyring-crypto net mic-open
//!
//! Prints one JSON object on stdout; timing samples are raw microseconds (or
//! nanoseconds where named) so the harness computes every percentile the same way.
//! Safety: synthetic audio only; loopback mock provider; D-Bus suites refuse to run
//! outside an isolated bus; ydotool only talks to a fake socket owned by this
//! process; `net` sends unauthenticated GET / requests (no credentials, no audio);
//! `mic-open` refuses to run without its explicit consent flag and discards audio.
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};
use xflow_core::{
    config::SttConfig,
    ipc::{Request, Response},
    AppContext, AudioClip, DesktopEvent, State, TranscriptionOptions,
};

struct Report(Map<String, Value>);
impl Report {
    fn samples(&mut self, name: &str, unit: &str, values: Vec<f64>) {
        let values: Vec<f64> = values
            .iter()
            .map(|v| (v * 1000.0).round() / 1000.0)
            .collect();
        self.0
            .insert(name.into(), json!({"unit": unit, "samples": values}));
    }
    fn value(&mut self, name: &str, unit: &str, value: impl Into<Value>) {
        self.0
            .insert(name.into(), json!({"unit": unit, "value": value.into()}));
    }
}

fn us(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1e6
}

fn time_us<T>(f: impl FnOnce() -> T) -> f64 {
    let started = Instant::now();
    black_box(f());
    us(started.elapsed())
}

struct Args {
    suite: String,
    samples: Option<usize>,
    hosts: Vec<String>,
    mic_consent: bool,
}

fn args() -> Result<Args> {
    let mut iter = std::env::args().skip(1);
    let suite = iter
        .next()
        .context("usage: xflow-bench <suite> [options]")?;
    let mut parsed = Args {
        suite,
        samples: None,
        hosts: vec![],
        mic_consent: false,
    };
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--samples" => parsed.samples = Some(iter.next().context("--samples N")?.parse()?),
            "--hosts" => {
                parsed.hosts = iter
                    .next()
                    .context("--hosts a,b")?
                    .split(',')
                    .map(str::to_owned)
                    .collect()
            }
            "--i-consent-to-open-the-microphone" => parsed.mic_consent = true,
            other => bail!("unknown option {other}"),
        }
    }
    Ok(parsed)
}

#[tokio::main(worker_threads = 2)] // Same runtime shape as xflowd.
async fn main() -> Result<()> {
    let args = args()?;
    let n = |default: usize| args.samples.unwrap_or(default).max(1);
    let mut report = Report(Map::new());
    match args.suite.as_str() {
        "wav" => wav(&mut report, n(1))?,
        "stt" => stt(&mut report, n(30)).await?,
        "dbus" => dbus(&mut report, n(1000)).await?,
        "dbus-serve" => return dbus_serve().await,
        "spawn" => spawn(&mut report, n(100), false).await?,
        "spawn-fake-ydotool" => spawn(&mut report, n(100), true).await?,
        "tui" => tui(&mut report, n(2000))?,
        "json" => json_suite(&mut report, n(30))?,
        "keyring-crypto" => keyring_crypto(&mut report, n(50)),
        "net" => net(&mut report, &args.hosts, n(5)).await?,
        "mic-open" => mic_open(&mut report, args.mic_consent, n(5))?,
        other => bail!("unknown suite {other}"),
    }
    println!(
        "{}",
        json!({"suite": args.suite, "metrics": Value::Object(report.0)})
    );
    Ok(())
}

// ---------------------------------------------------------------- audio / WAV

/// Deterministic speech-like signal: three partials, 4 Hz syllable envelope, light noise.
fn synth(seconds: f64, rate: u32, channels: u16) -> AudioClip {
    let frames = (seconds * f64::from(rate)) as usize;
    let mut seed = 0x2545_F491_4F6C_DD1D_u64;
    let mut samples = Vec::with_capacity(frames * usize::from(channels));
    let tau = std::f64::consts::TAU;
    for i in 0..frames {
        let t = i as f64 / f64::from(rate);
        let envelope = 0.5 + 0.5 * (tau * 4.0 * t).sin();
        let voice = 0.5 * (tau * 180.0 * t).sin()
            + 0.3 * (tau * 720.0 * t).sin()
            + 0.2 * (tau * 2400.0 * t).sin();
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let noise = ((seed >> 40) as f64 / f64::from(1u32 << 24) - 0.5) * 0.02;
        let value = (0.3 * envelope * voice + noise) as f32;
        samples.extend(std::iter::repeat_n(value, usize::from(channels)));
    }
    AudioClip {
        samples,
        sample_rate: rate,
        channels,
    }
}

/// Windowed-sinc (Blackman) low-pass FIR, unity DC gain. `cutoff` is in Hz at `rate`.
fn lowpass_taps(count: usize, cutoff: f64, rate: f64) -> Vec<f32> {
    let fc = cutoff / rate;
    let mid = (count - 1) as f64 / 2.0;
    let tau = std::f64::consts::TAU;
    let mut taps: Vec<f64> = (0..count)
        .map(|i| {
            let x = i as f64 - mid;
            let sinc = if x == 0.0 {
                2.0 * fc
            } else {
                (tau * fc * x).sin() / (std::f64::consts::PI * x)
            };
            let w = 0.42 - 0.5 * (tau * i as f64 / (count - 1) as f64).cos()
                + 0.08 * (2.0 * tau * i as f64 / (count - 1) as f64).cos();
            sinc * w
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    taps.iter_mut().for_each(|t| *t /= sum);
    taps.into_iter().map(|t| t as f32).collect()
}

/// Prototype for the providers/platform owners: downmix + integer-factor polyphase
/// decimation (48 kHz → 16 kHz with factor 3). Only every `factor`-th output is computed.
fn downmix_decimate(audio: &AudioClip, factor: usize, taps: &[f32]) -> AudioClip {
    let channels = usize::from(audio.channels);
    let half = taps.len() / 2;
    let mut padded = vec![0.0_f32; half];
    padded.extend(
        audio
            .samples
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32),
    );
    let frames = padded.len() - half;
    padded.extend(std::iter::repeat_n(0.0, taps.len()));
    let samples = (0..frames / factor)
        .map(|out| {
            let window = &padded[out * factor..out * factor + taps.len()];
            // Eight partial sums let LLVM vectorize despite float non-associativity.
            let mut acc = [0.0_f32; 8];
            let (head, tail) = window.split_at(window.len() / 8 * 8);
            let (taps_head, taps_tail) = taps.split_at(head.len());
            for (w, t) in head
                .as_chunks::<8>()
                .0
                .iter()
                .zip(taps_head.as_chunks::<8>().0)
            {
                for lane in 0..8 {
                    acc[lane] += w[lane] * t[lane];
                }
            }
            acc.iter().sum::<f32>() + tail.iter().zip(taps_tail).map(|(a, b)| a * b).sum::<f32>()
        })
        .collect();
    AudioClip {
        samples,
        sample_rate: audio.sample_rate / factor as u32,
        channels: 1,
    }
}

fn tone(freq: f64, seconds: f64, rate: u32) -> AudioClip {
    let samples = (0..(seconds * f64::from(rate)) as usize)
        .map(|i| (std::f64::consts::TAU * freq * i as f64 / f64::from(rate)).sin() as f32 * 0.5)
        .collect();
    AudioClip {
        samples,
        sample_rate: rate,
        channels: 1,
    }
}

fn rms(samples: &[f32]) -> f64 {
    (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
}

fn wav(report: &mut Report, scale: usize) -> Result<()> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let taps = lowpass_taps(95, 7_200.0, 48_000.0);
    for (seconds, iterations) in [(5.0, 30), (30.0, 10), (120.0, 4)] {
        let iterations = iterations * scale;
        for (rate, channels) in [(48_000, 2), (48_000, 1), (16_000, 1)] {
            let clip = synth(seconds, rate, channels);
            let name = format!("{}k_{}ch_{}s", rate / 1000, channels, seconds as u32);
            let mut encode = Vec::new();
            let mut size = 0;
            for _ in 0..iterations {
                let started = Instant::now();
                let wav = xflow_providers::encode_wav(&clip)?;
                encode.push(us(started.elapsed()));
                size = wav.len();
            }
            report.samples(&format!("encode_wav_{name}_us"), "us", encode);
            report.value(&format!("wav_bytes_{name}"), "bytes", size);
            report.value(
                &format!("capture_f32_bytes_{name}"),
                "bytes",
                clip.samples.len() * 4,
            );
            report.value(
                &format!("router_base64_bytes_{name}"),
                "bytes",
                size.div_ceil(3) * 4,
            );
        }
        // Proposed path: decimate 48 kHz stereo to 16 kHz mono, then encode.
        let clip = synth(seconds, 48_000, 2);
        let mut decimate = Vec::new();
        let mut decimate_encode = Vec::new();
        for _ in 0..iterations {
            let started = Instant::now();
            let small = downmix_decimate(&clip, 3, &taps);
            decimate.push(us(started.elapsed()));
            let wav = xflow_providers::encode_wav(&small)?;
            decimate_encode.push(us(started.elapsed()));
            black_box(wav);
        }
        let label = format!("48k_2ch_{}s", seconds as u32);
        report.samples(&format!("decimate_to_16k_{label}_us"), "us", decimate);
        report.samples(
            &format!("decimate_and_encode_16k_{label}_us"),
            "us",
            decimate_encode,
        );
        // Router protocol: base64 + JSON body construction for a 48 kHz mono upload.
        let wav = xflow_providers::encode_wav(&synth(seconds, 48_000, 1))?;
        let router: Vec<f64> = (0..iterations)
            .map(|_| {
                time_us(|| {
                    let body = json!({"model": "m", "input_audio": {"data": STANDARD.encode(&wav), "format": "wav"}, "response_format": "json"});
                    serde_json::to_vec(&body).map(|v| v.len())
                })
            })
            .collect();
        report.samples(
            &format!("router_base64_json_48k_1ch_{}s_us", seconds as u32),
            "us",
            router,
        );
    }
    // Decimator quality evidence: passband gain and alias rejection.
    for freq in [1_000.0, 3_400.0, 6_000.0, 9_000.0, 12_000.0, 20_000.0] {
        let input = tone(freq, 1.0, 48_000);
        let output = downmix_decimate(&input, 3, &taps);
        let skip = 200; // filter warm-up
        let gain = rms(&output.samples[skip..output.samples.len() - skip]) / rms(&input.samples);
        report.value(
            &format!("decimator_gain_db_{}hz", freq as u32),
            "dB",
            (20.0 * gain.max(1e-12).log10() * 100.0).round() / 100.0,
        );
    }
    report.value("decimator_taps", "count", taps.len());
    Ok(())
}

// ------------------------------------------------------- stop → text, loopback

const MOCK_BODY: &[u8] =
    br#"{"text":"The quick brown fox jumps over the lazy dog.","language":"en"}"#;

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Minimal HTTP/1.1 keep-alive server: reads a full request, replies with a fixed transcript.
async fn mock_provider() -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/v1/audio/transcriptions", listener.local_addr()?);
    let mut reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        MOCK_BODY.len()
    )
    .into_bytes();
    reply.extend_from_slice(MOCK_BODY);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let reply = reply.clone();
            tokio::spawn(async move {
                let _ = socket.set_nodelay(true);
                let mut buffer = Vec::with_capacity(1 << 16);
                loop {
                    let head = loop {
                        if let Some(offset) = find(&buffer, b"\r\n\r\n") {
                            break offset + 4;
                        }
                        if socket.read_buf(&mut buffer).await.unwrap_or(0) == 0 {
                            return;
                        }
                    };
                    let headers = String::from_utf8_lossy(&buffer[..head]).to_ascii_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    while buffer.len() < head + length {
                        buffer.reserve(head + length - buffer.len());
                        if socket.read_buf(&mut buffer).await.unwrap_or(0) == 0 {
                            return;
                        }
                    }
                    buffer.drain(..head + length);
                    if socket.write_all(&reply).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    Ok(url)
}

fn loopback_config(provider: &str, url: &str) -> SttConfig {
    SttConfig {
        provider: provider.into(),
        endpoint: Some(url.into()),
        model: Some("bench-model".into()),
        ..SttConfig::default()
    }
}

async fn stt(report: &mut Report, samples: usize) -> Result<()> {
    let wav = xflow_providers::encode_wav(&synth(5.0, 16_000, 1))?;
    report.samples(
        "multipart_build_16k_5s_us",
        "us",
        (0..samples)
            .map(|_| {
                time_us(|| {
                    reqwest::multipart::Form::new()
                        .text("model", "bench-model")
                        .part(
                            "file",
                            reqwest::multipart::Part::bytes(wav.clone())
                                .file_name("dictation.wav")
                                .mime_str("audio/wav")
                                .unwrap(),
                        )
                })
            })
            .collect(),
    );
    report.samples(
        "provider_response_parse_ns",
        "ns",
        per_op_ns(samples, 10_000, || {
            black_box(serde_json::from_slice::<Value>(black_box(MOCK_BODY)).unwrap());
        }),
    );
    let url = mock_provider().await?;
    let mut build = Vec::new();
    for _ in 0..samples {
        let started = Instant::now();
        black_box(xflow_providers::build_stt(
            &loopback_config("custom", &url),
            false,
        )?);
        build.push(us(started.elapsed()));
    }
    report.samples("stt_build_provider_us", "us", build);
    let cases = [
        ("custom", 48_000, 2, 5.0),
        ("custom", 48_000, 2, 30.0),
        ("custom", 48_000, 1, 30.0),
        ("custom", 16_000, 1, 5.0),
        ("custom", 16_000, 1, 30.0),
        ("openrouter", 48_000, 1, 30.0),
        ("openrouter", 16_000, 1, 30.0),
    ];
    for (provider, rate, channels, seconds) in cases {
        let stt = xflow_providers::build_stt(&loopback_config(provider, &url), false)?;
        let clip = synth(seconds, rate, channels);
        // Warm the keep-alive connection first, as a second dictation would find it.
        stt.transcribe(clip.clone(), TranscriptionOptions::default())
            .await?;
        let mut warm = Vec::new();
        for _ in 0..samples {
            let audio = clip.clone();
            let started = Instant::now();
            let transcript = stt
                .transcribe(audio, TranscriptionOptions::default())
                .await?;
            warm.push(us(started.elapsed()));
            black_box(transcript);
        }
        let name = format!(
            "stt_loopback_{}_{}k_{}ch_{}s_us",
            if provider == "custom" {
                "multipart"
            } else {
                "router_json"
            },
            rate / 1000,
            channels,
            seconds as u32
        );
        report.samples(&name, "us", warm);
    }
    // Cold: a fresh client (new pool) per request, including the loopback TCP connect.
    let clip = synth(5.0, 16_000, 1);
    let mut cold = Vec::new();
    for _ in 0..samples {
        let audio = clip.clone();
        let started = Instant::now();
        let stt = xflow_providers::build_stt(&loopback_config("custom", &url), false)?;
        black_box(
            stt.transcribe(audio, TranscriptionOptions::default())
                .await?,
        );
        cold.push(us(started.elapsed()));
    }
    report.samples("stt_loopback_cold_client_16k_1ch_5s_us", "us", cold);
    Ok(())
}

// ------------------------------------------------------------------- D-Bus

const CONTEXT_JSON: &str =
    r#"{"app_id":"org.gnome.TextEditor.desktop","window_id":"42","selected_text":null}"#;

struct ShellStub;
#[zbus::interface(name = "org.xflow.Shell")]
impl ShellStub {
    fn context(&self) -> String {
        CONTEXT_JSON.into()
    }
}

type Actor = tokio::sync::mpsc::Sender<(Request, tokio::sync::oneshot::Sender<Response>)>;

/// Proposed org.xflow.Daemon.Command(s) -> s, forwarding through an actor channel
/// like the daemon's Unix-socket connection handler does.
struct DaemonStub(Actor);
#[zbus::interface(name = "org.xflow.Daemon")]
impl DaemonStub {
    async fn command(&self, request: &str) -> zbus::fdo::Result<String> {
        let value: Value = serde_json::from_str(request)
            .map_err(|_| zbus::fdo::Error::InvalidArgs("invalid JSON".into()))?;
        let command = value
            .get("command")
            .cloned()
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("missing command".into()))?;
        let request: Request = serde_json::from_value(json!({ "command": command }))
            .map_err(|_| zbus::fdo::Error::InvalidArgs("unknown command".into()))?;
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.0
            .send((request, reply))
            .await
            .map_err(|_| zbus::fdo::Error::Failed("daemon stopped".into()))?;
        let response = receive
            .await
            .map_err(|_| zbus::fdo::Error::Failed("daemon stopped".into()))?;
        serde_json::to_string(&response).map_err(|_| zbus::fdo::Error::Failed("encode".into()))
    }
    #[zbus(signal)]
    async fn event(context: &zbus::SignalContext<'_>, json: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn level(context: &zbus::SignalContext<'_>, level: f64) -> zbus::Result<()>;
}

fn require_isolated_bus() -> Result<()> {
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if std::env::var_os("XFLOW_BENCH_ISOLATED_BUS").is_none()
        || runtime.is_empty()
        || !address.starts_with(&format!("unix:path={runtime}/bus"))
        || address.contains("/run/user/")
    {
        bail!("refusing to use a non-isolated session bus; run through scripts/benchmark.py");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_preserves_duration_and_rejects_alias_band() {
        let taps = lowpass_taps(95, 7_200.0, 48_000.0);
        let input = tone(1_000.0, 1.0, 48_000);
        let output = downmix_decimate(&input, 3, &taps);
        assert_eq!(
            (output.sample_rate, output.channels, output.samples.len()),
            (16_000, 1, 16_000)
        );
        assert!((rms(&output.samples[200..15_800]) / rms(&input.samples) - 1.0).abs() < 0.01);
        let stopband = downmix_decimate(&tone(12_000.0, 1.0, 48_000), 3, &taps);
        assert!(rms(&stopband.samples[200..15_800]) < 0.0001);
        let wav = xflow_providers::encode_wav(&output).unwrap();
        assert_eq!(wav.len(), 32_044);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
    }
}

async fn stub_server() -> Result<zbus::Connection> {
    let (actor, mut commands) = tokio::sync::mpsc::channel::<(Request, _)>(32);
    tokio::spawn(async move {
        while let Some((_request, reply)) = commands.recv().await {
            let reply: tokio::sync::oneshot::Sender<Response> = reply;
            let _ = reply.send(Response::status(State::Listening, 0.0));
        }
    });
    Ok(zbus::ConnectionBuilder::session()?
        .name("org.xflow.Shell")?
        .name("org.xflow.Daemon")?
        .serve_at("/org/xflow/Shell", ShellStub)?
        .serve_at("/org/xflow/Daemon", DaemonStub(actor))?
        .build()
        .await?)
}

/// Exact replica of xflow-platform desktop.rs `shell_context` (new connection per call).
async fn shell_context_new_connection() -> Result<AppContext> {
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.xflow.Shell",
        "/org/xflow/Shell",
        "org.xflow.Shell",
    )
    .await?;
    let json: String = proxy.call("Context", &()).await?;
    Ok(serde_json::from_str(&json)?)
}

fn command_json() -> String {
    json!({"command": "toggle", "mode": "dictation",
           "context": serde_json::from_str::<Value>(CONTEXT_JSON).unwrap(), "t0_us": 123_456_789})
    .to_string()
}

async fn dbus(report: &mut Report, samples: usize) -> Result<()> {
    use zbus::export::futures_util::StreamExt;
    require_isolated_bus()?;
    let server = stub_server().await?;
    let connections = samples.min(200);
    let mut setup = Vec::new();
    for _ in 0..connections {
        let started = Instant::now();
        let connection = zbus::Connection::session().await?;
        setup.push(us(started.elapsed()));
        drop(connection);
    }
    report.samples("dbus_connection_setup_us", "us", setup);
    let mut fresh = Vec::new();
    for _ in 0..connections {
        let started = Instant::now();
        black_box(shell_context_new_connection().await?);
        fresh.push(us(started.elapsed()));
    }
    report.samples("context_new_connection_us", "us", fresh);

    let client = zbus::Connection::session().await?;
    let shell = zbus::Proxy::new(
        &client,
        "org.xflow.Shell",
        "/org/xflow/Shell",
        "org.xflow.Shell",
    )
    .await?;
    let mut reused = Vec::new();
    for _ in 0..samples {
        let started = Instant::now();
        let json: String = shell.call("Context", &()).await?;
        black_box(serde_json::from_str::<AppContext>(&json)?);
        reused.push(us(started.elapsed()));
    }
    report.samples("context_reused_proxy_us", "us", reused);

    let daemon = zbus::Proxy::new(
        &client,
        "org.xflow.Daemon",
        "/org/xflow/Daemon",
        "org.xflow.Daemon",
    )
    .await?;
    let request = command_json();
    let mut command = Vec::new();
    for _ in 0..samples {
        let started = Instant::now();
        let json: String = daemon.call("Command", &(request.as_str(),)).await?;
        black_box(serde_json::from_str::<Response>(&json)?);
        command.push(us(started.elapsed()));
    }
    report.samples("command_rtt_reused_proxy_us", "us", command);

    // Event signal cost: today's JSON string vs a typed level-only signal.
    let event = DesktopEvent {
        state: State::Listening,
        level: 0.421_337,
        message: None,
    };
    let event_json = serde_json::to_string(&event)?;
    let path = "/org/xflow/Daemon";
    let iface = "org.xflow.Daemon";
    let json_message =
        zbus::Message::signal(path, iface, "Event")?.build(&(event_json.as_str(),))?;
    let level_message = zbus::Message::signal(path, iface, "Level")?.build(&(0.421_337_f64,))?;
    report.value("event_json_payload_bytes", "bytes", event_json.len());
    report.value(
        "event_json_message_bytes",
        "bytes",
        json_message.data().len(),
    );
    report.value(
        "level_typed_message_bytes",
        "bytes",
        level_message.data().len(),
    );
    let mut emit_json = Vec::new();
    let mut emit_level = Vec::new();
    for _ in 0..samples {
        let started = Instant::now();
        let json = serde_json::to_string(&event)?;
        server
            .emit_signal(None::<()>, path, iface, "Event", &(json.as_str(),))
            .await?;
        emit_json.push(us(started.elapsed()));
        let started = Instant::now();
        server
            .emit_signal(None::<()>, path, iface, "Level", &(0.421_337_f64,))
            .await?;
        emit_level.push(us(started.elapsed()));
    }
    report.samples("event_emit_json_us", "us", emit_json);
    report.samples("level_emit_typed_us", "us", emit_level);

    // Delivery latency to a subscriber (emit → subscriber stream), paced at 2 ms.
    let mut events = daemon.receive_signal("Event").await?;
    let base = Instant::now();
    let deliveries = samples.min(500);
    let receiver = tokio::spawn(async move {
        let mut latencies = Vec::new();
        while latencies.len() < deliveries {
            let Some(signal) = events.next().await else {
                break;
            };
            let received = base.elapsed();
            let (json,): (String,) = signal.body().deserialize()?;
            let event: DesktopEvent = serde_json::from_str(&json)?;
            let sent: u64 = event.message.as_deref().unwrap_or("0").parse()?;
            latencies.push(us(received) - sent as f64 / 1000.0);
        }
        anyhow::Ok(latencies)
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    for _ in 0..deliveries {
        let stamped = DesktopEvent {
            message: Some(base.elapsed().as_nanos().to_string()),
            ..event.clone()
        };
        let json = serde_json::to_string(&stamped)?;
        server
            .emit_signal(None::<()>, path, iface, "Event", &(json.as_str(),))
            .await?;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let latencies = tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .context("signal delivery timed out")???;
    report.samples("event_delivery_json_us", "us", latencies);
    Ok(())
}

/// Serve the stub names until stdin closes, for the GJS (GNOME Shell runtime) client.
async fn dbus_serve() -> Result<()> {
    require_isolated_bus()?;
    let _server = stub_server().await?;
    println!("ready");
    tokio::task::spawn_blocking(|| {
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut Vec::new())
    })
    .await??;
    Ok(())
}

// ------------------------------------------------------------- subprocesses

async fn spawn_status(program: &str, args: &[&str], env: &[(&str, &str)]) -> Result<f64> {
    let started = Instant::now();
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    tokio::time::timeout(Duration::from_secs(5), command.status())
        .await
        .context("spawn timed out")??;
    Ok(us(started.elapsed()))
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

async fn spawn(report: &mut Report, samples: usize, fake_keys: bool) -> Result<()> {
    let probes: [(&str, &str, &[&str]); 3] = [
        ("spawn_true_us", "true", &[]),
        ("spawn_wl_copy_version_us", "wl-copy", &["--version"]),
        ("spawn_ydotool_help_us", "ydotool", &["--help"]),
    ];
    for (name, program, args) in probes {
        if !on_path(program) {
            report.value(name, "us", Value::Null);
            continue;
        }
        let mut values = Vec::new();
        for _ in 0..samples {
            values.push(spawn_status(program, args, &[]).await?);
        }
        report.samples(name, "us", values);
    }
    // Normal runs invoke help/version only. The optional historical fixture sends
    // datagrams to its own socket; it is never included in the baseline harness.
    if !fake_keys || !on_path("ydotool") {
        return Ok(());
    }
    // virtual_paste() replica against a fake ydotoold socket owned by this process:
    // the datagrams are counted and dropped, nothing reaches uinput or the desktop.
    let dir = std::env::temp_dir().join(format!("xflow-bench-ydotool-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let socket_path = dir.join("fake.sock");
    let _ = std::fs::remove_file(&socket_path);
    let socket = std::os::unix::net::UnixDatagram::bind(&socket_path)?;
    socket.set_read_timeout(Some(Duration::from_millis(1)))?;
    let fake = socket_path.to_string_lossy().into_owned();
    let env = [("YDOTOOL_SOCKET", fake.as_str())];
    let ctrl_v = ["key", "29:1", "47:1", "47:0", "29:0"];
    let ctrl_v_no_delay = ["key", "--key-delay", "0", "29:1", "47:1", "47:0", "29:0"];
    for (name, args) in [
        ("ydotool_key_ctrl_v_fake_socket_us", &ctrl_v[..]),
        (
            "ydotool_key_ctrl_v_delay0_fake_socket_us",
            &ctrl_v_no_delay[..],
        ),
    ] {
        let mut values = Vec::new();
        let mut datagrams = 0;
        for _ in 0..samples.min(30) {
            values.push(spawn_status("ydotool", args, &env).await?);
            let mut buffer = [0_u8; 64];
            while socket.recv(&mut buffer).is_ok() {
                datagrams += 1;
            }
        }
        report.value(
            &name.replace("_us", "_datagrams_per_run"),
            "count",
            datagrams as f64 / values.len() as f64,
        );
        report.samples(name, "us", values);
    }
    drop(socket);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

// ---------------------------------------------------------------------- TUI

/// Replica of the draw closure in crates/xflow-app/src/tui.rs.
fn draw_tui(frame: &mut ratatui::Frame, status: &Response, transcript: &str) {
    use ratatui::{
        layout::{Constraint, Layout},
        widgets::{Block, Gauge, Paragraph},
    };
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(format!(
            "{:?}  {}",
            status.state,
            status.message.as_deref().unwrap_or("")
        ))
        .block(Block::bordered().title("xflow")),
        rows[0],
    );
    frame.render_widget(
        Gauge::default()
            .block(Block::bordered().title("Microphone"))
            .ratio(status.level.clamp(0.0, 1.0) as f64),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(transcript).block(Block::bordered().title("Last transcript (l to load)")),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new("Space toggle • Esc cancel • l last • c copy • p paste • q quit"),
        rows[3],
    );
}

#[derive(Clone, Default)]
struct ByteCounter(std::rc::Rc<std::cell::Cell<usize>>);
impl std::io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.set(self.0.get() + bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn tui(report: &mut Report, samples: usize) -> Result<()> {
    use ratatui::{
        backend::{CrosstermBackend, TestBackend},
        layout::Rect,
        Terminal, TerminalOptions, Viewport,
    };
    let transcript = "Postgres is ready. The quick brown fox jumps over the lazy dog.";
    for (width, height) in [(80_u16, 24_u16), (160, 48), (240, 67)] {
        let size = format!("{width}x{height}");
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        let mut status = Response::status(State::Listening, 0.0);
        let mut level = Vec::new();
        for i in 0..samples {
            status.level = (i % 20) as f32 / 20.0;
            let started = Instant::now();
            terminal.draw(|frame| draw_tui(frame, &status, transcript))?;
            level.push(us(started.elapsed()));
        }
        report.samples(&format!("tui_level_redraw_{size}_us"), "us", level);
        let mut state = Vec::new();
        for i in 0..samples / 4 {
            status = if i % 2 == 0 {
                Response::status(State::Processing, 0.0)
            } else {
                Response::error(
                    State::Error,
                    "provider returned HTTP 429; response body omitted",
                )
            };
            let started = Instant::now();
            terminal.draw(|frame| draw_tui(frame, &status, transcript))?;
            state.push(us(started.elapsed()));
        }
        report.samples(&format!("tui_state_redraw_{size}_us"), "us", state);

        // Real crossterm escape output (diffed frames) into a byte counter.
        let counter = ByteCounter::default();
        let mut terminal = Terminal::with_options(
            CrosstermBackend::new(counter.clone()),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, width, height)),
            },
        )?;
        let mut status = Response::status(State::Listening, 0.0);
        terminal.draw(|frame| draw_tui(frame, &status, transcript))?;
        let first = counter.0.get();
        let mut timings = Vec::new();
        for i in 0..samples {
            status.level = (i % 20) as f32 / 20.0 + 0.025;
            let started = Instant::now();
            terminal.draw(|frame| draw_tui(frame, &status, transcript))?;
            timings.push(us(started.elapsed()));
        }
        report.value(&format!("tui_full_frame_bytes_{size}"), "bytes", first);
        report.value(
            &format!("tui_level_redraw_bytes_{size}"),
            "bytes",
            (counter.0.get() - first) as f64 / samples as f64,
        );
        report.samples(
            &format!("tui_level_redraw_crossterm_{size}_us"),
            "us",
            timings,
        );
    }
    Ok(())
}

// ----------------------------------------------------------- JSON encodings

fn per_op_ns(samples: usize, batch: usize, mut f: impl FnMut()) -> Vec<f64> {
    (0..samples)
        .map(|_| {
            let started = Instant::now();
            for _ in 0..batch {
                f();
            }
            started.elapsed().as_nanos() as f64 / batch as f64
        })
        .collect()
}

fn json_suite(report: &mut Report, samples: usize) -> Result<()> {
    let event = DesktopEvent {
        state: State::Listening,
        level: 0.421_337,
        message: None,
    };
    let response = Response::status(State::Listening, 0.421_337);
    let encoded = serde_json::to_string(&response)?;
    let batch = 10_000;
    report.samples(
        "json_desktop_event_encode_ns",
        "ns",
        per_op_ns(samples, batch, || {
            black_box(serde_json::to_string(black_box(&event)).ok());
        }),
    );
    report.samples(
        "json_response_encode_ns",
        "ns",
        per_op_ns(samples, batch, || {
            black_box(serde_json::to_vec(black_box(&response)).ok());
        }),
    );
    report.samples(
        "json_response_decode_ns",
        "ns",
        per_op_ns(samples, batch, || {
            black_box(serde_json::from_str::<Response>(black_box(&encoded)).ok());
        }),
    );
    report.samples(
        "json_request_decode_ns",
        "ns",
        per_op_ns(samples, batch, || {
            black_box(serde_json::from_str::<Request>(black_box(r#"{"command":"toggle"}"#)).ok());
        }),
    );
    report.value("json_response_bytes", "bytes", encoded.len());
    report.value(
        "json_desktop_event_bytes",
        "bytes",
        serde_json::to_string(&event)?.len(),
    );
    Ok(())
}

// ------------------------------------------------- keyring session crypto

/// Arithmetic surrogate only: this is NOT keyring latency. Secret Service uses a
/// different pow implementation plus D-Bus, HKDF and AES; none are measured here.
fn keyring_crypto(report: &mut Report, samples: usize) {
    use num_bigint::BigUint;
    let prime = BigUint::parse_bytes(
        b"FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE65381FFFFFFFFFFFFFFFF",
        16,
    )
    .expect("valid prime");
    let generator = BigUint::from(2_u32);
    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let values = (0..samples)
        .map(|_| {
            let bytes: Vec<u8> = (0..128)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as u8
                })
                .collect();
            let private = BigUint::from_bytes_be(&bytes);
            time_us(|| generator.modpow(&private, &prime))
        })
        .collect();
    report.samples("keyring_dh_modpow_1024_us", "us", values);
    report.value(
        "keyring_crypto_scope",
        "text",
        "num-bigint modpow surrogate; no keyring access",
    );
}

// ------------------------------------------------- provider network setup

async fn net(report: &mut Report, hosts: &[String], samples: usize) -> Result<()> {
    // Same client settings as xflow-providers `client()`: rustls, HTTP/1.1 only.
    let client = || {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
    };
    async fn get(client: &reqwest::Client, url: &str) -> Result<(f64, f64, String)> {
        let started = Instant::now();
        let mut response = client.get(url).send().await?;
        let headers = us(started.elapsed());
        let version = format!("{:?}", response.version());
        let mut total = 0;
        while let Some(chunk) = response.chunk().await? {
            total += chunk.len();
            if total > 1 << 20 {
                break;
            }
        }
        Ok((headers, us(started.elapsed()), version))
    }
    for host in hosts {
        if !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            bail!("invalid host {host}");
        }
        let url = format!("https://{host}/");
        let key = host.replace(['.', '-'], "_");
        let mut cold = Vec::new();
        let mut version = String::new();
        let mut errors = 0;
        for _ in 0..samples {
            match get(&client()?, &url).await {
                Ok((_, total, v)) => {
                    cold.push(total);
                    version = v;
                }
                Err(_) => errors += 1,
            }
        }
        let warm_client = client()?;
        let mut warm = Vec::new();
        if get(&warm_client, &url).await.is_ok() {
            for _ in 0..samples {
                match get(&warm_client, &url).await {
                    Ok((_, total, _)) => warm.push(total),
                    Err(_) => errors += 1,
                }
            }
        }
        report.samples(&format!("reqwest_cold_{key}_us"), "us", cold);
        report.samples(&format!("reqwest_warm_{key}_us"), "us", warm);
        report.value(&format!("reqwest_version_{key}"), "text", version);
        report.value(&format!("reqwest_errors_{key}"), "count", errors);
    }
    Ok(())
}

// ------------------------------------------- microphone open (consent-gated)

fn mic_open(report: &mut Report, consent: bool, samples: usize) -> Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::mpsc;
    if !consent {
        bail!("mic-open opens the default microphone; run scripts/bench/mic_open.py, which asks for consent");
    }
    fn open<T: cpal::SizedSample>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        first: mpsc::Sender<(Instant, usize)>,
    ) -> Result<cpal::Stream> {
        let mut sent = false;
        // Audio is discarded immediately; only the arrival time and length are kept.
        Ok(device.build_input_stream(
            config,
            move |data: &[T], _| {
                if !sent {
                    sent = true;
                    let _ = first.send((Instant::now(), data.len()));
                }
            },
            |_| (),
            Some(Duration::from_secs(3)),
        )?)
    }
    fn build(
        device: &cpal::Device,
        format: cpal::SampleFormat,
        config: &cpal::StreamConfig,
        first: mpsc::Sender<(Instant, usize)>,
    ) -> Result<cpal::Stream> {
        match format {
            cpal::SampleFormat::F32 => open::<f32>(device, config, first),
            cpal::SampleFormat::I16 => open::<i16>(device, config, first),
            cpal::SampleFormat::I32 => open::<i32>(device, config, first),
            cpal::SampleFormat::U16 => open::<u16>(device, config, first),
            other => bail!("unsupported sample format {other}"),
        }
    }
    struct Phases {
        host: f64,
        device: f64,
        config: f64,
        build: f64,
        play: f64,
        first_callback: f64,
        callback_samples: usize,
        close: f64,
    }
    let mut runs = Vec::new();
    let mut description = Value::Null;
    for _ in 0..samples {
        let t0 = Instant::now();
        let host = cpal::default_host();
        let t1 = Instant::now();
        let device = host
            .default_input_device()
            .context("no default microphone")?;
        let t2 = Instant::now();
        let supported = device.default_input_config()?;
        let t3 = Instant::now();
        let (tx, rx) = mpsc::channel();
        let stream = build(&device, supported.sample_format(), &supported.config(), tx)?;
        let t4 = Instant::now();
        stream.play()?;
        let t5 = Instant::now();
        let (first, len) = rx.recv_timeout(Duration::from_secs(3))?;
        let t6 = Instant::now();
        drop(stream);
        let t7 = Instant::now();
        description = json!({"host": format!("{:?}", host.id()), "sample_rate": supported.sample_rate().0,
            "channels": supported.channels(), "format": supported.sample_format().to_string(),
            "buffer_size": format!("{:?}", supported.buffer_size())});
        runs.push(Phases {
            host: us(t1 - t0),
            device: us(t2 - t1),
            config: us(t3 - t2),
            build: us(t4 - t3),
            play: us(t5 - t4),
            first_callback: us(first.saturating_duration_since(t0)),
            callback_samples: len,
            close: us(t7 - t6),
        });
        std::thread::sleep(Duration::from_millis(300));
    }
    report.value("mic_default_config", "json", description);
    report.samples("mic_host_us", "us", runs.iter().map(|r| r.host).collect());
    report.samples(
        "mic_default_device_us",
        "us",
        runs.iter().map(|r| r.device).collect(),
    );
    report.samples(
        "mic_default_config_us",
        "us",
        runs.iter().map(|r| r.config).collect(),
    );
    report.samples(
        "mic_build_stream_us",
        "us",
        runs.iter().map(|r| r.build).collect(),
    );
    report.samples("mic_play_us", "us", runs.iter().map(|r| r.play).collect());
    report.samples(
        "mic_start_to_first_callback_us",
        "us",
        runs.iter().map(|r| r.first_callback).collect(),
    );
    report.samples("mic_close_us", "us", runs.iter().map(|r| r.close).collect());
    report.value(
        "mic_first_callback_samples",
        "count",
        runs.last().map_or(0, |r| r.callback_samples),
    );

    // Proposed: keep the Device + config cached; reopen only the stream per session.
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .context("no default microphone")?;
    let supported = device.default_input_config()?;
    let mut cached = Vec::new();
    for _ in 0..samples {
        let t0 = Instant::now();
        let (tx, rx) = mpsc::channel();
        let stream = build(&device, supported.sample_format(), &supported.config(), tx)?;
        stream.play()?;
        let (first, _) = rx.recv_timeout(Duration::from_secs(3))?;
        cached.push(us(first.saturating_duration_since(t0)));
        drop(stream);
        std::thread::sleep(Duration::from_millis(300));
    }
    report.samples("mic_cached_device_to_first_callback_us", "us", cached);

    // Can the server resample for us? Try 16 kHz mono directly.
    let ranges: Vec<Value> = device
        .supported_input_configs()?
        .map(|r| {
            json!({"channels": r.channels(), "min_rate": r.min_sample_rate().0,
                        "max_rate": r.max_sample_rate().0, "format": r.sample_format().to_string()})
        })
        .collect();
    report.value("mic_supported_ranges", "json", ranges);
    let direct = cpal::StreamConfig {
        channels: 1,
        sample_rate: cpal::SampleRate(16_000),
        buffer_size: cpal::BufferSize::Default,
    };
    let t0 = Instant::now();
    let (tx, rx) = mpsc::channel();
    let outcome = build(&device, cpal::SampleFormat::F32, &direct, tx).and_then(|stream| {
        stream.play()?;
        let (first, len) = rx.recv_timeout(Duration::from_secs(3))?;
        Ok((us(first.saturating_duration_since(t0)), len))
    });
    match outcome {
        Ok((latency, len)) => {
            report.value("mic_direct_16k_mono_first_callback_us", "us", latency);
            report.value("mic_direct_16k_mono_callback_samples", "count", len);
        }
        Err(error) => report.value("mic_direct_16k_mono_error", "text", error.to_string()),
    }
    Ok(())
}
