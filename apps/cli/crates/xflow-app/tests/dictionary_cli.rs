use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Home(tempfile::TempDir);

impl Home {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn config(&self, name: &str, toml: &str) -> PathBuf {
        let path = self.0.path().join(name);
        std::fs::write(&path, toml).unwrap();
        path
    }
    fn file(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
    fn run(&self, config: &Path, args: &[&str]) -> Output {
        let root = self.0.path();
        Command::new(env!("CARGO_BIN_EXE_xflow"))
            .env_clear()
            .env("HOME", root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .arg("--config")
            .arg(config)
            .args(args)
            .output()
            .unwrap()
    }
    fn ok(&self, config: &Path, args: &[&str]) -> Output {
        let output = self.run(config, args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    fn import(&self, config: &Path, file: &Path) -> Output {
        self.run(config, &["dictionary", "import", file.to_str().unwrap()])
    }
    fn list(&self, config: &Path) -> Value {
        serde_json::from_slice(&self.ok(config, &["--json", "dictionary", "list"]).stdout).unwrap()
    }
}

const SOURCE: &str = r#"[dictionary]
words = ["Postgres", "Kubectl", "naïve café"]
auto_learn = false
learned = ["Kubectl"]
never_learn = ["teh"]
[[dictionary.replacements]]
from = "cloud"
to = "Claude"
[[dictionary.replacements]]
from = "post grass"
to = "Postgres"
[[dictionary.replacements]]
from = "um"
to = ""
"#;
const EMPTY: &str = "[dictionary]\nwords = []\n";

fn export(home: &Home) -> Vec<u8> {
    let source = home.config("source.toml", SOURCE);
    home.ok(&source, &["--json", "dictionary", "export"]).stdout
}

#[test]
fn compact_and_pretty_json_exports_round_trip_without_learning_state() {
    let home = Home::new();
    let compact = export(&home);
    let exported: Value = serde_json::from_slice(&compact).unwrap();
    let pretty = serde_json::to_string_pretty(&exported).unwrap();
    for (name, bytes) in [
        ("compact.json", compact),
        ("pretty.json", pretty.clone().into()),
        ("bom.json", format!("\u{feff}{pretty}").into()),
    ] {
        let destination = home.config(&format!("{name}.toml"), EMPTY);
        let output = home.import(&destination, &home.file(name, bytes));
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("were not imported"));
        let imported = home.list(&destination);
        assert_eq!(imported["words"], exported["words"], "{name}");
        assert_eq!(imported["replacements"], exported["replacements"], "{name}");
        // Learning state stays the destination's own (defaults here).
        assert_eq!(imported["auto_learn"], true, "{name}");
        assert_eq!(imported["learned"], json!([]), "{name}");
        assert_eq!(imported["never_learn"], json!([]), "{name}");
    }
}

#[test]
fn json_import_merges_and_keeps_existing_entries_on_conflict() {
    let home = Home::new();
    let destination = home.config(
        "destination.toml",
        r#"[dictionary]
words = ["Postgres", "Keep"]
auto_learn = false
learned = ["Keep"]
never_learn = ["nope"]
[[dictionary.replacements]]
from = "cloud"
to = "Claude"
[[dictionary.replacements]]
from = "post grass"
to = "Postgres"
"#,
    );
    let file = home.file(
        "merge.json",
        json!({
            "words": ["Postgres", "New", "New"],
            "replacements": [
                {"from": "CLOUD", "to": "Cloud"},
                {"from": "post grass", "to": "Postgres"},
                {"from": "foo", "to": "bar"},
                {"from": "Foo", "to": "baz"},
            ],
            "auto_learn": true,
            "learned": ["New"],
            "never_learn": [],
        })
        .to_string(),
    );
    let output = home.import(&destination, &file);
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(r#"Kept "cloud" -> "Claude"; skipped "CLOUD" -> "Cloud""#));
    assert!(stderr.contains(r#"Kept "foo" -> "bar"; skipped "Foo" -> "baz""#));
    let d = home.list(&destination);
    assert_eq!(d["words"], json!(["Postgres", "Keep", "New"]));
    assert_eq!(
        d["replacements"],
        json!([
            {"from": "cloud", "to": "Claude"},
            {"from": "post grass", "to": "Postgres"},
            {"from": "foo", "to": "bar"},
        ])
    );
    assert_eq!(d["auto_learn"], false);
    assert_eq!(d["learned"], json!(["Keep"]));
    assert_eq!(d["never_learn"], json!(["nope"]));
}

#[test]
fn plain_text_import_still_adds_one_word_per_line() {
    let home = Home::new();
    let destination = home.config("destination.toml", "[dictionary]\nwords = [\"Alpha\"]\n");
    let file = home.file("words.txt", "Alpha\n  Beta  \n\nGamma Delta\r\nBeta\n");
    assert!(home.import(&destination, &file).status.success());
    let d = home.list(&destination);
    assert_eq!(d["words"], json!(["Alpha", "Beta", "Gamma Delta"]));
    assert_eq!(d["replacements"], json!([]));
}

#[test]
fn invalid_imports_fail_and_leave_config_unchanged() {
    let home = Home::new();
    let destination = home.config("destination.toml", SOURCE);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("truncated", br#"{"words": ["a""#.to_vec()),
        ("array", br#"["a", "b"]"#.to_vec()),
        ("unknown field", br#"{"word": ["a"]}"#.to_vec()),
        ("wrong type", br#"{"words": "a"}"#.to_vec()),
        (
            "bad replacement",
            br#"{"replacements": [{"from": "a"}]}"#.to_vec(),
        ),
        ("trailing data", br#"{"words": ["a"]} extra"#.to_vec()),
        ("blank word", br#"{"words": ["ok", "  "]}"#.to_vec()),
        (
            "blank from",
            br#"{"replacements": [{"from": " ", "to": "x"}]}"#.to_vec(),
        ),
        (
            "long word",
            format!(r#"{{"words": ["{}"]}}"#, "x".repeat(10_000)).into(),
        ),
        ("oversized", vec![b'a'; 1024 * 1024 + 1]),
        ("not utf-8", vec![0xff, b'\n']),
    ];
    for (name, bytes) in cases {
        let output = home.import(&destination, &home.file("bad", bytes));
        assert!(!output.status.success(), "{name} was accepted");
        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            SOURCE,
            "{name} changed the config"
        );
    }
    let output = home.import(&destination, &home.file("bad", "{ nope"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid dictionary JSON"));
}
