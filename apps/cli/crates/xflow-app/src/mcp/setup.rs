use super::{read_sources, write_sources};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    fs,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
};

const CLIENTS: [&str; 10] = [
    "claude-code",
    "codex",
    "gemini",
    "vscode",
    "cursor",
    "claude-desktop",
    "zed",
    "lm-studio",
    "windsurf",
    "chatgpt",
];
fn validate(client: &str) -> Result<()> {
    if !CLIENTS.contains(&client) {
        bail!("Unknown client; choose {}", CLIENTS.join(", "));
    }
    Ok(())
}
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is required")
}
fn config(client: &str) -> Result<(PathBuf, &'static str)> {
    let home = home()?;
    Ok(match client {
        "cursor" => (home.join(".cursor/mcp.json"), "mcpServers"),
        "claude-desktop" => (
            home.join("Library/Application Support/Claude/claude_desktop_config.json"),
            "mcpServers",
        ),
        "vscode" => {
            #[cfg(target_os = "macos")]
            let p = home.join("Library/Application Support/Code/User/mcp.json");
            #[cfg(not(target_os = "macos"))]
            let p = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("Code/User/mcp.json");
            (p, "servers")
        }
        "codex" => (home.join(".codex/config.toml"), "mcp_servers"),
        _ => bail!("Client has no managed config file"),
    })
}
fn program(client: &str) -> String {
    std::env::var(format!(
        "XFLOW_MCP_{}_BIN",
        client.replace('-', "_").to_uppercase()
    ))
    .unwrap_or_else(|_| {
        match client {
            "claude-code" => "claude",
            "vscode" => "code",
            other => other,
        }
        .into()
    })
}
fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}
fn confirm(message: &str) -> Result<bool> {
    if !interactive() {
        return Ok(false);
    }
    eprint!("{message} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
fn vendor(client: &str) -> &str {
    match client {
        "claude-code" | "claude-desktop" => "Anthropic",
        "codex" | "chatgpt" => "OpenAI",
        "gemini" => "Google",
        "cursor" => "Cursor and your selected model vendor",
        _ => "your selected model vendor",
    }
}
fn stamp() -> String {
    // UTC without an extra runtime dependency.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{:09}Z", now.as_secs(), now.subsec_nanos())
}
pub(super) fn atomic_write(
    path: &Path,
    bytes: &[u8],
    permissions: Option<fs::Permissions>,
) -> Result<()> {
    let parent = path.parent().context("Config path has no parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    if let Some(mode) = permissions {
        temp.as_file().set_permissions(mode)?;
    }
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
fn read_file(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if m.uid() != unsafe { libc::geteuid() } {
                    bail!("Config is not owned by this user");
                }
            }
            if !m.is_file() || m.file_type().is_symlink() {
                bail!("Unsafe client config file");
            }
            Ok(Some(fs::read_to_string(path)?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn save(path: &Path, bytes: &[u8], old: Option<&str>) -> Result<()> {
    let permissions = if old.is_some() {
        Some(fs::metadata(path)?.permissions())
    } else {
        None
    };
    fs::create_dir_all(path.parent().context("Missing parent")?)?;
    if let Some(old) = old {
        let backup = PathBuf::from(format!("{}.xflow-backup-{}", path.display(), stamp()));
        // A private temporary file also prevents broad umasks leaking a backup.
        atomic_write(&backup, old.as_bytes(), permissions.clone())?;
    }
    atomic_write(path, bytes, permissions)
}
fn entry(exe: &Path) -> Value {
    json!({"command":exe,"args":["mcp"]})
}
fn json_edit(path: &Path, key: &str, new: Option<Value>, force: bool) -> Result<()> {
    let old = read_file(path)?;
    let mut doc: Value = match &old {
        Some(s) => serde_json::from_str(s).with_context(|| format!("Cannot parse {}; JSONC/trailing commas are not supported. Add this snippet manually: {}",path.display(),json!({key:{"xflow":new}})))?,
        None => json!({}),
    };
    let object = doc
        .as_object_mut()
        .context("Client config must be a JSON object")?;
    if !object.contains_key(key) && new.is_none() {
        return Ok(());
    }
    let servers = object
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Server config must be an object")?;
    let previous = servers.get("xflow");
    if previous == new.as_ref() {
        println!("xflow registration unchanged");
        return Ok(());
    }
    if let (Some(previous), Some(new)) = (previous, &new) {
        eprintln!(
            "Existing xflow command: {}\nNew xflow: {new}",
            previous.get("command").unwrap_or(&Value::Null)
        );
        if !force && !confirm("Replace this xflow registration?")? {
            bail!("Kept existing registration; use --force to replace it");
        }
    }
    match new {
        Some(v) => {
            servers.insert("xflow".into(), v);
        }
        None => {
            servers.shift_remove("xflow");
        }
    }
    let mut bytes = serde_json::to_vec_pretty(&doc)?;
    bytes.push(b'\n');
    save(path, &bytes, old.as_deref())
}
fn codex_edit(path: &Path, exe: Option<&Path>, force: bool) -> Result<()> {
    let old = read_file(path)?;
    let mut doc = old
        .as_deref()
        .unwrap_or("")
        .parse::<toml_edit::DocumentMut>()
        .context("Cannot parse Codex TOML; config unchanged")?;
    if doc
        .get("mcp_servers")
        .is_some_and(|v| v.as_table_like().is_none())
    {
        bail!("Codex mcp_servers must be a table; config unchanged");
    }
    let previous = doc.get("mcp_servers").and_then(|v| v.get("xflow"));
    if let Some(exe) = exe {
        let exe = exe.to_str().context("Binary path must be UTF-8")?;
        let same = previous.is_some_and(|v| {
            v.get("command").and_then(toml_edit::Item::as_str) == Some(exe)
                && v.get("args")
                    .and_then(toml_edit::Item::as_array)
                    .is_some_and(|a| {
                        a.len() == 1 && a.get(0).and_then(toml_edit::Value::as_str) == Some("mcp")
                    })
        });
        if same {
            return Ok(());
        }
        if let Some(previous) = previous {
            eprintln!(
                "Existing xflow command: {}\nNew xflow: {}",
                previous
                    .get("command")
                    .and_then(toml_edit::Item::as_str)
                    .unwrap_or("unknown"),
                entry(Path::new(exe))
            );
            if !force && !confirm("Replace this xflow registration?")? {
                bail!("Kept existing registration; use --force");
            }
        }
        if doc.get("mcp_servers").is_none() {
            doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let mut table = toml_edit::Table::new();
        table["command"] = toml_edit::value(exe);
        let mut args = toml_edit::Array::new();
        args.push("mcp");
        table["args"] = toml_edit::value(args);
        doc["mcp_servers"]["xflow"] = toml_edit::Item::Table(table);
    } else {
        if previous.is_none() {
            return Ok(());
        }
        doc["mcp_servers"]
            .as_table_like_mut()
            .context("Invalid mcp_servers")?
            .remove("xflow");
    }
    save(path, doc.to_string().as_bytes(), old.as_deref())
}
fn stable_path(exe: &Path) -> PathBuf {
    let components = exe.components().collect::<Vec<_>>();
    if let Some(i) = components
        .windows(2)
        .position(|c| c[0].as_os_str() == "Cellar" && c[1].as_os_str() == "xflow")
    {
        if components.len() != i + 5
            || components[i + 3].as_os_str() != "bin"
            || components[i + 4].as_os_str() != "xflow"
        {
            return exe.to_owned();
        }
        let prefix: PathBuf = components[..i].iter().collect();
        let stable = prefix.join("bin/xflow");
        if stable.canonicalize().is_ok_and(|p| {
            prefix
                .join("Cellar/xflow")
                .canonicalize()
                .is_ok_and(|root| p.starts_with(root))
        }) {
            return stable;
        }
    }
    exe.to_owned()
}
fn snippet(client: &str, exe: &Path) -> Value {
    match client {
        "zed" => json!({"context_servers":{"xflow":{"command":exe,"args":["mcp"],"env":{}}}}),
        "vscode" => json!({"servers":{"xflow":{"type":"stdio","command":exe,"args":["mcp"]}}}),
        _ => json!({"mcpServers":{"xflow":entry(exe)}}),
    }
}
fn add_args(client: &str, exe: &Path) -> Vec<String> {
    let exe = exe.to_string_lossy().into_owned();
    let raw: Vec<&str> = match client {
        "claude-code" => vec!["mcp", "add", "--scope", "user", "xflow", "--", &exe, "mcp"],
        "codex" => vec!["mcp", "add", "xflow", "--", &exe, "mcp"],
        "gemini" => vec!["mcp", "add", "-s", "user", "xflow", &exe, "mcp"],
        _ => vec![],
    };
    if client == "vscode" {
        return vec![
            "--add-mcp".into(),
            json!({"name":"xflow","type":"stdio","command":exe,"args":["mcp"]}).to_string(),
        ];
    }
    raw.into_iter().map(str::to_owned).collect()
}
fn print_command(program: &str, args: &[String]) {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    println!(
        "{} {}",
        quote(program),
        args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ")
    );
}
fn run_cli(client: &str, args: &[String]) -> Result<bool> {
    match Command::new(program(client)).args(args).output() {
        Ok(out) if out.status.success() => Ok(true),
        Ok(_) => {
            bail!("{client} registration command failed; client config may need manual inspection")
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn get_cli(client: &str) -> Result<Option<Value>> {
    let args = match client {
        "claude-code" => vec!["mcp", "get", "xflow"],
        "codex" => vec!["mcp", "get", "xflow", "--json"],
        "gemini" => vec!["mcp", "list"],
        _ => return Ok(None),
    };
    match Command::new(program(client)).args(args).output() {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Ok(v) = serde_json::from_str(&s) {
                return Ok(Some(v));
            }
            // Only use the xflow registration from human-readable client output.
            let relevant = if client == "gemini" {
                s.lines().find(|l| l.contains("xflow:")).unwrap_or("")
            } else {
                &s
            };
            let command = relevant
                .lines()
                .find_map(|l| l.trim().strip_prefix("Command:").map(str::trim))
                .or_else(|| {
                    relevant
                        .split_once("xflow:")
                        .map(|(_, s)| s.trim().split(" mcp").next().unwrap_or(""))
                });
            let args = relevant
                .lines()
                .find_map(|l| l.trim().strip_prefix("Args:").map(str::trim))
                .map(|a| a.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
                .unwrap_or_else(|| {
                    if client == "gemini" && relevant.contains(" mcp") {
                        vec!["mcp".into()]
                    } else {
                        vec![]
                    }
                });
            Ok(command.map(|c| json!({"command":c,"args":args})))
        }
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn registered(client: &str) -> Result<Option<Value>> {
    if let Some(v) = get_cli(client)? {
        return Ok(Some(v));
    }
    if let Ok((path, key)) = config(client) {
        if let Some(text) = read_file(&path)? {
            if client == "codex" {
                let v = text.parse::<toml::Value>()?;
                return Ok(v
                    .get(key)
                    .and_then(|v| v.get("xflow"))
                    .map(|v| serde_json::to_value(v).unwrap_or(Value::Null)));
            }
            let v: Value =
                serde_json::from_str(&text).context("Cannot parse client JSON; left unchanged")?;
            return Ok(v.get(key).and_then(|v| v.get("xflow")).cloned());
        }
    }
    Ok(None)
}
pub(super) fn install(
    client: &str,
    dictations: bool,
    force: bool,
    print: bool,
    consent: &Path,
) -> Result<()> {
    validate(client)?;
    if client == "chatgpt" {
        println!("Use `xflow mcp install codex` for Codex in ChatGPT desktop. Plain ChatGPT chat: export arrives later. Advanced, user-run Secure MCP Tunnel: https://developers.openai.com/api/docs/guides/secure-mcp-tunnels (results transit OpenAI; not tested by xflow). No public tunnel is needed.");
        return Ok(());
    }
    let exe = stable_path(&std::env::current_exe()?);
    if exe.components().any(|c| c.as_os_str() == "target")
        || exe.starts_with(std::env::temp_dir())
        || exe.starts_with("/private/tmp")
    {
        eprintln!("Warning: {} is a development or temporary binary; reinstall MCP from a stable installation",exe.display());
    }
    let args = add_args(client, &exe);
    if print {
        if args.is_empty() {
            println!("{}", serde_json::to_string_pretty(&snippet(client, &exe))?);
        } else {
            print_command(&program(client), &args);
        }
        println!("Global dictation consent is unchanged. Enable with `xflow mcp sources --dictations on`; matching text goes to {}.",vendor(client));
        return Ok(());
    }
    if matches!(client, "zed" | "lm-studio" | "windsurf") {
        let mut sources = read_sources(consent);
        if dictations || (!sources.dictations && confirm(&format!("Share your dictation history with {client}? When you ask about it, matching text is sent to {}.",vendor(client)))?) {
            sources.dictations = true;
            write_sources(consent, sources)?;
        }
        println!("{}", serde_json::to_string_pretty(&snippet(client, &exe))?);
        if !sources.dictations {
            println!("Sources stay off. Enable with `xflow mcp sources --dictations on`.");
        }
        println!(
            "Consent is global across all MCP clients; matching text is sent to {}.",
            vendor(client)
        );
        return Ok(());
    }
    let mut replace = force;
    let previous = if matches!(client, "claude-code" | "codex" | "gemini" | "vscode") {
        registered(client)?
    } else {
        None
    };
    let desired = entry(&exe);
    if let Some(previous) = &previous {
        let existing = previous.get("transport").unwrap_or(previous);
        if existing.get("command") != desired.get("command")
            || existing.get("args") != desired.get("args")
        {
            eprintln!(
                "Existing xflow command: {}\nNew xflow: {desired}",
                existing.get("command").unwrap_or(&Value::Null)
            );
            if !replace && !confirm("Replace this xflow registration?")? {
                bail!("Kept existing registration; use --force");
            }
            replace = true;
        }
    }
    let mut sources = read_sources(consent);
    if dictations || (!sources.dictations && confirm(&format!("Share your dictation history with {client}? When you ask about it, matching text is sent to {}.",vendor(client)))?) {
        sources.dictations=true; write_sources(consent,sources)?;
    }
    if !sources.dictations {
        println!("Sources stay off. Enable with `xflow mcp sources --dictations on`.");
    }
    println!(
        "Consent is global across all MCP clients; matching text is sent to {}.",
        vendor(client)
    );
    match client {
        "cursor" | "claude-desktop" => {
            let (path, key) = config(client)?;
            if client == "claude-desktop" && !path.parent().is_some_and(Path::is_dir) {
                println!(
                    "Claude Desktop directory is absent; add {}",
                    snippet(client, &exe)
                );
                return Ok(());
            }
            json_edit(&path, key, Some(desired), force)
        }
        _ => {
            if run_cli(client, &args)? {
                return Ok(());
            }
            if client == "codex" {
                let (path, _) = config(client)?;
                codex_edit(&path, Some(&exe), replace)
            } else {
                println!("Client CLI missing; run:");
                print_command(&program(client), &args);
                Ok(())
            }
        }
    }
}
pub(super) fn uninstall(client: &str) -> Result<()> {
    validate(client)?;
    match client {
        "cursor" | "claude-desktop" | "vscode" => {
            let (p, k) = config(client)?;
            json_edit(&p, k, None, true)
        }
        "claude-code" | "codex" | "gemini" => {
            let args: Vec<String> = match client {
                "claude-code" => vec!["mcp", "remove", "--scope", "user", "xflow"],
                "gemini" => vec!["mcp", "remove", "-s", "user", "xflow"],
                _ => vec!["mcp", "remove", "xflow"],
            }
            .into_iter()
            .map(str::to_owned)
            .collect();
            if run_cli(client, &args)? {
                return Ok(());
            }
            if client == "codex" {
                let (p, _) = config(client)?;
                codex_edit(&p, None, true)
            } else {
                print_command(&program(client), &args);
                Ok(())
            }
        }
        _ => {
            println!("Remove only the xflow entry in {client}'s MCP settings.");
            Ok(())
        }
    }
}
pub(super) fn status(consent: &Path) -> Result<()> {
    println!(
        "This binary: {} ({})",
        std::env::current_exe()?.display(),
        env!("CARGO_PKG_VERSION")
    );
    let s = read_sources(consent);
    println!(
        "Global consent: dictations={}, meetings={}",
        s.dictations, s.meetings
    );
    for client in CLIENTS {
        let registration = match registered(client) {
            Ok(v) => v,
            Err(_) => {
                println!("{client}: registration unreadable");
                continue;
            }
        };
        let Some(v) = registration else {
            println!(
                "{client}: not registered{}",
                if matches!(client, "zed" | "lm-studio" | "windsurf" | "chatgpt") {
                    " (manual/guidance only; inspect client settings)"
                } else {
                    ""
                }
            );
            continue;
        };
        let transport = v.get("transport").unwrap_or(&v);
        let Some(command) = transport.get("command").and_then(Value::as_str) else {
            println!("{client}: registered; command could not be read");
            continue;
        };
        let exists = Path::new(command).is_file();
        let version = if exists {
            Command::new(command)
                .arg("--version")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_else(|| "unavailable".into())
        } else {
            "unavailable".into()
        };
        println!(
            "{client}: registered command={command:?}, exists={exists}, version={version}{}",
            if version != format!("xflow {}", env!("CARGO_PKG_VERSION")) {
                " WARNING: version differs from this binary"
            } else {
                ""
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_merge_backup_permissions_and_removal() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("mcp.json");
        fs::write(
            &p,
            "{\"z\":1,\"mcpServers\":{\"other\":{},\"xflow\":{\"command\":\"old\"}},\"a\":2}",
        )
        .unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(json_edit(&p, "mcpServers", Some(entry(Path::new("new"))), false).is_err());
        json_edit(&p, "mcpServers", Some(entry(Path::new("new"))), true).unwrap();
        let first = fs::read_to_string(&p).unwrap();
        assert!(first.find("\"z\"").unwrap() < first.find("\"a\"").unwrap());
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 2);
        json_edit(&p, "mcpServers", Some(entry(Path::new("new"))), true).unwrap();
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 2);
        json_edit(&p, "mcpServers", None, true).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v["mcpServers"].get("other").is_some());
        assert!(v["mcpServers"].get("xflow").is_none());
        fs::write(&p, "{ // JSONC\n}").unwrap();
        assert!(json_edit(&p, "mcpServers", Some(entry(Path::new("new"))), true).is_err());
        let p = d.path().join("new.json");
        json_edit(&p, "mcpServers", Some(entry(Path::new("new"))), false).unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[test]
    fn codex_preserves_comments_and_cellar_path() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("codex.toml");
        fs::write(
            &p,
            "# keep me\nmodel = 'test'\n[mcp_servers.other]\ncommand='other'\n",
        )
        .unwrap();
        codex_edit(&p, Some(Path::new("/bin/xflow")), false).unwrap();
        let s = fs::read_to_string(&p).unwrap();
        assert!(s.contains("# keep me"));
        assert!(s.contains("mcp_servers.other"));
        assert!(!s.contains("--sources"));
        codex_edit(&p, None, true).unwrap();
        assert!(fs::read_to_string(&p)
            .unwrap()
            .contains("mcp_servers.other"));
        fs::write(&p, "mcp_servers = 42\n").unwrap();
        assert!(codex_edit(&p, Some(Path::new("/bin/xflow")), true).is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "mcp_servers = 42\n");
        let exe = d.path().join("Cellar/xflow/1/bin/xflow");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();
        fs::create_dir(d.path().join("bin")).unwrap();
        let stable = d.path().join("bin/xflow");
        symlink(&exe, &stable).unwrap();
        assert_eq!(stable_path(&exe), stable);
    }
}
