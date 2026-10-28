use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use xflow_core::config::Config;

#[derive(Parser)]
#[command(version, about = "Local dictation daemon")]
#[cfg_attr(
    debug_assertions,
    command(
        after_help = "Development only: WAV capture without a microphone\n  XFLOW_TEST_AUDIO=/absolute/path/to.wav xflowd --config /path/to/qa.toml\nReloads the WAV on each recording; native STT, formatting and paste still run.\nDo not enable test-support or set XFLOW_TEST_SINK for desktop QA.\nRelease builds ignore XFLOW_TEST_AUDIO. See docs/test-audio.md."
    )
)]
#[cfg_attr(
    not(debug_assertions),
    command(
        after_help = "Release build: XFLOW_TEST_AUDIO is ignored. Use a debug build for WAV capture QA (docs/test-audio.md)."
    )
)]
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
    // Keys older builds left in the Keychain move once, silently; one that would
    // need approval stays put (`xflow key status` moves it in the foreground).
    std::thread::spawn(|| xflow_providers::migrate_keychain(None));
    #[cfg(unix)]
    {
        xflow_app::daemon::serve_with_path(config, path).await
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("native Windows daemon adapter is not implemented yet")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn help_explains_test_audio_build_gate() {
        let help = Args::command().render_long_help().to_string();
        assert!(help.contains("XFLOW_TEST_AUDIO"));
        assert!(help.contains(if cfg!(debug_assertions) {
            "Development only"
        } else {
            "Release build: XFLOW_TEST_AUDIO is ignored"
        }));
    }
}
