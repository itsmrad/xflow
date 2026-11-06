//! On-device speech: supervises the `xflow-stt-coreml` helper over the frame
//! protocol in `tools/local-stt/PROTOCOL.md`. No network, no cloud fallback.
use super::*;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

pub const ON_DEVICE: &str = "on-device";
pub const MODEL_ID: &str = "parakeet-tdt-0.6b-v3";
/// Written last by the installer; a model directory without it is never loaded.
pub const INSTALLED_MARKER: &str = ".xflow-installed";
pub const HELPER_NAME: &str = "xflow-stt-coreml";
/// Apple Silicon macOS; the helper itself refuses macOS older than 14.
pub const SUPPORTED: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));
pub const UNSUPPORTED: &str =
    "On-device speech is not supported on this machine; it needs an Apple Silicon Mac with macOS 14 or newer.";
/// Parakeet TDT 0.6B v3 languages (same list as the bundled model catalog).
pub const LANGUAGES: &[&str] = &[
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt",
    "pl", "pt", "ro", "ru", "sk", "sl", "es", "sv", "uk",
];
const MAX_HEADER: u32 = 65_536;
const MAX_PAYLOAD: u32 = 41_943_040;
const MAX_SAMPLES: usize = 19_200_000;
const IDLE: Duration = Duration::from_secs(30);
/// The first Core ML load on a machine takes about 20 s.
const LOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// A failure of the local helper or model. Never retried in the cloud.
#[derive(Debug)]
pub struct OnDeviceError {
    pub code: String,
    pub message: String,
}
impl std::fmt::Display for OnDeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "on-device speech failed ({}): {}",
            self.code, self.message
        )
    }
}
impl std::error::Error for OnDeviceError {}
fn fail(code: &str, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(OnDeviceError {
        code: code.into(),
        message: message.into(),
    })
}

/// `$XDG_DATA_HOME/xflow` or `~/.local/share/xflow`, as `xflow_app::paths::data_dir`, made absolute.
fn data_dir() -> Result<PathBuf> {
    let env = |name| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = env("XDG_DATA_HOME")
        .or_else(|| env("HOME").map(|home| home.join(".local/share")))
        .ok_or_else(|| anyhow!("HOME or XDG_DATA_HOME is required"))?;
    Ok(std::path::absolute(base)?.join("xflow"))
}
/// Parent of the managed model directory and the installer's staging.
pub fn models_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("models"))
}
pub fn model_dir() -> Result<PathBuf> {
    Ok(models_dir()?.join(MODEL_ID))
}
pub fn installed(model_dir: &Path) -> bool {
    model_dir.join(INSTALLED_MARKER).is_file()
}
/// `XFLOW_STT_HELPER` (absolute, for tests and development), else the helper
/// next to the running executable. Never searched on PATH.
pub fn helper_path() -> Result<PathBuf> {
    let path = match std::env::var_os("XFLOW_STT_HELPER").filter(|v| !v.is_empty()) {
        Some(path) => {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                bail!("XFLOW_STT_HELPER must be an absolute path");
            }
            path
        }
        None => std::env::current_exe()?
            .parent()
            .ok_or_else(|| anyhow!("cannot locate the xflow executable"))?
            .join(HELPER_NAME),
    };
    if !path.is_file() {
        bail!(
            "the on-device speech helper is missing at {}; reinstall xflow",
            path.display()
        );
    }
    Ok(path)
}

/// Helper and model readiness without loading the model (`doctor`, `providers test`).
pub async fn check() -> Result<CheckReport> {
    let started = std::time::Instant::now();
    if !SUPPORTED {
        bail!(UNSUPPORTED);
    }
    let helper = helper_path()?;
    let dir = model_dir()?;
    if !installed(&dir) {
        bail!("the on-device model is not installed; run `xflow models install {MODEL_ID}`");
    }
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        command(&helper).arg("--version").output(),
    )
    .await
    .map_err(|_| anyhow!("the on-device speech helper did not answer --version"))??;
    let version: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| anyhow!("the on-device speech helper printed an invalid version"))?;
    if !output.status.success() || version["protocol"] != 1 {
        bail!("the on-device speech helper speaks an unsupported protocol; reinstall xflow");
    }
    Ok(CheckReport {
        ok: true,
        latency_ms: started.elapsed().as_millis() as u64,
        detail: format!(
            "On-device helper {} ready; model {MODEL_ID} installed (not loaded by this check)",
            version["helper"].as_str().unwrap_or("unknown")
        ),
    })
}

/// The helper's environment starts empty: no API keys or other secrets reach it.
fn command(helper: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(helper);
    command.env_clear();
    for name in ["HOME", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}

struct Helper {
    // Killed when dropped: a cancelled or failed request never leaves it running.
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}
impl Helper {
    async fn send(&mut self, header: &Value, payload: &[u8]) -> Result<()> {
        let header = serde_json::to_vec(header)?;
        let mut frame = Vec::with_capacity(header.len() + 8);
        frame.extend_from_slice(&(header.len() as u32).to_le_bytes());
        frame.extend_from_slice(&header);
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        self.stdin.write_all(&frame).await?;
        self.stdin.write_all(payload).await?;
        self.stdin.flush().await?;
        Ok(())
    }
    /// The next non-progress frame's header; payloads are not used in replies.
    async fn reply(&mut self) -> Result<Value> {
        loop {
            let header_len = self.stdout.read_u32_le().await?;
            if !(1..=MAX_HEADER).contains(&header_len) {
                return Err(fail("protocol", "the helper sent an oversized frame"));
            }
            let mut header = vec![0; header_len as usize];
            self.stdout.read_exact(&mut header).await?;
            let payload_len = self.stdout.read_u32_le().await?;
            if payload_len > MAX_PAYLOAD {
                return Err(fail("protocol", "the helper sent an oversized frame"));
            }
            tokio::io::copy(
                &mut (&mut self.stdout).take(payload_len as u64),
                &mut tokio::io::sink(),
            )
            .await?;
            let header: Value = serde_json::from_slice(&header)
                .map_err(|_| fail("protocol", "the helper sent invalid JSON"))?;
            match header["op"].as_str() {
                Some("progress") => continue,
                Some("error") => {
                    return Err(fail(
                        header["code"].as_str().unwrap_or("protocol"),
                        header["message"].as_str().unwrap_or("unknown helper error"),
                    ))
                }
                Some(_) => return Ok(header),
                None => return Err(fail("protocol", "the helper sent a frame without op")),
            }
        }
    }
    async fn request(&mut self, header: Value, payload: &[u8]) -> Result<Value> {
        self.send(&header, payload).await?;
        self.reply().await
    }
    async fn shutdown(mut self) {
        let _ = self.send(&json!({"op":"shutdown"}), &[]).await;
        if tokio::time::timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}

#[derive(Default)]
struct Slot {
    helper: Option<Helper>,
    /// Requests served; the idle timer exits only if none came after it started.
    uses: u64,
}

pub struct OnDevice {
    helper: Option<PathBuf>,
    model_dir: PathBuf,
    idle: Duration,
    slot: Arc<tokio::sync::Mutex<Slot>>,
}
impl OnDevice {
    /// `helper: None` discovers the helper when it is first started.
    pub fn new(helper: Option<PathBuf>, model_dir: PathBuf) -> Self {
        Self {
            helper,
            model_dir,
            idle: IDLE,
            slot: Default::default(),
        }
    }
    async fn start(&self) -> Result<Helper> {
        if !installed(&self.model_dir) {
            return Err(fail(
                "model_missing",
                format!(
                    "the on-device model is not installed; run `xflow models install {MODEL_ID}`"
                ),
            ));
        }
        let path = match &self.helper {
            Some(path) => path.clone(),
            None => helper_path()?,
        };
        let model_dir = self
            .model_dir
            .to_str()
            .ok_or_else(|| anyhow!("the model directory path is not UTF-8"))?;
        let mut child = command(&path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow!("could not start the on-device speech helper: {e}"))?;
        let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
        let mut helper = Helper {
            child,
            stdin: stdin.ok_or_else(|| anyhow!("helper stdin unavailable"))?,
            stdout: BufReader::new(stdout.ok_or_else(|| anyhow!("helper stdout unavailable"))?),
        };
        let hello = json!({"op":"hello","protocol":1,"model_dir":model_dir});
        let ready = tokio::time::timeout(LOAD_TIMEOUT, helper.request(hello, &[]))
            .await
            .map_err(|_| fail("load_failed", "the model did not load in time"))?
            .map_err(stopped)?;
        if ready["op"] != "ready" || ready["protocol"] != 1 {
            return Err(fail("protocol", "the helper did not answer hello"));
        }
        Ok(helper)
    }
    /// Shut the helper down once it has been idle for `self.idle`.
    fn exit_when_idle(&self, uses: u64) {
        let slot = Arc::downgrade(&self.slot);
        let idle = self.idle;
        tokio::spawn(async move {
            tokio::time::sleep(idle).await;
            let Some(slot) = slot.upgrade() else { return };
            let mut slot = slot.lock().await;
            if slot.uses == uses {
                if let Some(helper) = slot.helper.take() {
                    drop(slot);
                    helper.shutdown().await;
                }
            }
        });
    }
    fn used(&self, slot: &mut Slot, helper: Helper) {
        slot.helper = Some(helper);
        slot.uses += 1;
        self.exit_when_idle(slot.uses);
    }
}
/// A closed pipe means the helper exited or crashed.
fn stopped(error: anyhow::Error) -> anyhow::Error {
    if error.is::<OnDeviceError>() {
        error
    } else {
        fail(
            "helper_stopped",
            "the on-device speech helper stopped unexpectedly",
        )
    }
}
fn pcm(audio: &AudioClip) -> Result<Vec<u8>> {
    let samples = audio::mono16(audio)?;
    if samples.is_empty() || samples.len() > MAX_SAMPLES {
        return Err(fail(
            "bad_audio",
            "audio is empty or longer than 20 minutes",
        ));
    }
    Ok(samples
        .iter()
        .flat_map(|&s| audio::pcm16(s).to_le_bytes())
        .collect())
}

#[async_trait]
impl SpeechToText for OnDevice {
    fn name(&self) -> &str {
        ON_DEVICE
    }
    fn model(&self) -> &str {
        MODEL_ID
    }
    async fn warm(&self) -> Result<()> {
        let mut slot = self.slot.lock().await;
        let helper = match slot.helper.take() {
            Some(helper) => helper,
            None => self.start().await?,
        };
        self.used(&mut slot, helper);
        Ok(())
    }
    async fn transcribe(
        &self,
        audio: AudioClip,
        _options: TranscriptionOptions,
    ) -> Result<Transcript> {
        let seconds = audio.samples.len() as u64
            / (audio.sample_rate.max(1) as u64 * audio.channels.max(1) as u64);
        let payload = tokio::task::spawn_blocking(move || pcm(&audio))
            .await
            .map_err(|_| anyhow!("audio conversion task failed"))??;
        // One request at a time. The helper is owned by this future while it works,
        // so dropping the future (a cancelled dictation) kills it.
        let mut slot = self.slot.lock().await;
        let mut helper = match slot.helper.take() {
            Some(helper) => helper,
            None => self.start().await?,
        };
        let id = slot.uses;
        let request = json!({"op":"transcribe","id":id,"sample_rate":16000});
        let reply = tokio::time::timeout(
            Duration::from_secs(60 + seconds),
            helper.request(request, &payload),
        )
        .await
        .map_err(|_| fail("inference_failed", "transcription timed out"))?;
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                // These two leave the helper running and ready (PROTOCOL.md).
                if error
                    .downcast_ref::<OnDeviceError>()
                    .is_some_and(|e| e.code == "bad_audio" || e.code == "inference_failed")
                {
                    self.used(&mut slot, helper);
                }
                return Err(stopped(error));
            }
        };
        if reply["op"] != "result" || reply["id"] != id {
            return Err(fail("protocol", "the helper answered a different request"));
        }
        let text = reply["text"]
            .as_str()
            .ok_or_else(|| fail("protocol", "the helper result has no text"))?
            .to_owned();
        self.used(&mut slot, helper);
        Ok(Transcript {
            provider: None,
            text,
            language: None,
        })
    }
}

/// Rejected before any provider is built: on-device mode has no cloud route.
pub fn validate(config: &SttConfig) -> Result<()> {
    if !config.fallback.is_empty() {
        bail!("on-device speech has no cloud backup; unset stt.fallback");
    }
    if config.language.is_some() || !config.languages.is_empty() {
        bail!("on-device speech detects the language itself; unset stt.language and stt.languages");
    }
    Ok(())
}
pub fn build() -> Result<Arc<dyn SpeechToText>> {
    if !SUPPORTED {
        bail!(UNSUPPORTED);
    }
    Ok(Arc::new(OnDevice::new(None, model_dir()?)))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const FAKE: &str = include_str!("../tests/fake_stt_helper.py");

    struct Fake {
        dir: tempfile::TempDir,
    }
    impl Fake {
        /// A helper script running the fake in `mode`; it logs to `dir/log`.
        fn new(mode: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let python = [
                "/usr/bin/python3",
                "/opt/homebrew/bin/python3",
                "/usr/local/bin/python3",
            ]
            .into_iter()
            .find(|p| Path::new(p).is_file())
            .expect("python3 for the fake helper");
            std::fs::write(dir.path().join("fake.py"), FAKE).unwrap();
            let script = dir.path().join("helper");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\nexec {python} {}/fake.py {mode} {}\n",
                    dir.path().display(),
                    dir.path().display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let model = dir.path().join("model");
            std::fs::create_dir(&model).unwrap();
            std::fs::write(model.join(INSTALLED_MARKER), "").unwrap();
            Self { dir }
        }
        fn engine(&self) -> OnDevice {
            OnDevice::new(
                Some(self.dir.path().join("helper")),
                self.dir.path().join("model"),
            )
        }
        fn log(&self) -> String {
            std::fs::read_to_string(self.dir.path().join("log")).unwrap_or_default()
        }
        fn pid(&self) -> i32 {
            self.log()
                .lines()
                .filter_map(|l| l.strip_prefix("pid "))
                .next_back()
                .unwrap()
                .parse()
                .unwrap()
        }
    }
    fn clip(seconds: f32) -> AudioClip {
        AudioClip {
            samples: vec![0.25; (16_000.0 * seconds) as usize],
            sample_rate: 16_000,
            channels: 1,
        }
    }
    fn code(error: &anyhow::Error) -> String {
        error
            .downcast_ref::<OnDeviceError>()
            .map(|e| e.code.clone())
            .unwrap_or_else(|| format!("not typed: {error:#}"))
    }
    async fn dead(pid: i32) -> bool {
        for _ in 0..200 {
            let mut status = 0;
            // Our own child: reaped here once killed (also proves it is gone).
            let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if reaped == pid || reaped == -1 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[tokio::test]
    async fn hello_progress_and_transcribe_send_i16_pcm_and_the_model_dir() {
        let fake = Fake::new("ok");
        let engine = fake.engine();
        let text = engine
            .transcribe(clip(0.5), Default::default())
            .await
            .unwrap()
            .text;
        // The fake answers with the PCM byte count and the first sample.
        assert_eq!(text, "bytes=16000 first=8192");
        let log = fake.log();
        assert!(log.contains(&format!(
            "hello {}",
            fake.dir.path().join("model").display()
        )));
        // Second request reuses the same helper.
        engine
            .transcribe(clip(0.1), Default::default())
            .await
            .unwrap();
        assert_eq!(fake.log().matches("pid ").count(), 1);
    }
    #[tokio::test]
    async fn crash_mid_request_is_typed_and_the_next_request_restarts_once() {
        let fake = Fake::new("crash_first");
        let engine = fake.engine();
        let error = engine
            .transcribe(clip(0.1), Default::default())
            .await
            .unwrap_err();
        assert_eq!(code(&error), "helper_stopped");
        assert!(engine
            .transcribe(clip(0.1), Default::default())
            .await
            .is_ok());
        assert_eq!(fake.log().matches("pid ").count(), 2);
    }

    #[tokio::test]
    async fn protocol_error_and_oversize_frames_are_rejected() {
        for (mode, expected) in [
            ("bad_json", "protocol"),
            ("oversize", "protocol"),
            ("load_error", "model_invalid"),
        ] {
            let fake = Fake::new(mode);
            let error = fake
                .engine()
                .transcribe(clip(0.1), Default::default())
                .await
                .unwrap_err();
            assert_eq!(code(&error), expected, "{mode}");
        }
    }

    #[tokio::test]
    async fn bad_audio_keeps_the_helper_running() {
        let fake = Fake::new("bad_audio_first");
        let engine = fake.engine();
        let error = engine
            .transcribe(clip(0.1), Default::default())
            .await
            .unwrap_err();
        assert_eq!(code(&error), "bad_audio");
        engine
            .transcribe(clip(0.1), Default::default())
            .await
            .unwrap();
        assert_eq!(fake.log().matches("pid ").count(), 1);
    }

    #[tokio::test]
    async fn idle_helper_is_shut_down() {
        let fake = Fake::new("ok");
        let mut engine = fake.engine();
        engine.idle = Duration::from_millis(200);
        engine.warm().await.unwrap();
        let pid = fake.pid();
        assert!(dead(pid).await);
        assert!(fake.log().contains("shutdown"));
        assert!(engine.slot.lock().await.helper.is_none());
        // Starts again on the next request.
        engine
            .transcribe(clip(0.1), Default::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn dropping_an_in_flight_request_kills_the_helper() {
        let fake = Fake::new("hang");
        let engine = Arc::new(fake.engine());
        let task = tokio::spawn({
            let engine = engine.clone();
            async move { engine.transcribe(clip(0.1), Default::default()).await }
        });
        for _ in 0..200 {
            if fake.log().contains("transcribe") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(fake.log().contains("transcribe"));
        task.abort();
        let _ = task.await;
        assert!(dead(fake.pid()).await);
        assert!(engine.slot.lock().await.helper.is_none());
    }

    #[tokio::test]
    async fn missing_model_marker_never_starts_the_helper() {
        let fake = Fake::new("ok");
        std::fs::remove_file(fake.dir.path().join("model").join(INSTALLED_MARKER)).unwrap();
        let error = fake
            .engine()
            .transcribe(clip(0.1), Default::default())
            .await
            .unwrap_err();
        assert_eq!(code(&error), "model_missing");
        assert!(fake.log().is_empty());
    }

    #[tokio::test]
    async fn helper_environment_has_no_api_keys() {
        let fake = Fake::new("ok");
        let command = command(&fake.dir.path().join("helper"));
        let names: Vec<_> = command
            .as_std()
            .get_envs()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|n| n == "HOME" || n == "TMPDIR"),
            "{names:?}"
        );
        // The fake also reports every variable name it received.
        fake.engine().warm().await.unwrap();
        let env = fake.log();
        let env = env.lines().find(|l| l.starts_with("env ")).unwrap();
        assert!(!env.contains("KEY") && !env.contains("TOKEN"), "{env}");
    }

    #[test]
    fn on_device_rejects_cloud_routes_and_fixed_languages() {
        let mut config = SttConfig {
            provider: ON_DEVICE.into(),
            ..SttConfig::default()
        };
        assert!(validate(&config).is_ok());
        config.fallback = vec!["groq".into()];
        assert!(validate(&config).is_err());
        config.fallback.clear();
        config.languages = vec!["en".into()];
        assert!(validate(&config).is_err());
    }
}
