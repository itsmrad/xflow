//! "AI apps": MCP sharing and client registration. No rendering here; the work
//! is `mcp::setup`, the same code the `xflow mcp` commands print.
use super::state::{Action, App, Effect, Modal, Page};
use crate::mcp::{self, setup};
use anyhow::Result;
use std::path::Path;

pub(crate) use setup::{ClientStatus, Registration, Report, Status};

/// Display order; print-only clients and ChatGPT guidance last.
pub const CLIENTS: [(&str, &str); 10] = [
    ("claude-code", "Claude Code"),
    ("claude-desktop", "Claude Desktop"),
    ("codex", "Codex"),
    ("cursor", "Cursor"),
    ("vscode", "VS Code"),
    ("gemini", "Gemini CLI"),
    ("zed", "Zed"),
    ("lm-studio", "LM Studio"),
    ("windsurf", "Windsurf"),
    ("chatgpt", "ChatGPT"),
];
/// The consent `xflow mcp install` asks for.
pub const CONSENT: &str = "When you ask about it, matching text is sent to that app's AI provider.";
/// Footer, from the MCP guide.
pub const LEAVES: &str =
    "Leaves this computer: matching text an AI app asks for, sent to that app's AI provider.";
pub const INJECTION: &str = "Dictated content is untrusted and can contain prompt injection; avoid combining it with tools that can send private data out.";

/// Slow setup work, run off the UI loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Status,
    Install { client: &'static str, force: bool },
    Remove(&'static str),
    Share(bool),
    ShareMeetings(bool),
}
#[derive(Debug)]
pub enum Done {
    Status(Status),
    Installed(&'static str, Report),
    /// A different xflow is registered; nothing changed until the user confirms.
    Replace {
        client: &'static str,
        existing: String,
    },
    Removed(&'static str, Report),
    Shared(bool),
    SharedMeetings(bool),
}
#[derive(Default)]
pub struct AiApps {
    pub status: Option<Status>,
    /// A job is running; new ones wait.
    pub busy: bool,
    /// Client waiting for the replace confirmation.
    pub pending: Option<&'static str>,
    /// What the last action left for the user to do by hand, per client.
    pub manual: Option<(&'static str, String)>,
}
/// Runs a job with the CLI's own code; never prompts (the TUI owns the terminal).
pub fn run(job: Job, config: &Path) -> Result<Done> {
    let consent = mcp::consent_path(config);
    Ok(match job {
        Job::Status => Done::Status(setup::status(&consent)?),
        Job::Install { client, force } => {
            match setup::install(client, false, false, force, false, false, &consent) {
                Ok(report) => Done::Installed(client, report),
                Err(e) => match e.downcast_ref::<setup::Replace>() {
                    Some(r) => Done::Replace {
                        client,
                        existing: r.existing.clone(),
                    },
                    None => return Err(e),
                },
            }
        }
        Job::Remove(client) => Done::Removed(client, setup::uninstall(client)?),
        Job::ShareMeetings(on) => {
            mcp::set_meetings(&consent, on)?;
            Done::SharedMeetings(on)
        }
        Job::Share(on) => {
            mcp::set_dictations(&consent, on)?;
            Done::Shared(on)
        }
    })
}
pub fn name(client: &str) -> &'static str {
    CLIENTS
        .iter()
        .find(|(id, _)| *id == client)
        .map_or("this app", |(_, name)| name)
}
impl AiApps {
    pub fn client(&self, id: &str) -> Option<&ClientStatus> {
        self.status
            .as_ref()?
            .clients
            .iter()
            .find(|c| c.client == id)
    }
    pub fn sharing(&self) -> Option<bool> {
        self.status.as_ref().map(|s| s.sources.dictations)
    }
}
/// Row 0 is sharing; then one row per client.
pub fn rows(app: &App) -> Vec<(String, String)> {
    let sharing = match app.ai.sharing() {
        Some(true) => "on",
        Some(false) => "off",
        None => "…",
    };
    std::iter::once(("Dictation history:".into(), sharing.into()))
        .chain(std::iter::once((
            "Meeting transcripts:".into(),
            match app.ai.status.as_ref().map(|s| s.sources.meetings) {
                Some(true) => "on",
                Some(false) => "off",
                None => "…",
            }
            .into(),
        )))
        .chain(CLIENTS.iter().map(|(id, name)| {
            let value = match app.ai.client(id) {
                None => "Checking…".into(),
                Some(_) if *id == "chatgpt" => "Use Codex".into(),
                Some(c) => {
                    let found = if c.detected { "Found" } else { "Not found" };
                    let set = match &c.registration {
                        Registration::Command { .. } if c.registration.version_differs() => {
                            "set up · ! other version"
                        }
                        Registration::Command { .. } | Registration::NoCommand => "set up",
                        Registration::Unreadable => "can't read its settings",
                        Registration::Missing if setup::print_only(id) => "add by hand",
                        Registration::Missing => "not set up",
                    };
                    format!("{found} · {set}")
                }
            };
            (name.to_string(), value)
        }))
        .collect()
}
/// The selected client, if the selection is a client row.
pub fn selected(app: &App) -> Option<&'static str> {
    app.selected()
        .checked_sub(2)
        .and_then(|i| CLIENTS.get(i))
        .map(|(id, _)| *id)
}
fn start(app: &mut App, job: Job) -> Vec<Effect> {
    if app.ai.busy {
        app.toast = Some("One moment — still working on the last change.".into());
        return vec![];
    }
    app.ai.busy = true;
    vec![Effect::Mcp(job, app.path.clone())]
}
pub fn refresh(app: &mut App) -> Vec<Effect> {
    if app.ai.busy {
        return vec![];
    }
    app.ai.busy = true;
    vec![Effect::Mcp(Job::Status, app.path.clone())]
}
pub fn choose(app: &mut App) -> Vec<Effect> {
    let Some(client) = selected(app) else {
        let meetings = app.selected() == 1;
        let sharing = if meetings {
            app.ai.status.as_ref().map(|s| s.sources.meetings)
        } else {
            app.ai.sharing()
        };
        return match sharing {
            None => vec![],
            Some(true) => start(
                app,
                if meetings {
                    Job::ShareMeetings(false)
                } else {
                    Job::Share(false)
                },
            ),
            Some(false) => {
                app.modal = Some(Modal::Confirm {
                    action: if meetings {Action::ShareMeetings} else {Action::Share},
                    prompt: format!("Share your {} with AI apps? {CONSENT} This applies to every AI app you set up.", if meetings {"meeting transcripts"} else {"dictation history"}),
                });
                vec![]
            }
        };
    };
    if client == "chatgpt" {
        app.toast = Some("Set up Codex to use xflow in ChatGPT desktop.".into());
    } else if setup::print_only(client) {
        app.toast = Some(format!(
            "Add the settings shown to {} yourself.",
            name(client)
        ));
    } else {
        app.menu(
            name(client),
            vec![
                (Action::Install, "Install — add xflow to this app"),
                (Action::Remove, "Remove xflow from this app"),
            ],
        );
    }
    vec![]
}
/// Install, Remove, Replace (confirmed) and Share (confirmed) on this page.
pub fn action(app: &mut App, action: Action) -> Vec<Effect> {
    if app.page != Page::AiApps {
        return vec![];
    }
    match action {
        Action::ShareMeetings => start(app, Job::ShareMeetings(true)),
        Action::Share => start(app, Job::Share(true)),
        Action::Replace => match app.ai.pending.take() {
            Some(client) => start(
                app,
                Job::Install {
                    client,
                    force: true,
                },
            ),
            None => vec![],
        },
        Action::Install | Action::Remove => match selected(app) {
            Some(client) if !setup::print_only(client) => {
                let job = if action == Action::Install {
                    Job::Install {
                        client,
                        force: false,
                    }
                } else {
                    Job::Remove(client)
                };
                start(app, job)
            }
            _ => vec![],
        },
        _ => vec![],
    }
}
pub fn done(app: &mut App, result: Result<Done>) -> Vec<Effect> {
    app.ai.busy = false;
    let done = match result {
        Ok(done) => done,
        Err(e) => {
            app.toast = Some(super::options::plain(&format!("{e:#}")));
            return vec![];
        }
    };
    match done {
        Done::Status(status) => {
            app.ai.status = Some(status);
            return vec![];
        }
        Done::Replace { client, existing } => {
            app.ai.pending = Some(client);
            app.modal = Some(Modal::Confirm {
                action: Action::Replace,
                prompt: format!(
                    "{} already uses a different xflow ({existing}). Replace it with this one?",
                    name(client)
                ),
            });
            return vec![];
        }
        Done::Installed(client, report) => {
            app.toast = Some(match &report.manual {
                Some(_) => format!(
                    "Couldn't set up {} by itself — see Details.",
                    name(client)
                ),
                None if app.ai.sharing() == Some(false) => format!(
                    "xflow is set up in {}. Restart it; sharing is off, so it can't read history yet.",
                    name(client)
                ),
                None => format!(
                    "xflow is set up in {}. Restart it to use xflow.",
                    name(client)
                ),
            });
            app.ai.manual = report.manual.map(|m| (client, m));
        }
        Done::Removed(client, report) => {
            app.toast = Some(match &report.manual {
                Some(_) => format!(
                    "Couldn't remove it from {} by itself — see Details.",
                    name(client)
                ),
                None => format!("xflow is removed from {}.", name(client)),
            });
            app.ai.manual = report.manual.map(|m| (client, m));
        }
        Done::SharedMeetings(on) => {
            app.toast = Some(format!(
                "Meeting transcripts sharing is {}.",
                if on { "on" } else { "off" }
            ));
        }
        Done::Shared(on) => {
            app.toast = Some(
                if on {
                    "Dictation history is shared with AI apps you set up."
                } else {
                    "Dictation history is no longer shared."
                }
                .into(),
            );
        }
    }
    refresh(app)
}
/// The details panel for the selected row.
pub fn details(app: &App) -> String {
    let Some(client) = selected(app) else {
        let meetings = app.selected() == 1;
        let state = match if meetings {
            app.ai.status.as_ref().map(|s| s.sources.meetings)
        } else {
            app.ai.sharing()
        } {
            Some(true) => "on. Enter to turn it off.",
            Some(false) => "off. Enter to turn it on.",
            None => "being checked…",
        };
        return format!(
            "Sharing · {} is {state}\n\n{CONSENT} One switch for every AI app.",
            if meetings {
                "Meeting transcripts"
            } else {
                "Dictation history"
            }
        );
    };
    let Some(c) = app.ai.client(client) else {
        return "Checking…".into();
    };
    let mut text = name(client).to_string();
    if let Some(guide) = c.manual.as_deref().filter(|_| client == "chatgpt") {
        return format!("{text}\n\n{guide}");
    }
    text.push_str(if c.detected {
        "\nFound on this computer."
    } else {
        "\nNot found on this computer."
    });
    match &c.registration {
        Registration::Command {
            command, version, ..
        } => {
            text.push_str(&format!("\nSet up with xflow at {command}"));
            if c.registration.version_differs() {
                let this = app.ai.status.as_ref().map_or("", |s| s.version);
                text.push_str(&format!("\n! That xflow is a different version ({version}; this is {this}). Install again to use this one."));
            }
        }
        Registration::NoCommand => text.push_str("\nSet up, but its xflow path can't be read."),
        Registration::Unreadable => text.push_str("\nIts settings can't be read."),
        Registration::Missing if setup::print_only(client) => {}
        Registration::Missing => text.push_str("\nNot set up yet."),
    }
    if let Some(snippet) = &c.manual {
        text.push_str(&format!(
            "\n\nAdd this to its settings yourself:\n{snippet}"
        ));
    } else {
        text.push_str("\n\nEnter to install or remove.");
    }
    if let Some((_, manual)) = app.ai.manual.as_ref().filter(|(id, _)| *id == client) {
        text.push_str(&format!("\n\nDo this yourself:\n{manual}"));
    }
    text
}
