use super::{read_sources, write_sources};
use crate::context::Sources;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    fs,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) const CLIENTS: [&str; 10] = [
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
/// Clients whose configuration xflow only prints (or explains); it never edits them.
pub(crate) fn print_only(client: &str) -> bool {
    matches!(client, "zed" | "lm-studio" | "windsurf" | "chatgpt")
}
pub(crate) const CHATGPT: &str = "Use `xflow mcp install codex` for Codex in ChatGPT desktop. Plain ChatGPT chat: export arrives later. Advanced, user-run Secure MCP Tunnel: https://developers.openai.com/api/docs/guides/secure-mcp-tunnels (results transit OpenAI; not tested by xflow). No public tunnel is needed.";
/// What an install or uninstall did, for the CLI to print and the TUI to show.
#[derive(Debug, Default)]
pub(crate) struct Report {
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
    /// A command or snippet the user must run or add by hand.
    pub manual: Option<String>,
}
/// A different xflow entry exists and replacing it was not confirmed; nothing changed.
#[derive(Debug)]
pub(crate) struct Replace {
    pub existing: String,
}
impl std::fmt::Display for Replace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Kept existing registration ({}); use --force to replace it",
            self.existing
        )
    }
}
impl std::error::Error for Replace {}
#[derive(Debug)]
pub(crate) enum Registration {
    Missing,
    Unreadable,
    NoCommand,
    Command {
        command: String,
        exists: bool,
        version: String,
    },
}
impl Registration {
    pub(crate) fn version_differs(&self) -> bool {
        matches!(self, Self::Command { version, .. } if *version != format!("xflow {}", env!("CARGO_PKG_VERSION")))
    }
}
#[derive(Debug)]
pub(crate) struct ClientStatus {
    pub client: &'static str,
    pub detected: bool,
    pub registration: Registration,
    /// Print-only clients: the snippet or guidance to apply by hand.
    pub manual: Option<String>,
}
#[derive(Debug)]
pub(crate) struct Status {
    pub exe: PathBuf,
    pub version: &'static str,
    pub sources: Sources,
    pub clients: Vec<ClientStatus>,
}
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
            "lm-studio" => "lms",
            other => other,
        }
        .into()
    })
}
fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}
/// `ask` is false when a caller (the TUI) owns the terminal and asks itself.
fn confirm(ask: bool, message: &str) -> Result<bool> {
    if !ask || !interactive() {
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
/// First number a rewrite through `Value` would change. `text` must already parse as JSON.
fn lossy_number(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let (mut i, mut in_string) = (0, false);
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_string => i += 1,
            b'"' => in_string = !in_string,
            b'-' | b'0'..=b'9' if !in_string => {
                let len = bytes[i..]
                    .iter()
                    .position(|b| !matches!(b, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'))
                    .unwrap_or(bytes.len() - i);
                let token = &text[i..i + len];
                let exact = if token.bytes().all(|b| b == b'-' || b.is_ascii_digit()) {
                    token.parse::<i64>().is_ok() || token.parse::<u64>().is_ok()
                } else {
                    serde_json::from_str::<serde_json::Number>(token)
                        .is_ok_and(|n| n.to_string() == token)
                };
                if !exact {
                    return Some(token);
                }
                i += len - 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}
/// Ok(false) when nothing needed to change.
fn json_edit(path: &Path, key: &str, new: Option<Value>, force: bool, ask: bool) -> Result<bool> {
    let old = read_file(path)?;
    let manual = || format!("Add this snippet manually: {}", json!({key:{"xflow":new}}));
    let mut doc: Value = match &old {
        Some(s) => serde_json::from_str(s).with_context(|| {
            format!(
                "Cannot parse {}; JSONC/trailing commas are not supported. {}",
                path.display(),
                manual()
            )
        })?,
        None => json!({}),
    };
    let object = doc
        .as_object_mut()
        .context("Client config must be a JSON object")?;
    if !object.contains_key(key) && new.is_none() {
        return Ok(false);
    }
    let servers = object
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Server config must be an object")?;
    let previous = servers.get("xflow");
    if previous == new.as_ref() {
        return Ok(false);
    }
    if let Some(number) = old.as_deref().and_then(lossy_number) {
        bail!(
            "Cannot rewrite {} without changing the number {number}. {}",
            path.display(),
            manual()
        );
    }
    if let (Some(previous), Some(new)) = (previous, &new) {
        let existing = previous.get("command").unwrap_or(&Value::Null).to_string();
        if !force
            && !confirm(
                ask,
                &format!("Existing xflow command: {existing}\nNew xflow: {new}\nReplace this xflow registration?"),
            )?
        {
            return Err(Replace { existing }.into());
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
    save(path, &bytes, old.as_deref())?;
    Ok(true)
}
fn codex_edit(path: &Path, exe: Option<&Path>, force: bool, ask: bool) -> Result<()> {
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
            let existing = previous
                .get("command")
                .and_then(toml_edit::Item::as_str)
                .unwrap_or("unknown")
                .to_owned();
            if !force
                && !confirm(
                    ask,
                    &format!(
                        "Existing xflow command: {existing}\nNew xflow: {}\nReplace this xflow registration?",
                        entry(Path::new(exe))
                    ),
                )?
            {
                return Err(Replace { existing }.into());
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
fn command_line(program: &str, args: &[String]) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    format!(
        "{} {}",
        quote(program),
        args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ")
    )
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
/// `ask` lets a terminal prompt for consent and replacement; without it nothing is
/// asked, consent stays as it is, and a differing entry fails with [`Replace`].
pub(crate) fn install(
    client: &str,
    dictations: bool,
    force: bool,
    print: bool,
    ask: bool,
    consent: &Path,
) -> Result<Report> {
    validate(client)?;
    let mut report = Report::default();
    if client == "chatgpt" {
        report.notes.push(CHATGPT.into());
        return Ok(report);
    }
    let exe = stable_path(&std::env::current_exe()?);
    if exe.components().any(|c| c.as_os_str() == "target")
        || exe.starts_with(std::env::temp_dir())
        || exe.starts_with("/private/tmp")
    {
        report.warnings.push(format!("Warning: {} is a development or temporary binary; reinstall MCP from a stable installation",exe.display()));
    }
    let args = add_args(client, &exe);
    if print {
        report.manual = Some(if args.is_empty() {
            serde_json::to_string_pretty(&snippet(client, &exe))?
        } else {
            command_line(&program(client), &args)
        });
        report.notes.push(format!("Global dictation consent is unchanged. Enable with `xflow mcp sources --dictations on`; matching text goes to {}.",vendor(client)));
        return Ok(report);
    }
    if matches!(client, "zed" | "lm-studio" | "windsurf") {
        let mut sources = read_sources(consent);
        if dictations || (!sources.dictations && confirm(ask, &format!("Share your dictation history with {client}? When you ask about it, matching text is sent to {}.",vendor(client)))?) {
            sources.dictations = true;
            write_sources(consent, sources)?;
        }
        report.manual = Some(serde_json::to_string_pretty(&snippet(client, &exe))?);
        if !sources.dictations {
            report
                .notes
                .push("Sources stay off. Enable with `xflow mcp sources --dictations on`.".into());
        }
        report.notes.push(format!(
            "Consent is global across all MCP clients; matching text is sent to {}.",
            vendor(client)
        ));
        return Ok(report);
    }
    let mut replace = force;
    let mut differs = false;
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
            let command = existing.get("command").unwrap_or(&Value::Null).to_string();
            if !replace
                && !confirm(
                    ask,
                    &format!("Existing xflow command: {command}\nNew xflow: {desired}\nReplace this xflow registration?"),
                )?
            {
                return Err(Replace { existing: command }.into());
            }
            replace = true;
            differs = true;
        }
    }
    let mut sources = read_sources(consent);
    if dictations || (!sources.dictations && confirm(ask, &format!("Share your dictation history with {client}? When you ask about it, matching text is sent to {}.",vendor(client)))?) {
        sources.dictations=true; write_sources(consent,sources)?;
    }
    if !sources.dictations {
        report
            .notes
            .push("Sources stay off. Enable with `xflow mcp sources --dictations on`.".into());
    }
    report.notes.push(format!(
        "Consent is global across all MCP clients; matching text is sent to {}.",
        vendor(client)
    ));
    if client == "claude-code" {
        if let Some(old) = previous.as_ref().filter(|_| differs) {
            claude_replace(&args, old)?;
            return Ok(report);
        }
        if previous.is_some() {
            report.notes.push("xflow registration unchanged".into());
            return Ok(report);
        }
    }
    match client {
        "cursor" | "claude-desktop" => {
            let (path, key) = config(client)?;
            if client == "claude-desktop" && !path.parent().is_some_and(Path::is_dir) {
                report
                    .notes
                    .push("Claude Desktop directory is absent; add:".into());
                report.manual = Some(snippet(client, &exe).to_string());
                return Ok(report);
            }
            if !json_edit(&path, key, Some(desired), force, ask)? {
                report.notes.push("xflow registration unchanged".into());
            }
        }
        _ => {
            if run_cli(client, &args)? {
                return Ok(report);
            }
            if client == "codex" {
                let (path, _) = config(client)?;
                codex_edit(&path, Some(&exe), replace, ask)?;
            } else {
                report.notes.push("Client CLI missing; run:".into());
                report.manual = Some(command_line(&program(client), &args));
            }
        }
    }
    Ok(report)
}
/// Claude Code refuses `mcp add` for an existing name, so remove first and
/// put the old entry back if the new add fails.
fn claude_replace(args: &[String], old: &Value) -> Result<()> {
    let remove = ["mcp", "remove", "--scope", "user", "xflow"].map(String::from);
    if !run_cli("claude-code", &remove)? {
        bail!("Claude Code CLI missing");
    }
    let Err(e) = run_cli("claude-code", args) else {
        return Ok(());
    };
    let old = old.get("transport").unwrap_or(old);
    let mut back: Vec<String> = ["mcp", "add", "--scope", "user", "xflow", "--"]
        .map(String::from)
        .into();
    back.push(old["command"].as_str().unwrap_or_default().to_owned());
    back.extend(
        old["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(str::to_owned)),
    );
    match run_cli("claude-code", &back) {
        Ok(true) => Err(e.context("Kept the previous Claude Code registration")),
        _ => Err(e.context("Previous Claude Code registration was removed; reinstall xflow")),
    }
}
pub(crate) fn uninstall(client: &str) -> Result<Report> {
    validate(client)?;
    let mut report = Report::default();
    match client {
        "cursor" | "claude-desktop" | "vscode" => {
            let (p, k) = config(client)?;
            json_edit(&p, k, None, true, false)?;
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
                return Ok(report);
            }
            if client == "codex" {
                let (p, _) = config(client)?;
                codex_edit(&p, None, true, false)?;
            } else {
                report.manual = Some(command_line(&program(client), &args));
            }
        }
        _ => report.notes.push(format!(
            "Remove only the xflow entry in {client}'s MCP settings."
        )),
    }
    Ok(report)
}
fn on_path(program: &str) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return path.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
}
/// The client's CLI is on PATH or its settings directory exists.
fn detected(client: &str) -> bool {
    let dir = match client {
        "zed" => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home().ok().map(|h| h.join(".config")))
            .map(|c| c.join("zed")),
        "lm-studio" => home().ok().map(|h| h.join(".lmstudio")),
        "windsurf" => home().ok().map(|h| h.join(".codeium/windsurf")),
        "chatgpt" => return false,
        _ => config(client)
            .ok()
            .and_then(|(p, _)| p.parent().map(Path::to_owned)),
    };
    dir.is_some_and(|d| d.is_dir()) || on_path(&program(client))
}
fn version_of(command: &str) -> String {
    Command::new(command)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".into())
}
/// Consent, every client's registration and the registered binaries' versions.
/// Runs client CLIs and registered binaries, so keep it off any UI thread.
pub(crate) fn status(consent: &Path) -> Result<Status> {
    let exe = std::env::current_exe()?;
    let stable = stable_path(&exe);
    let clients = CLIENTS
        .iter()
        .map(|&client| {
            let registration = match registered(client) {
                Err(_) => Registration::Unreadable,
                Ok(None) => Registration::Missing,
                Ok(Some(v)) => {
                    let transport = v.get("transport").unwrap_or(&v);
                    match transport.get("command").and_then(Value::as_str) {
                        None => Registration::NoCommand,
                        Some(command) => {
                            let exists = Path::new(command).is_file();
                            Registration::Command {
                                command: command.into(),
                                exists,
                                version: if exists {
                                    version_of(command)
                                } else {
                                    "unavailable".into()
                                },
                            }
                        }
                    }
                }
            };
            let manual = match client {
                "chatgpt" => Some(CHATGPT.into()),
                "zed" | "lm-studio" | "windsurf" => {
                    serde_json::to_string_pretty(&snippet(client, &stable)).ok()
                }
                _ => None,
            };
            ClientStatus {
                client,
                detected: detected(client),
                registration,
                manual,
            }
        })
        .collect();
    Ok(Status {
        exe,
        version: env!("CARGO_PKG_VERSION"),
        sources: read_sources(consent),
        clients,
    })
}
pub(crate) fn print_report(report: &Report) {
    for line in &report.warnings {
        eprintln!("{line}");
    }
    for line in &report.notes {
        println!("{line}");
    }
    if let Some(manual) = &report.manual {
        println!("{manual}");
    }
}
pub(crate) fn print_status(status: &Status) {
    println!("This binary: {} ({})", status.exe.display(), status.version);
    println!(
        "Global consent: dictations={}, meetings={}",
        status.sources.dictations, status.sources.meetings
    );
    for c in &status.clients {
        let (client, found) = (c.client, if c.detected { "found" } else { "not found" });
        match &c.registration {
            Registration::Unreadable => println!("{client}: {found}; registration unreadable"),
            Registration::Missing => println!(
                "{client}: {found}; not registered{}",
                if print_only(client) {
                    " (manual/guidance only; inspect client settings)"
                } else {
                    ""
                }
            ),
            Registration::NoCommand => {
                println!("{client}: {found}; registered; command could not be read")
            }
            Registration::Command {
                command,
                exists,
                version,
            } => println!(
                "{client}: {found}; registered command={command:?}, exists={exists}, version={version}{}",
                if c.registration.version_differs() {
                    " WARNING: version differs from this binary"
                } else {
                    ""
                }
            ),
        }
    }
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
        let refused = json_edit(
            &p,
            "mcpServers",
            Some(entry(Path::new("new"))),
            false,
            false,
        );
        assert!(refused.unwrap_err().downcast_ref::<Replace>().is_some());
        json_edit(&p, "mcpServers", Some(entry(Path::new("new"))), true, false).unwrap();
        let first = fs::read_to_string(&p).unwrap();
        assert!(first.find("\"z\"").unwrap() < first.find("\"a\"").unwrap());
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 2);
        json_edit(&p, "mcpServers", Some(entry(Path::new("new"))), true, false).unwrap();
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 2);
        json_edit(&p, "mcpServers", None, true, false).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v["mcpServers"].get("other").is_some());
        assert!(v["mcpServers"].get("xflow").is_none());
        fs::write(&p, "{ // JSONC\n}").unwrap();
        assert!(json_edit(&p, "mcpServers", Some(entry(Path::new("new"))), true, false).is_err());
        let p = d.path().join("new.json");
        json_edit(
            &p,
            "mcpServers",
            Some(entry(Path::new("new"))),
            false,
            false,
        )
        .unwrap();
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
        codex_edit(&p, Some(Path::new("/bin/xflow")), false, false).unwrap();
        let s = fs::read_to_string(&p).unwrap();
        assert!(s.contains("# keep me"));
        assert!(s.contains("mcp_servers.other"));
        assert!(!s.contains("--sources"));
        codex_edit(&p, None, true, false).unwrap();
        assert!(fs::read_to_string(&p)
            .unwrap()
            .contains("mcp_servers.other"));
        fs::write(&p, "mcp_servers = 42\n").unwrap();
        assert!(codex_edit(&p, Some(Path::new("/bin/xflow")), true, false).is_err());
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
