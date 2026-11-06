//! "On this Mac (offline)": the facts pane shared by setup and Settings, the
//! install job (download, cancel, retry, remove) and the snapshot-keeping switch.
use super::state::{App, Effect, Modal, Step};
use crate::{config_edit, local_models::Facts};
use anyhow::{anyhow, bail, Result};
use crossterm::event::{KeyCode, KeyEvent};
use std::{sync::Arc, time::Instant};
use tokio::sync::Notify;
use xflow_providers::ON_DEVICE;

/// The model download. Only one runs at a time; it never changes settings.
#[derive(Clone, Debug, Default)]
pub enum Job {
    #[default]
    Idle,
    Running {
        done: u64,
        total: u64,
        started: Instant,
        cancel: Arc<Notify>,
    },
    /// Cancel was pressed; the download stops at its next step. Staged files stay for a resume.
    Cancelling,
    Failed(String),
}

/// The pane's main Enter action.
#[derive(Debug, PartialEq)]
pub enum Next {
    Back,
    Wait,
    Install,
    Retry,
    Use,
    Done,
}

impl App {
    pub fn on_device_next(&self) -> Next {
        let Some(facts) = &self.facts else {
            return Next::Back;
        };
        match &self.install {
            _ if !facts.supported => Next::Back,
            Job::Running { .. } | Job::Cancelling => Next::Wait,
            _ if facts.installed && self.config.stt.provider == ON_DEVICE => Next::Done,
            _ if facts.installed => Next::Use,
            Job::Failed(_) => Next::Retry,
            Job::Idle => Next::Install,
        }
    }
    /// Show the facts. Opening it never downloads and never changes settings.
    pub fn open_on_device(&mut self, setup: bool) {
        self.facts = Some((self.probe)());
        self.modal = Some(Modal::OnDevice {
            setup,
            removing: false,
            error: None,
        });
    }
    pub(super) fn on_device_key(
        &mut self,
        setup: bool,
        removing: bool,
        key: KeyEvent,
    ) -> Vec<Effect> {
        let mut effects = vec![];
        let reopen = |app: &mut Self, removing, error| {
            app.modal = Some(Modal::OnDevice {
                setup,
                removing,
                error,
            })
        };
        let installed = self.facts.as_ref().is_some_and(|f| f.installed);
        match key.code {
            KeyCode::Esc if removing => reopen(self, false, None),
            KeyCode::Enter if removing => match self.remove_model() {
                Ok(next) => {
                    effects = next;
                    reopen(self, false, None);
                }
                Err(e) => reopen(self, false, Some(format!("{e:#}"))),
            },
            KeyCode::Esc => {
                // The download keeps going; API speech keeps working meanwhile.
                if setup {
                    effects = self.open_setup(Step::Provider);
                }
            }
            KeyCode::Char('x') => {
                if let Job::Running { cancel, .. } = &self.install {
                    cancel.notify_one();
                    self.install = Job::Cancelling;
                }
                reopen(self, false, None);
            }
            KeyCode::Char('d') | KeyCode::Delete
                if installed && matches!(self.install, Job::Idle | Job::Failed(_)) =>
            {
                reopen(self, true, None)
            }
            KeyCode::Enter => match self.on_device_next() {
                Next::Back if setup => effects = self.open_setup(Step::Provider),
                Next::Back => (),
                Next::Wait => reopen(self, false, None),
                Next::Install | Next::Retry => {
                    let cancel = Arc::new(Notify::new());
                    self.install = Job::Running {
                        done: 0,
                        total: self.facts.as_ref().map_or(0, |f| f.download_bytes),
                        started: Instant::now(),
                        cancel: cancel.clone(),
                    };
                    effects.push(Effect::InstallModel(cancel));
                    reopen(self, false, None);
                }
                Next::Use => match self.use_on_device() {
                    Ok(()) if setup => effects = self.open_setup(Step::Microphone),
                    Ok(()) => self.toast = Some("Speech now runs on this Mac.".into()),
                    Err(e) => reopen(self, false, Some(format!("{e:#}"))),
                },
                Next::Done if setup => effects = self.open_setup(Step::Microphone),
                Next::Done => (),
            },
            _ => reopen(self, removing, None),
        }
        effects
    }
    pub fn install_progress(&mut self, now: u64, of: u64) {
        if let Job::Running { done, total, .. } = &mut self.install {
            (*done, *total) = (now, of);
        }
    }
    pub fn install_done(&mut self, result: Result<bool>) {
        let cancelled = matches!(self.install, Job::Cancelling);
        self.install = match result {
            Ok(_) => {
                if let Some(facts) = &mut self.facts {
                    facts.installed = true;
                }
                self.toast = Some("On-device model installed.".into());
                Job::Idle
            }
            Err(_) if cancelled => {
                self.toast = Some("Download paused. Install picks up where it stopped.".into());
                Job::Idle
            }
            Err(e) => Job::Failed(format!("{e:#}")),
        };
    }
    pub fn model_removed(&mut self, result: Result<bool>) {
        match result {
            Ok(_) => {
                if let Some(facts) = &mut self.facts {
                    facts.installed = false;
                }
                self.toast =
                    Some("On-device model removed. Keys and settings are unchanged.".into());
            }
            Err(e) => self.toast = Some(format!("Couldn't remove the model: {e:#}")),
        }
    }
    /// `[stt]` goes to on-device; the API route is saved in `on_device.previous_api`.
    fn use_on_device(&mut self) -> Result<()> {
        if self.saving {
            bail!("One moment — saving your last change");
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| anyhow!("Settings are still loading"))?;
        config_edit::use_on_device(file)?;
        self.sync_config();
        Ok(())
    }
    /// Back to the saved API route, unchanged (custom model, endpoint, languages, hints, key).
    pub(super) fn leave_on_device(&mut self) -> Result<()> {
        if self.saving {
            bail!("One moment — saving your last change");
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| anyhow!("Settings are still loading"))?;
        config_edit::leave_on_device(file)?;
        self.sync_config();
        Ok(())
    }
    /// Removing the model in use switches back to the saved API route first.
    fn remove_model(&mut self) -> Result<Vec<Effect>> {
        if self.config.stt.provider == ON_DEVICE {
            self.leave_on_device()?;
        }
        Ok(vec![Effect::RemoveModel])
    }
    /// Current state first, so short terminals keep progress and recovery visible.
    pub fn on_device_text(&self, removing: bool, error: Option<&str>) -> String {
        let Some(facts) = &self.facts else {
            return "Model information unavailable. Esc goes back.".into();
        };
        let mut lines = vec![];
        if removing {
            lines.push(format!(
                "Remove the model ({})? Enter removes it; Esc keeps it.",
                crate::local_models::size(facts.download_bytes)
            ));
        }
        if let Some(error) = error {
            lines.push(format!("Error: {error}"));
        }
        lines.push(status(facts, &self.install, &self.config.stt.provider));
        lines.push(String::new());
        lines.push("Speech never leaves this computer and needs no API key.".into());
        lines.push(String::new());
        lines.extend(facts.lines());
        lines.join("\n")
    }
}

fn status(facts: &Facts, job: &Job, provider: &str) -> String {
    use crate::local_models::size;
    match job {
        _ if !facts.supported => "Not available on this machine. Keep an API service.".into(),
        Job::Running {
            done,
            total,
            started,
            ..
        } => {
            let rate = *done as f64 / started.elapsed().as_secs_f64().max(0.001);
            format!(
                "Downloading… {} of {} · {}/s. You can keep using xflow meanwhile.",
                size(*done),
                size(*total),
                size(rate as u64)
            )
        }
        Job::Cancelling => "Stopping the download…".into(),
        Job::Failed(why) => format!("Download stopped: {why}. Retry resumes it."),
        _ if facts.installed && provider == ON_DEVICE => "In use: speech runs on this Mac.".into(),
        _ if facts.installed => "Installed and ready to use.".into(),
        Job::Idle => format!(
            "Not installed. Install downloads {} once.",
            size(facts.download_bytes)
        ),
    }
}
