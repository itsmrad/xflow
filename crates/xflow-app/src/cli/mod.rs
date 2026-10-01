mod args;
mod config;
mod doctor;
mod ipc;
mod output;
mod provider;
mod setup;
mod system;

use args::{Cli, Command};
use clap::{CommandFactory, FromArgMatches};
use output::Output;
use std::{io::Write, path::PathBuf, process::ExitCode};
use xflow_core::ipc::Request;

type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
struct Error {
    code: u8,
    message: String,
    hint: String,
    source: Option<anyhow::Error>,
}
impl Error {
    fn new(code: u8, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: hint.into(),
            source: None,
        }
    }
    fn config(error: impl std::fmt::Display) -> Self {
        Self::new(
            4,
            error.to_string(),
            "Run xflow config validate or xflow config show",
        )
    }
    fn provider(error: impl std::fmt::Display) -> Self {
        Self::new(
            5,
            error.to_string(),
            "Check xflow providers info and xflow key status",
        )
    }
    fn daemon(error: impl std::fmt::Display) -> Self {
        Self::new(
            3,
            error.to_string(),
            "Run xflow daemon start or start xflowd in another terminal",
        )
    }
    fn broken_pipe(&self) -> bool {
        self.source.as_ref().is_some_and(|e| {
            e.chain().any(|e| {
                e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
            })
        })
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl From<anyhow::Error> for Error {
    fn from(source: anyhow::Error) -> Self {
        Self {
            code: 1,
            message: source.to_string(),
            hint: "Run xflow <command> --help for usage".into(),
            source: Some(source),
        }
    }
}
macro_rules! from_error { ($($kind:ty),*) => {$ (
    impl From<$kind> for Error { fn from(error: $kind) -> Self { anyhow::Error::from(error).into() } }
)*}; }
from_error!(
    std::io::Error,
    serde_json::Error,
    std::string::FromUtf8Error,
    tokio::time::error::Elapsed,
    tempfile::PersistError
);

pub fn main() -> ExitCode {
    let argv: Vec<_> = std::env::args_os().collect();
    let json = argv.iter().any(|v| v == "--json");
    let color = argv
        .windows(2)
        .find(|v| v[0] == "--color")
        .and_then(|v| v[1].to_str())
        .or_else(|| {
            argv.iter()
                .filter_map(|v| v.to_str())
                .find_map(|v| v.strip_prefix("--color="))
        });
    let clap_color = match color {
        Some("never") => clap::ColorChoice::Never,
        Some("always") => clap::ColorChoice::Always,
        _ => clap::ColorChoice::Auto,
    };
    let matches = match Cli::command().color(clap_color).try_get_matches_from(argv) {
        Ok(matches) => matches,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                return ExitCode::SUCCESS;
            }
            if json {
                let _ = writeln!(
                    std::io::stderr(),
                    "{}",
                    serde_json::json!({"ok":false,"error":error.to_string(),"hint":"Run xflow --help","exit_code":2})
                );
            } else {
                let _ = error.print();
            }
            return ExitCode::from(2);
        }
    };
    let cli = Cli::from_arg_matches(&matches).expect("clap validated arguments");
    let out = Output::new(cli.json, cli.quiet, cli.color);
    match run(&cli, &out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.broken_pipe() => ExitCode::SUCCESS,
        Err(error) => {
            if out.json {
                let _ = writeln!(
                    std::io::stderr(),
                    "{}",
                    serde_json::json!({"ok":false,"error":error.message,"hint":error.hint,"exit_code":error.code})
                );
            } else {
                eprintln!(
                    "error: {}\nhint: {}",
                    output::clean(&error.message),
                    output::clean(&error.hint)
                );
            }
            ExitCode::from(error.code)
        }
    }
}
fn path(cli: &Cli) -> Result<PathBuf> {
    let path = match &cli.config {
        Some(path) => path.clone(),
        None => xflow_app::paths::config_path()?,
    };
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}
fn run(cli: &Cli, out: &Output) -> Result<()> {
    match &cli.command {
        Command::Completions { shell } => {
            let mut bytes = Vec::new();
            clap_complete::generate(*shell, &mut Cli::command(), "xflow", &mut bytes);
            let script = String::from_utf8(bytes)?;
            out.data(
                &serde_json::json!({"shell":shell.to_string(),"script":script}),
                script.trim_end(),
            )?;
            Ok(())
        }
        Command::Start(mode) | Command::Toggle(mode) => {
            let response = ipc::request(&ipc::mode_request(
                matches!(cli.command, Command::Toggle(_)),
                mode.command,
                xflow_core::ipc::Delivery::Inject,
            ))?;
            if out.json {
                out.response(&response)?;
            } else {
                out.confirm(&format!("{:?}", response.state).to_lowercase())?;
            }
            Ok(())
        }
        Command::Stop
        | Command::Cancel
        | Command::Status
        | Command::Last
        | Command::Copy
        | Command::Paste
        | Command::Stats
        | Command::Shutdown => {
            let request = match cli.command {
                Command::Stop => Request::Stop,
                Command::Cancel => Request::Cancel,
                Command::Status => Request::Status,
                Command::Last => Request::Last,
                Command::Copy => Request::CopyLast,
                Command::Paste => Request::PasteLast,
                Command::Stats => Request::Stats,
                _ => Request::Shutdown,
            };
            let response = ipc::request(&request)?;
            if matches!(
                cli.command,
                Command::Stop
                    | Command::Cancel
                    | Command::Copy
                    | Command::Paste
                    | Command::Shutdown
            ) && !out.json
            {
                out.confirm(
                    &response
                        .message
                        .unwrap_or_else(|| format!("{:?}", response.state).to_lowercase()),
                )?;
            } else {
                out.response(&response)?;
            }
            Ok(())
        }
        Command::Watch => ipc::block_on(ipc::watch(out)),
        Command::Listen {
            once,
            seconds,
            command,
        } => ipc::block_on(ipc::listen(*once, *seconds, *command, out)),
        Command::Tui => ipc::block_on(async { xflow_app::tui::run().await.map_err(Into::into) }),
        Command::History { page, action } => ipc::history(page, action.as_ref(), out),
        Command::ClearHistory { yes } => {
            config::confirm(*yes, "Delete all dictation history?")?;
            out.response(&ipc::request(&Request::ClearHistory)?)?;
            Ok(())
        }
        Command::Config { action } => config::run(&path(cli)?, action.as_ref(), out),
        Command::Dictionary { action } => config::dictionary(&path(cli)?, action.as_ref(), out),
        Command::Snippets { action } => config::snippets(&path(cli)?, action.as_ref(), out),
        Command::Styles { action } => config::styles(&path(cli)?, action.as_ref(), out),
        Command::Overlay { action } => system::overlay(action.as_ref(), out),
        Command::Providers { action } => provider::run(&path(cli)?, action.as_ref(), out),
        Command::Models { provider } => provider::models(&path(cli)?, provider.as_deref(), out),
        Command::Key {
            action,
            provider,
            stdin,
        } => provider::key(
            &path(cli)?,
            action.as_ref(),
            provider.as_deref(),
            *stdin,
            out,
        ),
        Command::Devices => provider::devices(out),
        Command::Transcribe {
            file,
            provider,
            model,
            language,
        } => provider::transcribe(
            &path(cli)?,
            file,
            provider.as_deref(),
            model.as_deref(),
            language.as_deref(),
            out,
        ),
        Command::Doctor => doctor::run(&path(cli)?, out),
        Command::Setup(options) => setup::run(&path(cli)?, options, out),
        Command::Daemon { action } => system::daemon(action, out),
        Command::Service { action } => system::service(action, out),
        Command::Extension { action } => system::extension(action, out),
        Command::Init => {
            let path = path(cli)?;
            xflow_app::paths::private_dir(
                path.parent()
                    .ok_or_else(|| Error::config("Config path has no parent"))?,
            )?;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&path)
                .map_err(|_| {
                    Error::new(
                        4,
                        "Could not create config; an existing file is preserved",
                        "Use xflow setup to edit an existing config",
                    )
                })?;
            file.write_all(config::TEMPLATE.as_bytes())?;
            out.confirm(&format!("Created {}", path.display()))?;
            Ok(())
        }
        Command::Version => {
            let daemon = ipc::request(&Request::Status);
            let version = env!("CARGO_PKG_VERSION");
            let human = match &daemon {
                Ok(r) => format!(
                    "xflow {version}\nxflowd {} (protocol {})",
                    r.version.as_deref().unwrap_or("unknown"),
                    r.protocol.unwrap_or_default()
                ),
                Err(e) => format!("xflow {version}\nDaemon unavailable: {}", e.message),
            };
            out.data(&serde_json::json!({"cli_version":version,"protocol":xflow_core::ipc::PROTOCOL_VERSION,"daemon":daemon.ok()}), &human)?;
            Ok(())
        }
    }
}
