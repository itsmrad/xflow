//! CLI reads and writes a scratch meeting database. No daemon is involved.
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::{fs::symlink, fs::PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::Once,
};
use xflow_app::meeting::{
    export_meeting_checking, ContractError, ExportFormat, MeetingReader, MeetingStore,
};
use xflow_core::meeting::{MeetingId, MAX_IMPORT_BYTES};

fn init_sqlite() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        xflow_app::store::skip_bulk_page_cache();
    });
}

struct Sandbox {
    _root: tempfile::TempDir,
    /// Canonical path. macOS `/tmp` is a symlink, and `SQLITE_OPEN_NOFOLLOW` rejects it.
    home: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        init_sqlite();
        let root = tempfile::tempdir().unwrap();
        let home = root.path().canonicalize().unwrap();
        Self { _root: root, home }
    }
    fn home(&self) -> &Path {
        &self.home
    }
    fn db(&self) -> PathBuf {
        self.home().join("data/xflow/meetings.db")
    }
    fn xflow(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xflow"));
        command
            .env_clear()
            .env("HOME", self.home())
            .env("XDG_DATA_HOME", self.home().join("data"))
            .env("XDG_CONFIG_HOME", self.home().join("config"))
            .env("XDG_RUNTIME_DIR", self.home().join("run"))
            .env("NO_COLOR", "1")
            .env("PATH", "/usr/bin:/bin");
        command
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        self.xflow().args(args).output().unwrap()
    }
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/meeting-notes")
            .join(name),
    )
    .unwrap()
}

fn import_at(db: &Path, bytes: &[u8]) -> MeetingId {
    if let Some(parent) = db.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    MeetingStore::at(db.to_path_buf())
        .import_json(bytes)
        .unwrap()
        .meeting
}

fn label(meeting: &MeetingId) -> String {
    format!("{}:{}", meeting.store_id, meeting.id)
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}
fn stderr(output: &std::process::Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn meeting_json(import_id: &str, started: i64, duration: i64, count: usize, text: &str) -> Vec<u8> {
    let segments: Vec<Value> = (0..count)
        .map(|i| {
            json!({
                "source_id": "mic",
                "sequence": i,
                "range": {"start_ms": (i as i64) * 1000, "end_ms": (i as i64) * 1000 + 500},
                "text": text,
            })
        })
        .collect();
    serde_json::to_vec(&json!({
        "schema_version": 1,
        "import_id": import_id,
        "provenance": "imported_unverified",
        "started_at_ms": started,
        "ended_at_ms": started + duration,
        "timezone": "Z",
        "sources": [{"id": "mic", "epoch": 1}],
        "segments": segments,
        "gaps": [],
    }))
    .unwrap()
}

#[test]
fn help_lists_meeting_commands() {
    let box_ = Sandbox::new();
    let output = box_.run(&["meeting", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    for name in ["import", "list", "search", "get", "export", "delete"] {
        assert!(text.contains(name), "{text}");
    }
    assert!(text.contains("do not use the daemon"), "{text}");
    assert!(!text.contains("only writer"), "{text}");
}

#[test]
fn mixed_script_list_search_and_get_keep_provenance() {
    let box_ = Sandbox::new();
    let meeting = import_at(&box_.db(), &fixture("mixed-script.json"));
    let listed = box_.run(&["meeting", "list", "--json"]);
    assert!(listed.status.success(), "{}", stderr(&listed));
    let page: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(page["meetings"][0]["notes"], "absent");
    assert_eq!(page["meetings"][0]["provenance"], "imported_unverified");
    assert_eq!(page["meetings"][0]["meeting"]["store_id"], meeting.store_id);
    assert_eq!(page["meetings"][0]["meeting"]["id"], meeting.id);

    let found = box_.run(&["meeting", "search", "東京"]);
    assert!(found.status.success(), "{}", stderr(&found));
    assert!(stdout(&found).contains("東京"));
    assert!(!stdout(&found).contains("speaker"));
    assert!(!stdout(&found).as_bytes().contains(&0x1b));
    for query in [
        "कि",
        "niño",
        "ação",
        "useEffect",
        "git rebase",
        "kubectl",
        "OAuth",
        "42",
        "José",
        "李明",
        "Søren",
        "محمد",
    ] {
        let output = box_.run(&["meeting", "search", query]);
        assert!(output.status.success(), "{query}: {}", stderr(&output));
        assert!(
            stdout(&output).contains(query),
            "{query} missing in {}",
            stdout(&output)
        );
    }
    let literal = box_.run(&["meeting", "search", "\"NEAR\" AND *"]);
    assert!(literal.status.success(), "{}", stderr(&literal));
    assert!(stdout(&literal).contains("NEAR"));
    let other_case = box_.run(&["meeting", "search", "oauth"]);
    assert!(other_case.status.success(), "{}", stderr(&other_case));
    assert!(
        stdout(&other_case).contains("OAuth"),
        "{}",
        stdout(&other_case)
    );

    let got = box_.run(&["meeting", "get", &label(&meeting)]);
    assert!(got.status.success(), "{}", stderr(&got));
    let text = stdout(&got);
    assert!(text.contains("notes: absent"), "{text}");
    assert!(text.contains("provenance: imported_unverified"), "{text}");
    assert!(text.contains("source silent"), "{text}");
    assert!(text.contains("Conference room"), "{text}");
    assert!(!text.contains("speaker"), "{text}");
    assert!(!text.as_bytes().contains(&0x1b), "{text}");
}

#[test]
fn export_over_64kib_sanitizes_markdown_and_refuses_overwrite() {
    let box_ = Sandbox::new();
    let filler = "x".repeat(3500);
    let bytes = meeting_json("wide", 1_700_000_000_000, 30_000, 20, &filler);
    assert!(bytes.len() > 64 * 1024, "{}", bytes.len());
    let meeting = import_at(&box_.db(), &bytes);
    let dest = box_.home().join("wide.json");
    let output = box_.run(&[
        "meeting",
        "export",
        &label(&meeting),
        "--format",
        "json",
        "--output",
        dest.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let written = fs::read(&dest).unwrap();
    assert!(written.len() > 64 * 1024, "{}", written.len());
    assert_eq!(
        fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let malicious = import_at(&box_.db(), &fixture("mixed-script.json"));
    let md = box_.home().join("notes.md");
    let exported = box_.run(&[
        "meeting",
        "export",
        &label(&malicious),
        "--format",
        "md",
        "--output",
        md.to_str().unwrap(),
    ]);
    assert!(exported.status.success(), "{}", stderr(&exported));
    let markdown = fs::read_to_string(&md).unwrap();
    assert!(
        markdown.contains("notes: absent") || markdown.contains("Notes: absent"),
        "{markdown}"
    );
    assert!(markdown.contains("imported_unverified"));
    assert!(markdown.contains("source silent"));
    assert!(markdown.contains("東京"));
    assert!(!markdown.contains("![boom]"), "{markdown}");
    assert!(!markdown.as_bytes().contains(&0x1b));
    assert!(!markdown.contains("speaker"));
    let stored = fs::read(box_.db()).unwrap();
    assert!(
        stored.windows(4).any(|w| w == b"\x1b[2J"),
        "stored text was rewritten"
    );

    let again = box_.run(&[
        "meeting",
        "export",
        &label(&malicious),
        "--format",
        "md",
        "--output",
        md.to_str().unwrap(),
    ]);
    assert!(!again.status.success(), "{}", stdout(&again));
    assert_eq!(fs::read_to_string(&md).unwrap(), markdown);

    let forced = box_.run(&[
        "meeting",
        "export",
        &label(&malicious),
        "--format",
        "text",
        "--output",
        md.to_str().unwrap(),
        "--force",
    ]);
    assert!(forced.status.success(), "{}", stderr(&forced));
    let text = fs::read_to_string(&md).unwrap();
    assert!(text.contains("notes: absent"));
    assert!(!text.as_bytes().contains(&0x1b));

    let link = box_.home().join("linked.md");
    symlink(&md, &link).unwrap();
    let followed = box_.run(&[
        "meeting",
        "export",
        &label(&malicious),
        "--output",
        link.to_str().unwrap(),
        "--force",
    ]);
    assert!(!followed.status.success(), "{}", stderr(&followed));
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(&md).unwrap(), text);
}

#[test]
fn malformed_and_newer_databases_are_left_unchanged() {
    let box_ = Sandbox::new();
    let db = box_.db();
    let parent = db.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(&db, b"not a database").unwrap();
    fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::read(&db).unwrap();
    let garbage = box_.run(&["meeting", "list"]);
    assert!(!garbage.status.success(), "{}", stdout(&garbage));
    assert_eq!(fs::read(&db).unwrap(), before);

    fs::remove_file(&db).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.pragma_update(None, "user_version", 3).unwrap();
    drop(conn);
    fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
    let newer_bytes = fs::read(&db).unwrap();
    let newer = box_.run(&["meeting", "search", "東京"]);
    assert!(!newer.status.success(), "{}", stdout(&newer));
    assert!(stderr(&newer).contains("newer"), "{}", stderr(&newer));
    assert_eq!(fs::read(&db).unwrap(), newer_bytes);
}

#[test]
fn export_invalidation_writes_nothing() {
    let box_ = Sandbox::new();
    let bytes = meeting_json("paged", 1_700_000_000_000, 25_000, 25, "one line");
    let db = box_.db();
    let meeting = import_at(&db, &bytes);
    let reader = MeetingReader::at(db.clone());
    let page = reader
        .transcript(&xflow_core::meeting::TranscriptQuery {
            meeting: meeting.clone(),
            range: xflow_core::meeting::TimeRange {
                start_ms: 0,
                end_ms: 25_000,
            },
            snapshot: None,
            cursor: None,
        })
        .unwrap();
    let segment = page.segments[0].id;
    let dest = box_.home().join("paged.txt");
    let error = export_meeting_checking(&db, &meeting, ExportFormat::Text, &dest, false, |page| {
        if page == 1 {
            MeetingStore::at(db.clone())
                .correct_segment(&meeting, segment, 1, "edited mid export")
                .unwrap();
        }
    })
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ContractError>(),
        Some(&ContractError::StaleCursor)
    );
    assert!(!dest.exists());
}

#[test]
fn import_and_delete_work_without_a_daemon() {
    let box_ = Sandbox::new();
    let cleaned = json_ok(&box_.run(&["meeting", "delete", "--json", "--retry"]));
    assert_eq!(cleaned["delete_status"], "physically_cleaned");
    assert!(!box_.db().exists());
    let wide = meeting_json("offline", 86_400_000, 30_000, 20, &"y".repeat(3500));
    assert!(wide.len() > 64 * 1024 && wide.len() <= MAX_IMPORT_BYTES);
    let file = box_.home().join("offline.json");
    fs::write(&file, &wide).unwrap();
    let imported = box_.run(&["meeting", "import", "--json", file.to_str().unwrap()]);
    let imported = json_ok(&imported);
    assert_eq!(imported["already_present"], false);
    assert!(box_.db().exists());
    let id = format!(
        "{}:{}",
        imported["meeting"]["store_id"].as_str().unwrap(),
        imported["meeting"]["id"]
    );
    let removed = json_ok(&box_.run(&["meeting", "delete", "--json", &id]));
    assert_eq!(removed["delete_status"], "physically_cleaned");

    let huge = box_.home().join("huge.json");
    fs::write(&huge, vec![b'x'; MAX_IMPORT_BYTES + 1]).unwrap();
    let before = fs::read(box_.db()).unwrap();
    let oversized = box_.run(&["meeting", "import", huge.to_str().unwrap()]);
    assert_eq!(oversized.status.code(), Some(1), "{}", stderr(&oversized));
    assert!(stderr(&oversized).contains("8 MiB"));
    assert_eq!(fs::read(box_.db()).unwrap(), before);
}

fn json_ok(output: &std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "exit {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn calendar_filter_uses_the_supplied_timezone() {
    let box_ = Sandbox::new();
    let bytes = meeting_json("new-year", 86_400_000, 1_000, 1, "second day");
    import_at(&box_.db(), &bytes);
    let included = box_.run(&[
        "meeting",
        "list",
        "--since",
        "1970-01-02",
        "--until",
        "1970-01-03",
        "--timezone",
        "Z",
    ]);
    assert!(included.status.success(), "{}", stderr(&included));
    assert!(
        stdout(&included).contains("86400000"),
        "{}",
        stdout(&included)
    );
    assert!(
        !stdout(&included).contains("No meetings"),
        "{}",
        stdout(&included)
    );

    let excluded = box_.run(&[
        "meeting",
        "list",
        "--since",
        "1970-01-03",
        "--until",
        "1970-01-04",
        "--timezone",
        "Z",
    ]);
    assert!(excluded.status.success(), "{}", stderr(&excluded));
    assert!(
        stdout(&excluded).contains("No meetings"),
        "{}",
        stdout(&excluded)
    );

    let bare = box_.run(&[
        "meeting",
        "list",
        "--since",
        "1970-01-02",
        "--until",
        "1970-01-03",
    ]);
    assert_eq!(bare.status.code(), Some(2), "{}", stderr(&bare));
}

#[test]
fn delete_without_a_target_is_usage() {
    let box_ = Sandbox::new();
    let output = box_.run(&["meeting", "delete"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(!box_.db().exists());
}

fn write_import(box_: &Sandbox, id: &str) -> std::path::PathBuf {
    let path = box_.home().join(format!("{id}.json"));
    fs::write(&path, meeting_json(id, 1_700_000_000_000, 1_000, 1, id)).unwrap();
    path
}

fn spawn(box_: &Sandbox, args: &[&str]) -> std::process::Child {
    box_.xflow()
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

/// Success, or a busy store. Anything else is a failure.
fn finish(child: std::process::Child) -> bool {
    let output = child.wait_with_output().unwrap();
    if output.status.success() {
        serde_json::from_slice::<Value>(&output.stdout).expect("json");
        true
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        assert!(err.contains("busy"), "{err}");
        false
    }
}

fn present(box_: &Sandbox, text: &str) -> bool {
    let output = box_.run(&["meeting", "search", "--json", text]);
    assert!(output.status.success(), "{}", stderr(&output));
    let page: Value = serde_json::from_slice(&output.stdout).unwrap();
    page["segments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|segment| segment["text"].as_str().unwrap().contains(text))
}

fn healthy(box_: &Sandbox) {
    let conn = rusqlite::Connection::open_with_flags(
        box_.db(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    let listed = box_.run(&["meeting", "list", "--json"]);
    assert!(listed.status.success(), "{}", stderr(&listed));
}

#[test]
fn concurrent_imports_create_one_database() {
    let box_ = Sandbox::new();
    let mut children = Vec::new();
    for n in 0..6 {
        let id = format!("race-{n}");
        let file = write_import(&box_, &id);
        children.push((
            id,
            spawn(
                &box_,
                &["meeting", "import", "--json", file.to_str().unwrap()],
            ),
        ));
    }
    for (id, child) in children {
        if finish(child) {
            assert!(present(&box_, &id), "{id} was acknowledged and then lost");
        }
    }
    healthy(&box_);
}

#[test]
fn import_races_a_delete_and_a_busy_writer() {
    let box_ = Sandbox::new();
    let doomed = write_import(&box_, "doomed");
    let imported = json_ok(&box_.run(&["meeting", "import", "--json", doomed.to_str().unwrap()]));
    let id = format!(
        "{}:{}",
        imported["meeting"]["store_id"].as_str().unwrap(),
        imported["meeting"]["id"]
    );
    let arriving = write_import(&box_, "arriving");
    let deleter = spawn(&box_, &["meeting", "delete", "--json", &id]);
    let importer = spawn(
        &box_,
        &["meeting", "import", "--json", arriving.to_str().unwrap()],
    );
    let deleted = finish(deleter);
    let arrived = finish(importer);
    if deleted {
        assert!(!present(&box_, "doomed"));
    }
    if arrived {
        assert!(present(&box_, "arriving"));
    }
    healthy(&box_);

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let held = box_.db();
    let holder = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open_with_flags(
            &held,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        conn.execute_batch("BEGIN IMMEDIATE;").unwrap();
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    ready_rx.recv().unwrap();
    let blocked = write_import(&box_, "blocked");
    let output = box_
        .xflow()
        .args(["meeting", "import", blocked.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(stderr(&output).contains("busy"), "{}", stderr(&output));
    assert!(!present(&box_, "blocked"));
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    assert!(box_
        .run(&["meeting", "import", blocked.to_str().unwrap()])
        .status
        .success());
    assert!(present(&box_, "blocked"));
    healthy(&box_);
}

#[test]
fn delete_with_a_held_snapshot_keeps_a_concurrent_import() {
    let box_ = Sandbox::new();
    let sentinel = "SENTINEL-m1-9f3a2c7e-UNIQUE";
    let file = box_.home().join("sentinel.json");
    fs::write(
        &file,
        meeting_json("sentinel", 1_700_000_000_000, 1_000, 1, sentinel),
    )
    .unwrap();
    let imported = json_ok(&box_.run(&["meeting", "import", "--json", file.to_str().unwrap()]));
    let id = format!(
        "{}:{}",
        imported["meeting"]["store_id"].as_str().unwrap(),
        imported["meeting"]["id"]
    );
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let held = box_.db();
    let holder = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open_with_flags(
            &held,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        conn.execute_batch("BEGIN; SELECT count(*) FROM segments;")
            .unwrap();
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    ready_rx.recv().unwrap();
    let extra = write_import(&box_, "during-hold");
    let deleter = spawn(&box_, &["meeting", "delete", "--json", &id]);
    let importer = spawn(
        &box_,
        &["meeting", "import", "--json", extra.to_str().unwrap()],
    );
    let deleted = deleter.wait_with_output().unwrap();
    let arrived = finish(importer);
    assert!(deleted.status.success(), "{}", stderr(&deleted));
    let body: Value = serde_json::from_slice(&deleted.stdout).unwrap();
    assert_eq!(body["delete_status"], "physical_cleanup_pending");
    assert!(
        arrived,
        "import during a read snapshot was not acknowledged"
    );
    assert!(present(&box_, "during-hold"));
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    let cleaned = json_ok(&box_.run(&["meeting", "delete", "--json", "--retry"]));
    assert_eq!(cleaned["delete_status"], "physically_cleaned");
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = std::path::PathBuf::from(format!("{}{suffix}", box_.db().display()));
        if let Ok(bytes) = fs::read(&path) {
            assert!(
                !bytes
                    .windows(sentinel.len())
                    .any(|window| window == sentinel.as_bytes()),
                "sentinel still in {}",
                path.display()
            );
        }
    }
    assert!(present(&box_, "during-hold"));
    healthy(&box_);
}

#[test]
fn export_refuses_to_replace_the_database() {
    let box_ = Sandbox::new();
    let meeting = import_at(
        &box_.db(),
        &meeting_json("keep", 1_700_000_000_000, 1_000, 1, "kept"),
    );
    let before = fs::read(box_.db()).unwrap();
    let id = label(&meeting);
    let direct = box_.run(&[
        "meeting",
        "export",
        &id,
        "--output",
        box_.db().to_str().unwrap(),
        "--force",
    ]);
    assert!(!direct.status.success(), "{}", stdout(&direct));
    assert!(
        stderr(&direct).contains("meeting or history database"),
        "{}",
        stderr(&direct)
    );
    assert_eq!(fs::read(box_.db()).unwrap(), before);

    let spelled = box_.home().join("data/xflow/../xflow/meetings.db");
    let spelled = box_.run(&[
        "meeting",
        "export",
        &id,
        "--output",
        spelled.to_str().unwrap(),
        "--force",
    ]);
    assert!(!spelled.status.success(), "{}", stdout(&spelled));
    assert!(
        stderr(&spelled).contains("meeting or history database"),
        "{}",
        stderr(&spelled)
    );
    assert_eq!(fs::read(box_.db()).unwrap(), before);

    let alias = box_.home().join("alias.db");
    fs::hard_link(box_.db(), &alias).unwrap();
    let linked = box_.run(&[
        "meeting",
        "export",
        &id,
        "--output",
        alias.to_str().unwrap(),
        "--force",
    ]);
    assert!(!linked.status.success(), "{}", stderr(&linked));
    assert!(
        stderr(&linked).contains("meeting or history database"),
        "{}",
        stderr(&linked)
    );
    assert_eq!(fs::read(box_.db()).unwrap(), before);
    fs::remove_file(&alias).unwrap();

    let history = box_.home().join("data/xflow/history.db");
    fs::write(&history, b"history-sentinel").unwrap();
    let history_export = box_.run(&[
        "meeting",
        "export",
        &id,
        "--output",
        history.to_str().unwrap(),
        "--force",
    ]);
    assert!(
        !history_export.status.success(),
        "{}",
        stdout(&history_export)
    );
    assert_eq!(fs::read(&history).unwrap(), b"history-sentinel");
    assert_eq!(fs::read(box_.db()).unwrap(), before);
    assert!(box_.run(&["meeting", "list"]).status.success());
}

#[test]
fn export_leaves_a_preexisting_file_byte_identical() {
    let box_ = Sandbox::new();
    let meeting = import_at(
        &box_.db(),
        &meeting_json("prior", 1_700_000_000_000, 1_000, 1, "prior"),
    );
    let dest = box_.home().join("already.txt");
    let original = b"other process successfully saved this";
    fs::write(&dest, original).unwrap();
    let output = box_.run(&[
        "meeting",
        "export",
        &label(&meeting),
        "--output",
        dest.to_str().unwrap(),
    ]);
    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("pass --force"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read(&dest).unwrap(), original);
}

#[test]
fn json_export_keeps_multiline_text() {
    let box_ = Sandbox::new();
    let text = "line one\n\tline two\u{1b}";
    let source = "mic\troom";
    let bytes = serde_json::to_vec(&json!({
        "schema_version": 1,
        "import_id": "lines",
        "provenance": "imported_unverified",
        "started_at_ms": 1_700_000_000_000i64,
        "ended_at_ms": 1_700_000_001_000i64,
        "timezone": "Z",
        "sources": [{"id": source, "epoch": 1}],
        "segments": [{
            "source_id": source,
            "sequence": 0,
            "range": {"start_ms": 0, "end_ms": 500},
            "text": text,
        }],
        "gaps": [],
    }))
    .unwrap();
    let meeting = import_at(&box_.db(), &bytes);
    let json_path = box_.home().join("lines.json");
    let exported = box_.run(&[
        "meeting",
        "export",
        &label(&meeting),
        "--format",
        "json",
        "--output",
        json_path.to_str().unwrap(),
    ]);
    assert!(exported.status.success(), "{}", stderr(&exported));
    let parsed: Value = serde_json::from_slice(&fs::read(&json_path).unwrap()).unwrap();
    assert_eq!(parsed["segments"][0]["text"], text);
    assert_eq!(parsed["segments"][0]["source_id"], source);
    assert_eq!(parsed["sources"][0]["id"], source);

    let text_path = box_.home().join("lines.txt");
    let plain = box_.run(&[
        "meeting",
        "export",
        &label(&meeting),
        "--format",
        "text",
        "--output",
        text_path.to_str().unwrap(),
    ]);
    assert!(plain.status.success(), "{}", stderr(&plain));
    let plain_bytes = fs::read(&text_path).unwrap();
    assert!(!plain_bytes.contains(&0x1b));
    assert!(!plain_bytes.contains(&b'\t'));
    assert!(String::from_utf8(plain_bytes)
        .unwrap()
        .contains("line one  line two"));

    let md_path = box_.home().join("lines.md");
    let markdown = box_.run(&[
        "meeting",
        "export",
        &label(&meeting),
        "--format",
        "md",
        "--output",
        md_path.to_str().unwrap(),
    ]);
    assert!(markdown.status.success(), "{}", stderr(&markdown));
    let markdown_bytes = fs::read(&md_path).unwrap();
    assert!(!markdown_bytes.contains(&0x1b));
    assert!(!markdown_bytes.contains(&b'\t'));
}

#[test]
fn export_to_a_bare_filename_uses_the_current_directory() {
    let box_ = Sandbox::new();
    let meeting = import_at(
        &box_.db(),
        &meeting_json("bare", 1_700_000_000_000, 1_000, 1, "bare name"),
    );
    let output = box_
        .xflow()
        .current_dir(box_.home())
        .args([
            "meeting",
            "export",
            &label(&meeting),
            "--output",
            "notes.json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(box_.home().join("notes.json").is_file());
}

#[test]
fn import_and_list_work_through_a_symlinked_data_directory() {
    let box_ = Sandbox::new();
    let real = box_.home().join("real-data");
    fs::create_dir_all(&real).unwrap();
    let link = box_.home().join("linked-data");
    symlink(&real, &link).unwrap();
    let file = box_.home().join("via-link.json");
    fs::write(
        &file,
        meeting_json("via-link", 1_700_000_000_000, 1_000, 1, "through a link"),
    )
    .unwrap();
    let imported = box_
        .xflow()
        .env("XDG_DATA_HOME", &link)
        .args(["meeting", "import", "--json", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(imported.status.success(), "{}", stderr(&imported));
    let listed = box_
        .xflow()
        .env("XDG_DATA_HOME", &link)
        .args(["meeting", "list"])
        .output()
        .unwrap();
    assert!(listed.status.success(), "{}", stderr(&listed));
    assert!(
        !stdout(&listed).contains("No meetings"),
        "{}",
        stdout(&listed)
    );
    let found = box_
        .xflow()
        .env("XDG_DATA_HOME", &link)
        .args(["meeting", "search", "through a link"])
        .output()
        .unwrap();
    assert!(found.status.success(), "{}", stderr(&found));
    assert!(
        stdout(&found).contains("through a link"),
        "{}",
        stdout(&found)
    );
}
