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
    assert!(stdout.contains("Sources stay off."));
    assert!(stdout.contains("sources --dictations on"));
    assert!(!d.path().join("config/xflow/mcp.toml").exists());
    let args = fs::read_to_string(log).unwrap();
    assert!(args.ends_with("mcp\n"));
    assert!(!args.contains("--sources"));
    let out = command(d.path())
        .args(["mcp", "install", "cursor", "--dictations"])
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
        assert_eq!(list.tools.len(), 2);
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
