use super::{
    ai_apps, chat_view,
    options::{self, glyph, value_text},
    state::{commands, fuzzy, Action, App, Editor, Modal, Page, Step, PAGES},
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Sparkline, Wrap},
    Frame,
};
use xflow_core::{
    ipc::{CleanupOutcome, Grant},
    Mode, State,
};

#[derive(Default)]
pub struct HitMap {
    pub tabs: Vec<(Rect, Page)>,
    pub rows: Vec<(Rect, usize)>,
    pub actions: Vec<(Rect, Action)>,
    pub options: Vec<(Rect, usize)>,
}
pub(super) fn block(app: &App, title: impl Into<String>) -> Block<'static> {
    let title = title.into();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(app.theme.base().fg(app.theme.muted))
        .style(app.theme.base());
    if title.is_empty() {
        block
    } else {
        block.title(Span::styled(
            format!(" {title} "),
            app.theme.base().add_modifier(Modifier::BOLD),
        ))
    }
}
pub(super) fn muted(app: &App) -> Style {
    app.theme.base().fg(app.theme.muted)
}
pub(super) fn accent(app: &App) -> Style {
    app.theme.base().fg(app.theme.accent)
}
fn panel(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    text: impl Into<ratatui::text::Text<'static>>,
    app: &App,
) {
    frame.render_widget(
        Paragraph::new(text.into())
            .style(app.theme.base())
            .block(block(app, title).padding(ratatui::widgets::Padding::horizontal(1)))
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0)),
        area,
    );
}
/// (glyph, word, hint, is_error) for the big status.
pub fn status(app: &App) -> (&'static str, &'static str, String, bool) {
    let message = app.status.message.as_deref().map(options::plain);
    if !app.connected {
        return (glyph("○", "o"), "Not running", app.helper_down(), true);
    }
    match app.status.state {
        State::Idle if app.needs_setup() => (
            glyph("○", "o"),
            "Not set up",
            "Press Enter to set up xflow — it takes about a minute.".into(),
            false,
        ),
        State::Idle => (
            glyph("●", "*"),
            "Ready",
            message.unwrap_or_else(|| match app.shortcut() {
                Some(key) => format!("Hold {key} and speak — or press Space here."),
                None => "Press Space to start dictating, and again to finish.".into(),
            }),
            false,
        ),
        State::Listening => (
            glyph("●", "*"),
            "Listening",
            "Speak now. Press Space to finish, or x to cancel.".into(),
            false,
        ),
        State::Processing if app.status.mode == Some(Mode::Ask) => (
            glyph("◌", "~"),
            "Answering",
            message.unwrap_or_else(|| "Thinking…".into()),
            false,
        ),
        State::Processing => (
            glyph("◌", "~"),
            "Thinking",
            "Turning your words into text…".into(),
            false,
        ),
        State::Success => (
            glyph("✓", "v"),
            "Done",
            message.unwrap_or_else(|| "Your text is in place. It's also saved in History.".into()),
            false,
        ),
        State::Error => (
            glyph("!", "!"),
            "Something went wrong",
            message.unwrap_or_else(|| "Please try again.".into()),
            true,
        ),
    }
}
pub fn draw(frame: &mut Frame, app: &App) -> HitMap {
    let area = frame.area();
    let theme = app.theme;
    let mut hits = HitMap::default();
    frame.render_widget(Block::default().style(theme.base()), area);
    if area.width < 40 || area.height < 12 {
        let (_, word, _, _) = status(app);
        // Letters are typing on a ready Chat page.
        let quit = if app.page == Page::Chat && app.chat_ready() {
            "Ctrl+C quit"
        } else {
            "q quit"
        };
        let text = format!("xflow · {word}\n\nThis window is too small.\nMake it bigger to see everything.\n\n{quit}");
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .style(theme.base()),
            area,
        );
        return hits;
    }
    // Long toasts wrap onto a second line instead of being clipped.
    let toast_lines = match &app.toast {
        Some(toast) if toast.chars().count() + 1 > area.width as usize => 2,
        _ => 1,
    };
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(toast_lines),
        Constraint::Length(1),
    ])
    .split(area);
    // Header: brand left, live status right.
    let (icon, word, _, error) = status(app);
    let mut chip = format!("{icon} {word}");
    if app.connected && app.status.state == State::Listening {
        chip.push_str(&format!("  {}", meter(app.status.level, 10)));
    }
    let header = Layout::horizontal([
        Constraint::Min(1),
        Constraint::Length(chip.chars().count() as u16 + 1),
    ])
    .split(rows[0]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" xflow", accent(app).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  ·  {}", app.page.title()), muted(app)),
        ])),
        header[0],
    );
    frame.render_widget(
        Paragraph::new(chip).style(
            app.theme
                .base()
                .fg(if error { theme.error } else { theme.accent })
                .add_modifier(Modifier::BOLD),
        ),
        header[1],
    );
    // 16 leaves 14 inside the border: " 3  Dictionary" and " 8  Your stats".
    let nav_width = if area.width >= 70 { 18 } else { 16 };
    let body =
        Layout::horizontal([Constraint::Length(nav_width), Constraint::Min(1)]).split(rows[1]);
    let selected = app.page.index();
    let items = PAGES
        .iter()
        .enumerate()
        .map(|(i, page)| {
            let mut label = format!(
                " {}  {}",
                ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-"][i],
                page.title()
            );
            if i == selected {
                label.replace_range(..1, ">");
            }
            ListItem::new(label)
        })
        .collect::<Vec<_>>();
    let mut nav = ListState::default().with_selected(Some(app.page.index()));
    frame.render_stateful_widget(
        List::new(items)
            .block(block(app, ""))
            .highlight_style(theme.selected()),
        body[0],
        &mut nav,
    );
    for (i, page) in PAGES.iter().enumerate().skip(nav.offset()) {
        let y = body[0].y + 1 + (i - nav.offset()) as u16;
        if y + 1 < body[0].bottom() {
            hits.tabs.push((
                Rect::new(body[0].x + 1, y, body[0].width.saturating_sub(2), 1),
                *page,
            ));
        }
    }
    match app.page {
        Page::Home => home(frame, body[1], app, &mut hits),
        Page::Dictionary => dictionary(frame, body[1], app, &mut hits),
        Page::Stats => stats(frame, body[1], app),
        Page::Styles => styles(frame, body[1], app, &mut hits),
        Page::AiApps => {
            // The two privacy lines stay visible under the list.
            let notes =
                [ai_apps::LEAVES, ai_apps::INJECTION].map(|s| format!("{}{s}", glyph("• ", "- ")));
            let width = body[1].width.saturating_sub(2);
            let height = notes.iter().map(|n| wrapped(n, width)).sum();
            let parts =
                Layout::vertical([Constraint::Min(8), Constraint::Length(height)]).split(body[1]);
            listing(frame, parts[0], app, &mut hits);
            frame.render_widget(
                Paragraph::new(notes.join("\n"))
                    .wrap(Wrap { trim: true })
                    .style(muted(app))
                    .block(Block::new().padding(ratatui::widgets::Padding::horizontal(1))),
                parts[1],
            );
        }
        Page::Chat if app.chat_ready() => chat_view::draw(frame, body[1], app),
        _ => listing(frame, body[1], app, &mut hits),
    }
    if let Some(toast) = &app.toast {
        frame.render_widget(
            Paragraph::new(format!(" {toast}"))
                .wrap(Wrap { trim: true })
                .style(accent(app).add_modifier(Modifier::BOLD)),
            rows[2],
        );
    }
    // Keys line: plain words, clickable when mouse is on.
    let mut x = rows[3].x + 1;
    for (i, (label, action)) in keys(app).into_iter().enumerate() {
        let gap = if i == 0 { 0 } else { 3 };
        let width = label.chars().count() as u16;
        if x + gap + width > rows[3].right() {
            break;
        }
        if gap > 0 {
            frame.render_widget(
                Paragraph::new(" · ").style(muted(app)),
                Rect::new(x, rows[3].y, gap, 1),
            );
        }
        let rect = Rect::new(x + gap, rows[3].y, width, 1);
        frame.render_widget(Paragraph::new(label).style(muted(app)), rect);
        if let Some(end) = label.find(' ') {
            let key = &label[..end];
            if key.len() == 1 || key.len() == 6 || (key.len() <= 5 && !key.starts_with('P')) {
                let cols = frame.area().width as usize;
                let start = rect.y as usize * cols + rect.x as usize;
                let buf = frame.buffer_mut();
                for cell in start..start + key.chars().count() {
                    buf.content[cell].modifier.insert(Modifier::BOLD);
                }
            }
        }
        if let Some(action) = action {
            hits.actions.push((rect, action));
        }
        x += gap + width;
    }
    if let Some(modal) = &app.modal {
        hits = HitMap::default();
        draw_modal(frame, rows[1], app, modal, &mut hits);
    }
    // IPC/config strings are untrusted terminal text. A control byte in a cell
    // must never become an OSC clipboard operation or another terminal escape.
    for cell in &mut frame.buffer_mut().content {
        if cell.symbol().chars().any(char::is_control) {
            let clean = cell
                .symbol()
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>();
            cell.set_symbol(if clean.is_empty() { " " } else { &clean });
        }
    }
    hits
}
/// The keys that work right now, in plain words.
pub fn keys(app: &App) -> Vec<(&'static str, Option<Action>)> {
    let pick = vec![
        ("↑↓ move", None),
        ("Enter choose", None),
        ("Esc back", None),
    ];
    match &app.modal {
        Some(Modal::Editor(e)) if e.capturing => vec![
            ("Press the key or combination now", None),
            ("Esc stop", None),
        ],
        Some(Modal::Editor(e)) if e.picking() && e.fields.len() > 1 => {
            vec![
                ("↑↓ change", None),
                ("Enter next", None),
                ("Tab field", None),
                ("Esc back", None),
            ]
        }
        Some(Modal::Editor(e)) if e.picking() => pick,
        Some(Modal::Editor(e)) if e.fields[e.focus].secret => vec![
            ("Paste your key", None),
            ("Enter save", None),
            ("Esc back", None),
        ],
        Some(Modal::Editor(e)) if e.focus + 1 < e.fields.len() => {
            vec![("Type", None), ("Enter next", None), ("Esc back", None)]
        }
        Some(Modal::Editor(_)) => vec![("Type", None), ("Enter save", None), ("Esc back", None)],
        Some(Modal::Confirm { .. }) => vec![("Enter yes", None), ("Esc no", None)],
        Some(Modal::Help) => vec![("↑↓ scroll", None), ("Esc close", None)],
        Some(Modal::Palette { .. }) => vec![
            ("Type to find", None),
            ("↑↓ move", None),
            ("Enter run", None),
            ("Esc close", None),
        ],
        Some(Modal::Search) => vec![
            ("Type to search", None),
            ("Enter done", None),
            ("Esc clear", None),
        ],
        Some(Modal::Setup(Step::Welcome)) => vec![("Enter start", None), ("Esc later", None)],
        Some(Modal::Setup(Step::MicCheck)) => {
            vec![("Enter sounds good", None), ("Esc stop setup", None)]
        }
        Some(Modal::Setup(Step::Permissions)) => {
            vec![("Enter ask again", None), ("Esc later", None)]
        }
        Some(Modal::Setup(Step::Globe)) => {
            vec![("Enter open Keyboard settings", None), ("Esc later", None)]
        }
        Some(Modal::Setup(Step::Try)) => vec![
            ("Space start / stop", None),
            ("Enter finish", None),
            ("Esc stop setup", None),
        ],
        Some(Modal::Setup(_)) => vec![("Enter close", None)],
        Some(Modal::OnDevice { removing: true, .. }) => {
            vec![("Enter remove", None), ("Esc keep", None)]
        }
        Some(Modal::OnDevice { .. }) => on_device_keys(app),
        Some(Modal::ChatConsent { .. }) => vec![("y allow", None), ("n or Esc don't send", None)],
        None if app.page == Page::Chat && app.chat_ready() => chat_view::keys(app),
        None => {
            let mut out: Vec<(&'static str, Option<Action>)> = match app.page {
                Page::Home => vec![
                    ("Space dictate", Some(Action::Toggle)),
                    ("c copy", Some(Action::Copy)),
                    ("p paste", Some(Action::Paste)),
                ],
                Page::History => vec![
                    ("/ search", Some(Action::Search)),
                    ("Enter options", Some(Action::Choose)),
                    ("↑↓ move", None),
                ],
                Page::Styles => {
                    let choose = if app.selected() == 0 {
                        "Enter add"
                    } else {
                        "Enter options"
                    };
                    return vec![
                        (choose, Some(Action::Choose)),
                        ("↑↓ move", None),
                        ("q quit", Some(Action::Quit)),
                        ("Del delete", Some(Action::Delete)),
                        ("? help", Some(Action::Help)),
                        ("Esc back", Some(Action::Back)),
                        ("Tab next page", None),
                    ];
                }
                // No "↑↓ move" here: the footer must fit "q quit" at 80 columns.
                Page::Dictionary => vec![
                    ("Enter options", Some(Action::Choose)),
                    ("q quit", Some(Action::Quit)),
                    ("Del delete", Some(Action::Delete)),
                    ("Tab next page", None),
                    ("? help", Some(Action::Help)),
                ],
                Page::Snippets => {
                    vec![
                        ("Enter choose", Some(Action::Choose)),
                        ("Del delete", Some(Action::Delete)),
                    ]
                }
                Page::Settings | Page::Overlay => {
                    vec![("↑↓ move", None), ("Enter change", Some(Action::Choose))]
                }
                Page::Chat => vec![("↑↓ move", None), ("Enter choose", Some(Action::Choose))],
                Page::Doctor if app.doctor_grant(app.selected()).is_some() => vec![
                    ("Enter fix", Some(Action::Choose)),
                    ("r refresh", Some(Action::Refresh)),
                ],
                Page::Stats | Page::Doctor => {
                    vec![("↑↓ move", None), ("r refresh", Some(Action::Refresh))]
                }
                Page::AiApps => vec![
                    ("Enter choose", Some(Action::Choose)),
                    ("r refresh", Some(Action::Refresh)),
                ],
            };
            if app.page == Page::Dictionary {
                return out;
            }
            if app.page != Page::Home {
                out.push(("Esc back", Some(Action::Back)));
            } else if app.status.failed_audio.is_some() {
                out.push(("R retry", Some(Action::Retry)));
            }
            out.extend([
                ("Tab next page", None),
                ("? help", Some(Action::Help)),
                ("q quit", Some(Action::Quit)),
            ]);
            out
        }
    }
}
/// A text level bar, e.g. `█████░░░░░`.
pub fn meter(level: f32, width: usize) -> String {
    let level = if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (level * width as f32).round() as usize;
    glyph("█", "#").repeat(filled) + &glyph("░", "-").repeat(width - filled)
}
/// Short bodies keep the status word and a Last dictation row; a settled body stays 8 + 6.
fn home_bands(height: u16, listening: bool) -> (u16, u16) {
    const STATUS: u16 = 8;
    const SETUP: u16 = 6;
    const LAST: u16 = 3;
    if height >= STATUS + SETUP + LAST {
        return (STATUS, SETUP);
    }
    let mut status = 3.min(height);
    let mut left = height - status;
    let last = LAST.min(left);
    left -= last;
    let setup = if left >= 3 { left.min(SETUP) } else { 0 };
    let spare = left - setup;
    let cap = if listening { STATUS } else { 6 };
    status += spare.min(cap.saturating_sub(status));
    (status, setup)
}
fn home(frame: &mut Frame, area: Rect, app: &App, hits: &mut HitMap) {
    let theme = app.theme;
    let listening = app.connected && app.status.state == State::Listening;
    let (status_h, setup_h) = home_bands(area.height, listening);
    let rows = Layout::vertical([
        Constraint::Length(status_h),
        Constraint::Length(setup_h),
        Constraint::Length(area.height.saturating_sub(status_h.saturating_add(setup_h))),
    ])
    .split(area);
    let (icon, word, hint, error) = status(app);
    let color = if error { theme.error } else { theme.accent };
    let card = block(app, "");
    let inner = card.inner(rows[0]);
    frame.render_widget(card, rows[0]);
    let indent = |y: u16, h: u16| {
        Rect::new(
            inner.x + 5,
            inner.y + y,
            inner.width.saturating_sub(6),
            h.min(inner.height.saturating_sub(y)),
        )
    };
    // The full card keeps a blank row above the word. A short card starts at the top.
    let word_y = u16::from(inner.height >= 6);
    if inner.height > word_y {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("  {icon}  {}", word.to_uppercase()),
                app.theme.base().fg(color).add_modifier(Modifier::BOLD),
            )),
            Rect::new(inner.x, inner.y + word_y, inner.width, 1),
        );
    }
    let hint_y = word_y + 1;
    let hint_h = inner.height.saturating_sub(hint_y).min(2);
    if hint_h > 0 {
        frame.render_widget(
            Paragraph::new(hint)
                .style(muted(app))
                .wrap(Wrap { trim: true }),
            indent(hint_y, hint_h),
        );
    }
    if listening {
        let spark_y = hint_y + hint_h;
        let spark_h = inner.height.saturating_sub(spark_y).min(2);
        if spark_h > 0 {
            let levels = app.levels.iter().copied().collect::<Vec<_>>();
            frame.render_widget(
                Sparkline::default()
                    .data(&levels)
                    .max(100)
                    .style(accent(app)),
                indent(spark_y, spark_h),
            );
        }
    }
    if setup_h > 0 && app.needs_setup() {
        panel(
            frame,
            rows[1],
            "Let's get you set up",
            "Choose a speech service, add its key, pick your microphone and your shortcut.\nPress Enter to start — it takes about a minute.",
            app,
        );
        hits.actions.push((rows[1], Action::Setup));
    } else if setup_h > 0 {
        let model = app
            .status
            .model
            .clone()
            .or(app.config.stt.model.clone())
            .unwrap_or_default();
        let lines = [
            (
                "Shortcut",
                app.shortcut()
                    .map(|k| format!("{k} — hold to talk, double-tap for hands-free"))
                    .unwrap_or_else(|| "Space in this window".into()),
            ),
            (
                "Speech",
                format!(
                    "{} · {}",
                    value_text(
                        "stt.provider",
                        app.status
                            .provider
                            .as_deref()
                            .unwrap_or(&app.config.stt.provider)
                    )
                    .split(" — ")
                    .next()
                    .unwrap_or(""),
                    value_text("stt.model", &model)
                ),
            ),
            (
                "Formatting",
                value_text(
                    "cleanup.mode",
                    super::state::mode_label(app.config.cleanup.mode),
                ),
            ),
            (
                "Microphone",
                value_text(
                    "recording.device",
                    app.config.recording.device.as_deref().unwrap_or(""),
                ),
            ),
        ];
        let text = lines
            .into_iter()
            .map(|(k, v)| {
                Line::from(vec![
                    Span::styled(format!("{k:<12}"), muted(app)),
                    Span::raw(v),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(text)
                .block(block(app, "Your setup").padding(ratatui::widgets::Padding::horizontal(1))),
            rows[1],
        );
        hits.actions.push((rows[1], Action::Go(Page::Settings)));
    }
    let last = if app.transcript.is_empty() {
        Line::from(Span::styled(
            match app.shortcut() {
                Some(key) => format!("Nothing yet — hold {key} and speak."),
                None => "Nothing yet — press Space and speak.".into(),
            },
            muted(app),
        ))
        .into()
    } else {
        ratatui::text::Text::from(app.transcript.clone())
    };
    let title = if app.status.mode == Some(Mode::Ask) {
        "Answer"
    } else {
        "Last dictation"
    };
    panel(frame, rows[2], title, last, app);
}
// Dictionary owns its layout so compact terminals do not change other lists.
fn dictionary(frame: &mut Frame, area: Rect, app: &App, hits: &mut HitMap) {
    let (list, detail) = if area.width >= 90 {
        let parts = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area);
        (parts[0], parts[1])
    } else {
        let detail_height = (area.height / 3).clamp(4, 8);
        let parts =
            Layout::vertical([Constraint::Min(5), Constraint::Length(detail_height)]).split(area);
        (parts[0], parts[1])
    };
    let rows = app.rows();
    let count = rows.len().saturating_sub(app.adds());
    let title = format!("Dictionary ({count})");
    let list_block = block(app, title);
    let inner = list_block.inner(list);
    frame.render_widget(list_block, list);
    let parts = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    frame.render_widget(
        Paragraph::new("↑↓ move  n add word").style(muted(app)),
        parts[0],
    );
    let items = rows
        .iter()
        .enumerate()
        .map(|(index, (label, kind))| {
            let line = if index < app.adds() {
                Line::from(Span::styled(
                    label.clone(),
                    accent(app).add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(vec![
                    Span::styled(format!("{kind}: "), muted(app)),
                    Span::raw(label.clone()),
                ])
            };
            ListItem::new(line)
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.selected()));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(app.theme.selected())
            .highlight_symbol("> "),
        parts[1],
        &mut state,
    );
    for index in state.offset()..rows.len() {
        let y = parts[1].y + (index - state.offset()) as u16;
        if y >= parts[1].bottom() {
            break;
        }
        hits.rows
            .push((Rect::new(parts[1].x, y, parts[1].width, 1), index));
    }
    let (title, text) = if count == 0 {
        (
            "Start here",
            "No saved words yet.\nAdd a word or correction above.".into(),
        )
    } else {
        details(app, &rows)
    };
    let title = if app.selected() < app.adds() {
        title
    } else {
        "Enter options / Del"
    };
    let text = if app.selected() >= app.adds() {
        format!("{text}\n\nDel deletes this entry.")
    } else {
        text
    };
    panel(frame, detail, title, text, app);
}

fn styles(frame: &mut Frame, area: Rect, app: &App, hits: &mut HitMap) {
    let (list, detail) = if area.width >= 90 {
        let parts = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        (parts[0], parts[1])
    } else {
        let parts = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length((area.height / 2).clamp(4, 12)),
        ])
        .split(area);
        (parts[0], parts[1])
    };
    let rows = app.rows();
    let items = rows
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            ListItem::new(Span::styled(
                name.clone(),
                if i == 0 {
                    accent(app).add_modifier(Modifier::BOLD)
                } else {
                    app.theme.base()
                },
            ))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.selected()));
    frame.render_stateful_widget(
        List::new(items)
            .block(block(app, format!("Styles ({})", app.config.styles.len())))
            .highlight_style(app.theme.selected())
            .highlight_symbol("> "),
        list,
        &mut state,
    );
    for index in state.offset()..rows.len() {
        let y = list.y + 1 + (index - state.offset()) as u16;
        if y >= list.bottom().saturating_sub(1) {
            break;
        }
        hits.rows.push((
            Rect::new(list.x + 1, y, list.width.saturating_sub(2), 1),
            index,
        ));
    }
    let selected = app
        .selected()
        .checked_sub(1)
        .and_then(|i| app.config.styles.get(i));
    let (title, text) = match selected {
        Some(s) => ("Selected style", format!(
            "{}\nApps: {}\nCleanup: {}\nTone: {}",
            s.name,
            s.apps.iter().map(|a| options::app_name(a)).collect::<Vec<_>>().join(", "),
            s.mode.map(|m| value_text("cleanup.mode", super::state::mode_label(m)))
                .unwrap_or_else(|| "Same as Settings".into()),
            s.prompt.as_deref().filter(|p| !p.is_empty()).unwrap_or("Default for cleanup mode"),
        )),
        None if app.config.styles.is_empty() => ("Start here", "No styles yet.\nEnter adds a writing tone for specific apps.".into()),
        None => ("Add a style", "Choose a name, matching apps, cleanup and tone.\nThe first matching style is used. Enter to add.".into()),
    };
    let parts = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(detail);
    let detail_block = block(app, title).padding(ratatui::widgets::Padding::horizontal(1));
    let page_height = detail_block.inner(parts[0]).height.clamp(1, 8);
    frame.render_widget(
        Paragraph::new(text)
            .style(app.theme.base())
            .block(detail_block)
            .wrap(Wrap { trim: false })
            .scroll(((app.scroll / 8).saturating_mul(page_height), 0)),
        parts[0],
    );
    frame.render_widget(
        Paragraph::new("PgUp/PgDn details").style(muted(app)),
        parts[1],
    );
}

fn listing(frame: &mut Frame, area: Rect, app: &App, hits: &mut HitMap) {
    let rows = app.rows();
    if app.page == Page::History && rows.is_empty() {
        panel(frame, area, "History", empty(app), app);
        return;
    }
    let (list, detail) = if area.width >= 90 {
        let parts = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area);
        (parts[0], parts[1])
    } else if app.page == Page::History {
        // Share short windows; taller windows earn a longer transcript preview.
        let detail_height = (area.height / 2).clamp(5, 12);
        let parts =
            Layout::vertical([Constraint::Min(4), Constraint::Length(detail_height)]).split(area);
        (parts[0], parts[1])
    } else {
        let parts = Layout::vertical([Constraint::Min(6), Constraint::Length(8)]).split(area);
        (parts[0], parts[1])
    };
    let title = match app.page {
        Page::History if !app.query.is_empty() => format!("History · matching “{}”", app.query),
        Page::History if app.total > app.history.len() as u64 => {
            format!(
                "History · {}–{} of {}",
                app.offset + 1,
                app.offset + app.history.len(),
                app.total
            )
        }
        _ => app.page.title().into(),
    };
    if rows.is_empty() {
        panel(frame, list, &title, empty(app), app);
    } else {
        let two_line = app.page == Page::History && area.height >= 18;
        let items = rows
            .iter()
            .enumerate()
            .map(|(i, (label, value))| {
                if app.page == Page::History && !two_line {
                    ListItem::new(label.clone())
                } else {
                    item(app, i, label, value, &rows)
                }
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default().with_selected(Some(app.selected()));
        frame.render_stateful_widget(
            List::new(items)
                .block(block(app, title))
                .highlight_style(app.theme.selected())
                .highlight_symbol(glyph("› ", "> ")),
            list,
            &mut state,
        );
        let height = if two_line { 2 } else { 1 };
        for index in state.offset()..rows.len() {
            let y = list.y + 1 + ((index - state.offset()) * height) as u16;
            if y + 1 >= list.bottom() {
                break;
            }
            hits.rows.push((
                Rect::new(
                    list.x + 1,
                    y,
                    list.width.saturating_sub(2),
                    (height as u16).min(list.bottom().saturating_sub(y + 1)),
                ),
                index,
            ));
        }
    }
    let (title, text) = details(app, &rows);
    panel(frame, detail, title, text, app);
}
fn item<'a>(
    app: &App,
    index: usize,
    label: &str,
    value: &str,
    rows: &[(String, String)],
) -> ListItem<'a> {
    match app.page {
        Page::History => ListItem::new(vec![
            Line::from(label.to_string()),
            Line::from(Span::styled(format!("  {value}"), muted(app))),
        ]),
        Page::Settings => {
            let (group, name) = options::row(label)
                .map(|r| (r.1, r.2))
                .unwrap_or(("", label));
            let first = index == 0 || options::row(&rows[index - 1].0).map(|r| r.1) != Some(group);
            Line::from(vec![
                Span::styled(
                    format!("{:<10}", if first { group } else { "" }),
                    muted(app).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{name:<22}")),
                Span::styled(
                    value.to_string(),
                    if label.starts_with("action.") {
                        accent(app)
                    } else {
                        muted(app)
                    },
                ),
            ])
            .into()
        }
        Page::AiApps => Line::from(vec![
            Span::raw(format!("{label:<22}")),
            Span::styled(
                value.to_string(),
                if value.contains('!') {
                    app.theme.base().fg(app.theme.error)
                } else if index < 2 {
                    accent(app)
                } else {
                    muted(app)
                },
            ),
        ])
        .into(),
        Page::Doctor => {
            let style = if label.starts_with('!') {
                app.theme.base().fg(app.theme.error)
            } else {
                app.theme.base()
            };
            ListItem::new(Span::styled(label.to_string(), style))
        }
        _ if index < app.adds() => ListItem::new(Span::styled(
            label.to_string(),
            accent(app).add_modifier(Modifier::BOLD),
        )),
        _ => Line::from(vec![
            Span::raw(format!("{label:<28}")),
            Span::styled(value.lines().next().unwrap_or("").to_string(), muted(app)),
        ])
        .into(),
    }
}
fn empty(app: &App) -> String {
    let speak = match app.shortcut() {
        Some(key) => format!("hold {key} and speak"),
        None => "press Space on Home and speak".into(),
    };
    match app.page {
        Page::History if !app.connected => format!("Your history shows up once xflow's background helper is running.\n\n{}", app.helper_down()),
        Page::History if !app.query.is_empty() => format!("Nothing matches “{}”.\n\nPress Esc to clear the search.", app.query),
        Page::History => format!("No dictations yet — {speak}."),
        Page::Overlay if cfg!(target_os = "macos") => "The floating recording pill is part of xflow on Linux (GNOME).\n\nOn this Mac, the status is shown at the top of this window.".into(),
        Page::Overlay if app.overlay_error => "The recording pill needs the xflow GNOME extension.\n\nInstall it with the xflow setup command, then press r.".into(),
        _ => "Loading…".into(),
    }
}
fn details(app: &App, rows: &[(String, String)]) -> (&'static str, String) {
    let pos = app.selected();
    let adds = app.adds();
    match app.page {
        Page::History => {
            let text = app.detail.as_ref().or_else(|| app.history.get(pos)).map(|h| {
                let mut text = format!("{}\n\n", h.text);
                let mut said = format!("Said {}", options::ago(h.created_at, app.now()));
                if let Some(app_id) = &h.app_id {
                    said.push_str(&format!(" in {}", options::app_name(app_id)));
                }
                text.push_str(&said);
                if let (Some(d), Some(l)) = (h.duration_ms, h.latency_ms) {
                    text.push_str(&format!("\n{:.1} s of speech · ready in {:.1} s", d as f64 / 1000.0, l as f64 / 1000.0));
                }
                if let Some(raw) = h.raw_text.as_deref().filter(|r| r.trim() != h.text.trim()) {
                    text.push_str(&format!("\n\nBefore cleanup:\n{raw}"));
                }
                let kept = match h.cleanup {
                    Some(CleanupOutcome::Rejected) => Some("it changed too much"),
                    Some(CleanupOutcome::Slow) => Some("it was too slow"),
                    Some(CleanupOutcome::Failed) => Some("it failed"),
                    _ => None,
                };
                if let Some(why) = kept {
                    text.push_str(&format!("\n\nCleanup was not used: {why}. These are your words as transcribed."));
                }
                if h.cleanup == Some(CleanupOutcome::Discarded) {
                    text.push_str("\n\nDiscarded: you took it back, so nothing was pasted.");
                }
                text
            });
            ("Details", text.unwrap_or_else(|| "Pick a dictation to see it in full.".into()))
        }
        Page::Dictionary => ("About", match pos {
            0 => "Add names, brands and terms so xflow recognizes them and spells them your way.".into(),
            1 => "A correction swaps a word xflow keeps mishearing for the one you meant — for example “post grass” → “Postgres”.".into(),
            _ => match app.config.dictionary.words.get(pos - adds) {
                Some(word) => format!("{word}\n\nxflow recognizes this word and spells it this way.\n\nEnter for edit / delete options."),
                None => app.config.dictionary.replacements.get(pos - adds - app.config.dictionary.words.len())
                    .map(|r| format!("When you say “{}”, xflow writes “{}”.\n\nEnter for edit / delete options.", r.from, r.to))
                    .unwrap_or_default(),
            },
        }),
        Page::Snippets => ("About", match pos.checked_sub(adds).and_then(|i| app.config.snippets.get(i)) {
            Some(s) => format!("When you say “{}”, xflow inserts:\n\n{}", s.trigger, s.text),
            None => "Snippets save typing: say a short phrase like “my email” and xflow inserts the full text.".into(),
        }),
        Page::Settings => ("About", rows.get(pos).map(|(key, value)| {
            let (_, group, label, help) = options::row(key).copied().unwrap_or(("", "", "", ""));
            let verb = if key.starts_with("action.") { "Enter to open." } else { "Enter to change." };
            format!("{group} · {label}\n{value}\n\n{help}\n\n{verb}")
        }).unwrap_or_default()),
        Page::Overlay => ("About", app.overlay.get(pos).map(|s| format!("{}\n{}\n\nEnter to change. Changes apply right away.", s.summary, super::state::overlay_value(&s.value))).unwrap_or_default()),
        Page::AiApps => ("Details", ai_apps::details(app)),
        Page::Chat => ("Set up chat", app.chat_details(pos)),
        Page::Doctor => ("Details", rows.get(pos).map(|(row, more)| if more.is_empty() { row } else { more }.clone()).unwrap_or_else(|| "Checking…".into())),
        _ => ("", String::new()),
    }
}
fn stats(frame: &mut Frame, area: Rect, app: &App) {
    if !app.stats_loaded {
        let text = if app.connected {
            "No dictations yet — your stats appear after your first one.".into()
        } else {
            format!(
                "Your stats show up once xflow's background helper is running.\n\n{}",
                app.helper_down()
            )
        };
        panel(frame, area, "Your stats", text, app);
        return;
    }
    let s = &app.stats;
    let time_saved = (s.words as f64 / 40.0 - s.audio_ms as f64 / 60000.0).max(0.0);
    let seconds = |ms: Option<u64>| {
        ms.map(|v| format!("{:.1} s", v as f64 / 1000.0))
            .unwrap_or_else(|| "—".into())
    };
    let lines = [
        ("Dictations", format!("{} · {} words", s.sessions, s.words)),
        (
            "Today",
            format!("{} dictations · {} words", s.sessions_today, s.words_today),
        ),
        (
            "Streak",
            format!(
                "{} {} in a row",
                s.streak_days,
                if s.streak_days == 1 { "day" } else { "days" }
            ),
        ),
        (
            "Speaking pace",
            s.wpm
                .map(|w| format!("{w:.0} words a minute"))
                .unwrap_or_else(|| "—".into()),
        ),
        (
            "Time saved",
            format!("about {time_saved:.0} minutes compared with typing"),
        ),
        (
            "Typical wait",
            format!(
                "{} (slowest {})",
                seconds(s.latency_p50_ms),
                seconds(s.latency_p95_ms)
            ),
        ),
    ];
    let text = lines
        .into_iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k:<15}"), muted(app)),
                Span::raw(v),
            ])
        })
        .collect::<Vec<_>>();
    let rows = Layout::vertical([Constraint::Length(8), Constraint::Min(4)]).split(area);
    frame.render_widget(
        Paragraph::new(text)
            .block(block(app, "Your stats").padding(ratatui::widgets::Padding::horizontal(1))),
        rows[0],
    );
    frame.render_widget(
        Sparkline::default()
            .data(&app.activity)
            .style(accent(app))
            .block(block(app, "Words in your recent dictations")),
        rows[1],
    );
}
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    // Too narrow for side margins: span the whole width so borders don't overlap.
    let w = if area.width <= width + 4 {
        area.width
    } else {
        width
    };
    let h = height.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}
fn draw_modal(frame: &mut Frame, area: Rect, app: &App, modal: &Modal, hits: &mut HitMap) {
    let theme = app.theme;
    let height = match modal {
        Modal::Help => 23,
        Modal::Palette { .. } => 20,
        Modal::Search => 7,
        Modal::Confirm { .. } | Modal::ChatConsent { .. } => 8,
        Modal::Setup(_) => 16,
        Modal::OnDevice { .. } => 22,
        Modal::Editor(e) if e.picking() => {
            let field = &e.fields[e.focus];
            (field.choices.len() + e.fields.len() + intro_height(&e.intro, 70) as usize + 6).min(24)
                as u16
        }
        Modal::Editor(e) => (e.fields.len() * 4 + 8) as u16,
    };
    let rect = popup(area, 76, height);
    frame.render_widget(Clear, rect);
    let pad = ratatui::widgets::Padding::horizontal(1);
    match modal {
        Modal::Help => {
            let mut text = vec![
                "Getting around".to_string(),
                "  ↑ ↓      move            Enter   choose or change".into(),
                "  Esc      go back         Tab     next page".into(),
                "  1 – 0    jump to a page  ?       this help".into(),
                String::new(),
                "Dictating".into(),
                "  Space    start / stop    x       cancel".into(),
                "  c        copy last       p       paste last".into(),
                String::new(),
                "Chat (typing goes to the box)".into(),
                "  Enter    send            Esc     cancel or leave".into(),
                "  Ctrl+R   retry           Ctrl+N  new chat".into(),
                String::new(),
                "Lists".into(),
                "  n        add             Del     delete".into(),
                "  /        search history  Ctrl+P  find any action".into(),
                String::new(),
                "Changes save automatically. q quits.".into(),
            ];
            if app.config.ui.mouse {
                text.push("You can also click and scroll with the mouse.".into());
            }
            frame.render_widget(
                Paragraph::new(text.join("\n"))
                    .block(block(app, "Help").padding(pad))
                    .scroll((app.scroll, 0)),
                rect,
            );
        }
        Modal::Palette { query, selected } => {
            let outer = block(app, "Find an action").padding(pad);
            let inner = outer.inner(rect);
            frame.render_widget(outer, rect);
            let parts = Layout::vertical([Constraint::Length(2), Constraint::Min(2)]).split(inner);
            frame.render_widget(
                Paragraph::new(format!("{} {query}▏", glyph("›", ">"))),
                parts[0],
            );
            let items = commands()
                .into_iter()
                .filter(|c| fuzzy(query, &c.title))
                .map(|c| {
                    ListItem::new(Line::from(vec![
                        Span::raw(format!("{:<40}", c.title)),
                        Span::styled(c.key, muted(app)),
                    ]))
                })
                .collect::<Vec<_>>();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items).highlight_style(theme.selected()),
                parts[1],
                &mut state,
            );
        }
        Modal::Search => frame.render_widget(
            Paragraph::new(format!(
                "{} {}▏\n\nResults update as you type.",
                glyph("⌕", "/"),
                app.query
            ))
            .block(block(app, "Search history").padding(pad)),
            rect,
        ),
        Modal::Confirm { prompt, .. } => frame.render_widget(
            Paragraph::new(format!("{prompt}\n\nEnter = yes   ·   Esc = no"))
                .wrap(Wrap { trim: false })
                .block(block(app, "Are you sure?").padding(pad)),
            rect,
        ),
        Modal::ChatConsent { name, .. } => frame.render_widget(
            Paragraph::new(App::chat_consent_text(name))
                .wrap(Wrap { trim: false })
                .block(block(app, "Share your dictations?").padding(pad)),
            rect,
        ),
        Modal::Setup(step) => setup(frame, rect, app, *step),
        Modal::Editor(e) => editor(frame, rect, app, e, hits),
        Modal::OnDevice {
            setup,
            removing,
            error,
        } => frame.render_widget(
            Paragraph::new(app.on_device_text(*removing, error.as_deref()))
                .wrap(Wrap { trim: false })
                .block(
                    block(
                        app,
                        if *setup {
                            "Step 2 of 4 · On this Mac (offline)"
                        } else {
                            "Speech · On this Mac (offline)"
                        },
                    )
                    .padding(pad),
                ),
            rect,
        ),
    }
}
fn on_device_keys(app: &App) -> Vec<(&'static str, Option<Action>)> {
    use super::on_device::{Job, Next};
    let mut keys = match app.on_device_next() {
        Next::Back => vec![("Enter back", None)],
        Next::Wait => vec![],
        Next::Install => vec![("Enter install", None)],
        Next::Retry => vec![("Enter retry", None)],
        Next::Use => vec![("Enter use it", None)],
        Next::Done => vec![("Enter done", None)],
    };
    if matches!(app.install, Job::Running { .. }) {
        keys.push(("x cancel", None));
    }
    if app.facts.as_ref().is_some_and(|f| f.installed)
        && matches!(app.install, Job::Idle | Job::Failed(_))
    {
        keys.push(("d remove", None));
    }
    keys.push(("Esc back", None));
    keys
}
fn setup(frame: &mut Frame, rect: Rect, app: &App, step: Step) {
    let speak = match app.shortcut() {
        Some(key) => format!("Hold {key} and speak anywhere"),
        None => "Press Space on Home and speak".into(),
    };
    let (title, text) = match step {
        Step::Welcome => ("Welcome to xflow", "Speak, and your words appear wherever you're typing.\n\nLet's set things up — it takes about a minute:\n\n   1  Choose a speech service\n   2  Add its API key\n   3  Pick your microphone\n   4  Choose your shortcut\n\nPress Enter to begin, or Esc to do this later.".to_string()),
        Step::MicCheck if !app.connected => ("Step 3 of 4 · Microphone check", format!("The microphone can be checked once xflow's background helper is running.\n\n{}\n\nPress Enter to continue anyway.", app.helper_down())),
        Step::MicCheck => ("Step 3 of 4 · Microphone check", format!(
            "Say something. The bar should move while you speak.\n\n   {}\n\nNothing is recorded or sent during this check.\nIf the bar stays still, press Esc and pick another microphone in Settings.",
            meter(if app.status.state == State::Listening { app.status.level } else { 0.0 }, 30)
        )),
        Step::Try => {
            let line = match app.status.state {
                State::Listening => format!("Listening…   {}", meter(app.status.level, 20)),
                State::Processing => "Thinking…".into(),
                State::Success if !app.transcript.is_empty() => format!("You said: “{}”\n\nPerfect. Press Enter to finish.", app.transcript.trim()),
                State::Error => options::plain(app.status.message.as_deref().unwrap_or("")),
                _ => "Press Space to start.".into(),
            };
            let how = match app.shortcut() {
                Some(key) => format!("Hold {key}, say something, then let go.\nOr press Space to start, and Space again to stop."),
                None => "Press Space, say a sentence, then press Space again.".into(),
            };
            ("Try it", format!("{how}\nYour words show up here.\n\n{line}"))
        }
        Step::Permissions if app.permissions.is_none() && !app.connected => ("Almost done · Mac permissions", format!("{}\n\nPermissions are checked once xflow's background helper is running; this moves on by itself.", app.helper_down())),
        Step::Globe => ("Almost done · Globe key", "Your shortcut is the Fn (Globe) key, so set it to do nothing else: in System Settings › Keyboard, set “Press 🌐 key to” to “Do Nothing”.\n\nOtherwise each press also opens emoji or switches your input source.\n\nWaiting for the change — this moves on by itself.".into()),
        Step::Permissions => {
            let ask = match app.grant {
                Some(Grant::InputMonitoring) => "Your shortcut isn't reaching xflow yet. In System Settings, switch on xflowd under Input Monitoring.",
                Some(Grant::Accessibility) => "In System Settings, switch on xflowd under Accessibility so xflow can hear your shortcut and type your words into apps.",
                Some(Grant::Microphone) if app.microphone == Some(false) => "In System Settings, switch on xflowd under Microphone so xflow can hear you.",
                _ => "Checking the microphone… If your Mac asks, click Allow.",
            };
            // Only after waiting a while or asking again: an old xflowd row that looks on.
            let stale = if app.stale_hint() && (app.grant != Some(Grant::Microphone) || app.microphone == Some(false)) {
                format!("{}\n\n", xflow_core::ipc::STALE_ROWS)
            } else {
                String::new()
            };
            ("Almost done · Mac permissions", format!("{stale}{ask}\n\nWaiting for the switch — this moves on by itself."))
        }
        _ => ("You're all set", format!("{speak} — your words appear where you're typing.\n\nEverything you dictate is also saved in History.\nPress Enter to go to Home.")),
    };
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(block(app, title).padding(ratatui::widgets::Padding::uniform(1))),
        rect,
    );
}
fn editor(frame: &mut Frame, rect: Rect, app: &App, e: &Editor, hits: &mut HitMap) {
    let outer = block(app, e.title.clone()).padding(ratatui::widgets::Padding::horizontal(1));
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);
    let intro_lines = intro_height(&e.intro, inner.width);
    let error = if e.capturing {
        Some("Press the key or combination you want now…".to_string())
    } else {
        e.error.clone()
    };
    let parts = Layout::vertical([
        Constraint::Length(intro_lines),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(e.intro.clone())
            .style(muted(app))
            .wrap(Wrap { trim: false }),
        parts[0],
    );
    if let Some(error) = error {
        let style = if e.capturing {
            accent(app)
        } else {
            app.theme.base().fg(app.theme.error)
        };
        frame.render_widget(
            Paragraph::new(error)
                .style(style)
                .wrap(Wrap { trim: false }),
            parts[2],
        );
    }
    let body = parts[1];
    if e.picking() {
        let summary = if e.fields.len() > 1 {
            e.fields.len() as u16 + 1
        } else {
            0
        };
        let split = Layout::vertical([Constraint::Length(summary), Constraint::Min(1)]).split(body);
        let lines = e
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let label = f.labels.get(f.position()).cloned().unwrap_or_default();
                let style = if i == e.focus {
                    app.theme.base().add_modifier(Modifier::BOLD)
                } else {
                    muted(app)
                };
                Line::from(vec![
                    Span::styled(
                        format!(
                            "{} {:<10}",
                            if i == e.focus { glyph("›", ">") } else { " " },
                            f.label
                        ),
                        style,
                    ),
                    Span::styled(label, style),
                ])
            })
            .collect::<Vec<_>>();
        if summary > 0 {
            frame.render_widget(Paragraph::new(lines), split[0]);
        }
        let field = &e.fields[e.focus];
        let mut state = ListState::default().with_selected(Some(field.position()));
        let items = field
            .labels
            .iter()
            .map(|l| ListItem::new(l.clone()))
            .collect::<Vec<_>>();
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(app.theme.selected())
                .highlight_symbol(glyph("› ", "> ")),
            split[1],
            &mut state,
        );
        for (row, index) in (state.offset()..field.choices.len()).enumerate() {
            if row as u16 >= split[1].height {
                break;
            }
            hits.options.push((
                Rect::new(split[1].x, split[1].y + row as u16, split[1].width, 1),
                index,
            ));
        }
    } else {
        let mut y = body.y;
        for (i, field) in e.fields.iter().enumerate() {
            if y + 3 > body.bottom() {
                break;
            }
            let value = if i == e.focus {
                field.caret()
            } else {
                field.display()
            };
            let style = if i == e.focus {
                app.theme.base()
            } else {
                muted(app)
            };
            frame.render_widget(
                Paragraph::new(value).style(style).block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(if i == e.focus {
                            accent(app)
                        } else {
                            muted(app)
                        })
                        .title(format!(" {} ", field.label)),
                ),
                Rect::new(body.x, y, body.width, 3),
            );
            y += 4;
        }
    }
}
/// Rows the intro takes when wrapped, plus one blank line below it.
/// Lines a word-wrapped paragraph of `text` takes at `width`.
fn wrapped(text: &str, width: u16) -> u16 {
    let width = width.max(1) as usize;
    let (mut lines, mut used) = (1, 0);
    for word in text.split(' ').map(|w| w.chars().count()) {
        if used > 0 && used + 1 + word > width {
            lines += 1;
            used = word;
        } else {
            used += word + usize::from(used > 0);
        }
    }
    lines
}
fn intro_height(intro: &str, width: u16) -> u16 {
    if intro.is_empty() {
        return 0;
    }
    let width = width.max(1) as usize;
    let lines: usize = intro
        .lines()
        .map(|l| l.chars().count().div_ceil(width).max(1))
        .sum();
    (lines as u16 + 1).min(6)
}
