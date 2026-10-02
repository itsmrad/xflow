use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "xflow",
    version,
    about = "Fast Linux dictation, from your terminal",
    after_help = "Start here: xflow setup\nExplore: xflow <command> --help\nPipe text: xflow listen --seconds 5\nExit codes: 0 success, 1 operation failed, 2 usage, 3 daemon unavailable, 4 config, 5 provider, 6 doctor."
)]
pub struct Cli {
    /// Emit machine-readable JSON (watch emits one object per event).
    #[arg(long, global = true)]
    pub json: bool,
    /// Suppress confirmations; requested data and errors remain visible.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Control colour in human output.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub color: Color,
    /// Read/write an alternate configuration file.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Color {
    Auto,
    Always,
    Never,
}
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Set up a provider, microphone and optional desktop integration.
    Setup(Setup),
    /// Begin recording (bind to shortcut press for push-to-talk).
    Start(ModeArgs),
    /// Stop recording and begin transcription.
    Stop,
    /// Start or stop hands-free recording.
    Toggle(ModeArgs),
    /// Discard the active recording or pending transcription.
    Cancel,
    /// Show daemon state, provider, model, version and timings.
    Status,
    /// Subscribe to daemon events until interrupted.
    Watch,
    /// Print dictation to stdout without changing the desktop.
    Listen {
        /// Finish after one dictation; otherwise repeat in an interactive terminal.
        #[arg(long)]
        once: bool,
        /// Stop after N seconds; otherwise Enter stops recording.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=3600))]
        seconds: Option<u64>,
        #[arg(long)]
        command: bool,
    },
    /// Transcribe an audio file without a running daemon.
    Transcribe {
        file: PathBuf,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        language: Option<String>,
    },
    /// Print the last transcript.
    Last,
    /// Copy the last transcript.
    #[command(visible_alias = "copy-last")]
    Copy,
    /// Insert the last transcript into the focused app.
    #[command(visible_alias = "paste-last")]
    Paste,
    /// Browse, search and export local dictation history.
    History {
        #[command(flatten)]
        page: Page,
        #[command(subcommand)]
        action: Option<History>,
    },
    /// Clear all history (legacy alias for history clear).
    #[command(hide = true)]
    ClearHistory {
        #[arg(long)]
        yes: bool,
    },
    /// Inspect or edit validated, comment-preserving TOML configuration.
    Config {
        #[command(subcommand)]
        action: Option<Config>,
    },
    /// List providers, inspect capabilities and select a default.
    Providers {
        #[command(subcommand)]
        action: Option<Providers>,
    },
    /// List a provider's models, marking the selected and default model.
    Models { provider: Option<String> },
    /// Manage provider credentials in OS credential storage.
    Key {
        #[command(subcommand)]
        action: Option<Key>,
        provider: Option<String>,
        #[arg(long)]
        stdin: bool,
    },
    /// Manage vocabulary and text replacements.
    Dictionary {
        #[command(subcommand)]
        action: Option<Dictionary>,
    },
    /// Manage spoken shortcuts that expand to saved text.
    Snippets {
        #[command(subcommand)]
        action: Option<Snippets>,
    },
    /// Manage cleanup styles selected by focused application.
    Styles {
        #[command(subcommand)]
        action: Option<Styles>,
    },
    /// Inspect and change GNOME overlay/shortcut settings.
    Overlay {
        #[command(subcommand)]
        action: Option<Overlay>,
    },
    /// List available microphone input devices.
    Devices,
    /// Show local dictation statistics.
    Stats,
    /// Diagnose configuration and desktop dependencies without recording.
    Doctor,
    /// Control the systemd user daemon or view its logs.
    Daemon {
        #[command(subcommand)]
        action: Daemon,
    },
    /// Install or uninstall the graphical-session user service.
    Service {
        #[command(subcommand)]
        action: Service,
    },
    /// Install or manage the bundled GNOME Shell extension.
    Extension {
        #[command(subcommand)]
        action: Extension,
    },
    /// Generate shell completion scripts.
    Completions { shell: clap_complete::Shell },
    /// Show CLI and, when available, daemon versions.
    Version,
    /// Open the terminal control centre.
    Tui,
    /// Create the default config without overwriting an existing file.
    #[command(hide = true)]
    Init,
    /// Shut down the daemon (legacy command).
    #[command(hide = true)]
    Shutdown,
}
#[derive(Args, Debug)]
pub struct ModeArgs {
    #[arg(long)]
    pub command: bool,
}
#[derive(Args, Debug, Default)]
pub struct Setup {
    /// Accept defaults without prompts (desktop actions still require flags).
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Full transcription URL for custom/local providers.
    #[arg(long)]
    pub endpoint: Option<String>,
    #[arg(long)]
    pub language: Option<String>,
    #[arg(long)]
    pub device: Option<String>,
    /// Read a key from stdin instead of prompting.
    #[arg(long)]
    pub key_stdin: bool,
    /// Make a non-billable provider connection check.
    #[arg(long)]
    pub test: bool,
    #[arg(long)]
    pub install_extension: bool,
    #[arg(long)]
    pub enable_service: bool,
}
#[derive(Args, Debug)]
pub struct Page {
    #[arg(short = 'n', long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub limit: u16,
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    #[arg(long)]
    pub query: Option<String>,
}
#[derive(Subcommand, Debug)]
pub enum History {
    List(Page),
    Search {
        query: String,
        #[arg(short = 'n', long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    Show {
        id: i64,
    },
    Copy {
        id: i64,
    },
    Paste {
        id: i64,
    },
    Delete {
        id: i64,
    },
    Export {
        #[arg(long, value_enum, default_value = "json")]
        format: Format,
        #[arg(long)]
        query: Option<String>,
    },
    Clear {
        #[arg(long)]
        yes: bool,
    },
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Format {
    Json,
    Csv,
    Md,
}
#[derive(Subcommand, Debug)]
pub enum Config {
    Path,
    Show,
    Get {
        key: String,
    },
    Set {
        key: String,
        value: String,
    },
    Unset {
        key: String,
    },
    Edit,
    Validate,
    Reset {
        #[arg(long)]
        yes: bool,
    },
}
#[derive(Subcommand, Debug)]
pub enum Providers {
    List,
    Info {
        provider: String,
    },
    Test {
        provider: Option<String>,
    },
    Use {
        provider: String,
        #[arg(long)]
        model: Option<String>,
    },
}
#[derive(Subcommand, Debug)]
pub enum Key {
    Set {
        provider: String,
        #[arg(long)]
        stdin: bool,
    },
    Remove {
        provider: String,
    },
    Status {
        provider: Option<String>,
    },
}
#[derive(Subcommand, Debug)]
pub enum Dictionary {
    List,
    Add {
        #[arg(required = true)]
        words: Vec<String>,
    },
    Remove {
        #[arg(required = true)]
        words: Vec<String>,
    },
    Replace {
        from: String,
        to: String,
    },
    Unreplace {
        from: String,
    },
    Import {
        file: PathBuf,
    },
    Export,
}
#[derive(Subcommand, Debug)]
pub enum Snippets {
    List,
    Add { trigger: String, text: String },
    Remove { trigger: String },
}
#[derive(Subcommand, Debug)]
pub enum Styles {
    List,
    Add {
        name: String,
        #[arg(long, required = true, value_delimiter = ',')]
        apps: Vec<String>,
        #[arg(long, value_enum)]
        mode: Option<Cleanup>,
        #[arg(long)]
        prompt: Option<String>,
    },
    Remove {
        name: String,
    },
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Cleanup {
    Raw,
    Light,
    Polished,
    Custom,
}
#[derive(Subcommand, Debug)]
pub enum Overlay {
    List,
    Get { key: String },
    Set { key: String, value: String },
    Reset { key: Option<String> },
}
#[derive(Subcommand, Debug)]
pub enum Daemon {
    Start,
    Stop,
    Restart,
    Status,
    Logs {
        #[arg(short, long)]
        follow: bool,
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: u16,
    },
}
#[derive(Subcommand, Debug)]
pub enum Service {
    Install {
        #[arg(long)]
        enable: bool,
    },
    Uninstall,
}
#[derive(Subcommand, Debug)]
pub enum Extension {
    Install {
        #[arg(long)]
        enable: bool,
    },
    Uninstall,
    Enable,
    Disable,
    Status,
}
#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    #[test]
    fn definition_and_legacy_aliases() {
        Cli::command().debug_assert();
        for args in [
            vec!["xflow", "copy-last"],
            vec!["xflow", "paste-last"],
            vec!["xflow", "key", "groq", "--stdin"],
            vec!["xflow", "history", "-n", "10"],
            vec!["xflow", "config", "set", "stt.model", "whisper-large-v3"],
            vec!["xflow", "history", "search", "hello", "--json"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
        assert!(Cli::try_parse_from(["xflow", "listen", "--seconds", "0"]).is_err());
    }
}
