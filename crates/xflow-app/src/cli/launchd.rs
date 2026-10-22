use super::{
    args,
    output::Output,
    system::{atomic, tool},
    Error, Result,
};
use anyhow::Context;
use std::{
    io::BufRead,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const LABEL: &str = "com.xflow.daemon";
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .context("HOME is required for the LaunchAgent")
        .map_err(Into::into)
}
fn unit_path() -> Result<PathBuf> {
    Ok(home()?.join("Library/LaunchAgents/com.xflow.daemon.plist"))
}
fn log_path() -> Result<PathBuf> {
    Ok(home()?.join("Library/Logs/xflowd.log"))
}
fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}
fn target() -> String {
    format!("{}/{LABEL}", domain())
}
fn loaded() -> Result<bool> {
    Ok(Command::new("/bin/launchctl")
        .args(["print", &target()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success())
}
fn plist(binary: &Path, log: &Path) -> Result<String> {
    fn xml(path: &Path) -> Result<String> {
        Ok(path
            .to_str()
            .context("LaunchAgent paths must be UTF-8")?
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;"))
    }
    let binary = xml(binary)?;
    let log = xml(log)?;
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>{binary}</string></array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><true/>
<key>ProcessType</key><string>Interactive</string>
<key>StandardOutPath</key><string>{log}</string>
<key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
"#
    ))
}
fn bootout() -> Result<()> {
    tool("/bin/launchctl", &["bootout", &target()])?;
    // bootout can return before launchd has removed the job from the GUI domain.
    for _ in 0..100 {
        if !loaded()? {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Err(Error::new(
        1,
        "LaunchAgent is still unloading",
        "Wait briefly, then retry xflow daemon status",
    ))
}
fn bootstrap() -> Result<()> {
    tool("/bin/launchctl", &["enable", &target()])?;
    tool(
        "/bin/launchctl",
        &[
            "bootstrap",
            &domain(),
            unit_path()?.to_str().context("non-UTF-8 plist path")?,
        ],
    )?;
    Ok(())
}
pub fn service(action: &args::Service, out: &Output) -> Result<()> {
    match action {
        args::Service::Install { enable } => {
            let binary = std::env::current_exe()?.with_file_name("xflowd");
            if !binary.is_file() {
                return Err(Error::new(
                    1,
                    "xflowd must be installed beside xflow",
                    "Build/install both binaries, then run xflow service install --enable",
                ));
            }
            let path = unit_path()?;
            let log = log_path()?;
            std::fs::create_dir_all(log.parent().context("log path has no parent")?)?;
            let contents = plist(&binary, &log)?;
            let was_loaded = loaded()?;
            atomic(&path, contents.as_bytes())?;
            // Adopt the existing hand-installed label and reload its executable path.
            if was_loaded {
                bootout()?;
            }
            if *enable || was_loaded {
                bootstrap()?;
            }
            out.confirm(if *enable || was_loaded {
                "LaunchAgent installed and loaded at ~/Library/LaunchAgents/com.xflow.daemon.plist"
            } else {
                "LaunchAgent installed at ~/Library/LaunchAgents/com.xflow.daemon.plist; start with xflow daemon start"
            })?;
        }
        args::Service::Uninstall => {
            if loaded()? {
                bootout()?;
            }
            let path = unit_path()?;
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            out.confirm("LaunchAgent uninstalled; logs retained")?;
        }
    }
    Ok(())
}
pub fn daemon(action: &args::Daemon, out: &Output) -> Result<()> {
    use args::Daemon::*;
    match action {
        Start | Restart => {
            if !unit_path()?.exists() {
                service(&args::Service::Install { enable: false }, out)?;
            }
            if !loaded()? {
                bootstrap()?;
            }
            let target = target();
            tool(
                "/bin/launchctl",
                if matches!(action, Restart) {
                    vec!["kickstart", "-k", &target]
                } else {
                    vec!["kickstart", &target]
                }
                .as_slice(),
            )?;
            out.confirm(if matches!(action, Restart) {
                "Daemon restarted under launchd"
            } else {
                "Daemon started under launchd"
            })?;
        }
        Stop => {
            // Shutdown alone would trigger KeepAlive and immediately restart xflowd.
            if loaded()? {
                bootout()?;
            }
            out.confirm("Daemon stopped (LaunchAgent unloaded)")?;
        }
        Status => {
            let loaded = loaded()?;
            let text = if loaded {
                tool("/bin/launchctl", &["print", &target()])?
            } else {
                "LaunchAgent is not loaded; run xflow daemon start".into()
            };
            out.data(
                &serde_json::json!({"service":LABEL,"loaded":loaded,"properties":text}),
                text.trim_end(),
            )?;
        }
        Logs { follow, lines } => {
            let path = log_path()?;
            let lines = lines.to_string();
            let mut args = vec!["-n", &lines];
            if *follow {
                args.push("-F");
            }
            args.push(path.to_str().context("non-UTF-8 log path")?);
            if !follow {
                let text = tool("/usr/bin/tail", &args)?;
                out.data(&serde_json::json!({"logs":text}), text.trim_end())?;
            } else {
                let mut child = Command::new("/usr/bin/tail")
                    .args(args)
                    .stdout(Stdio::piped())
                    .spawn()?;
                for line in
                    std::io::BufReader::new(child.stdout.take().context("log output unavailable")?)
                        .lines()
                {
                    let result = line.map_err(Error::from).and_then(|line| {
                        out.data(&serde_json::json!({"message":line}), &line)
                            .map_err(Into::into)
                    });
                    if let Err(error) = result {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(error);
                    }
                }
                if !child.wait()?.success() {
                    return Err(Error::new(1, "tail failed", "Run xflow doctor"));
                }
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launchagent_escapes_paths_and_adopts_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("com.xflow.daemon.plist");
        atomic(&path, b"hand-installed").unwrap();
        let contents = plist(Path::new("/a&b/<xflowd>"), Path::new("/a/log file")).unwrap();
        atomic(&path, contents.as_bytes()).unwrap();
        let result = std::fs::read_to_string(path).unwrap();
        assert!(result.contains("/a&amp;b/&lt;xflowd&gt;"));
        assert!(
            result.contains("<key>KeepAlive</key><true/>")
                && result.contains("<key>RunAtLoad</key><true/>")
        );
        assert!(result.contains(LABEL) && result.matches("/a/log file").count() == 2);
    }
}
