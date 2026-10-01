use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use xflow_core::ipc::{Request, Response};

#[derive(Parser)]
#[command(version, about = "Fast local dictation control")]
struct Args {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Start microphone capture. Bind to a shortcut's press event for push-to-talk.
    Start,
    /// Stop capture and transcribe asynchronously. Bind to its release event.
    Stop,
    /// Toggle hands-free capture.
    Toggle,
    Cancel,
    Status,
    Last,
    CopyLast,
    PasteLast,
    History {
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
    },
    ClearHistory,
    /// Subscribe to state and audio-level events until interrupted.
    Watch,
    Tui,
    /// Create the default TOML file; refuses to overwrite an existing config.
    Init,
    /// Store a provider key in OS credential storage. Input is hidden.
    Key {
        provider: String,
        #[arg(long)]
        stdin: bool,
    },
    /// Report desktop dependencies without accessing the microphone or sending audio.
    Doctor,
    Shutdown,
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Commands::Init => {
            let path = xflow_app::paths::config_path()?;
            xflow_app::paths::private_dir(path.parent().context("config has no parent")?)?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            use std::io::Write;
            options
                .open(&path)
                .with_context(|| {
                    format!(
                        "cannot create {}; existing config is preserved",
                        path.display()
                    )
                })?
                .write_all(include_bytes!("../../../../config/default.toml"))?;
            println!("Created {}", path.display());
            return Ok(());
        }
        Commands::Key { provider, stdin } => {
            let secret = if stdin {
                use std::io::Read;
                let mut value = String::new();
                std::io::stdin().take(4097).read_to_string(&mut value)?;
                if value.len() > 4096 {
                    bail!("API key input too long");
                }
                value.trim().to_owned()
            } else {
                rpassword::prompt_password("API key: ")?
            };
            if secret.is_empty() {
                bail!("API key cannot be empty");
            }
            xflow_providers::save_key(&provider, &secret).await?;
            println!("Key stored in OS credential storage");
            return Ok(());
        }
        Commands::Doctor => {
            doctor(args.json)?;
            return Ok(());
        }
        _ => (),
    }
    #[cfg(unix)]
    {
        if matches!(args.command, Commands::Tui) {
            return xflow_app::tui::run().await;
        }
        if matches!(args.command, Commands::Watch) {
            use tokio::io::BufReader;
            let mut stream = xflow_app::transport::connect().await?;
            xflow_app::transport::write_frame(&mut stream, &Request::Subscribe).await?;
            let mut reader = BufReader::new(stream);
            loop {
                tokio::select! {
                    response = xflow_app::transport::read_frame::<Response, _>(&mut reader) => match response? { Some(response) => print_response(response, args.json)?, None => break },
                    _ = tokio::signal::ctrl_c() => break,
                }
            }
            return Ok(());
        }
        let request = match args.command {
            Commands::Start => Request::start(),
            Commands::Stop => Request::Stop,
            Commands::Toggle => Request::toggle(),
            Commands::Cancel => Request::Cancel,
            Commands::Status => Request::Status,
            Commands::Last => Request::Last,
            Commands::CopyLast => Request::CopyLast,
            Commands::PasteLast => Request::PasteLast,
            Commands::History { limit } => Request::History {
                limit,
                offset: 0,
                query: None,
            },
            Commands::ClearHistory => Request::ClearHistory,
            Commands::Shutdown => Request::Shutdown,
            _ => unreachable!(),
        };
        print_response(xflow_app::transport::request(&request).await?, args.json)
    }
    #[cfg(not(unix))]
    {
        bail!("Windows IPC adapter not implemented yet")
    }
}
fn print_response(response: Response, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(&response)?);
    } else if let Some(text) = &response.text {
        println!("{text}");
    } else if !response.history.is_empty() {
        for entry in &response.history {
            println!("{}\t{}\t{}", entry.id, entry.provider, entry.text);
        }
    } else {
        println!(
            "{:?}{}",
            response.state,
            response
                .message
                .as_ref()
                .map(|m| format!(": {m}"))
                .unwrap_or_default()
        );
    }
    if !response.ok {
        bail!("daemon command failed");
    }
    Ok(())
}
fn doctor(json: bool) -> Result<()> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".into());
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into());
    let dependencies: Vec<_> = ["wl-copy", "xclip", "ydotool", "xdotool", "gnome-extensions"]
        .into_iter()
        .map(|tool| {
            let present = std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
                .unwrap_or(false);
            (tool, present)
        })
        .collect();
    if json {
        println!(
            "{}",
            serde_json::json!({"desktop":desktop,"session":session,"tools":dependencies,"config":xflow_app::paths::config_path()?.display().to_string(),"native_desktop_validation":"requires manual desktop test"})
        );
    } else {
        println!("Desktop: {desktop}; session: {session}");
        for (tool, present) in dependencies {
            println!("{tool}: {}", if present { "found" } else { "missing" });
        }
        println!("GNOME Wayland uses the bundled Shell extension for hotkeys and overlay.\nPaste requires configured ydotool/uinput access; clipboard-only fallback remains available.");
    }
    Ok(())
}
