//! Real processes and real SIGKILLs against the production journal: the
//! `xflow` binary, its detached meeting worker and a loopback stub provider.
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

struct Stub {
    url: String,
    requests: Arc<AtomicUsize>,
}
/// Text is a hash of the uploaded audio, so a window sent twice gives the same
/// text and a duplicate accepted segment would be visible.
fn stub(delay: Duration) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/audio/transcriptions",
        listener.local_addr().unwrap()
    );
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { return };
            let count = count.clone();
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let mut buf = [0u8; 65536];
                loop {
                    let Ok(n) = socket.read(&mut buf) else { return };
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&bytes[..at]).to_ascii_lowercase();
                        let size: usize = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        if bytes.len() >= at + 4 + size {
                            break;
                        }
                    }
                }
                count.fetch_add(1, Ordering::SeqCst);
                let riff = bytes.windows(4).position(|w| w == b"RIFF").unwrap_or(0);
                let end = bytes
                    .windows(4)
                    .rposition(|w| w == b"\r\n--")
                    .unwrap_or(bytes.len());
                let text = format!(
                    "w{:016x}",
                    xflow_core::stable_hash(&bytes[riff..end.max(riff)])
                );
                std::thread::sleep(delay);
                let body = format!("{{\"text\":\"{text}\"}}");
                let _ = socket.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes(),
                );
            });
        }
    });
    Stub { url, requests }
}

struct Sandbox {
    root: tempfile::TempDir,
}
impl Sandbox {
    fn new(stub: &Stub) -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config/xflow");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            format!("[stt]\nprovider = \"custom\"\nmodel = \"stub\"\nendpoint = {:?}\ntimeout_secs = 5\n[privacy]\noffline = true\n", stub.url),
        )
        .unwrap();
        let run = root.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { root }
    }
    fn path(&self) -> &Path {
        self.root.path()
    }
    fn db(&self) -> PathBuf {
        self.path().join("data/xflow/meetings.db")
    }
    fn spool(&self) -> PathBuf {
        self.path().join("data/xflow/meeting-audio")
    }
    fn xflow(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xflow"));
        command
            .env_clear()
            .env("HOME", self.path())
            .env("XDG_DATA_HOME", self.path().join("data"))
            .env("XDG_CONFIG_HOME", self.path().join("config"))
            .env("XDG_RUNTIME_DIR", self.path().join("run"))
            .env("NO_COLOR", "1")
            .env("PATH", "/usr/bin:/bin")
            .args(args)
            .current_dir(self.path());
        command
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        self.xflow(args).output().unwrap()
    }
    /// Writes `seconds` of nonrepeating 16 kHz mono audio.
    fn wav(&self, name: &str, seconds: u32, seed: u64) -> String {
        let path = self.path().join(name);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        for _ in 0..seconds * 16_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            writer.write_sample(((x >> 48) as i16) / 4).unwrap();
        }
        writer.finalize().unwrap();
        path.to_string_lossy().into_owned()
    }
    fn ro(&self) -> Connection {
        Connection::open_with_flags(self.db(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }
    fn n(&self, sql: &str) -> i64 {
        self.ro().query_row(sql, [], |r| r.get(0)).unwrap()
    }
    fn worker_running(&self) -> bool {
        let Ok(file) = std::fs::File::open(self.spool().join("worker.lock")) else {
            return false;
        };
        match fs2::FileExt::try_lock_shared(&file) {
            Ok(()) => {
                let _ = fs2::FileExt::unlock(&file);
                false
            }
            Err(_) => true,
        }
    }
    /// The running worker's pid, only if it really is our meeting worker.
    fn worker_pid(&self) -> Option<i32> {
        if !self.worker_running() {
            return None;
        }
        let pid: i64 = self
            .ro()
            .query_row("SELECT pid FROM worker", [], |r| r.get(0))
            .ok()?;
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        String::from_utf8_lossy(&cmdline)
            .contains("meeting-worker")
            .then_some(pid as i32)
    }
    fn wait_idle(&self, limit: Duration) {
        let start = Instant::now();
        // Give a just-spawned worker time to take its lock.
        std::thread::sleep(Duration::from_millis(300));
        while self.worker_running() {
            assert!(
                start.elapsed() < limit,
                "worker still running after {limit:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Acknowledged pieces: (meeting, source, seq) -> hash.
type Acked = BTreeMap<(i64, i64, i64), String>;

/// Checks every journal invariant and returns the acknowledged pieces now on disk.
fn invariants(sandbox: &Sandbox, before: &Acked, deleted: &[i64]) -> Acked {
    let db = sandbox.ro();
    let store: String = db
        .query_row("SELECT store_id FROM metadata", [], |r| r.get(0))
        .unwrap();
    let mut now = Acked::new();
    let mut statement = db
        .prepare("SELECT meeting_id,source_idx,seq,samples,hash,state FROM pieces")
        .unwrap();
    let rows: Vec<(i64, i64, i64, i64, String, String)> = statement
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for (meeting, source, seq, samples, hash, state) in rows {
        if state == "ready" {
            let path = sandbox
                .spool()
                .join(format!("{store}-{meeting}"))
                .join(format!("s{source}-{seq:06}.pcm"));
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|_| panic!("acknowledged piece lost: {path:?}"));
            assert_eq!(bytes.len() as i64, 88 + samples * 2, "{path:?}");
            assert_eq!(
                format!("{:016x}", xflow_core::stable_hash(&bytes[88..])),
                hash,
                "{path:?}"
            );
        }
        now.insert((meeting, source, seq), hash);
    }
    for (key @ (meeting, source, seq), hash) in before {
        if deleted.contains(meeting) {
            continue;
        }
        match now.get(key) {
            Some(h) => assert_eq!(h, hash, "piece {key:?} changed"),
            None => {
                // Released only after every job that reads it was accepted.
                let (start, samples) = (seq * 80_000, 80_000);
                let open: i64 = db
                    .query_row(
                        "SELECT count(*) FROM jobs WHERE meeting_id=?1 AND source_idx=?2 AND overlap_sample<?3 AND end_sample>?4 AND state NOT IN ('accepted','abandoned')",
                        rusqlite::params![meeting, source, start + samples, start],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(
                    open, 0,
                    "piece {key:?} deleted before its jobs were accepted"
                );
                let covered: i64 = db
                    .query_row(
                        "SELECT count(*) FROM jobs WHERE meeting_id=?1 AND source_idx=?2 AND overlap_sample<?3 AND end_sample>?4",
                        rusqlite::params![meeting, source, start + samples, start],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert!(covered > 0, "piece {key:?} vanished without a job");
            }
        }
    }
    let duplicates: i64 = db
        .query_row(
            "SELECT count(*) FROM (SELECT 1 FROM segments GROUP BY meeting_id,text HAVING count(*)>1)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(duplicates, 0, "an accepted segment was stored twice");
    let mixed: i64 = db
        .query_row(
            "SELECT count(*) FROM (SELECT job_id FROM segment_provenance GROUP BY job_id HAVING count(DISTINCT attempt_id)>1)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mixed, 0);
    for meeting in deleted {
        for table in [
            "meetings WHERE id",
            "segments WHERE meeting_id",
            "pieces WHERE meeting_id",
            "jobs WHERE meeting_id",
        ] {
            let n: i64 = db
                .query_row(
                    &format!("SELECT count(*) FROM {table}=?1"),
                    [meeting],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0, "deleted meeting {meeting} came back in {table}");
        }
        assert!(!sandbox.spool().join(format!("{store}-{meeting}")).exists());
    }
    now
}

/// Everything queued finished: one segment per window, no audio left.
fn complete(sandbox: &Sandbox) {
    assert_eq!(
        sandbox.n("SELECT count(*) FROM jobs WHERE state NOT IN ('accepted','abandoned')"),
        0
    );
    assert_eq!(
        sandbox.n("SELECT count(*) FROM segments"),
        sandbox.n("SELECT count(*) FROM jobs WHERE state='accepted'")
    );
    assert_eq!(sandbox.n("SELECT count(*) FROM pieces"), 0);
    let files = std::fs::read_dir(sandbox.spool())
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .flat_map(|d| std::fs::read_dir(d.path()).unwrap().flatten())
        .filter(|f| f.file_name().to_string_lossy().contains(".pcm"))
        .count();
    assert_eq!(files, 0, "audio left after acceptance");
}
fn kill9(pid: i32) {
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}
fn session_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit(')')
        .next()?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()
}

#[test]
fn worker_survives_its_cli_terminal_and_sighup() {
    let stub = stub(Duration::from_millis(400));
    let sandbox = Sandbox::new(&stub);
    let audio = sandbox.wav("call.wav", 180, 1);
    // A real pseudo-terminal as the CLI's controlling terminal.
    let (master, slave) = unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master >= 0);
        assert_eq!(libc::grantpt(master), 0);
        assert_eq!(libc::unlockpt(master), 0);
        let mut name = [0 as libc::c_char; 128];
        assert_eq!(libc::ptsname_r(master, name.as_mut_ptr(), name.len()), 0);
        let path = std::ffi::CStr::from_ptr(name.as_ptr())
            .to_string_lossy()
            .into_owned();
        (
            master,
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap(),
        )
    };
    let mut command = sandbox.xflow(&["meeting", "process", &audio, "--wait"]);
    command
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut cli = command.spawn().unwrap();
    let start = Instant::now();
    while sandbox.worker_pid().is_none()
        || sandbox.n("SELECT count(*) FROM jobs WHERE state='accepted'") < 1
    {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "worker never started"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let worker = sandbox.worker_pid().unwrap();
    let cli_pid = cli.id() as i32;
    assert_ne!(
        session_of(worker),
        session_of(cli_pid),
        "worker has its own session"
    );
    assert_eq!(session_of(worker), Some(worker), "worker leads its session");
    kill9(cli_pid);
    cli.wait().unwrap();
    unsafe {
        libc::killpg(cli_pid, libc::SIGHUP);
        libc::close(master);
    }
    let done_before = sandbox.n("SELECT count(*) FROM jobs WHERE state='accepted'");
    assert!(done_before < 6, "the kill happened mid-transcription");
    sandbox.wait_idle(Duration::from_secs(60));
    complete(&sandbox);
    assert_eq!(sandbox.n("SELECT count(*) FROM segments"), 6);
    invariants(&sandbox, &Acked::new(), &[]);
    let log = std::fs::read_to_string(sandbox.spool().join("worker.log")).unwrap();
    assert!(!log.contains("\"w"), "log has no transcript text");
    assert_eq!(
        std::fs::metadata(sandbox.spool().join("worker.log"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn killed_worker_is_resumed_by_status_without_loss_or_duplicates() {
    let stub = stub(Duration::from_millis(300));
    let sandbox = Sandbox::new(&stub);
    let a = sandbox.wav("a.wav", 180, 2);
    let b = sandbox.wav("b.wav", 120, 3);
    let out = sandbox.run(&["meeting", "process", &a, &b]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let start = Instant::now();
    while sandbox.n("SELECT count(*) FROM jobs WHERE state='accepted'") < 2 {
        assert!(start.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(20));
    }
    let acked = invariants(&sandbox, &Acked::new(), &[]);
    kill9(sandbox.worker_pid().expect("worker running"));
    std::thread::sleep(Duration::from_millis(200));
    assert!(!sandbox.worker_running(), "the kernel released the lock");
    let acked = invariants(&sandbox, &acked, &[]);
    // A second start while one runs is a no-op.
    let status = sandbox.run(&["meeting", "status"]);
    assert!(status.status.success());
    let again = sandbox.run(&["meeting", "status", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(json["worker"]["running"], true);
    sandbox.wait_idle(Duration::from_secs(60));
    invariants(&sandbox, &acked, &[]);
    complete(&sandbox);
    assert_eq!(sandbox.n("SELECT count(*) FROM segments"), 10);
    let log = std::fs::read_to_string(sandbox.spool().join("worker.log")).unwrap();
    assert!(log.matches("meeting-worker[").count() >= 2);
}

#[cfg(feature = "test-support")]
#[test]
fn crash_at_every_durability_boundary_loses_nothing_acknowledged() {
    let stub = stub(Duration::ZERO);
    for point in [
        "piece:open",
        "piece:half",
        "piece:written",
        "piece:synced",
        "piece:renamed",
        "piece:dirsynced",
        "piece:committed",
        "result:reserved",
        "result:received",
        "result:committed",
        "result:unlinked",
    ] {
        let sandbox = Sandbox::new(&stub);
        let audio = sandbox.wav("a.wav", 70, 4);
        let crashed = sandbox
            .xflow(&["meeting", "process", &audio])
            .env("XFLOW_MEETING_FAULT", format!("{point}=2=kill"))
            .output()
            .unwrap();
        sandbox.wait_idle(Duration::from_secs(30));
        let piece = point.starts_with("piece");
        if piece {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(crashed.status.signal(), Some(9), "{point}: CLI was killed");
        }
        let acked = invariants(&sandbox, &Acked::new(), &[]);
        let durable: i64 = sandbox.n("SELECT COALESCE(MAX(start_sample+samples),0) FROM pieces");
        // Restart with no fault: `retry` (capture crash) or `status` (worker crash).
        let resumed = if piece {
            sandbox.run(&["meeting", "retry", "--wait"])
        } else {
            sandbox.run(&["meeting", "status"])
        };
        assert!(
            resumed.status.success(),
            "{point}: {}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        sandbox.wait_idle(Duration::from_secs(30));
        invariants(&sandbox, &acked, &[]);
        complete(&sandbox);
        let covered = sandbox.n("SELECT next_sample FROM capture_sources");
        assert!(covered >= durable, "{point}: acknowledged audio lost");
        let torn = sandbox.n("SELECT count(*) FROM gaps WHERE json LIKE '%torn%'");
        match point {
            "piece:half" | "piece:written" | "piece:synced" => assert_eq!(torn, 1, "{point}"),
            _ => assert_eq!(torn, 0, "{point}"),
        }
        if matches!(point, "piece:renamed" | "piece:dirsynced") {
            // The renamed orphan was validated and kept: 2 pieces durable.
            assert_eq!(covered, 160_000, "{point}");
        }
        if !piece {
            assert_eq!(sandbox.n("SELECT count(*) FROM segments"), 3, "{point}");
            assert_eq!(sandbox.n("SELECT state='captured' FROM captures"), 1);
        }
    }
}

#[cfg(feature = "test-support")]
#[test]
fn disk_and_permission_errors_stop_visibly_and_keep_audio() {
    let stub = stub(Duration::ZERO);
    for (fault, expect) in [
        ("piece:written=3=enospc", "No space left"),
        ("piece:synced=3=eio", "Input/output error"),
        ("piece:open=3=eacces", "Permission denied"),
    ] {
        let sandbox = Sandbox::new(&stub);
        let audio = sandbox.wav("a.wav", 70, 5);
        let out = sandbox
            .xflow(&["meeting", "process", &audio, "--wait"])
            .env("XFLOW_MEETING_FAULT", fault)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{fault}");
        assert!(
            stderr.contains("Capture stopped early") && stderr.contains(expect),
            "{fault}: {stderr}"
        );
        sandbox.wait_idle(Duration::from_secs(30));
        assert_eq!(
            sandbox.n("SELECT state='stopped' FROM captures"),
            1,
            "{fault}"
        );
        // The two saved pieces (10 s) were kept and transcribed; the rest is a known gap.
        assert_eq!(sandbox.n("SELECT count(*) FROM segments"), 1, "{fault}");
        assert_eq!(
            sandbox.n(
                "SELECT count(*) FROM gaps WHERE json LIKE '%\"start_ms\":10000,\"end_ms\":70000%'"
            ),
            1,
            "{fault}"
        );
        complete(&sandbox);
    }
    // EIO on the deferred unlink after a committed result: visible, then finished.
    let sandbox = Sandbox::new(&stub);
    let audio = sandbox.wav("a.wav", 40, 6);
    let out = sandbox
        .xflow(&["meeting", "process", &audio])
        .env("XFLOW_MEETING_FAULT", "result:unlinked=1=eio")
        .output()
        .unwrap();
    assert!(out.status.success());
    sandbox.wait_idle(Duration::from_secs(30));
    let status = sandbox.run(&["meeting", "status", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(json["worker"]["state"], "restarted");
    assert!(
        json["warnings"].to_string().contains("Input/output error"),
        "{json}"
    );
    sandbox.wait_idle(Duration::from_secs(30));
    complete(&sandbox);
    assert_eq!(sandbox.n("SELECT count(*) FROM segments"), 2);
}

/// Seeded random SIGKILLs of the CLI and the worker during import and
/// transcription. After each kill every invariant holds; at the end all
/// audio is transcribed exactly once.
#[test]
fn random_process_kills_lose_and_duplicate_nothing() {
    let stub = stub(Duration::from_millis(15));
    let sandbox = Sandbox::new(&stub);
    let a = sandbox.wav("a.wav", 60, 8);
    let b = sandbox.wav("b.wav", 45, 9);
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut next = |n: u64| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng % n
    };
    let mut acked = Acked::new();
    let mut deleted = Vec::new();
    let (mut kills_cli, mut kills_worker) = (0, 0);
    for trial in 0..30 {
        let args: Vec<&str> = match trial % 3 {
            0 => vec!["meeting", "process", &a, &b],
            1 => vec!["meeting", "status"],
            _ => vec!["meeting", "retry"],
        };
        let mut child: Child = sandbox
            .xflow(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(next(1_500)));
        if child.try_wait().unwrap().is_none() {
            kills_cli += 1;
        }
        kill9(child.id() as i32);
        child.wait().unwrap();
        if let Some(worker) = sandbox.worker_pid() {
            kill9(worker);
            kills_worker += 1;
        }
        std::thread::sleep(Duration::from_millis(100));
        if !sandbox.db().exists() {
            continue;
        }
        if trial == 15 {
            // Delete one meeting outright; it must never come back.
            let victim: Option<i64> = sandbox
                .ro()
                .query_row("SELECT min(id) FROM meetings", [], |r| r.get(0))
                .unwrap();
            if let Some(victim) = victim {
                let store: String = sandbox
                    .ro()
                    .query_row("SELECT store_id FROM metadata", [], |r| r.get(0))
                    .unwrap();
                let out = sandbox.run(&["meeting", "delete", &format!("{store}:{victim}")]);
                assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
                deleted.push(victim);
            }
        }
        acked = invariants(&sandbox, &acked, &deleted);
    }
    let out = sandbox.run(&["meeting", "retry", "--wait"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    sandbox.wait_idle(Duration::from_secs(60));
    invariants(&sandbox, &acked, &deleted);
    complete(&sandbox);
    let meetings = sandbox.n("SELECT count(*) FROM meetings");
    println!(
        "30 trials: {kills_cli} CLI kills mid-run, {kills_worker} worker kills; {meetings} meetings, {} segments, {} windows, {} requests, {} ambiguous attempts, {} interrupted captures, {} gaps",
        sandbox.n("SELECT count(*) FROM segments"),
        sandbox.n("SELECT count(*) FROM jobs"),
        stub.requests.load(Ordering::SeqCst),
        sandbox.n("SELECT count(*) FROM attempts WHERE state='ambiguous'"),
        sandbox.n("SELECT count(*) FROM captures WHERE state='interrupted'"),
        sandbox.n("SELECT count(*) FROM gaps"),
    );
    assert!(kills_worker > 0 && kills_cli > 0);
}
