use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use xflow_core::config::Config;

#[derive(Parser)]
#[command(version, about = "Local dictation daemon")]
struct Args {
    #[arg(long)]
    config: Option<PathBuf>,
    /// Parse and validate configuration without starting desktop/audio services.
    #[arg(long)]
    check_config: bool,
    /// Internal: print this executable's microphone authorization; `request` also
    /// asks while undecided. xflowd runs it as a child to keep AVFoundation out.
    #[cfg(target_os = "macos")]
    #[arg(long, hide = true, value_parser = ["check", "request"])]
    microphone: Option<String>,
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    #[cfg(target_os = "macos")]
    if let Some(mode) = args.microphone {
        let status = xflow_platform::microphone_permission(false);
        println!("{status}");
        if mode == "request" && status == "not determined" {
            xflow_platform::microphone_permission(true);
        }
        return Ok(());
    }
    let path = args
        .config
        .map(Ok)
        .unwrap_or_else(xflow_app::paths::config_path)?;
    let config = Config::load(&path)?;
    if args.check_config {
        println!("Configuration valid");
        return Ok(());
    }
    #[cfg(unix)]
    {
        xflow_app::daemon::serve_with_path(config, path).await
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("native Windows daemon adapter is not implemented yet")
    }
}
