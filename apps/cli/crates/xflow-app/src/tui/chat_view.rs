//! Draws the Chat page from its state; every decision lives in `chat.rs`.
use super::{
    chat::{Chat, Kind},
    render::{accent, block, muted},
    state::{Action, App},
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Padding, Paragraph},
    Frame,
};

fn style(app: &App, kind: Kind) -> Style {
    match kind {
        Kind::You => accent(app).add_modifier(Modifier::BOLD),
        Kind::Answer => app.theme.base(),
        Kind::Note => muted(app),
        Kind::Error => app.theme.base().fg(app.theme.error),
    }
}
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let ctx = app.chat_ctx();
    let chat = &app.chat;
    let input = chat.input_view(
        area.width.saturating_sub(4) as usize,
        (area.height / 3) as usize,
    );
    let parts = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(input.rows.len() as u16 + 2),
    ])
    .split(area);
    let pad = Padding::horizontal(1);

    let outer = block(app, chat.title(&ctx)).padding(pad);
    let inner = outer.inner(parts[0]);
    frame.render_widget(outer, parts[0]);
    let banner = Chat::banner(&ctx);
    let height = inner.height.saturating_sub(u16::from(banner.is_some()));
    let window = chat.window(inner.width as usize, height as usize);
    let mut lines: Vec<Line> = vec![];
    lines.extend(banner.map(|text| Line::styled(text, muted(app))));
    lines.extend(
        window
            .rows
            .into_iter()
            .map(|row| Line::styled(row.text, style(app, row.kind))),
    );
    frame.render_widget(Paragraph::new(lines).style(app.theme.base()), inner);

    let title = if window.below {
        "Ask · more below, ↓ to follow"
    } else {
        "Ask · Enter sends"
    };
    let prompt = block(app, title).padding(pad);
    let field = prompt.inner(parts[1]);
    let rows: Vec<Line> = input.rows.into_iter().map(Line::from).collect();
    frame.render_widget(
        Paragraph::new(rows).style(app.theme.base()).block(prompt),
        parts[1],
    );
    if app.modal.is_none() {
        let (row, column) = input.cursor;
        frame.set_cursor_position((
            (field.x + column as u16).min(field.right().saturating_sub(1)),
            (field.y + row as u16).min(field.bottom().saturating_sub(1)),
        ));
    }
}
pub fn keys(app: &App) -> Vec<(&'static str, Option<Action>)> {
    app.chat
        .footer()
        .into_iter()
        .map(|label| (label, None))
        .collect()
}
