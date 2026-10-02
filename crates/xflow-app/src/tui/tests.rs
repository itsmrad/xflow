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

use super::{
    render,
    state::{Effect, Modal, PAGES},
    theme::{Theme, BUILTINS},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use std::{collections::BTreeMap, path::PathBuf};
fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> Vec<Effect> {
    app.key(KeyEvent::new(code, mods))
}
fn type_text(app: &mut App, text: &str) {
    app.paste(text);
}
fn populated() -> (tempfile::TempDir, App) {
    let (dir, mut app) = app();
    app.disk = Some(app.file.as_ref().unwrap().to_string());
    app.path = PathBuf::from("/isolated/xflow/config.toml");
    app.connected = true;
    app.connection = "Connected".into();
    app.event(Response {
        text: Some("The best ideas start with a conversation.".into()),
        provider: Some("groq".into()),
        model: Some("whisper-large-v3-turbo".into()),
        version: Some("0.1.0".into()),
        protocol: Some(2),
        ..Response::status(State::Success, 0.0)
    });
    app.history = vec![HistoryEntry {
        id: 42,
        text: "The best ideas start with a conversation.".into(),
        raw_text: Some("um the best ideas start with a conversation".into()),
        provider: "groq".into(),
        model: Some("whisper-large-v3-turbo".into()),
        app_id: Some("code".into()),
        language: Some("en".into()),
        duration_ms: Some(2800),
        latency_ms: Some(430),
        created_at: 1790859600,
        ..Default::default()
    }];
    app.total = 1;
    app.set("dictionary.words", r#"["xflow", "Ratatui"]"#)
        .unwrap();
    app.set(
        "snippets",
        r#"[{trigger="my email", text="me@example.com"}]"#,
    )
    .unwrap();
    app.dirty = false;
    (dir, app)
}
fn screen(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            render::draw(frame, app);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..height {
        let row = (0..width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>();
        text.push_str(row.trim_end());
        text.push('\n');
    }
    text
}
#[test]
fn responsive_snapshots_and_all_screens_render() {
    let (_dir, mut app) = populated();
    for (width, height) in [(120, 35), (80, 24), (34, 11), (1, 1)] {
        for page in PAGES {
            app.page = page;
            let text = screen(&app, width, height);
            assert!(!text.is_empty());
        }
    }
    for (width, height) in [(120, 35), (80, 24)] {
        for (name, page) in [
            ("home", Page::Home),
            ("history", Page::History),
            ("settings", Page::Settings),
        ] {
            app.page = page;
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("src/tui/snapshots/{name}-{width}x{height}.txt"));
            let actual = screen(&app, width, height);
            if std::env::var_os("XFLOW_UPDATE_TUI_SNAPSHOTS").is_some() {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, &actual).unwrap();
            }
            assert_eq!(
                actual,
                std::fs::read_to_string(&path).unwrap(),
                "{}",
                path.display()
            );
        }
    }
}
#[test]
fn forms_validate_without_losing_text_and_keep_multiline_snippets() {
    let (_dir, mut app) = app();
    app.page = Page::Snippets;
    app.action(Action::Add);
    type_text(&mut app, "signature");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::CONTROL);
    assert!(
        matches!(&app.modal,Some(Modal::Editor(e)) if e.error.is_some() && e.fields[0].value.as_str()=="signature")
    );
    type_text(&mut app, "Thanks,\nAda");
    key(&mut app, KeyCode::Enter, KeyModifiers::CONTROL);
    assert!(app.modal.is_none());
    assert_eq!(app.config.snippets[0].text, "Thanks,\nAda");
    assert!(app.dirty);
    app.action(Action::Add);
    type_text(&mut app, "signature");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    type_text(&mut app, "duplicate");
    key(&mut app, KeyCode::Enter, KeyModifiers::CONTROL);
    assert!(matches!(&app.modal,Some(Modal::Editor(e)) if e.error.is_some()));
    assert_eq!(app.config.snippets.len(), 1);
}
#[test]
fn every_optional_setting_and_theme_is_editable_and_invalid_colors_roll_back() {
    let (_dir, mut app) = app();
    let rows = app.settings();
    for key in [
        "stt.protocol",
        "stt.language",
        "cleanup.prompt",
        "recording.device",
        "sounds.error",
    ] {
        assert!(rows.iter().any(|(k, _)| k == key));
    }
    app.set("ui.themes.mine.bg", "#010203").unwrap();
    app.set("ui.theme", "mine").unwrap();
    let before = app.file.as_ref().unwrap().to_string();
    assert!(app.set("ui.themes.mine.bg", "invalid").is_err());
    assert_eq!(before, app.file.as_ref().unwrap().to_string());
    for name in BUILTINS {
        assert!(Theme::resolve(name, &BTreeMap::new(), ColorMode::TrueColor).is_ok());
        assert!(Theme::resolve(name, &BTreeMap::new(), ColorMode::Ansi).is_ok());
        assert_eq!(
            Theme::resolve(name, &BTreeMap::new(), ColorMode::None)
                .unwrap()
                .fg,
            ratatui::style::Color::Reset
        );
    }
    let mut custom = BTreeMap::new();
    custom.insert(
        "paper".into(),
        BTreeMap::from([("unknown".into(), "#ffffff".into())]),
    );
    assert!(Theme::resolve("paper", &custom, ColorMode::TrueColor).is_err());
}
#[test]
fn stale_detail_and_command_replies_do_not_roll_back_subscription() {
    let (_dir, mut app) = populated();
    app.status.state = State::Processing;
    app.response(Response::status(State::Idle, 0.0), Tag::Ordinary);
    assert_eq!(app.status.state, State::Processing);
    app.response(
        Response {
            entry: Some(HistoryEntry {
                id: 99,
                ..Default::default()
            }),
            ..Response::status(State::Idle, 0.0)
        },
        Tag::Detail(99),
    );
    assert!(app.detail.is_none());
    app.event(Response::status(State::Listening, f32::NAN));
    assert!(app.levels.iter().all(|v| *v <= 100));
    assert_eq!(app.status.provider.as_deref(), Some("groq"));
}
#[test]
fn save_conflicts_and_exports_preserve_existing_files() {
    let (dir, mut app) = app();
    let file = app.file.as_ref().unwrap();
    super::runtime::save(file, None).unwrap();
    let baseline = std::fs::read_to_string(file.path()).unwrap();
    app.set("ui.theme", "midnight").unwrap();
    std::fs::write(app.file.as_ref().unwrap().path(), "# external edit\n").unwrap();
    assert!(super::runtime::save(app.file.as_ref().unwrap(), Some(&baseline)).is_err());
    assert_eq!(
        std::fs::read_to_string(app.file.as_ref().unwrap().path()).unwrap(),
        "# external edit\n"
    );
    assert!(app.dirty);
    let path = dir.path().join("history.json");
    let entry = HistoryEntry {
        id: 7,
        text: "Private text".into(),
        ..Default::default()
    };
    super::runtime::export(&path, &entry).unwrap();
    assert!(super::runtime::export(&path, &entry).is_err());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
#[test]
fn palette_routes_actions_and_masked_key_never_appears_on_screen() {
    let (_dir, mut app) = populated();
    app.action(Action::Palette);
    for c in "gosettings".chars() {
        key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.page, Page::Settings);
    app.page = Page::Providers;
    app.providers = vec![super::integrations::Provider {
        id: "groq".into(),
        name: "Groq".into(),
        models: vec![],
        key: "Missing".into(),
        note: String::new(),
    }];
    app.action(Action::Key);
    type_text(&mut app, "test-secret-never-rendered");
    let text = screen(&app, 80, 24);
    assert!(!text.contains("test-secret"));
    assert!(text.contains('•'));
    let effects = key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        matches!(&effects[..],[Effect::SaveKey(id,secret)] if id=="groq" && secret.as_str()=="test-secret-never-rendered")
    );
}

#[test]
fn untrusted_transcripts_cannot_emit_terminal_controls() {
    let (_dir, mut app) = populated();
    app.transcript = "hello\u{1b}]52;c;clipboard\u{7}world".into();
    let text = screen(&app, 80, 24);
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains('\u{7}'));
    assert!(text.contains("hello"));
}

#[test]
fn cursor_edits_unicode_and_f2_submits_multifield_forms() {
    let (_dir, mut app) = app();
    app.page = Page::Dictionary;
    app.action(Action::Add);
    type_text(&mut app, "café");
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    type_text(&mut app, "e");
    key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
    assert_eq!(app.config.dictionary.words, ["cafe"]);
    app.page = Page::Snippets;
    app.action(Action::Add);
    type_text(&mut app, "sig");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    type_text(&mut app, "Thanks\nAda");
    key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
    assert_eq!(app.config.snippets[0].text, "Thanks\nAda");
}
#[test]
fn mouse_hit_targets_follow_scroll_and_modal_blocks_background_clicks() {
    let (_dir, mut app) = populated();
    app.page = Page::Doctor;
    let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
    let mut hits = render::HitMap::default();
    terminal
        .draw(|frame| hits = render::draw(frame, &app))
        .unwrap();
    assert!(hits.tabs.iter().any(|(_, p)| *p == Page::Doctor));
    assert!(!hits.tabs.iter().any(|(_, p)| *p == Page::Home));
    app.action(Action::Help);
    terminal
        .draw(|frame| hits = render::draw(frame, &app))
        .unwrap();
    assert!(hits.tabs.is_empty());
    assert!(hits.actions.is_empty());
}

#[test]
fn real_catalog_selection_resets_routes_and_models_without_losing_other_edits() {
    let (_dir, mut app) = app();
    app.set(
        "stt.endpoint",
        "http://127.0.0.1:8000/v1/audio/transcriptions",
    )
    .unwrap();
    app.set("stt.model", "old-model").unwrap();
    app.set("stt.protocol", "openai").unwrap();
    app.set("stt.api_key_env", "OLD_KEY").unwrap();
    app.set("recording.max_seconds", "123").unwrap();
    app.page = Page::Providers;
    app.refresh();
    assert_eq!(app.providers.len(), xflow_providers::stt_providers().len());
    app.selected[5] = app
        .providers
        .iter()
        .position(|p| p.id == "deepgram")
        .unwrap();
    app.action(Action::Model);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.config.stt.provider, "deepgram");
    assert_eq!(app.config.stt.model.as_deref(), Some("nova-3"));
    assert!(app.config.stt.endpoint.is_none());
    assert!(app.config.stt.protocol.is_none());
    assert!(app.config.stt.api_key_env.is_none());
    assert_eq!(app.config.recording.max_seconds, 123);
    assert!(app.dirty);
    app.action(Action::ProviderKind);
    assert_eq!(
        app.providers.len(),
        xflow_providers::cleanup_providers().len()
    );
    app.selected[5] = app.providers.iter().position(|p| p.id == "ollama").unwrap();
    let effects = app.action(Action::CheckProvider);
    assert!(
        matches!(&effects[..],[Effect::CheckProvider(config,true)] if config.cleanup.provider.as_deref()==Some("ollama"))
    );
    assert!(app.config.cleanup.provider.is_none()); // A connection check doesn't activate.
    app.action(Action::Edit);
    assert_eq!(app.config.cleanup.provider.as_deref(), Some("ollama"));
    assert!(
        super::integrations::selection(&app.config, "ollama", true, Some("bad\nmodel")).is_err()
    );
}

#[tokio::test]
async fn provider_connection_probe_uses_anonymous_loopback_get_without_audio() {
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line, "GET /v1/models HTTP/1.1\r\n");
        loop {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
            assert!(!line.to_ascii_lowercase().starts_with("authorization:"));
        }
        reader
            .get_mut()
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .await
            .unwrap();
    });
    let mut config = xflow_core::config::Config::default();
    config.privacy.offline = true;
    config.stt.provider = "custom".into();
    config.stt.endpoint = Some(format!("http://{address}/v1/audio/transcriptions"));
    config.stt.model = Some("mock".into());
    config.stt.timeout_secs = 2;
    assert!(!super::integrations::missing_key(&config).await);
    let result = super::integrations::check(&config, false).await.unwrap();
    assert!(result.contains('✓'));
    assert!(result.contains("non-billing"));
    tokio::time::timeout(std::time::Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn missing_key_has_guided_onboarding_with_an_existing_config_and_daemon() {
    let (_dir, mut app) = populated();
    app.missing_key = true;
    let text = screen(&app, 80, 24);
    assert!(text.contains("Getting started"));
    assert!(text.contains("Choose a provider"));
    let before = app.config.stt.clone();
    app.set("stt.api_key_env", "NEW_KEY").unwrap();
    app.key_status(&before, false);
    assert!(app.missing_key); // Obsolete credentials cannot suppress onboarding.
    app.key_status(&app.config.stt.clone(), false);
    assert!(!app.missing_key);
}
