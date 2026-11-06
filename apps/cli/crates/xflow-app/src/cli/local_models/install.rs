//! `xflow models install|remove`: pinned files, verified by size and SHA-256,
//! staged privately and promoted by one rename. Nothing runs in the background.
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use xflow_providers::on_device::{installed, INSTALLED_MARKER, MODEL_ID};

pub const BASE_URL: &str = "https://huggingface.co";

#[derive(Deserialize)]
pub struct File {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Deserialize)]
pub struct Artifact {
    pub repository: String,
    pub revision: String,
    pub download_bytes: u64,
    pub files: Vec<File>,
}

pub enum Source<'a> {
    /// `{base}/{repository}/resolve/{revision}/{path}`.
    Http(&'a str),
    /// A directory holding the same relative paths (`--from`).
    Dir(&'a Path),
}

fn staging(models: &Path) -> PathBuf {
    models.join(format!(".staging-{MODEL_ID}"))
}

/// Install into `models/<id>`; Ok(false) when it was already installed.
pub async fn install(
    artifact: &Artifact,
    source: Source<'_>,
    models: &Path,
    progress: bool,
) -> Result<bool> {
    let target = models.join(MODEL_ID);
    if installed(&target) {
        return Ok(false);
    }
    let stage = staging(models);
    xflow_app::paths::private_dir(models)?;
    xflow_app::paths::private_dir(&stage)?;
    let staged: u64 = artifact
        .files
        .iter()
        .filter_map(|f| std::fs::metadata(stage.join(&f.path)).ok())
        .map(|m| m.len())
        .sum();
    let need = (2 * artifact.download_bytes + (1 << 30)).saturating_sub(staged);
    if let Some(free) = free_bytes(&stage) {
        if free < need {
            bail!(
                "not enough disk space: {free} bytes free, {need} needed (twice the {} byte model plus 1 GiB)",
                artifact.download_bytes
            );
        }
    }
    let client = match source {
        Source::Http(base) => Some(
            reqwest::Client::builder()
                .https_only(base.starts_with("https://"))
                .connect_timeout(Duration::from_secs(30))
                .read_timeout(Duration::from_secs(60))
                .user_agent(concat!("xflow/", env!("CARGO_PKG_VERSION")))
                .build()?,
        ),
        Source::Dir(_) => None,
    };
    let mut meter = Meter::new(artifact.download_bytes, progress);
    for file in &artifact.files {
        if file.path.starts_with('/')
            || file.path.contains('\\')
            || file
                .path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            bail!("unsafe model file path {}", file.path);
        }
        let dest = stage.join(&file.path);
        if verify(&dest, file)? {
            meter.add(file.bytes);
            continue;
        }
        std::fs::create_dir_all(dest.parent().context("model file has no parent")?)?;
        let partial = stage.join(format!("{}.partial", file.path));
        let mut ok = false;
        // A second pass starts the file over: the bytes changed or were corrupt.
        for _ in 0..2 {
            let before = meter.done;
            match (&source, &client) {
                (Source::Http(base), Some(client)) => {
                    let url = format!(
                        "{base}/{}/resolve/{}/{}",
                        artifact.repository, artifact.revision, file.path
                    );
                    download(client, &url, &partial, file, &mut meter).await?;
                }
                (Source::Dir(dir), _) => {
                    std::fs::copy(dir.join(&file.path), &partial).with_context(|| {
                        format!("cannot copy {} from {}", file.path, dir.display())
                    })?;
                    meter.add(file.bytes);
                }
                _ => unreachable!(),
            }
            if verify(&partial, file)? {
                ok = true;
                break;
            }
            std::fs::remove_file(&partial)?;
            meter.done = before;
        }
        if !ok {
            bail!(
                "{} failed verification (size or SHA-256 differs from the pinned file); nothing was installed",
                file.path
            );
        }
        std::fs::rename(&partial, &dest)?;
    }
    meter.finish();
    // An unmarked directory is an interrupted earlier promotion; it was never used.
    remove_path(&target)?;
    std::fs::rename(&stage, &target)?;
    let mut marker = std::fs::File::create(target.join(INSTALLED_MARKER))?;
    writeln!(marker, "{}@{}", artifact.repository, artifact.revision)?;
    marker.sync_all()?;
    Ok(true)
}

async fn download(
    client: &reqwest::Client,
    url: &str,
    partial: &Path,
    file: &File,
    meter: &mut Meter,
) -> Result<()> {
    let mut have = std::fs::metadata(partial).map(|m| m.len()).unwrap_or(0);
    if have > file.bytes {
        have = 0;
    }
    meter.add(have);
    if have == file.bytes {
        return Ok(());
    }
    let mut request = client.get(url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut response = request
        .send()
        .await
        .with_context(|| format!("download of {} failed", file.path))?;
    let resume = match response.status().as_u16() {
        206 => {
            // Resume only when the server continues exactly where the partial file ends.
            let expected = format!("bytes {have}-{}/{}", file.bytes - 1, file.bytes);
            response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .is_some_and(|v| v.as_bytes() == expected.as_bytes())
        }
        200 => false,
        status => bail!("download of {} failed: HTTP {status}", file.path),
    };
    if !resume {
        meter.done -= have;
        if response.status().as_u16() == 206 {
            // A different range or a changed file: start this file over.
            std::fs::remove_file(partial)?;
            return Box::pin(download(client, url, partial, file, meter)).await;
        }
    }
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resume)
        .truncate(!resume)
        .open(partial)?;
    let mut written = if resume { have } else { 0 };
    while let Some(chunk) = response.chunk().await.with_context(|| {
        format!(
            "download of {} was interrupted; run the command again to resume",
            file.path
        )
    })? {
        written += chunk.len() as u64;
        if written > file.bytes {
            bail!("{} is larger than the pinned file", file.path);
        }
        out.write_all(&chunk)?;
        meter.add(chunk.len() as u64);
    }
    out.sync_all()?;
    Ok(())
}

fn verify(path: &Path, file: &File) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(m) if m.len() == file.bytes => (),
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    }
    let mut reader = std::fs::File::open(path)?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0; 1 << 20];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        context.update(&buffer[..n]);
    }
    let hex: String = context
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(hex.eq_ignore_ascii_case(&file.sha256))
}

/// Remove only the managed model directory and its staging. Ok(false) when neither existed.
pub fn remove(models: &Path) -> Result<bool> {
    let a = remove_path(&models.join(MODEL_ID))?;
    let b = remove_path(&staging(models))?;
    Ok(a || b)
}
fn remove_path(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path)?,
        // A symlink is removed itself, never followed.
        Ok(_) => std::fs::remove_file(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    }
    Ok(true)
}

#[cfg(unix)]
fn free_bytes(path: &Path) -> Option<u64> {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: path is NUL terminated; stat is read only after statvfs succeeds.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::useless_conversion)]
    u64::from(stat.f_bavail).checked_mul(u64::from(stat.f_frsize))
}
#[cfg(not(unix))]
fn free_bytes(_: &Path) -> Option<u64> {
    None
}

struct Meter {
    total: u64,
    done: u64,
    started: Instant,
    shown: Option<Instant>,
    enabled: bool,
}
impl Meter {
    fn new(total: u64, enabled: bool) -> Self {
        Self {
            total,
            done: 0,
            started: Instant::now(),
            shown: None,
            enabled,
        }
    }
    fn add(&mut self, bytes: u64) {
        self.done += bytes;
        if self.enabled
            && self
                .shown
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(250))
        {
            self.shown = Some(Instant::now());
            self.print();
        }
    }
    fn print(&self) {
        let rate = self.done as f64 / self.started.elapsed().as_secs_f64().max(0.001) / 1e6;
        eprint!(
            "\r{MODEL_ID}: {} / {} bytes ({rate:.1} MB/s)   ",
            self.done, self.total
        );
    }
    fn finish(&self) {
        if self.enabled {
            self.print();
            eprintln!();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn sha(bytes: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    fn content() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            (
                "A.mlmodelc/weights/weight.bin",
                (0..50_000u32).map(|i| (i % 251) as u8).collect(),
            ),
            ("vocab.json", b"{\"a\":1}".to_vec()),
        ]
    }
    fn artifact() -> Artifact {
        let files: Vec<File> = content()
            .into_iter()
            .map(|(path, bytes)| File {
                path: path.into(),
                bytes: bytes.len() as u64,
                sha256: sha(&bytes),
            })
            .collect();
        Artifact {
            repository: "org/repo".into(),
            revision: "abc".into(),
            download_bytes: files.iter().map(|f| f.bytes).sum(),
            files,
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Normal,
        IgnoreRange,
        /// Close the first response for the weights after this many body bytes.
        CutOnce(usize),
        /// Serve other bytes of the same length.
        Changed,
        /// Send a shorter body than the pinned size.
        Truncated,
    }
    type Requests = Vec<(String, Option<String>)>;
    struct Server {
        url: String,
        /// (path, Range header) per request.
        requests: Arc<Mutex<Requests>>,
    }
    async fn server(mode: Mode) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        tokio::spawn(async move {
            let mut cut = matches!(mode, Mode::CutOnce(_));
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read(&mut byte).await.unwrap() == 0 {
                        break;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8(head).unwrap();
                let target = head.split_whitespace().nth(1).unwrap().to_owned();
                let path = target.split("/resolve/abc/").nth(1).unwrap().to_owned();
                let range = head.lines().find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("range: bytes=")
                        .map(|r| r.trim_end_matches('-').to_owned())
                });
                log.lock().unwrap().push((path.clone(), range.clone()));
                let mut body = content().into_iter().find(|(p, _)| *p == path).unwrap().1;
                if mode == Mode::Changed {
                    body.iter_mut().for_each(|b| *b = b.wrapping_add(1));
                }
                if mode == Mode::Truncated {
                    body.truncate(body.len() / 2);
                }
                let total = body.len();
                let (status, extra, body) = match range.and_then(|r| r.parse::<usize>().ok()) {
                    Some(start) if mode != Mode::IgnoreRange => (
                        "206 Partial Content",
                        format!("Content-Range: bytes {start}-{}/{total}\r\n", total - 1),
                        body[start..].to_vec(),
                    ),
                    _ => ("200 OK", String::new(), body),
                };
                let mut response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                match mode {
                    Mode::CutOnce(n) if cut && path.ends_with("weight.bin") => {
                        cut = false;
                        response.extend_from_slice(&body[..n]);
                    }
                    _ => response.extend_from_slice(&body),
                }
                let _ = socket.write_all(&response).await;
                let _ = socket.shutdown().await;
            }
        });
        Server { url, requests }
    }
    fn weights(models: &Path) -> Vec<u8> {
        std::fs::read(models.join(MODEL_ID).join("A.mlmodelc/weights/weight.bin")).unwrap()
    }
    fn partial(models: &Path) -> PathBuf {
        staging(models).join("A.mlmodelc/weights/weight.bin.partial")
    }
    fn seed_partial(models: &Path, bytes: &[u8]) {
        let p = partial(models);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    #[tokio::test]
    async fn fresh_download_verifies_promotes_and_marks_last() {
        let root = tempfile::tempdir().unwrap();
        let models = root.path().join("models");
        let server = server(Mode::Normal).await;
        assert!(
            install(&artifact(), Source::Http(&server.url), &models, false)
                .await
                .unwrap()
        );
        assert_eq!(weights(&models), content()[0].1);
        assert!(installed(&models.join(MODEL_ID)));
        assert!(!staging(&models).exists());
        // Already installed: no request at all.
        let before = server.requests.lock().unwrap().len();
        assert!(
            !install(&artifact(), Source::Http(&server.url), &models, false)
                .await
                .unwrap()
        );
        assert_eq!(server.requests.lock().unwrap().len(), before);
    }

    #[tokio::test]
    async fn resume_uses_range_and_a_server_ignoring_it_restarts_the_file() {
        for mode in [Mode::Normal, Mode::IgnoreRange] {
            let root = tempfile::tempdir().unwrap();
            let models = root.path().join("models");
            seed_partial(&models, &content()[0].1[..1000]);
            let server = server(mode).await;
            install(&artifact(), Source::Http(&server.url), &models, false)
                .await
                .unwrap();
            assert_eq!(weights(&models), content()[0].1);
            let requests = server.requests.lock().unwrap();
            assert!(requests
                .iter()
                .any(|(p, r)| p.ends_with("weight.bin") && r.as_deref() == Some("1000")));
        }
    }

    #[tokio::test]
    async fn a_corrupt_partial_is_detected_and_downloaded_again() {
        let root = tempfile::tempdir().unwrap();
        let models = root.path().join("models");
        seed_partial(&models, &[0xff; 1000]);
        let server = server(Mode::Normal).await;
        install(&artifact(), Source::Http(&server.url), &models, false)
            .await
            .unwrap();
        assert_eq!(weights(&models), content()[0].1);
    }

    #[tokio::test]
    async fn changed_or_truncated_bytes_install_nothing() {
        for mode in [Mode::Changed, Mode::Truncated] {
            let root = tempfile::tempdir().unwrap();
            let models = root.path().join("models");
            let server = server(mode).await;
            let error = install(&artifact(), Source::Http(&server.url), &models, false)
                .await
                .unwrap_err();
            assert!(format!("{error:#}").contains("verification"), "{error:#}");
            assert!(!models.join(MODEL_ID).exists());
            assert!(!partial(&models).exists());
        }
    }

    #[tokio::test]
    async fn an_interrupted_download_resumes_on_the_next_run() {
        let root = tempfile::tempdir().unwrap();
        let models = root.path().join("models");
        let server = server(Mode::CutOnce(20_000)).await;
        let error = install(&artifact(), Source::Http(&server.url), &models, false)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("resume"), "{error:#}");
        assert_eq!(std::fs::metadata(partial(&models)).unwrap().len(), 20_000);
        assert!(!models.join(MODEL_ID).exists());
        install(&artifact(), Source::Http(&server.url), &models, false)
            .await
            .unwrap();
        assert_eq!(weights(&models), content()[0].1);
        assert!(server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, r)| r.as_deref() == Some("20000")));
    }

    #[tokio::test]
    async fn from_dir_copies_with_the_same_verification_and_replaces_an_unmarked_dir() {
        let root = tempfile::tempdir().unwrap();
        let models = root.path().join("models");
        let source = root.path().join("source");
        for (path, bytes) in content() {
            std::fs::create_dir_all(source.join(path).parent().unwrap()).unwrap();
            std::fs::write(source.join(path), bytes).unwrap();
        }
        // An unmarked leftover is not an installed model.
        std::fs::create_dir_all(models.join(MODEL_ID)).unwrap();
        std::fs::write(models.join(MODEL_ID).join("stale"), "x").unwrap();
        assert!(!installed(&models.join(MODEL_ID)));
        install(&artifact(), Source::Dir(&source), &models, false)
            .await
            .unwrap();
        assert_eq!(weights(&models), content()[0].1);
        assert!(!models.join(MODEL_ID).join("stale").exists());
        let wrong = tempfile::tempdir().unwrap();
        std::fs::write(source.join("vocab.json"), "{}").unwrap();
        let error = install(
            &artifact(),
            Source::Dir(&source),
            &wrong.path().join("m"),
            false,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("vocab.json"), "{error:#}");
    }

    #[test]
    fn remove_deletes_only_the_model_and_staging() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path();
        let models = data.join("models");
        std::fs::create_dir_all(models.join(MODEL_ID)).unwrap();
        std::fs::create_dir_all(staging(&models)).unwrap();
        for keep in ["keystore.json", "keystore.key", "history.db", "config.toml"] {
            std::fs::write(data.join(keep), "keep").unwrap();
        }
        std::fs::create_dir_all(models.join("other")).unwrap();
        assert!(remove(&models).unwrap());
        assert!(!models.join(MODEL_ID).exists() && !staging(&models).exists());
        for keep in ["keystore.json", "keystore.key", "history.db", "config.toml"] {
            assert_eq!(std::fs::read_to_string(data.join(keep)).unwrap(), "keep");
        }
        assert!(models.join("other").exists());
        assert!(!remove(&models).unwrap());
    }

    #[test]
    fn bundled_artifact_parses() {
        let a = super::super::artifact();
        assert_eq!(
            a.files.iter().map(|f| f.bytes).sum::<u64>(),
            a.download_bytes
        );
        assert_eq!(a.repository, "FluidInference/parakeet-tdt-0.6b-v3-coreml");
    }
}
