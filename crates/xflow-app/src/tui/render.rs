use super::{
    state::{commands, fuzzy, Action, App, Modal, Page, PAGES},
    theme::Theme,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Sparkline, Wrap},
    Frame,
};
use xflow_core::State;

#[derive(Default)]
pub struct HitMap {
    pub tabs: Vec<(Rect, Page)>,
    pub rows: Vec<(Rect, usize)>,
    pub actions: Vec<(Rect, Action)>,
}
fn block(title: impl Into<String>, theme: Theme) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title.into())
        .border_style(theme.base().fg(theme.muted))
}
fn panel(
    frame: &mut Frame,
    area: Rect,
    title: impl Into<String>,
    text: impl Into<String>,
    app: &App,
) {
    frame.render_widget(
        Paragraph::new(text.into())
            .style(app.theme.base())
            .block(block(title, app.theme))
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0)),
        area,
    );
}
pub fn draw(frame: &mut Frame, app: &App) -> HitMap {
    let area = frame.area();
    let theme = app.theme;
    let mut hits = HitMap::default();
    frame.render_widget(Block::default().style(theme.base()), area);
    if area.width < 35 || area.height < 12 {
        let text=format!("xflow · {}\n{}\n\nTerminal is small ({}×{}).\nResize to 80×24 for all controls.\n1..0 screens · ? help · q quit\nCtrl+S saves edits",app.page.title(),if app.connected{"Connected"}else{"Disconnected"},area.width,area.height);
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .style(theme.base()),
            area,
        );
        return hits;
    }
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(2),
        Constraint::Length(2),
    ])
    .split(area);
    let top = Layout::horizontal([
        Constraint::Length(if area.width >= 100 { 22 } else { 14 }),
        Constraint::Min(1),
    ])
    .split(rows[0]);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                " xflow",
                theme.base().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                if app.dirty {
                    " ● Unsaved edits"
                } else if app.saving {
                    " Saving…"
                } else {
                    " Voice, at your pace."
                },
                theme.base().fg(theme.muted),
            )),
        ]),
        top[0],
    );
    frame.render_widget(
        Paragraph::new(format!(
            "{}  ·  {}",
            app.page.title(),
            if app.connected {
                "Connected"
            } else {
                "Disconnected"
            }
        ))
        .block(block("Control center", theme)),
        top[1],
    );
    let body = Layout::horizontal([
        Constraint::Length(if area.width >= 100 { 19 } else { 15 }),
        Constraint::Min(1),
    ])
    .split(rows[1]);
    let items = PAGES
        .iter()
        .enumerate()
        .map(|(i, page)| {
            ListItem::new(format!(
                " {} {}",
                if i == 9 { 0 } else { i + 1 },
                page.title()
            ))
        })
        .collect::<Vec<_>>();
    let mut nav = ListState::default().with_selected(Some(app.page.index()));
    frame.render_stateful_widget(
        List::new(items)
            .block(block("Navigate", theme))
            .highlight_style(theme.selected())
            .highlight_symbol("›"),
        body[0],
        &mut nav,
    );
    for (i, page) in PAGES.iter().enumerate() {
        if (i as u16) + 1 < body[0].height.saturating_sub(1) {
            hits.tabs.push((
                Rect::new(
                    body[0].x + 1,
                    body[0].y + 1 + i as u16,
                    body[0].width.saturating_sub(2),
                    1,
                ),
                *page,
            ));
        }
    }
    match app.page {
        Page::Home => home(frame, body[1], app, &mut hits),
        Page::Stats => stats(frame, body[1], app),
        _ => listing(frame, body[1], app, &mut hits),
    }
    let mut x = rows[2].x;
    for (action, label) in context_actions(app.page) {
        let width = (label.chars().count() + 2) as u16;
        if x.saturating_add(width) > rows[2].right() {
            break;
        }
        let rect = Rect::new(x, rows[2].y, width, 1);
        frame.render_widget(
            Paragraph::new(format!(" {label} ")).style(theme.base().bg(theme.selection)),
            rect,
        );
        hits.actions.push((rect, action));
        x += width + 1;
    }
    let footer = app
        .toast
        .as_deref()
        .unwrap_or("? Help  ·  Ctrl+P Actions  ·  t Theme  ·  Ctrl+S Save  ·  q Quit");
    frame.render_widget(
        Paragraph::new(footer)
            .style(theme.base().fg(if app.toast.is_some() {
                theme.accent
            } else {
                theme.muted
            }))
            .wrap(Wrap { trim: false }),
        rows[3],
    );
    if let Some(modal) = &app.modal {
        draw_modal(frame, area, app, modal);
        hits = HitMap::default();
    }
    hits
}
fn home(frame: &mut Frame, area: Rect, app: &App, hits: &mut HitMap) {
    let theme = app.theme;
    let rows = Layout::vertical([
        Constraint::Length(7),
        Constraint::Min(4),
        Constraint::Length(4),
    ])
    .split(area);
    let state = if !app.connected {
        "○ OFFLINE"
    } else {
        match app.status.state {
            State::Idle => "○ READY",
            State::Listening => "● LISTENING",
            State::Processing => "◌ PROCESSING",
            State::Success => "✓ DONE",
            State::Error => "! ERROR",
        }
    };
    let mode = app
        .status
        .mode
        .map(|m| format!("{m:?}"))
        .unwrap_or_else(|| "Dictation".into());
    let provider = app
        .status
        .provider
        .as_deref()
        .unwrap_or(&app.config.stt.provider);
    let model = app
        .status
        .model
        .as_deref()
        .or(app.config.stt.model.as_deref())
        .unwrap_or("default model");
    let animation = if app.status.state == State::Processing {
        ["·  ·  ·", "●  ·  ·", "·  ●  ·", "·  ·  ●"][app.phase % 4]
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!(" {state}  {animation}"),
                theme
                    .base()
                    .fg(if app.status.state == State::Error {
                        theme.error
                    } else {
                        theme.accent
                    })
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(format!(" {mode}  ·  {provider} / {model}")),
            Line::from(Span::styled(
                format!(
                    " {}",
                    app.status.message.as_deref().unwrap_or(if app.connected {
                        "Space starts / stops · C command mode · x cancels"
                    } else {
                        "Reconnects automatically; configuration stays editable"
                    })
                ),
                theme.base().fg(theme.muted),
            )),
        ])
        .block(block("Live session", theme))
        .wrap(Wrap { trim: false }),
        rows[0],
    );
    let wave = Rect::new(
        rows[0].x + 2,
        rows[0].y + 4,
        rows[0].width.saturating_sub(4),
        2.min(rows[0].height.saturating_sub(5)),
    );
    let levels = app.levels.iter().copied().collect::<Vec<_>>();
    frame.render_widget(
        Sparkline::default()
            .data(&levels)
            .max(100)
            .style(theme.base().fg(theme.accent)),
        wave,
    );
    if !app.connected || app.disk.is_none() {
        let text=format!("Welcome to xflow\n\n1. Choose a provider in 6 Providers; k sets its key.\n2. m chooses a model; 7 Settings configures language and microphone.\n3. Ctrl+S saves. Start the daemon: xflow service start\n4. Space dictates; 8 Overlay configures global shortcuts.\n\n{}\n\nLast transcript:\n{}",app.connection,app.transcript);
        panel(frame, rows[1], "Getting started", text, app);
        hits.actions.push((rows[1], Action::Go(Page::Providers)));
    } else {
        panel(
            frame,
            rows[1],
            "Last transcript · PgUp/PgDn scroll",
            if app.transcript.is_empty() {
                "Your words will appear here. Space starts dictation.".into()
            } else {
                app.transcript.clone()
            },
            app,
        );
    }
    let latency = app
        .timings
        .as_ref()
        .map(|t| {
            format!(
                "{}ms total · STT {}ms · cleanup {}ms · delivery {}ms",
                t.total_ms,
                t.stt_ms,
                t.cleanup_ms.unwrap_or(0),
                t.inject_ms.unwrap_or(0)
            )
        })
        .unwrap_or_else(|| "Session timing appears after dictation".into());
    frame.render_widget(
        Paragraph::new(format!(
            "{}\nDaemon {} · protocol {} · {}",
            latency,
            app.status.version.as_deref().unwrap_or("—"),
            app.status
                .protocol
                .map(|p| p.to_string())
                .unwrap_or_else(|| "—".into()),
            if app.connected {
                "Connected"
            } else {
                "Disconnected"
            }
        ))
        .block(block("Latency & connection", theme))
        .style(theme.base())
        .wrap(Wrap { trim: false }),
        rows[2],
    );
}
fn listing(frame: &mut Frame, area: Rect, app: &App, hits: &mut HitMap) {
    let theme = app.theme;
    let (list, detail) = if area.width >= 76 {
        let parts = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        (parts[0], parts[1])
    } else {
        let parts =
            Layout::vertical([Constraint::Percentage(48), Constraint::Percentage(52)]).split(area);
        (parts[0], parts[1])
    };
    let title = match app.page {
        Page::History => format!(
            "History {}–{} / {} · / {}",
            if app.total == 0 { 0 } else { app.offset + 1 },
            app.offset + app.history.len(),
            app.total,
            app.query
        ),
        Page::Providers => format!(
            "{} providers",
            if app.cleanup { "Cleanup" } else { "Speech" }
        ),
        Page::Settings => {
            if app.dirty {
                "Settings · unsaved".into()
            } else {
                "Settings".into()
            }
        }
        _ => app.page.title().into(),
    };
    let rows = app.rows();
    if rows.is_empty() {
        frame.render_widget(Paragraph::new(match app.page{Page::History=>"No matching transcripts. / searches; r refreshes.",Page::Overlay=>"GNOME extension settings unavailable. xflow extension install, then r refreshes.",Page::Providers=>"Loading providers…",Page::Doctor=>"Running diagnostics…",_=>"No entries yet. N creates your first entry."}).wrap(Wrap{trim:false}).block(block(title,theme)),list);
    } else {
        let items = rows
            .iter()
            .map(|(label, value)| {
                ListItem::new(vec![
                    Line::from(label.clone()),
                    Line::from(Span::styled(
                        value.lines().next().unwrap_or("").to_owned(),
                        theme.base().fg(theme.muted),
                    )),
                ])
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default().with_selected(Some(app.selected()));
        frame.render_stateful_widget(
            List::new(items)
                .block(block(title, theme))
                .highlight_style(theme.selected())
                .highlight_symbol("› "),
            list,
            &mut state,
        );
        for index in state.offset()..rows.len() {
            let y = list.y + 1 + ((index - state.offset()) * 2) as u16;
            if y >= list.bottom().saturating_sub(1) {
                break;
            }
            hits.rows.push((
                Rect::new(
                    list.x + 1,
                    y,
                    list.width.saturating_sub(2),
                    2.min(list.bottom().saturating_sub(1) - y),
                ),
                index,
            ));
        }
    }
    let text=match app.page {
        Page::History=>app.detail.as_ref().or_else(||app.history.get(app.selected())).map(|h|format!("{}\n\n#{} · {} · {}\nApp: {} · language: {}\nAudio: {}ms · latency: {}ms\nMode: {:?} · timestamp: {}\n\nRaw transcription:\n{}",h.text,h.id,h.provider,h.model.as_deref().unwrap_or("default"),h.app_id.as_deref().unwrap_or("—"),h.language.as_deref().unwrap_or("auto"),h.duration_ms.unwrap_or(0),h.latency_ms.unwrap_or(0),h.mode,h.created_at,h.raw_text.as_deref().unwrap_or("Same as final text"))).unwrap_or_else(||"Select a transcript for details.\n\nc Copy · p Paste · Delete Remove\ne Export JSON · PgUp/PgDn Page".into()),
        Page::Dictionary=>rows.get(app.selected()).map(|(name,kind)|format!("{name}\n\n{kind}\n\nWords guide recognition; replacements correct whole words.\n\nN Add word · A Add replacement\nEnter Edit · Delete Remove\nCtrl+S Save and reload")).unwrap_or_else(||"Keep names, acronyms and specialist terms accurate.\n\nN Add word · A Add replacement".into()),
        Page::Snippets=>app.config.snippets.get(app.selected()).map(|s|format!("Say: {}\n\n{}",s.trigger,s.text)).unwrap_or_else(||"Say a trigger to expand text.\n\nN Add · Enter Edit · Delete Remove\nCtrl+J adds a newline.\nCtrl+Enter stages a multi-field form.".into()),
        Page::Styles=>app.config.styles.get(app.selected()).map(|s|format!("{}\nApps: {}\nMode: {:?}\n\n{}",s.name,s.apps.join(", "),s.mode,s.prompt.as_deref().unwrap_or("Inherit cleanup instructions"))).unwrap_or_else(||"Choose a tone for each app. First matching style wins.\n\nN Add · Enter Edit · Delete Remove".into()),
        Page::Providers=>app.providers.get(app.selected()).map(|p|format!("{} [{}]\n{}\n\nModels:\n{}\n\n{}\n\nEnter activates · m model · k key\nT tests · b switches STT/cleanup\nCtrl+S saves and reloads",p.name,p.id,p.key,p.models.join("\n"),p.note)).unwrap_or_default(),
        Page::Settings=>rows.get(app.selected()).map(|(key,value)|format!("{key}\n\n{value}\n\nEnter edits with schema validation.\n←/→ cycles form choices.\nu resets to the default.\n\nCtrl+S saves + reloads\nCtrl+R reverts unsaved edits\ni chooses the microphone\n\nConfig: {}",app.path.display())).unwrap_or_default(),
        Page::Overlay=>app.overlay.get(app.selected()).map(|s|format!("{}\n\n{}\n\nValue: {}\nType: {:?}\n\nEnter edits and applies immediately.\nShortcuts accept GVariant lists, for example ['<Super>space'].",s.key,s.summary,s.value,s.kind)).unwrap_or_else(||"Overlay and shortcuts are GNOME extension settings.\n\nInstall: xflow extension install\nEnable: xflow extension enable\nr refreshes".into()),
        Page::Doctor=>"Diagnostics never record audio or send paid requests.\n\nFollow each row's fix hints.\n6 Providers: choose model and key.\n7 Settings: edit configuration.\n8 Overlay: global shortcuts.\nr reruns checks.".into(),_=>String::new(),
    };
    panel(frame, detail, "Details · PgUp/PgDn scroll", text, app);
}
fn stats(frame: &mut Frame, area: Rect, app: &App) {
    if !app.stats_loaded {
        panel(
            frame,
            area,
            "Your activity",
            "No stats yet. Start the daemon and press r to refresh.",
            app,
        );
        return;
    }
    let s = &app.stats;
    let time_saved = (s.words as f64 / 40.0 - s.audio_ms as f64 / 60000.0).max(0.0);
    let text=format!("{} sessions       {} words\n\nToday: {} sessions · {} words\nStreak: {} days\nSpeaking pace: {} words/minute\nTime saved: {:.1} minutes (40 typed WPM estimate)\n\nLatency p50: {}ms    p95: {}ms\nRecorded audio: {:.1} minutes\n\nr Refresh · history is stored locally",s.sessions,s.words,s.sessions_today,s.words_today,s.streak_days,s.wpm.map(|w|format!("{w:.0}")).unwrap_or_else(||"—".into()),time_saved,s.latency_p50_ms.unwrap_or(0),s.latency_p95_ms.unwrap_or(0),s.audio_ms as f64/60000.0);
    let rows = Layout::vertical([Constraint::Min(12), Constraint::Length(5)]).split(area);
    panel(frame, rows[0], "Your voice, in numbers", text, app);
    // ponytail: Stats has no daily buckets. Show loaded history's word counts;
    // add daily activity when the shared Stats contract exposes it.
    let activity = app
        .history
        .iter()
        .rev()
        .map(|h| h.text.split_whitespace().count() as u64)
        .collect::<Vec<_>>();
    frame.render_widget(
        Sparkline::default()
            .data(&activity)
            .style(app.theme.base().fg(app.theme.accent))
            .block(block(
                "Recent activity · words in loaded history page",
                app.theme,
            )),
        rows[1],
    );
}
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}
fn draw_modal(frame: &mut Frame, area: Rect, app: &App, modal: &Modal) {
    let theme = app.theme;
    let rect = popup(
        area,
        86,
        match modal {
            Modal::Help => 30,
            Modal::Palette { .. } => 21,
            Modal::Search => 6,
            Modal::Confirm { .. } => 8,
            Modal::Editor(e) => (e.fields.len() * 4 + 7).min(30) as u16,
        },
    );
    frame.render_widget(Clear, rect);
    frame.render_widget(Block::default().style(theme.base()), rect);
    match modal {
        Modal::Help => {
            let text = commands()
                .iter()
                .map(|c| format!("{:<16} {}", c.key, c.title))
                .collect::<Vec<_>>()
                .join("\n");
            frame.render_widget(Paragraph::new(format!("{text}\n\nForms: Tab field · Ctrl+J newline · Ctrl+Enter apply · Esc cancel\nPgUp/PgDn scrolls · Esc closes")).block(block("Keyboard guide",theme)).wrap(Wrap{trim:false}).scroll((app.scroll,0)),rect);
        }
        Modal::Palette { query, selected } => {
            let inner = block("Actions · fuzzy search · Esc closes", theme).inner(rect);
            frame.render_widget(block("Actions · fuzzy search · Esc closes", theme), rect);
            let parts = Layout::vertical([Constraint::Length(2), Constraint::Min(2)]).split(inner);
            frame.render_widget(Paragraph::new(format!("› {query}")), parts[0]);
            let items = commands()
                .into_iter()
                .filter(|c| fuzzy(query, &c.title))
                .map(|c| ListItem::new(format!("{:<12} {}", c.key, c.title)))
                .collect::<Vec<_>>();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_style(theme.selected())
                    .highlight_symbol("› "),
                parts[1],
                &mut state,
            );
        }
        Modal::Search => frame.render_widget(
            Paragraph::new(format!(
                "/ {}\n\nSearches all history as you type · Enter finishes · Esc closes",
                app.query
            ))
            .wrap(Wrap { trim: false })
            .block(block("Search history", theme)),
            rect,
        ),
        Modal::Confirm { prompt, .. } => frame.render_widget(
            Paragraph::new(format!("{prompt}\n\ny / Enter confirms · n / Esc cancels"))
                .wrap(Wrap { trim: false })
                .block(block("Confirm", theme)),
            rect,
        ),
        Modal::Editor(e) => {
            let inner =
                block("Edit · Tab field · Ctrl+Enter apply · Esc cancel", theme).inner(rect);
            frame.render_widget(
                block("Edit · Tab field · Ctrl+Enter apply · Esc cancel", theme),
                rect,
            );
            let first = e
                .focus
                .saturating_sub((inner.height.saturating_sub(3) / 4) as usize);
            let mut y = inner.y;
            for (i, field) in e.fields.iter().enumerate().skip(first) {
                if y + 3 > inner.bottom().saturating_sub(2) {
                    break;
                }
                let value = field.display();
                let display = value
                    .lines()
                    .rev()
                    .take(2)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                frame.render_widget(
                    Paragraph::new(display)
                        .block(block(
                            format!("{} {}", if i == e.focus { "›" } else { " " }, field.label),
                            theme,
                        ))
                        .style(if i == e.focus {
                            theme.selected()
                        } else {
                            theme.base()
                        })
                        .wrap(Wrap { trim: false }),
                    Rect::new(inner.x, y, inner.width, 3),
                );
                y += 4;
            }
            let error = e.error.as_deref().unwrap_or(
                "Ctrl+U clears · Ctrl+J newline · ←/→ choices · Enter single-field apply",
            );
            frame.render_widget(
                Paragraph::new(error)
                    .style(theme.base().fg(if e.error.is_some() {
                        theme.error
                    } else {
                        theme.muted
                    }))
                    .wrap(Wrap { trim: false }),
                Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 2),
            );
        }
    }
}
fn context_actions(page: Page) -> Vec<(Action, &'static str)> {
    match page {
        Page::Home => vec![
            (Action::Toggle, "Space Dictate"),
            (Action::Command, "C Command"),
            (Action::Cancel, "x Cancel"),
            (Action::Copy, "c Copy"),
            (Action::Paste, "p Paste"),
        ],
        Page::History => vec![
            (Action::Search, "/ Search"),
            (Action::Copy, "c Copy"),
            (Action::Paste, "p Paste"),
            (Action::Delete, "Del Remove"),
            (Action::Export, "e Export"),
        ],
        Page::Dictionary => vec![
            (Action::Add, "N Word"),
            (Action::AddReplacement, "A Replacement"),
            (Action::Edit, "Enter Edit"),
            (Action::Delete, "Del Remove"),
        ],
        Page::Snippets | Page::Styles => vec![
            (Action::Add, "N Add"),
            (Action::Edit, "Enter Edit"),
            (Action::Delete, "Del Remove"),
        ],
        Page::Providers => vec![
            (Action::Edit, "Enter Activate"),
            (Action::Key, "k Key"),
            (Action::Model, "m Model"),
            (Action::CheckProvider, "T Test"),
            (Action::ProviderKind, "b Kind"),
        ],
        Page::Settings => vec![
            (Action::Edit, "Enter Edit"),
            (Action::Unset, "u Reset"),
            (Action::Save, "Ctrl+S Save"),
            (Action::Revert, "Ctrl+R Revert"),
        ],
        Page::Overlay => vec![(Action::Edit, "Enter Edit"), (Action::Refresh, "r Refresh")],
        Page::Stats | Page::Doctor => vec![
            (Action::Refresh, "r Refresh"),
            (Action::Go(Page::Providers), "6 Providers"),
        ],
    }
}
