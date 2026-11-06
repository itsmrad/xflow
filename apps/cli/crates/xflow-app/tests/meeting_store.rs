//! Scratch-directory tests for the private meeting store. No real data dir.
use anyhow::Error;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Once,
    time::{Duration, Instant},
};
use xflow_app::meeting::{ContractError, MeetingReader, MeetingStore};
use xflow_core::meeting::{
    DeleteStatus, ListFilter, MeetingId, NotesStatus, Provenance, SearchCursor, TimeRange,
    TranscriptQuery, MAX_IMPORT_BYTES, MAX_WINDOW_MS, PAGE_BYTES,
};

fn init_sqlite() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        xflow_app::store::skip_bulk_page_cache();
    });
}

fn scratch() -> tempfile::TempDir {
    init_sqlite();
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("history.db"),
        b"history-sentinel-do-not-touch",
    )
    .unwrap();
    dir
}

fn db_path(dir: &Path) -> PathBuf {
    dir.join("meetings.db")
}

fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            out.insert(
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            );
        }
    }
    out
}

fn contract(error: Error) -> ContractError {
    error.downcast::<ContractError>().expect("ContractError")
}
fn contract_ref(error: &Error) -> &ContractError {
    error
        .downcast_ref::<ContractError>()
        .expect("ContractError")
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::symlink_metadata(path).unwrap().permissions().mode()
}

fn meeting(import_id: &str, started: i64, duration: i64, segments: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema_version": 1,
        "import_id": import_id,
        "provenance": "imported_unverified",
        "title": "Notes",
        "started_at_ms": started,
        "ended_at_ms": started + duration,
        "timezone": "+00:00",
        "sources": [{"id": "mic", "epoch": 1}],
        "segments": segments,
        "gaps": [],
    }))
    .unwrap()
}

fn seg(seq: i64, start: i64, end: i64, text: &str) -> Value {
    json!({
        "source_id": "mic",
        "sequence": seq,
        "range": {"start_ms": start, "end_ms": end},
        "text": text,
    })
}

fn assert_page(value: &impl Serialize) {
    let bytes = serde_json::to_vec(value).unwrap().len();
    assert!(
        bytes <= PAGE_BYTES,
        "serialized page is {bytes} bytes, bound is {PAGE_BYTES}"
    );
}

fn history_untouched(dir: &Path) {
    assert_eq!(
        fs::read(dir.join("history.db")).unwrap(),
        b"history-sentinel-do-not-touch"
    );
}

fn chmod_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn paths_symlinks_and_permissions() {
    let dir = scratch();
    let bytes = meeting(
        "one",
        1_700_000_000_000,
        2_000,
        vec![seg(0, 0, 1000, "hello")],
    );
    let wrong = MeetingStore::at(dir.path().join("notes.db"));
    assert!(wrong.import_json(&bytes).is_err());
    assert!(!dir.path().join("notes.db").exists());

    let real = dir.path().join("real");
    fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
    let via_link = MeetingStore::at(dir.path().join("link").join("meetings.db"));
    assert!(via_link.import_json(&bytes).is_err());
    assert!(!real.join("meetings.db").exists());

    let store = MeetingStore::at(db_path(dir.path()));
    let imported = store.import_json(&bytes).unwrap();
    assert!(!imported.already_present);
    assert_eq!(mode(dir.path()) & 0o077, 0, "parent directory");
    let db = db_path(dir.path());
    assert_eq!(mode(&db) & 0o077, 0, "meetings.db");
    assert!(fs::symlink_metadata(&db).unwrap().is_file());
    let writing = store.private_modes(true).unwrap();
    assert!(
        writing.iter().any(|(name, _)| name == "-wal"),
        "writer did not create a WAL file: {writing:?}"
    );
    for (name, bits) in &writing {
        assert_eq!(bits & 0o077, 0, "{name} is {bits:o} in {writing:?}");
    }
    let reading = store.private_modes(false).unwrap();
    for (name, bits) in &reading {
        assert_eq!(bits & 0o077, 0, "{name} is {bits:o} in {reading:?}");
    }
    history_untouched(dir.path());

    let other = scratch();
    let target = db_path(other.path());
    fs::write(&target, b"do-not-follow").unwrap();
    chmod_private(&target);
    fs::remove_file(db_path(dir.path())).unwrap();
    std::os::unix::fs::symlink(&target, db_path(dir.path())).unwrap();
    assert!(store.import_json(&bytes).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"do-not-follow");
}

#[test]
fn malformed_foreign_and_newer_schemas_are_untouched() {
    let cases = [
        (
            "foreign",
            "CREATE TABLE history(id INTEGER PRIMARY KEY, text TEXT); INSERT INTO history VALUES (1,'keep');",
            ContractError::MalformedStore,
        ),
        (
            "newer",
            "PRAGMA user_version=3;",
            ContractError::UnsupportedSchema(3),
        ),
        (
            "malformed",
            "CREATE TABLE metadata(x); PRAGMA user_version=1;",
            ContractError::MalformedStore,
        ),
    ];
    for (name, sql, expected) in cases {
        let dir = scratch();
        let path = db_path(dir.path());
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(sql).unwrap();
        }
        chmod_private(&path);
        let before = tree(dir.path());
        let err = MeetingStore::at(path.clone())
            .import_json(&meeting("x", 10, 20, vec![seg(0, 0, 10, "nope")]))
            .unwrap_err();
        assert_eq!(contract_ref(&err), &expected, "{name}: {err:#}");
        assert_eq!(tree(dir.path()), before, "{name} bytes changed");
        let reader_err = MeetingReader::at(path).status(&MeetingId {
            store_id: "a".repeat(32),
            id: 1,
        });
        assert!(reader_err.is_err(), "{name} reader opened it");
        assert_eq!(tree(dir.path()), before, "{name} reader wrote");
        history_untouched(dir.path());
    }

    let dir = scratch();
    let path = db_path(dir.path());
    fs::write(&path, b"this is not sqlite").unwrap();
    chmod_private(&path);
    let before = tree(dir.path());
    assert!(MeetingStore::at(path)
        .import_json(&meeting("x", 10, 20, vec![seg(0, 0, 10, "nope")]))
        .is_err());
    assert_eq!(tree(dir.path()), before);
}

#[test]
fn oversized_and_invalid_imports_create_nothing() {
    let dir = scratch();
    let store = MeetingStore::at(db_path(dir.path()));
    let mut huge = vec![b'{'];
    huge.resize(MAX_IMPORT_BYTES + 1, b' ');
    let err = contract(store.import_json(&huge).unwrap_err());
    assert!(matches!(err, ContractError::InvalidImport(_)));
    assert!(!db_path(dir.path()).exists());

    for (label, bytes) in [
        ("truncated", &b"{"[..]),
        ("schema", &br#"{"schema_version":2}"#[..]),
        (
            "unknown",
            &br#"{"schema_version":1,"import_id":"a","extra":1}"#[..],
        ),
    ] {
        assert!(
            store.import_json(bytes).is_err(),
            "{label} should be rejected"
        );
        assert!(!db_path(dir.path()).exists(), "{label} created a database");
    }
    history_untouched(dir.path());
}

#[test]
fn replay_is_idempotent_and_conflict_keeps_the_original() {
    let dir = scratch();
    let store = MeetingStore::at(db_path(dir.path()));
    let body = meeting("same", 50, 1_000, vec![seg(0, 0, 500, "original words")]);
    let first = store.import_json(&body).unwrap();
    let second = store.import_json(&body).unwrap();
    assert!(second.already_present);
    assert_eq!(first.meeting, second.meeting);

    let mut value: Value = serde_json::from_slice(&body).unwrap();
    value["segments"][0]["text"] = json!("different words");
    let err = contract(
        store
            .import_json(&serde_json::to_vec(&value).unwrap())
            .unwrap_err(),
    );
    assert_eq!(err, ContractError::ImportConflict);
    let page = store
        .reader()
        .transcript(&TranscriptQuery {
            meeting: first.meeting.clone(),
            range: TimeRange {
                start_ms: 0,
                end_ms: 1_000,
            },
            snapshot: None,
            cursor: None,
        })
        .unwrap();
    assert_eq!(page.segments.len(), 1);
    assert_eq!(page.segments[0].text, "original words");
    history_untouched(dir.path());
}

#[test]
fn cursors_do_not_skip_or_duplicate_and_edits_invalidate_them() {
    let dir = scratch();
    let store = MeetingStore::at(db_path(dir.path()));
    let mut segments = Vec::new();
    for i in 0..25 {
        segments.push(seg(i, i * 1000, i * 1000 + 400, &format!("line {i}")));
    }
    segments.reverse();
    let started = 1_700_000_000_000;
    let imported = store
        .import_json(&meeting("paged", started, 30_000, segments))
        .unwrap();
    let reader = store.reader();
    let mut query = TranscriptQuery {
        meeting: imported.meeting.clone(),
        range: TimeRange {
            start_ms: 0,
            end_ms: 30_000,
        },
        snapshot: None,
        cursor: None,
    };
    let first = reader.transcript(&query).unwrap();
    assert_page(&first);
    assert_eq!(first.segments.len(), 20);
    assert!(first
        .segments
        .windows(2)
        .all(|w| w[0].range.start_ms <= w[1].range.start_ms));
    query.snapshot = Some(first.snapshot);
    query.cursor = first.next.clone();

    // A late row with an earlier time and a newer id must stay outside this snapshot.
    {
        let conn = Connection::open(db_path(dir.path())).unwrap();
        conn.execute(
            "INSERT INTO segments(meeting_id,source_id,sequence,start_ms,end_ms,text) VALUES (?1,'mic',99999,500,900,'late-earlier')",
            [imported.meeting.id],
        )
        .unwrap();
    }
    let second = reader.transcript(&query).unwrap();
    assert_page(&second);
    assert!(second.segments.iter().all(|s| s.text != "late-earlier"));
    assert!(second.next.is_none() || second.segments.len() == 20);
    let mut seen: Vec<_> = first
        .segments
        .iter()
        .chain(&second.segments)
        .map(|s| s.id)
        .collect();
    let mut rest = second.next.clone();
    let mut guard = 0;
    while let Some(cursor) = rest {
        query.cursor = Some(cursor);
        let page = reader.transcript(&query).unwrap();
        assert_page(&page);
        assert!(page.segments.iter().all(|s| s.text != "late-earlier"));
        seen.extend(page.segments.iter().map(|s| s.id));
        rest = page.next;
        guard += 1;
        assert!(guard < 10);
    }
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(seen.len(), sorted.len(), "duplicate ids in one snapshot");
    assert_eq!(seen.len(), 25, "snapshot skipped a committed segment");

    let fresh = reader
        .transcript(&TranscriptQuery {
            meeting: imported.meeting.clone(),
            range: TimeRange {
                start_ms: 0,
                end_ms: 30_000,
            },
            snapshot: None,
            cursor: None,
        })
        .unwrap();
    assert_eq!(fresh.segments[1].text, "late-earlier");
    // The late insert did not bump the revision, so the frozen cursor still reads.
    assert!(reader.transcript(&query).is_ok());
    store
        .correct_segment(
            &imported.meeting,
            first.segments[0].id,
            first.segments[0].revision,
            "line 0 corrected",
        )
        .unwrap();
    assert_eq!(
        contract(reader.transcript(&query).unwrap_err()),
        ContractError::StaleCursor
    );

    let after = reader
        .transcript(&TranscriptQuery {
            meeting: imported.meeting.clone(),
            range: TimeRange {
                start_ms: 0,
                end_ms: 30_000,
            },
            snapshot: None,
            cursor: None,
        })
        .unwrap();
    let cursor = after.next.clone().unwrap();
    store.delete(&imported.meeting).unwrap();
    assert_eq!(
        contract(
            reader
                .transcript(&TranscriptQuery {
                    meeting: imported.meeting.clone(),
                    range: TimeRange {
                        start_ms: 0,
                        end_ms: 30_000,
                    },
                    snapshot: Some(after.snapshot),
                    cursor: Some(cursor),
                })
                .unwrap_err()
        ),
        ContractError::StaleCursor
    );
    history_untouched(dir.path());
}

#[test]
fn list_highwater_excludes_new_meetings_until_a_new_snapshot() {
    let dir = scratch();
    let store = MeetingStore::at(db_path(dir.path()));
    let mut ids = Vec::new();
    for i in 0..21 {
        let imported = store
            .import_json(&meeting(
                &format!("m{i}"),
                1_000 + i,
                1_000,
                vec![seg(0, 0, 100, "listed")],
            ))
            .unwrap();
        ids.push(imported.meeting.id);
    }
    let filter = ListFilter {
        since_ms: 0,
        until_ms: 1_000_000,
    };
    let reader = store.reader();
    let first = reader.list(&filter, None).unwrap();
    assert_page(&first);
    assert_eq!(first.meetings.len(), 20);
    let cursor = first.next.clone().unwrap();
    let extra = store
        .import_json(&meeting(
            "later",
            50_000,
            1_000,
            vec![seg(0, 0, 100, "later")],
        ))
        .unwrap();
    let second = reader.list(&filter, Some(&cursor)).unwrap();
    assert_page(&second);
    let got: Vec<_> = first
        .meetings
        .iter()
        .chain(&second.meetings)
        .map(|m| m.meeting.id)
        .collect();
    assert_eq!(got, ids, "list skipped or duplicated under one highwater");
    assert!(!got.contains(&extra.meeting.id));
    let all = reader.list(&filter, None).unwrap();
    assert_eq!(all.meetings.len(), 20);
    let rest = reader.list(&filter, all.next.as_ref()).unwrap();
    let mut every: Vec<_> = all
        .meetings
        .iter()
        .chain(&rest.meetings)
        .map(|m| m.meeting.id)
        .collect();
    every.sort_unstable();
    let mut expect = ids.clone();
    expect.push(extra.meeting.id);
    expect.sort_unstable();
    assert_eq!(every, expect);

    let cursor = all.next.clone().unwrap();
    store
        .correct_segment(&all.meetings[0].meeting, 1, 1, "changed")
        .unwrap();
    assert_eq!(
        contract(reader.list(&filter, Some(&cursor)).unwrap_err()),
        ContractError::StaleCursor
    );

    let edge = store
        .import_json(&meeting("edge", 80_000, 1_000, vec![seg(0, 0, 50, "edge")]))
        .unwrap();
    let included = ListFilter {
        since_ms: 80_000,
        until_ms: 80_001,
    };
    assert_eq!(reader.list(&included, None).unwrap().meetings.len(), 1);
    let excluded = ListFilter {
        since_ms: 0,
        until_ms: 80_000,
    };
    assert!(reader
        .list(&excluded, None)
        .unwrap()
        .meetings
        .iter()
        .all(|m| m.meeting.id != edge.meeting.id));
    history_untouched(dir.path());
}

#[test]
fn search_is_literal_across_scripts_and_pages_stay_bounded() {
    let dir = scratch();
    let fixture = include_bytes!("fixtures/meeting-notes/mixed-script.json");
    let store = MeetingStore::at(db_path(dir.path()));
    let imported = store.import_json(fixture).unwrap();
    let reader = store.reader();
    let status = reader.status(&imported.meeting).unwrap();
    assert_page(&status);
    assert_eq!(status.summary.provenance, Provenance::ImportedUnverified);
    assert_eq!(status.summary.notes, NotesStatus::Absent);
    assert_eq!(status.summary.gap_count, 1);
    assert_eq!(status.gaps[0].reason, "source silent");
    assert_eq!(status.sources[0].id, "room");
    assert_eq!(status.summary.timezone, "America/Sao_Paulo");

    let transcript = reader
        .transcript(&TranscriptQuery {
            meeting: imported.meeting.clone(),
            range: TimeRange {
                start_ms: 0,
                end_ms: 12_000,
            },
            snapshot: Some(status.snapshot),
            cursor: None,
        })
        .unwrap();
    assert_page(&transcript);
    let texts: Vec<_> = transcript
        .segments
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(texts[0], "明日は東京で会いましょう");
    assert_eq!(
        texts.last().copied(),
        Some("\u{1b}[2J![boom](http://evil.example/x.png)")
    );
    assert_eq!(
        transcript
            .segments
            .iter()
            .find(|s| s.sequence == 11)
            .unwrap()
            .text,
        "\u{1b}[2J![boom](http://evil.example/x.png)"
    );

    let expect = [
        ("東京", "明日は東京で会いましょう"),
        ("हिंदी", "हिंदी और English मिलाकर"),
        ("कि", "कि"),
        ("años", "el niño cumplió años"),
        ("ação", "uma ação não esperada"),
        ("não", "uma ação não esperada"),
        ("useEffect", "call useEffect after git rebase"),
        ("git rebase", "call useEffect after git rebase"),
        ("kubectl", "kubectl rollout and OAuth"),
        ("OAuth", "kubectl rollout and OAuth"),
        ("42", "room 42 and port 1024"),
        ("1024", "room 42 and port 1024"),
        ("José", "José Muñoz met 李明 and Søren"),
        ("李明", "José Muñoz met 李明 and Søren"),
        ("Søren", "José Muñoz met 李明 and Søren"),
        ("Müller", "Müller, Алексей, and محمد"),
        ("Алексей", "Müller, Алексей, and محمد"),
        ("محمد", "Müller, Алексей, and محمد"),
        ("\"", "he said \"NEAR\" AND * today"),
        ("*", "he said \"NEAR\" AND * today"),
        ("NEAR", "he said \"NEAR\" AND * today"),
    ];
    for (term, text) in expect {
        let page = reader.search(term, None).unwrap();
        assert_page(&page);
        assert_eq!(
            page.segments.len(),
            1,
            "term {term:?} hits {:?}",
            page.segments.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
        assert_eq!(page.segments[0].text, text, "term {term:?}");
        assert!(page.next.is_none(), "term {term:?} was not exhausted");
    }
    // Case folds: "AND" is plain text that also finds the lowercase "and"s.
    let mut and: Vec<String> = reader
        .search("AND", None)
        .unwrap()
        .segments
        .into_iter()
        .map(|s| s.text)
        .collect();
    and.sort();
    assert_eq!(
        and,
        [
            "José Muñoz met 李明 and Søren",
            "Müller, Алексей, and محمد",
            "he said \"NEAR\" AND * today",
            "kubectl rollout and OAuth",
            "room 42 and port 1024",
        ]
    );
    let folded = reader.search("oauth", None).unwrap();
    assert_eq!(folded.segments.len(), 1);
    assert_eq!(folded.segments[0].text, "kubectl rollout and OAuth");
    history_untouched(dir.path());
}

fn hits(reader: &MeetingReader, term: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let page = reader.search(term, cursor.as_ref()).unwrap();
        assert_page(&page);
        assert!(page.segments.len() <= 20);
        out.extend(page.segments.into_iter().map(|s| s.text));
        match page.next {
            Some(next) => cursor = Some(next),
            None => return out,
        }
    }
}

#[test]
fn search_ignores_letter_case_but_not_accents() {
    let dir = scratch();
    let store = MeetingStore::at(db_path(dir.path()));
    let texts = [
        "Sign in with OAuth today",
        "PostgreSQL replicas lag",
        "the useEffect hook re-runs",
        "el niño llegó",
        "uma ação rápida",
        "明日は東京で会いましょう",
        "हिंदी और English मिलाकर",
        "قال محمد مرحبا",
        "李明来了",
        "he said \"NEAR\" AND * today",
        "Straße und STRASSE",
        "İstanbul toplantısı",
        "Ιστανμπούλ ΟΔΟΣ",
    ];
    let segments = texts
        .iter()
        .enumerate()
        .map(|(i, t)| seg(i as i64, i as i64 * 100, i as i64 * 100 + 50, t))
        .collect();
    store
        .import_json(&meeting("fold", 10, 5_000, segments))
        .unwrap();
    let reader = store.reader();
    for (term, expected) in [
        ("oauth", "Sign in with OAuth today"),
        ("OAUTH", "Sign in with OAuth today"),
        ("postgresql", "PostgreSQL replicas lag"),
        ("USEEFFECT", "the useEffect hook re-runs"),
        ("NIÑO", "el niño llegó"),
        ("AÇÃO", "uma ação rápida"),
        ("東京", "明日は東京で会いましょう"),
        ("会い", "明日は東京で会いましょう"),
        ("हिंदी", "हिंदी और English मिलाकर"),
        ("कर", "हिंदी और English मिलाकर"),
        ("محمد", "قال محمد مرحبا"),
        ("李明", "李明来了"),
        ("\"", "he said \"NEAR\" AND * today"),
        ("*", "he said \"NEAR\" AND * today"),
        ("\"NEAR\" AND *", "he said \"NEAR\" AND * today"),
        ("near", "he said \"NEAR\" AND * today"),
        // Exact case always matches, including characters without a simple fold.
        ("Straße", "Straße und STRASSE"),
        ("İstanbul", "İstanbul toplantısı"),
        ("İ", "İstanbul toplantısı"),
        ("ΟΔΟΣ", "Ιστανμπούλ ΟΔΟΣ"),
        ("οδοσ", "Ιστανμπούλ ΟΔΟΣ"),
    ] {
        assert_eq!(hits(&reader, term), [expected], "term {term:?}");
    }
    // Accents are letters, not case: no accent folding.
    assert!(hits(&reader, "nino").is_empty());
    assert!(hits(&reader, "acao").is_empty());
    // No full case folding: "ß" is not "ss" (STRASSE matches only itself).
    assert_eq!(hits(&reader, "strasse"), ["Straße und STRASSE"]);
    assert_eq!(hits(&reader, "STRAẞE"), ["Straße und STRASSE"]);
    // Dotted capital I has no one-character lowercase; record what each path does.
    println!(
        "istanbul -> {:?}; i (short) -> {} hits",
        hits(&reader, "istanbul"),
        hits(&reader, "i").len()
    );
}

/// Where Rust's short-term fold says two letters are the same, the index's
/// own match must agree, so short and long terms behave alike.
#[test]
fn index_fold_agrees_with_the_short_term_fold() {
    let dir = scratch();
    let store = MeetingStore::at(db_path(dir.path()));
    let letters: Vec<char> = (0x41u32..0x250)
        .chain(0x370..0x400)
        .chain(0x400..0x530)
        .filter_map(char::from_u32)
        .filter(|c| {
            c.is_alphabetic()
                && c.to_lowercase().count() == 1
                && c.to_lowercase().next() != Some(*c)
        })
        .collect();
    let mut stricter = Vec::new();
    for (n, chunk) in letters.chunks(900).enumerate() {
        let segments = chunk
            .iter()
            .enumerate()
            .map(|(i, c)| seg(i as i64, i as i64, i as i64 + 1, &format!("q{c}q")))
            .collect();
        store
            .import_json(&meeting(&format!("letters{n}"), 10, 1_000, segments))
            .unwrap();
    }
    let reader = store.reader();
    for c in &letters {
        let lower = c.to_lowercase().next().unwrap();
        let found = hits(&reader, &format!("q{lower}q"));
        if !found.contains(&format!("q{c}q")) {
            stricter.push(*c);
        }
    }
    println!(
        "{} letters checked; index stricter on {stricter:?}",
        letters.len()
    );
    // SQLite's fold table predates these Unicode 7+ letters. Only 1-2
    // character searches fold them; nothing else may differ.
    assert_eq!(stricter, ['Ϳ', 'Ԩ', 'Ԫ', 'Ԭ', 'Ԯ']);
}

#[test]
fn folded_short_scan_pages_without_skipping() {
    let dir = scratch();
    let store = MeetingStore::at(db_path(dir.path()));
    let segments = (0..300)
        .map(|i| {
            seg(
                i,
                i * 10,
                i * 10 + 5,
                &if i % 3 == 0 {
                    format!("Ab {i}")
                } else {
                    format!("zz {i}")
                },
            )
        })
        .collect();
    store
        .import_json(&meeting("short", 10, 5_000, segments))
        .unwrap();
    let reader = store.reader();
    let short = hits(&reader, "aB");
    assert_eq!(short.len(), 100);
    let long = hits(&reader, "AB ");
    assert_eq!(short, long);
}

#[test]
fn short_search_cursor_is_not_exhaustion_and_worst_case_page_fits() {
    let dir = scratch();
    let store = MeetingStore::at(db_path(dir.path()));
    let mut segments = Vec::new();
    for i in 0..256 {
        segments.push(seg(i, i * 100, i * 100 + 50, &format!("plain {i}")));
    }
    segments.push(seg(256, 256 * 100, 256 * 100 + 50, "finally 東京"));
    let imported = store
        .import_json(&meeting("scan", 10, 30_000, segments))
        .unwrap();
    let reader = store.reader();
    let first = reader.search("東京", None).unwrap();
    assert_page(&first);
    assert!(
        first.segments.is_empty(),
        "first short page should be empty"
    );
    let cursor: SearchCursor = first.next.expect("empty page is not exhaustion");
    let second = reader.search("東京", Some(&cursor)).unwrap();
    assert_page(&second);
    assert_eq!(second.segments.len(), 1);
    assert_eq!(second.segments[0].text, "finally 東京");
    assert!(second.next.is_none());

    let quotes = {
        let mut text = String::new();
        loop {
            let next = format!("{text}\"");
            if serde_json::to_vec(&next).unwrap().len() > 4600 || next.len() > 4096 {
                break;
            }
            text = next;
        }
        text
    };
    let source = "\"".repeat(64);
    let extreme = serde_json::to_vec(&json!({
        "schema_version": 1,
        "import_id": "extreme",
        "provenance": "imported_unverified",
        "started_at_ms": 100,
        "ended_at_ms": 5_000,
        "timezone": "Z",
        "sources": [{"id": source, "epoch": 1}],
        "segments": [
            {"source_id": source, "sequence": 0, "range": {"start_ms": 0, "end_ms": 1000}, "text": quotes},
            {"source_id": source, "sequence": 1, "range": {"start_ms": 1000, "end_ms": 1500}, "text": "tail"}
        ],
        "gaps": []
    }))
    .unwrap();
    let extreme_id = store.import_json(&extreme).unwrap().meeting;
    let page = reader
        .transcript(&TranscriptQuery {
            meeting: extreme_id.clone(),
            range: TimeRange {
                start_ms: 0,
                end_ms: 4_000,
            },
            snapshot: None,
            cursor: None,
        })
        .unwrap();
    assert_page(&page);
    assert_eq!(page.segments[0].text, quotes);
    assert_eq!(page.segments[0].source_id, source);
    if let Some(cursor) = page.next.clone() {
        let rest = reader
            .transcript(&TranscriptQuery {
                meeting: extreme_id,
                range: TimeRange {
                    start_ms: 0,
                    end_ms: 4_000,
                },
                snapshot: Some(page.snapshot),
                cursor: Some(cursor),
            })
            .unwrap();
        assert_page(&rest);
        assert_eq!(rest.segments[0].text, "tail");
    } else {
        assert_eq!(page.segments.len(), 2);
    }
    let found = reader.search("tail", None).unwrap();
    assert_page(&found);
    assert_eq!(found.segments.len(), 1);
    let _ = imported;
    history_untouched(dir.path());
}

#[test]
fn reader_cannot_write_and_held_snapshot_defers_cleanup() {
    let dir = scratch();
    let store = MeetingStore::at(db_path(dir.path()));
    let sentinel = "SENTINEL-m1-9f3a2c7e-UNIQUE";
    let imported = store
        .import_json(&meeting(
            "secret",
            10,
            2_000,
            vec![seg(0, 0, 1000, sentinel)],
        ))
        .unwrap();
    let (core, fts) = store.deletion_flags().unwrap();
    assert_eq!(core, 1, "core secure_delete");
    assert_eq!(fts, 1, "FTS secure-delete");

    let reader = store.reader();
    let before = fs::read(db_path(dir.path())).unwrap();
    reader.write_refused().unwrap();
    assert_eq!(fs::read(db_path(dir.path())).unwrap(), before);

    let status = reader.status(&imported.meeting).unwrap();
    assert_eq!(status.summary.notes, NotesStatus::Absent);
    let reopen = MeetingReader::at(db_path(dir.path()));
    let again = reopen.status(&imported.meeting).unwrap();
    assert_eq!(again.summary.meeting, imported.meeting);
    assert_eq!(again.summary.segment_count, 1);
    {
        let conn = Connection::open_with_flags(
            fs::canonicalize(db_path(dir.path())).unwrap(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 3);
        let ddl: String = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='metadata'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(ddl.starts_with("CREATE TABLE metadata "));
        let triggers: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='trigger'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(triggers, 3);
    }

    let hold = Connection::open_with_flags(
        fs::canonicalize(db_path(dir.path())).unwrap(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    hold.execute_batch("BEGIN; SELECT count(*) FROM segments;")
        .unwrap();
    assert_eq!(
        store.delete(&imported.meeting).unwrap(),
        DeleteStatus::PhysicalCleanupPending
    );
    assert_eq!(
        contract(reader.status(&imported.meeting).unwrap_err()),
        ContractError::NotFound
    );
    drop(hold);
    assert_eq!(
        store.retry_cleanup().unwrap(),
        DeleteStatus::PhysicallyCleaned
    );
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = PathBuf::from(format!("{}{suffix}", db_path(dir.path()).display()));
        if let Ok(bytes) = fs::read(&path) {
            assert!(
                !bytes
                    .windows(sentinel.len())
                    .any(|w| w == sentinel.as_bytes()),
                "sentinel still in {}",
                path.display()
            );
        }
    }
    history_untouched(dir.path());
}

#[test]
fn exact_ddl_match_rejects_a_rewritten_table() {
    let dir = scratch();
    let path = db_path(dir.path());
    let store = MeetingStore::at(path.clone());
    let imported = store
        .import_json(&meeting("ddl", 10, 1_000, vec![seg(0, 0, 100, "kept")]))
        .unwrap();
    assert!(store.reader().status(&imported.meeting).is_ok());
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql='CREATE TABLE metadata (x)' WHERE name='metadata';",
        )
        .unwrap();
    }
    let err = contract(
        MeetingReader::at(path)
            .status(&imported.meeting)
            .unwrap_err(),
    );
    assert_eq!(err, ContractError::MalformedStore);
}

#[test]
fn three_hour_transcript_pages_cover_every_segment_once() {
    let dir = scratch();
    let store = MeetingStore::at(db_path(dir.path()));
    let count = 180;
    let mut segments = Vec::new();
    for i in 0..count {
        let start = i * 60_000;
        segments.push(seg(
            count - 1 - i,
            start,
            start + 60_000,
            &format!("hour-slice-{i}-useEffect"),
        ));
    }
    let duration = count * 60_000;
    let imported = store
        .import_json(&meeting("three-hours", 0, duration, segments))
        .unwrap();
    let reader = store.reader();
    let mut seen = Vec::new();
    let mut start = 0;
    while start < duration {
        let end = (start + MAX_WINDOW_MS).min(duration);
        let mut query = TranscriptQuery {
            meeting: imported.meeting.clone(),
            range: TimeRange {
                start_ms: start,
                end_ms: end,
            },
            snapshot: None,
            cursor: None,
        };
        loop {
            let page = reader.transcript(&query).unwrap();
            assert_page(&page);
            seen.extend(page.segments.iter().map(|s| s.sequence));
            match page.next {
                Some(cursor) => {
                    query.snapshot = Some(cursor.snapshot);
                    query.cursor = Some(cursor);
                }
                None => break,
            }
        }
        start = end;
    }
    seen.sort_unstable();
    assert_eq!(seen, (0..count).collect::<Vec<_>>());
    history_untouched(dir.path());
}

#[test]
#[ignore = "synthetic scale; report p95, do not chase it"]
fn scale_about_100_hours() {
    let dir = scratch();
    let store = MeetingStore::at(db_path(dir.path()));
    let per_meeting = 1024;
    let meetings = 6;
    for m in 0..meetings {
        let mut segments = Vec::with_capacity(per_meeting);
        for i in 0..per_meeting {
            let start = (i as i64) * 60_000;
            let text = if i % 40 == 0 {
                format!("kubectl apply deployment {m}-{i}")
            } else {
                format!("ordinary sentence number {m}-{i} about the roadmap")
            };
            segments.push(seg(i as i64, start, start + 60_000, &text));
        }
        store
            .import_json(&meeting(
                &format!("scale-{m}"),
                m * 10_000_000_000,
                per_meeting as i64 * 60_000,
                segments,
            ))
            .unwrap();
    }
    let reader = store.reader();
    let hours = meetings * (per_meeting as i64) / 60;
    assert!(hours >= 100, "built {hours} hours");
    for _ in 0..3 {
        reader.search("kubectl", None).unwrap();
    }
    let mut samples = Vec::new();
    for _ in 0..40 {
        let started = Instant::now();
        let page = reader.search("kubectl", None).unwrap();
        samples.push(started.elapsed());
        assert!(!page.segments.is_empty());
        assert_page(&page);
    }
    samples.sort();
    let p95 = samples[(samples.len() as f64 * 0.95).ceil() as usize - 1];
    println!("scale_p95_ms={} hours={hours}", p95.as_secs_f64() * 1000.0);
    assert!(p95 < Duration::from_secs(5), "p95 {} ms", p95.as_millis());
}

#[test]
fn missing_database_is_not_created_by_delete_or_cleanup() {
    let dir = scratch();
    let path = db_path(dir.path());
    let store = MeetingStore::at(path.clone());
    let meeting = MeetingId {
        store_id: "a".repeat(32),
        id: 1,
    };
    assert_eq!(
        contract(store.delete(&meeting).unwrap_err()),
        ContractError::NotFound
    );
    assert!(!path.exists());
    assert_eq!(
        store.retry_cleanup().unwrap(),
        DeleteStatus::PhysicallyCleaned
    );
    assert!(!path.exists());
    assert_eq!(
        contract(
            store
                .correct_segment(&meeting, 1, 1, "unchanged")
                .unwrap_err()
        ),
        ContractError::NotFound
    );
    assert!(!path.exists());
    history_untouched(dir.path());
}
