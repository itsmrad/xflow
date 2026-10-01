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
}
#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    let args = Args::parse();
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
