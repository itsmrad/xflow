//! Meeting pages use the same list/detail navigation and chat reducer.
use super::state::{App, Effect, Page};
use crate::meeting::{notes::Notes, MeetingReader};
use anyhow::Result;
use xflow_core::meeting::*;
#[derive(Default)]
pub struct View {
    pub id: Option<MeetingId>,
    page: Option<MeetingPage>,
    notes: Option<Notes>,
}
#[derive(Debug)]
pub enum Job {
    List(Option<ListCursor>),
    Notes(MeetingId),
    Generate(MeetingId, std::path::PathBuf),
}
pub enum Done {
    List(MeetingPage),
    Notes(MeetingId, Option<Notes>),
}
impl View {
    pub fn refresh(&self) -> Job {
        self.id.clone().map(Job::Notes).unwrap_or(Job::List(None))
    }
    pub fn apply(&mut self, done: Done) {
        match done {
            Done::List(page) => {
                self.id = None;
                self.page = Some(page);
            }
            Done::Notes(id, notes) => {
                self.id = Some(id);
                self.notes = notes;
            }
        }
    }
    pub fn rows(&self) -> Vec<(String, String)> {
        if self.id.is_some() {
            let mut rows = vec![
                (
                    "Ask about this meeting".into(),
                    "Enter to open chat; provider consent applies".into(),
                ),
                (
                    "Generate cited notes".into(),
                    "Uses your consented AI gateway".into(),
                ),
            ];
            if let Some(notes) = &self.notes {
                rows.extend(notes.claims.iter().map(|c| {
                    (
                        format!("{}: {}", c.kind, crate::meeting::display_text(&c.text)),
                        format!("segment {} revision {}", c.segment_id, c.revision),
                    )
                }));
            } else {
                rows.push((
                    "No current notes".into(),
                    "Generate with xflow meeting notes ID --generate".into(),
                ));
            }
            rows
        } else {
            let mut rows: Vec<_> = self
                .page
                .as_ref()
                .map(|p| {
                    p.meetings
                        .iter()
                        .map(|m| {
                            (
                                crate::meeting::display_text(
                                    m.title.as_deref().unwrap_or("Untitled meeting"),
                                ),
                                format!(
                                    "{}:{} · {} segments · {} gaps",
                                    m.meeting.store_id, m.meeting.id, m.segment_count, m.gap_count
                                ),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            if self.page.as_ref().is_some_and(|p| p.next.is_some()) {
                rows.push(("Next meetings".into(), "Enter for next page".into()));
            }
            rows
        }
    }
    pub fn details(&self, pos: usize) -> String {
        if let Some(id) = &self.id {
            let command = format!("{}:{}", id.store_id, id.id);
            if let Some(c) = self
                .notes
                .as_ref()
                .and_then(|n| pos.checked_sub(2).and_then(|i| n.claims.get(i)))
            {
                return format!("{}\n\nEvidence: {}\nsegment {} revision {}\n\nGenerated text; verify against transcript.\nEsc returns to meetings.",crate::meeting::display_text(&c.text),crate::meeting::display_text(&c.quote),c.segment_id,c.revision);
            }
            return format!("Meeting {command}\n\nEnter on Ask to ask or summarize.\nGenerate: xflow meeting notes {command} --generate\nTerms: xflow meeting term FROM TO\nApply: xflow meeting apply-terms {command}\n\nImported labels and times may be unverified. Notes include only available transcript pages; inspect gaps. Esc returns to meetings.");
        }
        "Private meetings. Enter opens notes, then Ask opens chat. = opens this page; Tab changes pages. All actions work without colour.".into()
    }
}
pub fn choose(app: &mut App, pos: usize) -> Vec<Effect> {
    if let Some(id) = &app.meetings.id {
        if pos == 0 {
            app.ask_meeting = Some(id.clone());
            app.chat.meeting_chat();
            app.chat.paste("Summarize this meeting, including decisions, actions and uncertainties, with citations.");
            app.page = Page::Chat;
            let mut effects = vec![Effect::ChatCancel];
            effects.extend(app.refresh());
            return effects;
        }
        if pos == 1 {
            return vec![Effect::Meetings(Job::Generate(
                id.clone(),
                app.path.clone(),
            ))];
        }
    } else if let Some(page) = &app.meetings.page {
        let job = if let Some(m) = page.meetings.get(pos) {
            Some(Job::Notes(m.meeting.clone()))
        } else {
            page.next.clone().map(|c| Job::List(Some(c)))
        };
        if let Some(job) = job {
            app.selected[Page::Meetings.index()] = 0;
            return vec![Effect::Meetings(job)];
        }
    }
    vec![]
}
pub fn run(job: Job) -> Result<Done> {
    let path = crate::paths::data_dir()?.join("meetings.db");
    let reader = MeetingReader::at(path.clone());
    match job {
        Job::Generate(id, path) => {
            let cfg = crate::config_edit::ConfigFile::load(&path)?.config()?;
            let (ai, gateway) = xflow_providers::llm::build_provider_with_cleanup(
                &cfg.ai,
                &cfg.cleanup,
                cfg.privacy.offline,
            )?;
            anyhow::ensure!(ai.consented.contains(&ai.provider),"Allow the provider on the Chat page first, or run xflow meeting notes ID --generate in a terminal");
            let model = ai
                .model
                .clone()
                .or_else(|| {
                    xflow_providers::find_provider(
                        xflow_providers::ProviderKind::Chat,
                        &ai.provider,
                    )
                    .map(|p| p.default_model.into())
                })
                .unwrap_or_default();
            let notes = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(crate::meeting::notes::generate(
                    &crate::meeting::MeetingStore::at(reader.path.clone()),
                    &id,
                    &gateway,
                    &model,
                    ai.max_context_tokens,
                ))?;
            Ok(Done::Notes(id, Some(notes)))
        }
        Job::List(cursor) => Ok(Done::List(if !path.exists() {
            MeetingPage {
                meetings: vec![],
                next: None,
            }
        } else {
            reader.list(
                &ListFilter {
                    since_ms: 0,
                    until_ms: i64::MAX,
                },
                cursor.as_ref(),
            )?
        })),
        Job::Notes(id) => {
            let notes = reader.notes(&id)?;
            Ok(Done::Notes(id, notes))
        }
    }
}
