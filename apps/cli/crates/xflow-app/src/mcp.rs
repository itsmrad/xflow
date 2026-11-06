//! Local stdio transport; no daemon or model session is started.
mod protocol;
mod setup;

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
    },
}
pub fn run(args: &Mcp, config: &Path) -> Result<()> {
    let mut env = ContextEnv::current()?;
    env.config = config.to_owned();
    let consent = config.with_file_name("mcp.toml");
    match &args.action {
        None => {
            let sources = match args.sources.as_deref() {
                Some("none") => Sources::default(),
                Some(s) => Sources {
                    dictations: s.split(',').any(|v| v == "dictations"),
                    meetings: s.split(',').any(|v| v == "meetings"),
                },
                None => read_sources(&consent),
            };
            protocol::serve(&mut env, sources)
        }
        Some(Action::Install {
            client,
            dictations,
            force,
            print,
        }) => setup::install(client, *dictations, *force, *print, &consent),
        Some(Action::Uninstall { client }) => setup::uninstall(client),
        Some(Action::Status) => setup::status(&consent),
        Some(Action::Sources { dictations }) => {
            let mut sources = read_sources(&consent);
            if let Some(v) = dictations {
                sources.dictations = v == "on";
                write_sources(&consent, sources)?;
            }
            println!(
                "Global consent: dictations={}, meetings={} (all MCP clients)",
                sources.dictations, sources.meetings
            );
            Ok(())
        }
    }
}
fn read_sources(path: &Path) -> Sources {
    let value = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.parse::<toml::Value>().ok());
    let enabled = |key| {
        value
            .as_ref()
            .and_then(|v| v.get(key))
            .and_then(toml::Value::as_bool)
            .unwrap_or(false)
    };
    Sources {
        dictations: enabled("dictations"),
        meetings: enabled("meetings"),
    }
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
