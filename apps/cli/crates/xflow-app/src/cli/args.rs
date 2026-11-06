use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "xflow",
    version,
    about = "Fast dictation for developers, from your terminal",
    after_help = "Start here: xflow (in a terminal: setup, then the control centre)\nExplore: xflow <command> --help\nPipe text: xflow listen --seconds 5\nExit codes: 0 success, 1 operation failed, 2 usage, 3 daemon unavailable, 4 config, 5 provider, 6 doctor."
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
// Construct arguments only for the chosen command, so MCP setup does not tax other launches.
#[derive(Subcommand, Debug)]
#[command(defer = true)]
pub enum Command {
    /// Share read-only dictation context with local MCP clients.
    Mcp(xflow_app::mcp::Mcp),
    /// Ask a question about your dictations; excerpts go to the [ai] provider.
    Ask(Ask),
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
    /// Transcribe the failed dictation kept by privacy.keep_failed_audio again and print it.
    Retry {
        /// Also include the parts that were delivered when it failed.
        #[arg(long)]
        full: bool,
        /// Delete the kept audio instead.
        #[arg(long, conflicts_with = "full")]
        discard: bool,
    },
    /// Browse, search and export local dictation history.
    History {
        #[command(flatten)]
        page: Page,
        #[command(subcommand)]
        action: Option<History>,
    },
    /// Transcribe meeting audio, or import, list, search, show, export or delete
    /// a private meeting transcript.
    ///
    /// Every meeting command uses the local database in this process. Import and
    /// delete do not use the daemon. The import file is read here, with a cap.
    Meeting {
        #[command(subcommand)]
        action: MeetingCmd,
    },
    /// Internal: background meeting transcription started by `meeting process`.
    #[command(hide = true, name = "meeting-worker")]
    MeetingWorker,
    /// Internal: automatic meeting prompts, owned by the daemon control pipe.
    #[command(hide = true, name = "meeting-watch")]
    MeetingWatch,
    /// Internal: consent, Stop indicator and app audio for `meeting record`.
    #[cfg(target_os = "macos")]
    #[command(hide = true, name = "meeting-native")]
    MeetingNative {
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        no_mic: bool,
        #[arg(long)]
        yes: bool,
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
    /// List provider models, inspect the on-device model and hardware, or install/remove it.
    #[command(args_conflicts_with_subcommands = true)]
    Models {
        #[command(subcommand)]
        action: Option<ModelAction>,
        provider: Option<String>,
        /// Report on-device model requirements and host facts without installing or selecting a model.
        #[arg(long, conflicts_with = "provider")]
        on_device: bool,
    },
    /// Manage provider credentials in OS credential storage; a custom cleanup endpoint uses the `cleanup` entry.
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
    /// Inspect and change native overlay settings.
    Overlay {
        #[command(subcommand)]
        action: Option<Overlay>,
    },
    #[cfg(target_os = "macos")]
    #[command(hide = true)]
    OverlayPill,
    /// List available microphone input devices.
    Devices,
    /// Show local dictation statistics.
    Stats,
    /// Diagnose configuration and desktop dependencies without recording.
    Doctor,
    /// Control the systemd/launchd user daemon or view its logs.
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
    /// Open the terminal control centre (also plain `xflow` in a terminal).
    Tui,
    /// Create the default config without overwriting an existing file.
    #[command(hide = true)]
    Init,
    /// Shut down the daemon (legacy command).
    #[command(hide = true)]
    Shutdown,
}
#[derive(Args, Debug)]
pub struct Ask {
    /// The question; read from stdin when omitted.
    pub question: Option<String>,
    /// Use this provider instead of ai.provider.
    #[arg(long)]
    pub provider: Option<String>,
    /// Use this model instead of ai.model or the provider's default.
    #[arg(long)]
    pub model: Option<String>,
    /// Print retrieval, overhead, first-token and total times and tokens to stderr.
    #[arg(long)]
    pub timing: bool,
    /// Send only the question: no transcript search or read tools.
    #[arg(long)]
    pub no_context: bool,
    /// Transcript sources; meetings always require affirmative consent.
    #[arg(long, default_value = "dictations", value_parser = ["none", "dictations", "meetings", "both"])]
    pub sources: String,
    /// Allow this provider to receive transcript excerpts for this run; not saved.
    #[arg(long)]
    pub yes: bool,
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
    /// Correct a transcript (in $EDITOR, or with --text) and learn the corrected words.
    Edit {
        id: i64,
        /// The corrected transcript; omit to edit it in $VISUAL or $EDITOR.
        #[arg(long)]
        text: Option<String>,
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
pub enum MeetingCmd {
    /// Copy WAV audio into private storage and transcribe it into a meeting.
    ///
    /// Each file is one source, such as the microphone and the remote side,
    /// starting at the same moment. Your files are only read. The private copy
    /// is deleted as each part's text is stored; audio not yet transcribed is
    /// kept until `retry` succeeds or you delete the meeting, never expired.
    /// Uses the configured speech-to-text provider, never its backup.
    Process {
        /// One or two WAV files, up to 3 hours each.
        #[arg(required = true, num_args = 1..=2)]
        files: Vec<PathBuf>,
        #[arg(long)]
        title: Option<String>,
        /// Stay and show progress until transcription ends. Without it the
        /// command returns once the audio is stored; transcription continues
        /// in the background even if this terminal closes.
        #[arg(long)]
        wait: bool,
    },
    /// Transcription progress, audio waiting and its age, and audio storage use.
    ///
    /// Restarts background transcription that stopped unexpectedly for
    /// meetings you already asked to transcribe.
    Status {
        /// `store_id:id`; omit for every meeting with audio.
        id: Option<String>,
    },
    /// Recover after a crash, then transcribe waiting audio in the background.
    ///
    /// Failed parts get three new attempts. Audio is only sent to a provider
    /// for meetings you ran this or `process` for.
    Retry {
        /// `store_id:id`; omit for every meeting with waiting audio.
        id: Option<String>,
        /// Stay and show progress until transcription ends.
        #[arg(long)]
        wait: bool,
    },
    /// Read a versioned JSON transcript into the local meeting database.
    Import { file: PathBuf },
    /// Meetings whose start is in `[since, until)`.
    List {
        /// Unix milliseconds, or YYYY-MM-DD when `--timezone` is set.
        #[arg(long, default_value = "0")]
        since: String,
        /// Exclusive end. Unix milliseconds, or YYYY-MM-DD when `--timezone` is set.
        #[arg(long, default_value = "4102444800000")]
        until: String,
        /// Required for a calendar date. `Z` or `±HH:MM`. Never the machine's zone.
        #[arg(long)]
        timezone: Option<String>,
        /// JSON `next` cursor from the previous page.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Literal text search. Letter case is ignored, accents are not.
    Search {
        query: String,
        /// JSON `next` cursor from the previous page.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// One meeting: provenance, sources, gaps and notes.
    Get {
        /// `store_id:id` from list.
        id: String,
    },
    /// Write JSON, Markdown or timestamped text for one frozen snapshot.
    Export {
        id: String,
        #[arg(long, value_enum, default_value = "json")]
        format: MeetingFormat,
        #[arg(long)]
        output: PathBuf,
        /// Replace an existing regular file. Symlinks are never followed.
        #[arg(long)]
        force: bool,
    },
    /// Record a live meeting: your microphone (shared with dictation) and one
    /// app's audio, such as Zoom, Teams, Chime or the browser running Meet.
    ///
    /// macOS 14.2 or newer. Asks first, then shows an Xflow indicator with a
    /// Stop button while it records. Runs until stopped (Stop, Ctrl-C or
    /// `xflow meeting stop`), then transcribes in the background. Audio stays
    /// private on this Mac until it is sent to your transcription provider.
    Record {
        /// The app to record: bundle ID (us.zoom.xos), name (Zoom) or PID.
        /// Omit to choose in the dialog.
        #[arg(long)]
        app: Option<String>,
        /// A title for the meeting, 1 to 256 bytes.
        #[arg(long)]
        title: Option<String>,
        /// Record only the app's audio, not your microphone.
        #[arg(long)]
        no_mic: bool,
        /// Stop when stdin closes or reads `q`, for a launching program.
        #[arg(long, hide = true)]
        control_stdin: bool,
    },
    /// Stop the meeting being recorded and transcribe it.
    Stop,
    /// Delete one meeting and its audio, cancelling its transcription, or
    /// retry physical cleanup once.
    Delete {
        /// `store_id:id`. Omit with `--retry` to retry cleanup only.
        id: Option<String>,
        /// Try cleanup once more when a delete is still pending.
        #[arg(long)]
        retry: bool,
    },
    /// Watch for a Zoom, Google Meet, Microsoft Teams or Amazon Chime call and
    /// print each decision (app name and decision only) until interrupted.
    ///
    /// Runs in this process and opens no microphone. Browser calls need
    /// Accessibility to read window titles. Nothing is recorded yet.
    Detect,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum MeetingFormat {
    Json,
    Md,
    Text,
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
pub enum ModelAction {
    /// Download the pinned on-device model files (or copy them with --from) and verify them.
    Install {
        model: String,
        /// Copy from a local directory with the same files instead of downloading.
        #[arg(long, value_name = "DIR")]
        from: Option<PathBuf>,
    },
    /// Delete the installed on-device model and any partial download.
    Remove { model: String },
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
    /// Undo the latest word or correction xflow saved automatically, once
    /// (until config.toml changes or xflowd restarts).
    Undo,
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
    Get {
        key: String,
    },
    Set {
        key: String,
        value: String,
    },
    Reset {
        key: Option<String>,
    },
    /// Show the recording pill cycling through its states, without dictating;
    /// --notice shows the one-second learning notice instead.
    #[cfg(target_os = "macos")]
    #[command(hide = true)]
    Preview {
        #[arg(long)]
        notice: bool,
    },
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
            vec![
                "xflow",
                "history",
                "edit",
                "3",
                "--text",
                "Kubernetes rocks",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
        assert!(Cli::try_parse_from(["xflow", "listen", "--seconds", "0"]).is_err());
    }
}
