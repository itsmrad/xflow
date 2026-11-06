#![cfg(unix)]
use serde_json::{json, Value};
use std::{
    io::{BufRead, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn command(root: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_xflow"));
    c.env_clear()
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("PATH", root.join("no-programs"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}
fn exchange(lines: &[Value], sources: Option<&str>) -> (Vec<Value>, String, Duration) {
    let d = tempfile::tempdir().unwrap();
    let mut c = command(d.path());
    c.arg("mcp");
    if let Some(s) = sources {
        c.args(["--sources", s]);
    }
    let mut child = c.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for line in lines {
        writeln!(stdin, "{line}").unwrap();
    }
    let start = Instant::now();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect(),
        String::from_utf8(out.stderr).unwrap(),
        start.elapsed(),
    )
}
fn init(version: &str) -> Value {
    json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}})
}
#[test]
fn legacy_versions_and_errors() {
    for version in [
        "2024-11-05",
        "2025-03-26",
        "2025-06-18",
        "2025-11-25",
        "unknown",
    ] {
        let (out, _, _) = exchange(
            &[
                init(version),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":5}}),
                json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
                json!({"jsonrpc":"2.0","id":2,"method":"ping"}),
                json!({"jsonrpc":"2.0","id":3,"method":"missing"}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"missing"}}),
                json!([]),
            ],
            None,
        );
        assert_eq!(
            out[0]["result"]["protocolVersion"],
            if version == "unknown" {
                "2025-11-25"
            } else {
                version
            }
        );
        assert!(out[1]["result"].get("resultType").is_none());
        assert!(out[1]["result"].get("ttlMs").is_none());
        assert_eq!(
            out[1]["result"]["tools"][0]["annotations"]["readOnlyHint"],
            true
        );
        assert_eq!(out[2]["result"], json!({}));
        assert_eq!(out[3]["error"]["code"], -32601);
        assert_eq!(out[4]["error"]["code"], -32602);
        assert!(out[5].get("error").is_some());
    }
}
#[test]
fn modern_stateless_and_source_override() {
    let meta = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":{"name":"fixture","version":"1"}});
    let (out, logs, _) = exchange(
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":meta}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":meta}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"_meta":meta,"name":"search_dictations","arguments":{"query":"private-canary-query"}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1900-01-01"}}}),
        ],
        Some("dictations"),
    );
    assert_eq!(out[0]["result"]["resultType"], "complete");
    assert!(out[0]["result"]["supportedVersions"]
        .as_array()
        .unwrap()
        .contains(&json!("2026-07-28")));
    assert_eq!(out[1]["result"]["cacheScope"], "private");
    assert!(out[1]["result"]["ttlMs"].is_number());
    assert_eq!(
        out[2]["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "xflow"
    );
    assert!(out[2]["result"]["structuredContent"]["notice"]
        .as_str()
        .unwrap()
        .contains("database"));
    assert!(!logs.contains("private-canary-query"));
    assert_eq!(out[3]["error"]["code"], -32022);
    assert!(out[3]["error"]["data"]["supported"].is_array());
}
#[test]
fn invalid_request_ids_and_params() {
    let (out, _, _) = exchange(
        &[
            json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":1.5,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":true,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":1,"method":"ping","params":[]}),
        ],
        None,
    );
    for response in &out[..3] {
        assert_eq!(response["error"]["code"], -32600);
    }
    assert_eq!(out[3]["error"]["code"], -32602);
}
#[test]
fn malformed_oversize_recovers_and_eof_exits() {
    let d = tempfile::tempdir().unwrap();
    let mut child = command(d.path()).arg("mcp").spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "{{bad").unwrap();
    writeln!(input, "{}", "x".repeat(1024 * 1024 + 1)).unwrap();
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"ping"})).unwrap();
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut out = Vec::<Value>::new();
    for _ in 0..3 {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        out.push(serde_json::from_str(&line).unwrap());
    }
    let start = Instant::now();
    drop(input);
    let status = child.wait().unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(status.success());
    assert_eq!(out[0]["error"]["code"], -32700);
    assert!(out[1].get("error").is_some());
    assert_eq!(out[2]["result"], json!({}));
}
#[test]
fn install_uses_fakes_and_never_registers_source_override() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("home")).unwrap();
    let fake = d.path().join("fake-codex");
    let log = d.path().join("args");
    fs::write(&fake,"#!/bin/sh\ncase \"$2\" in get) exit 1;; esac\n/usr/bin/printf '%s\\n' \"$@\" > \"$FAKE_ARGS\"\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    let out = command(d.path())
        .env("XFLOW_MCP_CODEX_BIN", &fake)
        .env("FAKE_ARGS", &log)
        .args(["mcp", "install", "codex"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("Dictation history stays off."));
    assert!(stdout.contains("sources --dictations on"));
    assert!(!d.path().join("config/xflow/mcp.toml").exists());
    let args = fs::read_to_string(log).unwrap();
    assert!(args.ends_with("mcp\n"));
    assert!(!args.contains("--sources"));
    let out = command(d.path())
        .args(["mcp", "install", "cursor", "--dictations", "--meetings"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(fs::read_to_string(d.path().join("config/xflow/mcp.toml"))
        .unwrap()
        .contains("dictations = true"));
    assert!(fs::read_to_string(d.path().join("config/xflow/mcp.toml"))
        .unwrap()
        .contains("meetings = true"));
    let out = command(d.path())
        .args(["mcp", "sources", "--dictations", "off"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = command(d.path())
        .args(["mcp", "install", "codex"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let toml = fs::read_to_string(d.path().join("home/.codex/config.toml")).unwrap();
    assert!(!toml.contains("--sources"));
}
#[test]
fn claude_code_reinstall_is_noop_and_force_replaces() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("home")).unwrap();
    let fake = d.path().join("fake-claude");
    let state = d.path().join("state");
    // Like Claude Code: `mcp add` fails when the name already exists.
    fs::write(
        &fake,
        r#"#!/bin/sh
s="$FAKE_STATE"
case "$2" in
get) [ -f "$s" ] || exit 1; { read c; read a; } < "$s"; printf 'xflow:\n  Command: %s\n  Args: %s\n' "$c" "$a";;
remove) [ -f "$s" ] || exit 1; /bin/rm "$s";;
add) [ -f "$s" ] && exit 1; shift 6; [ -n "$FAKE_REJECT" ] && [ "$1" != /old/xflow ] && exit 1
    c=$1; shift; printf '%s\n%s\n' "$c" "$*" > "$s";;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(&state, "/old/xflow\nmcp\n").unwrap();
    let install = |extra: &[&str], reject: bool| {
        let mut c = command(d.path());
        c.env("XFLOW_MCP_CLAUDE_CODE_BIN", &fake)
            .env("FAKE_STATE", &state)
            .args(["mcp", "install", "claude-code"])
            .args(extra);
        if reject {
            c.env("FAKE_REJECT", "1");
        }
        c.output().unwrap()
    };
    assert!(!install(&[], false).status.success());
    assert!(!install(&["--force"], true).status.success());
    assert_eq!(fs::read_to_string(&state).unwrap(), "/old/xflow\nmcp\n");
    let out = install(&["--force"], false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let new = fs::read_to_string(&state).unwrap();
    assert!(!new.starts_with("/old/") && new.ends_with("\nmcp\n"));
    for extra in [&[][..], &["--force"]] {
        let out = install(extra, false);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(fs::read_to_string(&state).unwrap(), new);
    }
}
#[tokio::test]
async fn rmcp_client_oracle_both_lifecycles() {
    use rmcp::{
        model::{CallToolRequestParams, ClientConfig, ProtocolVersion},
        service::{ClientLifecycleMode, ClientServiceExt},
        transport::TokioChildProcess,
    };
    for mode in [
        ClientLifecycleMode::Initialize,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    ] {
        let d = tempfile::tempdir().unwrap();
        let mut c = tokio::process::Command::from(command(d.path()));
        c.arg("mcp");
        let transport = TokioChildProcess::new(c).unwrap();
        let client = tokio::time::timeout(
            Duration::from_secs(5),
            ClientConfig::default().serve_with_lifecycle(transport, mode),
        )
        .await
        .unwrap()
        .unwrap();
        let list = client.list_tools(None).await.unwrap();
        assert_eq!(list.tools.len(), 6);
        let result = client
            .call_tool(CallToolRequestParams::new("search_dictations"))
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        assert!(result.structured_content.is_some());
        client.cancel().await.unwrap();
    }
}

#[test]
fn bounded_private_history_results_and_override() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join("data/xflow")).unwrap();
    fs::create_dir_all(d.path().join("config/xflow")).unwrap();
    fs::write(
        d.path().join("config/xflow/mcp.toml"),
        "dictations=false\nfuture=true\n",
    )
    .unwrap();
    fs::write(
        d.path().join("config/xflow/config.toml"),
        "[privacy]\nhistory=true\nfuture_setting=true\n",
    )
    .unwrap();
    let path = d.path().join("data/xflow/history.db");
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE history(id INTEGER PRIMARY KEY,created_at INTEGER,text TEXT,provider TEXT,raw_text TEXT); PRAGMA user_version=1;").unwrap();
    let private = "private-canary-transcript 界 ![beacon](https://example.invalid) <img src=x>\n"
        .repeat(10000);
    c.execute(
        "INSERT INTO history VALUES(1,1700000000,?1,'test','raw-canary-never-return')",
        [&private],
    )
    .unwrap();
    drop(c);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    for (sources, enabled) in [
        (None, false),
        (Some("dictations"), true),
        (Some("none"), false),
    ] {
        let mut cmd = command(d.path());
        cmd.arg("mcp");
        if let Some(s) = sources {
            cmd.args(["--sources", s]);
        }
        let mut child = cmd.spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        writeln!(input, "{}", init("2025-11-25")).unwrap();
        writeln!(input,"{}",json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_dictation","arguments":{"id":1}}})).unwrap();
        drop(input);
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let logs = String::from_utf8(out.stderr).unwrap();
        assert!(!logs.contains("private-canary-transcript"));
        assert!(!logs.contains("raw-canary"));
        let stdout = String::from_utf8(out.stdout).unwrap();
        let line = stdout.lines().nth(1).unwrap();
        assert!(line.len() <= 24 * 1024);
        assert!(!line.contains("raw-canary"));
        let v: Value = serde_json::from_str(line).unwrap();
        let content = &v["result"]["structuredContent"];
        if enabled {
            assert_eq!(content["truncated"], true);
            assert!(content["text"]
                .as_str()
                .unwrap()
                .contains("[image: beacon]"));
            assert!(!content["text"].as_str().unwrap().contains("<img"));
            assert_eq!(
                serde_json::from_str::<Value>(v["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap(),
                *content
            );
        } else {
            assert!(content["notice"]
                .as_str()
                .unwrap()
                .contains("not been allowed"));
        }
    }
    let invalid = command(d.path())
        .args(["mcp", "--sources", "dictations", "install", "cursor"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
}

fn assert_tools(input: &mut impl Write, output: &mut impl BufRead, notice: Option<&str>) {
    for (name, arguments) in [
        ("search_dictations", json!({"query":"consent-canary"})),
        ("get_dictation", json!({"id":1})),
    ] {
        let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}});
        writeln!(input, "{call}").unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        let content = &v["result"]["structuredContent"];
        assert_eq!(line.contains("consent-canary"), notice.is_none(), "{line}");
        if let Some(notice) = notice {
            assert!(
                content["notice"].as_str().unwrap().contains(notice),
                "{line}"
            );
        }
    }
}
fn server(
    root: &std::path::Path,
    sources: Option<&str>,
) -> (
    std::process::Child,
    std::process::ChildStdin,
    std::io::BufReader<std::process::ChildStdout>,
) {
    let mut c = command(root);
    c.arg("mcp");
    if let Some(s) = sources {
        c.args(["--sources", s]);
    }
    let mut child = c.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    writeln!(input, "{}", init("2025-11-25")).unwrap();
    output.read_line(&mut String::new()).unwrap();
    (child, input, output)
}
#[test]
fn consent_changes_reach_a_running_server_and_fail_closed() {
    use std::fs;
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join("data/xflow")).unwrap();
    fs::create_dir_all(d.path().join("config/xflow")).unwrap();
    fs::write(
        d.path().join("config/xflow/config.toml"),
        "[privacy]\nhistory=true\n",
    )
    .unwrap();
    let c = rusqlite::Connection::open(d.path().join("data/xflow/history.db")).unwrap();
    c.execute_batch("CREATE TABLE history(id INTEGER PRIMARY KEY,created_at INTEGER,text TEXT,provider TEXT); INSERT INTO history VALUES(1,1700000000,'consent-canary','test'); PRAGMA user_version=1;").unwrap();
    drop(c);
    let set = |value: &str| {
        let out = command(d.path())
            .args(["mcp", "sources", "--dictations", value])
            .output()
            .unwrap();
        assert!(out.status.success());
    };
    let consent = d.path().join("config/xflow/mcp.toml");
    let (off, unreadable) = (Some("not been allowed"), Some("cannot read"));
    set("on");
    let (mut child, mut input, mut output) = server(d.path(), None);
    assert_tools(&mut input, &mut output, None);
    set("off");
    assert_tools(&mut input, &mut output, off);
    set("on");
    assert_tools(&mut input, &mut output, None);
    fs::write(&consent, "dictations = [").unwrap();
    assert_tools(&mut input, &mut output, unreadable);
    fs::write(&consent, "dictations = \"yes\"\n").unwrap();
    assert_tools(&mut input, &mut output, unreadable);
    set("on");
    assert_tools(&mut input, &mut output, None);
    fs::remove_file(&consent).unwrap();
    fs::create_dir(&consent).unwrap();
    assert_tools(&mut input, &mut output, unreadable);
    fs::remove_dir(&consent).unwrap();
    assert_tools(&mut input, &mut output, off);
    drop(input);
    assert!(child.wait().unwrap().success());

    // --sources stays a process-only override whatever the file says or becomes.
    set("off");
    let (mut child, mut input, mut output) = server(d.path(), Some("dictations"));
    assert_tools(&mut input, &mut output, None);
    fs::write(&consent, "dictations = [").unwrap();
    assert_tools(&mut input, &mut output, None);
    drop(input);
    assert!(child.wait().unwrap().success());
    fs::write(&consent, "dictations = true\n").unwrap();
    let (mut child, mut input, mut output) = server(d.path(), Some("none"));
    assert_tools(&mut input, &mut output, off);
    drop(input);
    assert!(child.wait().unwrap().success());
}
#[test]
fn json_rewrites_refuse_numbers_they_would_change() {
    use std::fs;
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("home/.cursor/mcp.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let run = |action: &str| {
        command(d.path())
            .args(["mcp", action, "cursor"])
            .output()
            .unwrap()
    };
    for number in ["18446744073709551617", "0.12345678901234567890123456789"] {
        for (action, servers) in [
            ("install", "{}"),
            ("uninstall", "{\"xflow\":{\"command\":\"old\"}}"),
        ] {
            let text = format!("{{\"n\":[{number}],\"mcpServers\":{servers}}}");
            fs::write(&path, &text).unwrap();
            let out = run(action);
            assert!(!out.status.success());
            let stderr = String::from_utf8(out.stderr).unwrap();
            assert!(stderr.contains(number), "{stderr}");
            assert!(stderr.contains("Add this snippet manually"), "{stderr}");
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
            assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        }
    }
    // Exact numbers, and digits or escapes inside strings, must not block a write.
    let kept = r#""s":"18446744073709551617 \" 1e5","18446744073709551617":0,"i":-9223372036854775808,"u":18446744073709551615,"f":[0.5,1.0,-2.5]"#;
    fs::write(&path, format!("{{{kept},\"mcpServers\":{{}}}}")).unwrap();
    let out = run("install");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written["i"], json!(i64::MIN));
    assert_eq!(written["u"], json!(u64::MAX));
    assert_eq!(written["f"], json!([0.5, 1.0, -2.5]));
    assert!(written["mcpServers"]["xflow"].is_object());
}
#[test]
fn status_reports_detection_and_version_and_replacing_needs_force() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("home")).unwrap();
    let script = |name: &str, body: &str| {
        let path = d.path().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    };
    let old = script("old-xflow", "echo 'xflow 0.0.1'\n");
    let claude = script(
        "fake-claude",
        &format!(
            "case \"$2\" in get) printf 'Command: {}\\nArgs: mcp\\n';; esac\n",
            old.display()
        ),
    );
    let run = |args: &[&str]| {
        command(d.path())
            .env("XFLOW_MCP_CLAUDE_CODE_BIN", &claude)
            .args(args)
            .output()
            .unwrap()
    };
    let out = run(&["mcp", "status"]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains(&format!(
            "claude-code: found; registered command={:?}",
            old.display().to_string()
        )),
        "{stdout}"
    );
    assert!(stdout.contains("version=xflow 0.0.1 WARNING: version differs"));
    assert!(stdout.contains("cursor: not found; not registered"));
    // Without a terminal a differing entry is kept unless forced.
    let out = run(&["mcp", "install", "claude-code"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("Kept existing registration"), "{stderr}");
    assert!(stderr.contains("old-xflow"), "{stderr}");
    assert!(run(&["mcp", "install", "claude-code", "--force"])
        .status
        .success());
}

#[test]
fn meeting_tools_protocol_consent_and_validation() {
    let d = tempfile::tempdir().unwrap();
    let (mut child, mut input, mut output) = server(d.path(), None);
    for (name, arguments) in [
        ("list_meetings", json!({})),
        ("search_meetings", json!({"query":"Kubernetes"})),
        (
            "get_transcript",
            json!({"meeting_id":{"store_id":"fixture","id":1}}),
        ),
        (
            "get_meeting_notes",
            json!({"meeting_id":{"store_id":"fixture","id":1}}),
        ),
    ] {
        writeln!(input,"{}",json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":arguments}})).unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let r: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(r["result"]["isError"], false, "{line}");
        assert!(r["result"]["structuredContent"]["notice"]
            .as_str()
            .unwrap()
            .contains("not been allowed"));
        let mut args = arguments;
        args["cursor"] = Value::Null;
        writeln!(input,"{}",json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":args}})).unwrap();
        line.clear();
        output.read_line(&mut line).unwrap();
        let r: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(r["error"]["code"], -32602);
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn meeting_calls_read_imported_data_and_live_consent() {
    let d = tempfile::tempdir().unwrap();
    let store = xflow_app::meeting::MeetingStore::at(d.path().join("data/xflow/meetings.db"));
    let id = store
        .import_json(include_bytes!("fixtures/meeting-notes/mixed-script.json"))
        .unwrap()
        .meeting;
    let (mut child, mut input, mut output) = server(d.path(), None);
    for enabled in [true, false, true] {
        let out = command(d.path())
            .args([
                "mcp",
                "sources",
                "--meetings",
                if enabled { "on" } else { "off" },
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        for (name, args) in [
            ("list_meetings", json!({})),
            ("search_meetings", json!({"query":"useEffect"})),
            ("get_transcript", json!({"meeting_id":id})),
            ("get_meeting_notes", json!({"meeting_id":id})),
        ] {
            writeln!(input,"{}",json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":args}})).unwrap();
            let mut line = String::new();
            output.read_line(&mut line).unwrap();
            let v: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(v["result"]["isError"], false, "{line}");
            let r = &v["result"]["structuredContent"];
            if !enabled {
                assert!(r["notice"].as_str().unwrap().contains("not been allowed"));
            } else {
                match name {
                    "list_meetings" => assert_eq!(r["items"][0]["meeting_id"], json!(id)),
                    "search_meetings" => assert_eq!(r["items"].as_array().unwrap().len(), 1),
                    "get_transcript" => assert_eq!(r["segments"].as_array().unwrap().len(), 12),
                    _ => assert_eq!(r["notice"], "no notes for this meeting yet"),
                }
            }
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}
