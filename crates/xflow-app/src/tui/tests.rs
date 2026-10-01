use super::{
    state::{Action, App, Page, Tag},
    theme::ColorMode,
};
use crate::config_edit::ConfigFile;
use xflow_core::{
    ipc::{HistoryEntry, Response},
    State,
};

fn app() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let file = ConfigFile::load(&path).unwrap();
    let mut app = App::new(path, ColorMode::TrueColor);
    app.load(file, None).unwrap();
    (dir, app)
}
#[test]
fn stale_history_responses_never_replace_current_search() {
    let (_dir, mut app) = app();
    app.page = Page::History;
    app.generation = 2;
    let response = Response {
        history: vec![HistoryEntry {
            text: "old".into(),
            ..Default::default()
        }],
        ..Response::status(State::Idle, 0.0)
    };
    app.response(response, Tag::History(1));
    assert!(app.history.is_empty());
}
#[test]
fn validation_and_quit_preserve_unsaved_edits() {
    let (_dir, mut app) = app();
    app.set("recording.max_seconds", "600").unwrap();
    assert!(app.set("recording.max_seconds", "601").is_err());
    assert_eq!(app.config.recording.max_seconds, 600);
    app.action(Action::Quit);
    assert!(!app.quit);
    assert!(app.modal.is_some());
}
