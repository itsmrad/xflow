use super::{
    options, render,
    state::{Action, App, EditTarget, Effect, Modal, Page, Step, Tag, PAGES},
    theme::{ColorMode, Theme, BUILTINS},
};
use crate::config_edit::ConfigFile;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use std::{collections::BTreeMap, path::PathBuf};
use xflow_core::{
    ipc::{Delivery, HistoryEntry, Request, Response},
    State,
};
use xflow_providers::KeySource;

fn app() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let file = ConfigFile::load(&path).unwrap();
    let mut app = App::new(path, ColorMode::TrueColor);
    app.load(file, None).unwrap();
    app.now = Some(1790859600 + 120);
    (dir, app)
}
fn key(app: &mut App, code: KeyCode) -> Vec<Effect> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE))
}
fn populated() -> (tempfile::TempDir, App) {
    let (dir, mut app) = app();
    app.disk = Some(app.file.as_ref().unwrap().to_string());
    app.path = PathBuf::from("/isolated/xflow/config.toml");
    app.connected = true;
    app.event(Response {
        text: Some("The best ideas start with a conversation.".into()),
        provider: Some("groq".into()),
        model: Some("whisper-large-v3-turbo".into()),
        ..Response::status(State::Success, 0.0)
    });
    app.history = vec![HistoryEntry {
        id: 42,
        text: "The best ideas start with a conversation.".into(),
        raw_text: Some("um the best ideas start with a conversation".into()),
        provider: "groq".into(),
        app_id: Some("com.apple.mail".into()),
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
    app.disk = Some(app.file.as_ref().unwrap().to_string());
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
fn editor(app: &App) -> &super::state::Editor {
    match &app.modal {
        Some(Modal::Editor(e)) => e,
        _ => panic!("no editor open"),
    }
}
/// Developer artifacts that must never reach the screen.
fn assert_plain(text: &str) {
    for jargon in [
        "stt.",
        "cleanup.",
        "recording.",
        "(unset)",
        "toml",
        "TOML",
        "\"",
        "daemon",
        "socket",
        "/isolated",
    ] {
        assert!(!text.contains(jargon), "{jargon:?} leaked:\n{text}");
    }
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
    app.set("recording.max_seconds", "1200").unwrap();
    assert!(app.set("recording.max_seconds", "1201").is_err());
    assert_eq!(app.config.recording.max_seconds, 1200);
    app.action(Action::Quit);
    assert!(!app.quit);
    assert!(app.modal.is_some());
}
#[test]
fn every_screen_renders_in_plain_language_at_any_size() {
    let (_dir, mut app) = populated();
    app.advanced = true;
    for (width, height) in [(120, 35), (80, 24), (34, 11), (1, 1)] {
        for page in PAGES {
            app.page = page;
            let text = screen(&app, width, height);
            assert!(!text.is_empty());
            if width >= 80 {
                assert_plain(&text);
            }
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
fn speaker_cancelling_toggle_starts_off_and_saves_both_choices() {
    let (_dir, mut app) = app();
    app.page = Page::Settings;
    app.selected[Page::Settings.index()] = app
        .settings()
        .iter()
        .position(|(name, _)| name == "recording.ignore_speakers")
        .unwrap();
    assert!(!app.config.recording.ignore_speakers);
    for enabled in [true, false] {
        key_enter(&mut app);
        key(&mut app, if enabled { KeyCode::Up } else { KeyCode::Down });
        key_enter(&mut app);
        assert!(app.modal.is_none());
        let file = app.file.as_ref().unwrap();
        file.save().unwrap();
        assert_eq!(
            ConfigFile::load(file.path())
                .unwrap()
                .config()
                .unwrap()
                .recording
                .ignore_speakers,
            enabled
        );
    }
}
#[test]
fn every_setting_is_an_option_list_that_ignores_typing_and_stages_a_choice() {
    let (_dir, mut app) = populated();
    app.page = Page::Settings;
    app.advanced = true;
    let rows = app.settings();
    for (index, (name, _)) in rows.iter().enumerate() {
        if name.starts_with("action.") || name == "recording.device" {
            continue;
        }
        app.selected[Page::Settings.index()] = index;
        key_enter(&mut app);
        let before = editor(&app).fields[0].value.to_string();
        assert!(editor(&app).picking(), "{name} is not a list");
        for c in "zz9".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert_eq!(
            editor(&app).fields[0].value.as_str(),
            before,
            "{name} accepted typing"
        );
        key(&mut app, KeyCode::Down);
        let effects = key_enter(&mut app);
        assert!(effects.is_empty());
        assert!(app.modal.is_none(), "{name}: {:?}", editor_error(&app));
    }
    assert!(app.dirty);
}
#[test]
fn esc_keeps_recordings_off_home_and_busy_reloads_retry() {
    let (_dir, mut app) = app();
    app.connected = true;
    app.event(Response::status(State::Listening, 0.1));
    app.page = Page::Settings;
    let cancels = |effects: &[Effect]| {
        effects
            .iter()
            .any(|e| matches!(e, Effect::Request(Request::Cancel, _)))
    };
    assert!(!cancels(&key(&mut app, KeyCode::Esc)));
    assert_eq!(app.page, Page::Home);
    assert!(cancels(&key(&mut app, KeyCode::Esc)));
    // A reload refused while busy is remembered and retried after the in-flight one.
    let refused = Response::error(State::Listening, "reload requires an idle daemon");
    app.response(refused, Tag::Reload);
    assert!(app.reload_pending);
    assert_plain(app.toast.as_deref().unwrap());
    let effects = app.response(Response::status(State::Idle, 0.0), Tag::Reload);
    assert!(matches!(
        &effects[..],
        [Effect::Request(Request::Reload, Tag::Reload)]
    ));
    assert!(!app.reload_pending);
}
fn key_enter(app: &mut App) -> Vec<Effect> {
    key(app, KeyCode::Enter)
}
fn editor_error(app: &App) -> Option<String> {
    match &app.modal {
        Some(Modal::Editor(e)) => e.error.clone(),
        _ => None,
    }
}
#[test]
fn hotkey_capture_records_chord_syntax() {
    let (_dir, mut app) = populated();
    app.page = Page::Settings;
    app.selected[Page::Settings.index()] = app
        .settings()
        .iter()
        .position(|(k, _)| k == "hotkey.key")
        .unwrap();
    key_enter(&mut app);
    key(&mut app, KeyCode::End);
    assert_eq!(editor(&app).fields[0].value.as_str(), options::CAPTURE);
    key_enter(&mut app);
    assert!(editor(&app).capturing);
    app.key(KeyEvent::new(
        KeyCode::Char(' '),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ));
    assert_eq!(editor(&app).fields[0].value.as_str(), "ctrl+alt+space");
    assert_eq!(options::key_label("ctrl+alt+space"), "Ctrl+Alt+Space");
    assert_eq!(options::key_label("right_option"), "Right Option");
    assert_eq!(options::key_label("f13"), "F13");
}
#[test]
fn cleanup_reuses_the_speech_service_and_save_refuses_what_reload_would_reject() {
    let (_dir, mut app) = app();
    app.page = Page::Settings;
    app.selected[Page::Settings.index()] = app
        .settings()
        .iter()
        .position(|(k, _)| k == "cleanup.mode")
        .unwrap();
    key_enter(&mut app);
    key(&mut app, KeyCode::Down);
    key_enter(&mut app);
    assert!(app.modal.is_none());
    assert_eq!(app.config.cleanup.provider.as_deref(), Some("groq"));
    super::runtime::save(app.file.as_ref().unwrap(), None).unwrap();
    let mut file = ConfigFile::load(app.file.as_ref().unwrap().path()).unwrap();
    file.unset("cleanup.provider").unwrap();
    let disk = std::fs::read_to_string(file.path()).unwrap();
    let error = super::runtime::save(&file, Some(&disk)).unwrap_err();
    assert_eq!(
        options::plain(&format!("{error:#}")),
        "Cleanup needs a writing service. Choose one in Settings → Writing → Service."
    );
}
#[test]
fn save_conflicts_preserve_external_edits() {
    let (_dir, mut app) = app();
    let file = app.file.as_ref().unwrap();
    super::runtime::save(file, None).unwrap();
    let baseline = std::fs::read_to_string(file.path()).unwrap();
    app.set("ui.theme", "midnight").unwrap();
    std::fs::write(app.file.as_ref().unwrap().path(), "# external edit\n").unwrap();
    let error = super::runtime::save(app.file.as_ref().unwrap(), Some(&baseline)).unwrap_err();
    assert!(options::plain(&error.to_string()).contains("changed somewhere else"));
    assert_eq!(
        std::fs::read_to_string(app.file.as_ref().unwrap().path()).unwrap(),
        "# external edit\n"
    );
}
#[test]
fn palette_routes_and_masked_key_never_appears_on_screen() {
    let (_dir, mut app) = populated();
    app.action(Action::Palette);
    for c in "gosettings".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key_enter(&mut app);
    assert_eq!(app.page, Page::Settings);
    app.selected[Page::Settings.index()] = app
        .settings()
        .iter()
        .position(|(k, _)| k == "action.stt_key")
        .unwrap();
    key_enter(&mut app);
    app.paste("test-secret-never-rendered\n");
    let text = screen(&app, 80, 24);
    assert!(!text.contains("test-secret"));
    assert!(text.contains('•'));
    assert!(text.contains("console.groq.com/keys"));
    let effects = key_enter(&mut app);
    assert!(
        matches!(&effects[..], [Effect::SaveKey(id, secret)] if id == "groq" && secret.as_str() == "test-secret-never-rendered")
    );
    // A custom cleanup server's key lives under its own key entry.
    app.set("cleanup.provider", "custom").unwrap();
    app.set("cleanup.endpoint", "http://127.0.0.1:9/v1/chat/completions")
        .unwrap();
    app.set("cleanup.model", "local").unwrap();
    app.selected[Page::Settings.index()] = app
        .settings()
        .iter()
        .position(|(k, _)| k == "action.cleanup_key")
        .unwrap();
    key_enter(&mut app);
    app.paste("cleanup-secret");
    let effects = key_enter(&mut app);
    assert!(matches!(&effects[..], [Effect::SaveKey(id, _)] if id == "cleanup"));
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
fn history_corrections_and_removed_learned_words_feed_learning() {
    let (_dir, mut app) = populated();
    // History → Enter → "Correct this transcript" opens the text, Enter saves it.
    app.page = Page::History;
    key_enter(&mut app);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key_enter(&mut app);
    assert!(matches!(&editor(&app).target, EditTarget::Transcript(42)));
    app.paste(" Thanks");
    let effects = key_enter(&mut app);
    assert!(matches!(
        &effects[..],
        [Effect::Request(Request::HistoryUpdate { id: 42, text }, _)]
            if text == "The best ideas start with a conversation. Thanks"
    ));
    // Deleting a learned word works like `xflow dictionary remove`.
    app.set("dictionary.learned", r#"["xflow"]"#).unwrap();
    app.page = Page::Dictionary;
    app.selected[Page::Dictionary.index()] = 2;
    key_enter(&mut app);
    key(&mut app, KeyCode::Down);
    key_enter(&mut app);
    key_enter(&mut app);
    assert_eq!(app.config.dictionary.words, ["Ratatui"]);
    assert!(app.config.dictionary.learned.is_empty());
    assert_eq!(app.config.dictionary.never_learn, ["xflow"]);
}
#[test]
fn lists_work_with_arrows_enter_and_esc_only() {
    let (_dir, mut app) = populated();
    app.page = Page::Dictionary;
    // "+ Add a word" is the first row.
    key_enter(&mut app);
    app.paste("café");
    key(&mut app, KeyCode::Left);
    key(&mut app, KeyCode::Delete);
    app.paste("e");
    key_enter(&mut app);
    assert_eq!(app.config.dictionary.words, ["xflow", "Ratatui", "cafe"]);
    // Enter on an item opens its options; Delete asks first.
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key_enter(&mut app);
    assert!(matches!(&editor(&app).target, EditTarget::Menu(_)));
    key(&mut app, KeyCode::Down);
    key_enter(&mut app);
    assert!(matches!(app.modal, Some(Modal::Confirm { .. })));
    key_enter(&mut app);
    assert_eq!(app.config.dictionary.words, ["Ratatui", "cafe"]);
    // Snippets: Enter moves to the next field, then saves.
    app.page = Page::Snippets;
    app.selected[Page::Snippets.index()] = 0;
    key_enter(&mut app);
    app.paste("sig");
    key_enter(&mut app);
    key_enter(&mut app);
    assert!(editor_error(&app).is_some());
    app.paste("Thanks\nAda");
    key_enter(&mut app);
    assert_eq!(app.config.snippets[1].text, "Thanks\nAda");
    // Esc goes back home without cancelling anything.
    assert!(key(&mut app, KeyCode::Esc)
        .iter()
        .all(|e| !matches!(e, Effect::Request(Request::Cancel, _))));
    assert_eq!(app.page, Page::Home);
}
#[test]
fn mouse_targets_follow_layout_and_modals_capture_clicks() {
    let (_dir, mut app) = populated();
    app.page = Page::Doctor;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut hits = render::HitMap::default();
    terminal
        .draw(|frame| hits = render::draw(frame, &app))
        .unwrap();
    assert!(hits.tabs.iter().any(|(_, p)| *p == Page::Doctor));
    assert!(hits.actions.iter().any(|(_, a)| *a == Action::Help));
    app.page = Page::Settings;
    app.selected[Page::Settings.index()] = app
        .settings()
        .iter()
        .position(|(k, _)| k == "ui.theme")
        .unwrap();
    key_enter(&mut app);
    terminal
        .draw(|frame| hits = render::draw(frame, &app))
        .unwrap();
    assert!(hits.tabs.is_empty());
    let midnight = BUILTINS.iter().position(|t| *t == "midnight").unwrap();
    assert!(hits.options.iter().any(|(_, i)| *i == midnight));
    app.pick(midnight);
    assert_eq!(app.config.ui.theme, "midnight");
    for name in BUILTINS {
        assert!(Theme::resolve(name, &BTreeMap::new(), ColorMode::Ansi).is_ok());
    }
}
#[test]
fn choosing_a_service_resets_its_routing_but_keeps_other_settings() {
    let (_dir, mut app) = app();
    app.set(
        "stt.endpoint",
        "http://127.0.0.1:8000/v1/audio/transcriptions",
    )
    .unwrap();
    app.set("stt.model", "old-model").unwrap();
    app.set("stt.api_key_env", "OLD_KEY").unwrap();
    app.set("recording.max_seconds", "123").unwrap();
    app.page = Page::Settings;
    app.selected[Page::Settings.index()] = app
        .settings()
        .iter()
        .position(|(k, _)| k == "stt.provider")
        .unwrap();
    key_enter(&mut app);
    while editor(&app).fields[0].value.as_str() != "deepgram" {
        key(&mut app, KeyCode::Down);
    }
    key_enter(&mut app);
    assert_eq!(app.config.stt.provider, "deepgram");
    assert!(
        app.config.stt.model.is_none()
            && app.config.stt.endpoint.is_none()
            && app.config.stt.api_key_env.is_none()
    );
    assert_eq!(app.config.recording.max_seconds, 123);
    assert!(app
        .settings()
        .iter()
        .any(|(k, v)| k == "stt.model" && v == "Recommended"));
}
#[test]
fn guided_setup_is_all_choices_except_the_key() {
    let (_dir, mut app) = app();
    app.connected = true;
    assert!(app.needs_setup());
    key_enter(&mut app); // Home → Welcome
    assert!(matches!(app.modal, Some(Modal::Setup(Step::Welcome))));
    assert_plain(&screen(&app, 80, 24));
    key_enter(&mut app);
    assert!(editor(&app).picking());
    while editor(&app).fields[0].value.as_str() != "openai" {
        key(&mut app, KeyCode::Down);
    }
    key_enter(&mut app);
    assert_eq!(app.config.stt.provider, "openai");
    assert!(editor(&app).fields[0].secret);
    app.paste("sk-test");
    let effects = key_enter(&mut app);
    assert!(matches!(&effects[..], [Effect::SaveKey(id, _), Effect::Devices] if id == "openai"));
    app.devices(vec!["USB Mic".into()]);
    key(&mut app, KeyCode::Down);
    key_enter(&mut app);
    assert_eq!(app.config.recording.device.as_deref(), Some("USB Mic"));
    assert!(matches!(app.modal, Some(Modal::Setup(Step::MicCheck))));
    // The level check starts once the daemon reloaded the new microphone, and is
    // cancelled before the daemon's own 120 s limit could upload it.
    let effects = app.response(Response::status(State::Idle, 0.0), Tag::Reload);
    assert!(matches!(
        &effects[..],
        [
            Effect::Request(
                Request::Start {
                    delivery: Delivery::None,
                    ..
                },
                _
            ),
            Effect::MicTimeout(after)
        ] if after.as_secs() < 10
    ));
    app.event(Response::status(State::Listening, 0.7));
    assert!(screen(&app, 80, 24).contains('█'));
    let effects = key_enter(&mut app);
    assert!(matches!(
        &effects[..],
        [Effect::Request(Request::Cancel, _)]
    ));
    app.event(Response::status(State::Idle, 0.0));
    key_enter(&mut app); // Shortcut
    assert!(matches!(app.modal, Some(Modal::Setup(Step::Try))));
    let effects = key(&mut app, KeyCode::Char(' '));
    assert!(matches!(
        &effects[..],
        [Effect::Request(
            Request::Toggle {
                delivery: Delivery::None,
                ..
            },
            _
        )]
    ));
    key_enter(&mut app);
    assert!(matches!(app.modal, Some(Modal::Setup(Step::Done))));
    key_enter(&mut app);
    assert!(app.modal.is_none() && app.setup.is_none());
}
#[test]
fn errors_become_one_plain_sentence() {
    assert_eq!(
        options::plain(
            "Daemon unavailable: cannot connect to /run/user/1000/xflow/daemon.sock: No such file"
        ),
        "xflow's background helper isn't running yet — xflow starts it by itself."
    );
    assert!(options::plain("API key unavailable; set GROQ_API_KEY")
        .starts_with("This service needs an API key"));
    assert!(options::plain(
        "API key unavailable; set the provider environment variable or run `xflow key set groq`"
    )
    .starts_with("This service needs an API key"));
    assert!(options::plain(
        "stored groq key can't be decrypted (damaged, or keystore.key changed); re-enter your key: `xflow key set groq`"
    )
    .contains("Add it again"));
    assert!(options::plain(
        "your groq key is still in the macOS Keychain, which asks before xflow can read it; run `xflow key status` once to move it"
    )
    .contains("xflow key status"));
    assert_eq!(
        options::plain("weird failure: at /tmp/x.toml line 3"),
        "Weird failure."
    );
    assert_eq!(options::value_text("stt.language", ""), "Automatic");
    assert_eq!(
        options::value_text("recording.device", "(unset)"),
        "System default"
    );
    assert_eq!(options::value_text("sounds.volume", "0.5"), "50%");
    assert_eq!(options::ago(0, 125), "2 min ago");
    assert_eq!(options::app_name("com.tinyspeck.slackmacgap"), "Slack");
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
    assert_eq!(
        super::integrations::missing_keys(&config).await,
        (KeySource::NotRequired, KeySource::NotRequired)
    );
    let result = super::integrations::check(&config, false).await.unwrap();
    assert!(result.starts_with("Connected"));
    tokio::time::timeout(std::time::Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}
#[test]
fn stale_key_lookups_cannot_suppress_setup() {
    let (_dir, mut app) = populated();
    app.missing_key = true;
    assert!(screen(&app, 80, 24).contains("Let's get you set up"));
    let before = app.config.clone();
    app.set("stt.api_key_env", "NEW_KEY").unwrap();
    app.key_status(&before, (KeySource::Keychain, KeySource::Keystore));
    assert!(app.missing_key);
    assert!(app.key_issue.is_none());
    let current = app.config.clone();
    app.key_status(&current, (KeySource::Keystore, KeySource::Keystore));
    assert!(!app.missing_key);
}
#[test]
fn keychain_and_missing_key_belong_to_their_own_routes() {
    let (_dir, mut app) = populated();
    app.set("cleanup.provider", "openai").unwrap();
    let config = app.config.clone();
    app.key_status(&config, (KeySource::Missing, KeySource::Keychain));
    let settings = app.settings();
    assert!(settings
        .iter()
        .any(|(k, v)| k == "action.stt_key" && v.contains("Missing")));
    assert!(settings
        .iter()
        .any(|(k, v)| k == "action.cleanup_key" && v.contains("Needs attention")));
    app.page = Page::Doctor;
    assert!(app
        .rows()
        .iter()
        .any(|(text, _)| text.contains("no API key")));
    app.set("cleanup.provider", "groq").unwrap();
    app.key_status(&config, (KeySource::Keychain, KeySource::Keystore));
    assert!(app.key_issue.is_some());
    assert!(
        app.cleanup_key_issue.is_some(),
        "stale cleanup result replaced the current route"
    );
}
#[test]
fn key_left_in_keychain_is_not_a_missing_key_and_the_wizard_never_vanishes() {
    let (_dir, mut app) = populated();
    let config = app.config.clone();
    app.key_status(&config, (KeySource::Keychain, KeySource::Keystore));
    assert!(!screen(&app, 100, 32).contains("Not set up"));
    assert!(app
        .settings()
        .iter()
        .any(|(k, v)| k == "action.stt_key" && v.contains("Needs attention")));
    // "Already added one?" with an empty key shows the microphone step at once.
    app.open_setup(Step::Key);
    assert!(matches!(&key_enter(&mut app)[..], [Effect::Devices]));
    assert!(
        matches!(&app.modal, Some(Modal::Editor(e)) if matches!(e.target, EditTarget::Setup(Step::Microphone)))
    );
    // With no saved key, an empty Enter explains instead of closing.
    app.key_status(&config, (KeySource::Missing, KeySource::Keystore));
    app.open_setup(Step::Key);
    key_enter(&mut app);
    assert!(editor(&app)
        .error
        .as_deref()
        .is_some_and(|e| e.contains("No saved key")));
    // A slow device list never covers the step the wizard moved on to.
    app.open_setup(Step::Welcome);
    app.devices(vec!["USB Mic".into()]);
    assert!(matches!(app.modal, Some(Modal::Setup(Step::Welcome))));
}
