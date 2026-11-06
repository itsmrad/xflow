//! Local stdio transport; no daemon or model session is started.
mod protocol;
pub(crate) mod setup;

use crate::context::{ContextEnv, Sources};
use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use std::path::Path;

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Mcp {
    /// Override source consent for this server process only.
    #[arg(long, value_parser = ["dictations", "meetings", "dictations,meetings", "none"])]
    pub sources: Option<String>,
    #[command(subcommand)]
    pub action: Option<Action>,
}
#[derive(Subcommand, Debug)]
#[command(defer = true)]
pub enum Action {
    /// Register this binary with a local MCP client. Consent is global.
    Install {
        client: String,
        /// Explicitly allow sharing dictation history with clients' model vendors.
        #[arg(long)]
        dictations: bool,
        /// Explicitly allow meeting transcripts to be sent to clients' AI providers.
        #[arg(long)]
        meetings: bool,
        /// Replace a different existing xflow entry.
        #[arg(long)]
        force: bool,
        /// Print registration without changing client configuration or consent.
        #[arg(long)]
        print: bool,
    },
    /// Remove only xflow's registration; global consent is kept.
    Uninstall { client: String },
    /// Show consent, registrations and binary versions.
    Status,
    /// Inspect or change global source consent.
    Sources {
        #[arg(long, value_parser = ["on", "off"])]
        dictations: Option<String>,
        #[arg(long, value_parser = ["on", "off"])]
        meetings: Option<String>,
    },
}
pub fn run(args: &Mcp, config: &Path) -> Result<()> {
    let mut env = ContextEnv::current()?;
    env.config = config.to_owned();
    let consent = consent_path(config);
    match &args.action {
        None => {
            let sources = args.sources.as_deref().map(|s| Sources {
                dictations: s.split(',').any(|v| v == "dictations"),
                meetings: s.split(',').any(|v| v == "meetings"),
            });
            protocol::serve(&mut env, sources, &consent)
        }
        Some(Action::Install {
            client,
            dictations,
            meetings,
            force,
            print,
        }) => {
            let report = setup::install(
                client,
                *dictations,
                *meetings,
                *force,
                *print,
                true,
                &consent,
            )?;
            setup::print_report(&report);
            Ok(())
        }
        Some(Action::Uninstall { client }) => {
            setup::print_report(&setup::uninstall(client)?);
            Ok(())
        }
        Some(Action::Status) => {
            setup::print_status(&setup::status(&consent)?);
            Ok(())
        }
        Some(Action::Sources {
            dictations,
            meetings,
        }) => {
            let mut sources = match dictations {
                Some(v) => set_dictations(&consent, v == "on")?,
                None => read_sources(&consent),
            };
            if let Some(v) = meetings {
                sources = set_meetings(&consent, v == "on")?;
            }
            println!(
                "Global consent: dictations={}, meetings={} (all MCP clients)",
                sources.dictations, sources.meetings
            );
            Ok(())
        }
    }
}
/// Global consent lives in `mcp.toml` next to `config.toml`.
pub(crate) fn consent_path(config: &Path) -> std::path::PathBuf {
    config.with_file_name("mcp.toml")
}
/// Turn global dictation consent on or off; other sources are kept.
pub(crate) fn set_dictations(consent: &Path, on: bool) -> Result<Sources> {
    let mut sources = read_sources(consent);
    sources.dictations = on;
    write_sources(consent, sources)?;
    Ok(sources)
}
pub(crate) fn set_meetings(consent: &Path, on: bool) -> Result<Sources> {
    let mut sources = read_sources(consent);
    sources.meetings = on;
    write_sources(consent, sources)?;
    Ok(sources)
}
/// `None` means consent exists but cannot be trusted; a missing file is plain "off".
fn load_sources(path: &Path) -> Option<Sources> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Sources::default()),
        Err(_) => return None,
    };
    let value = text.parse::<toml::Value>().ok()?;
    let enabled = |key| value.get(key).map_or(Some(false), toml::Value::as_bool);
    Some(Sources {
        dictations: enabled("dictations")?,
        meetings: enabled("meetings")?,
    })
}
fn read_sources(path: &Path) -> Sources {
    load_sources(path).unwrap_or_default()
}
fn write_sources(path: &Path, sources: Sources) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Consent path has no directory"))?;
    crate::paths::private_dir(parent)?;
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
        bail!("Unsafe consent file");
    }
    setup::atomic_write(
        path,
        format!(
            "dictations = {}\nmeetings = {}\n",
            sources.dictations, sources.meetings
        )
        .as_bytes(),
        None,
    )
}
