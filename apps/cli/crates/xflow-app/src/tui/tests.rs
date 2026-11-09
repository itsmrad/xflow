use super::{
    options, render,
    state::{Action, App, EditTarget, Effect, Modal, Page, Step, Tag, PAGES},
    theme::{ColorMode, Theme, BUILTINS},
};
use crate::config_edit::ConfigFile;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, style::Modifier, Terminal};
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
fn snippets_keep_text_actions_selection_and_hits_in_compact_terminals() {
    let (_dir, mut app) = populated();
    app.page = Page::Snippets;
    app.set("snippets", r#"[
        {trigger="Ada", text="Ship it."},
        {trigger="Kubernetes PostgreSQL useEffect git rebase OAuth kubectl", text="José and Mei: deploy 42 pods at 14:30.\nThen check error 503 and retry."},
        {trigger="Priya और Arjun", text="कल API deploy करना है, फिर tests चलाओ।"}
    ]"#).unwrap();
    let fixture = app.config.snippets.clone();
    for (theme, mode) in [
        ("midnight", ColorMode::TrueColor),
        ("paper", ColorMode::TrueColor),
        ("paper", ColorMode::None),
    ] {
        app.theme = Theme::resolve(theme, &BTreeMap::new(), mode).unwrap();
        for (width, height) in [(120, 32), (80, 24), (40, 16)] {
            for selected in 0..=fixture.len() {
                app.selected[Page::Snippets.index()] = selected;
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut hits = render::HitMap::default();
                terminal.draw(|f| hits = render::draw(f, &app)).unwrap();
                let text = screen(&app, width, height);
                assert!(text.contains("↑↓ move"), "{text}");
                assert!(text.contains("q quit"), "{text}");
                assert!(
                    text.contains(if selected == 0 {
                        "Enter add"
                    } else {
                        "Enter options"
                    }),
                    "{text}"
                );
                assert!(
                    text.contains(if selected == 0 {
                        "Add a snippet"
                    } else {
                        "Inserted text"
                    }),
                    "{text}"
                );
                if selected > 0 {
                    assert!(text.contains("Del delete"), "{text}");
                    assert!(
                        text.contains(if selected == 1 {
                            "Ship it."
                        } else if selected == 2 {
                            "José and Mei:"
                        } else {
                            "कल API"
                        }),
                        "{text}"
                    );
                }
                let (selected_rect, _) = hits
                    .rows
                    .iter()
                    .find(|(_, i)| *i == selected)
                    .expect("selected row has a hit region");
                let cell = &terminal.backend().buffer()[(selected_rect.x, selected_rect.y)];
                assert_eq!(cell.symbol(), ">");
                assert!(cell.modifier.contains(Modifier::BOLD | Modifier::REVERSED));
                for (rect, index) in &hits.rows {
                    assert!(*index <= fixture.len());
                    assert_eq!(rect.height, 1);
                    for x in rect.x..rect.right() {
                        assert!(!["─", "╰", "╯"]
                            .contains(&terminal.backend().buffer()[(x, rect.y)].symbol()));
                    }
                }
                key(&mut app, KeyCode::Enter);
                if selected > 0 {
                    assert!(matches!(&editor(&app).target, EditTarget::Menu(_)));
                    key(&mut app, KeyCode::Enter);
                }
                assert!(matches!(app.modal, Some(Modal::Editor(_))));
                key(&mut app, KeyCode::Esc);
                if selected > 0 {
                    key(&mut app, KeyCode::Delete);
                    assert!(matches!(app.modal, Some(Modal::Confirm { .. })));
                    key(&mut app, KeyCode::Esc);
                }
                assert_eq!(app.config.snippets.len(), fixture.len());
            }
            app.config.snippets.clear();
            app.selected[Page::Snippets.index()] = 0;
            let text = screen(&app, width, height);
            assert!(text.contains("No snippets yet"), "{text}");
            assert!(text.contains("Enter add"), "{text}");
            app.config.snippets = fixture.clone();
        }
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
fn history_keeps_selection_details_and_mouse_rows_at_short_heights() {
    let (_dir, mut app) = populated();
    app.page = Page::History;
    app.history = [
        "Ada: ship it.",
        "Kubernetes, PostgreSQL, useEffect, git rebase, OAuth and kubectl: check the deployment before Friday at 14:30.",
        "Priya और José: कल API deploy करना है, फिर tests चलाओ।",
        "A very long application name with several words and a short dictation",
        "Mei: ready.",
    ].into_iter().enumerate().map(|(i, text)| HistoryEntry {
        id: i as i64,
        text: text.into(),
        app_id: Some("A deliberately long editor application name".into()),
        ..app.history[0].clone()
    }).collect();
    app.total = app.history.len() as u64;
    let fixture = app.history.clone();
    let capture = std::env::var_os("XFLOW_HISTORY_CAPTURES").map(PathBuf::from);
    for (theme, mode) in [
        ("midnight", ColorMode::TrueColor),
        ("paper", ColorMode::TrueColor),
        ("none", ColorMode::None),
    ] {
        app.theme = Theme::resolve(
            if theme == "none" { "paper" } else { theme },
            &BTreeMap::new(),
            mode,
        )
        .unwrap();
        for (width, height) in [(120, 32), (80, 24), (40, 16)] {
            for selected in 0..app.history.len() {
                app.selected[Page::History.index()] = selected;
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut hits = render::HitMap::default();
                terminal.draw(|f| hits = render::draw(f, &app)).unwrap();
                let text = screen(&app, width, height);
                if let Some(root) = &capture {
                    std::fs::create_dir_all(root).unwrap();
                    std::fs::write(
                        root.join(format!("{width}x{height}-{theme}-{selected}.txt")),
                        &text,
                    )
                    .unwrap();
                }
                assert!(text.contains("Details"), "{text}");
                assert!(text.contains("Enter options"), "{text}");
                assert!(text.contains("/ search"), "{text}");
                assert!(
                    hits.rows.iter().any(|(_, i)| *i == selected),
                    "selected row disappeared: {text}"
                );
                for (rect, index) in &hits.rows {
                    assert!(*index < app.history.len());
                    for y in rect.y..rect.bottom() {
                        assert!(
                            !terminal.backend().buffer()[(rect.x, y)]
                                .symbol()
                                .contains('─'),
                            "hit region covers border"
                        );
                    }
                }
                assert!(terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .any(|cell| cell.modifier.contains(Modifier::REVERSED)));
            }
            app.history.clear();
            app.query = "unmatched".into();
            let text = screen(&app, width, height);
            assert!(text.contains("Nothing matches"), "{text}");
            if let Some(root) = &capture {
                std::fs::write(
                    root.join(format!("{width}x{height}-{theme}-no-match.txt")),
                    &text,
                )
                .unwrap();
            }
            app.query.clear();
            assert!(screen(&app, width, height).contains("No dictations yet"));
            app.history = fixture.clone();
        }
    }
    let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
    let start = std::time::Instant::now();
    for i in 0..30 {
        let (width, height) = [(120, 32), (80, 24), (40, 16)][i % 3];
        terminal
            .resize(ratatui::layout::Rect::new(0, 0, width, height))
            .unwrap();
        terminal
            .draw(|f| {
                render::draw(f, &app);
            })
            .unwrap();
    }
    eprintln!(
        "History resize/redraw: {:?} per frame (30 TestBackend draws)",
        start.elapsed() / 30
    );
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
        let effects = key(&mut app, KeyCode::F(2));
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
fn setup_turns_on_smart_formatting_with_the_speech_key_and_keeps_choices() {
    use xflow_core::CleanupMode;
    let (_dir, mut app) = app();
    app.open_setup(Step::Provider);
    key_enter(&mut app);
    assert_eq!(app.config.cleanup.mode, CleanupMode::Light);
    assert_eq!(app.config.cleanup.provider.as_deref(), Some("groq"));
    assert!(editor(&app)
        .intro
        .contains("Smart formatting: on · uses your Groq key · ~+0.2 s"));
    assert!(app
        .settings()
        .iter()
        .any(|(k, v)| k == "cleanup.mode" && v.starts_with("On")));
    // Turned off later: running setup again keeps it off.
    app.set("cleanup.mode", "raw").unwrap();
    app.open_setup(Step::Provider);
    key_enter(&mut app);
    assert_eq!(app.config.cleanup.mode, CleanupMode::Raw);
    // A speech service without cleanup stays off, silently.
    let (_dir, mut app) = self::app();
    app.set("stt.provider", "deepgram").unwrap();
    app.open_setup(Step::Provider);
    key_enter(&mut app);
    assert_eq!(app.config.cleanup.mode, CleanupMode::Raw);
    assert!(!editor(&app).intro.contains("Smart formatting"));
    // Loading an existing config never flips it (only setup does).
    assert_eq!(
        xflow_core::config::Config::default().cleanup.mode,
        CleanupMode::Raw
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
fn history_pastes_the_original_and_filters_only_with_a_protocol_4_daemon() {
    let (_dir, mut app) = populated();
    app.page = Page::History;
    assert!(key(&mut app, KeyCode::Char('o')).is_empty());
    assert!(app.toast.take().is_some_and(|t| t.contains("isn't ready")));
    // A protocol 3 xflowd would ignore `original` and the filter: say so, send nothing.
    app.status.protocol = Some(3);
    for code in [KeyCode::Char('o'), KeyCode::Char('f')] {
        assert!(key(&mut app, code).is_empty());
        assert!(app.modal.is_none());
        assert!(app
            .toast
            .take()
            .is_some_and(|t| t.contains("older version")));
    }
    app.status.protocol = Some(xflow_core::ipc::PROTOCOL_VERSION);
    assert!(matches!(
        &key(&mut app, KeyCode::Char('o'))[..],
        [Effect::Request(
            Request::HistoryPaste {
                id: 42,
                original: true
            },
            _
        )]
    ));
    // Enter offers it too, after "Correct this transcript".
    key_enter(&mut app);
    assert_eq!(
        editor(&app).fields[0].labels[3],
        "Paste the original, before cleanup"
    );
    app.modal = None;
    let raw = app.history[0].raw_text.take();
    assert!(key(&mut app, KeyCode::Char('o')).is_empty());
    assert!(app
        .toast
        .take()
        .is_some_and(|t| t.contains("no separate original")));
    key_enter(&mut app);
    assert!(!editor(&app).fields[0]
        .labels
        .iter()
        .any(|l| l.contains("original")));
    app.modal = None;
    app.history[0].raw_text = raw;
    // f: app, kind and period, each a list; Enter on the last applies them.
    key(&mut app, KeyCode::Char('f'));
    assert!(matches!(editor(&app).target, EditTarget::HistoryFilter));
    assert_eq!(editor(&app).fields[0].labels, ["Any app", "Mail"]);
    for downs in [1, 2, 1] {
        for _ in 0..downs {
            key(&mut app, KeyCode::Down);
        }
        if editor(&app).focus < 2 {
            key_enter(&mut app);
        }
    }
    let effects = key_enter(&mut app);
    let now = app.now();
    let filter = match &effects[..] {
        [Effect::Request(
            Request::History {
                offset: 0, filter, ..
            },
            Tag::History(_),
        )] => filter.clone(),
        _ => panic!("no history request"),
    };
    assert_eq!(filter.app_id.as_deref(), Some("com.apple.mail"));
    assert_eq!(filter.mode, Some(xflow_core::Mode::Command));
    assert_eq!(filter.since, Some(now - now.rem_euclid(86_400)));
    assert!(screen(&app, 80, 24).contains("History · Mail · Voice edits · Today"));
    app.history.clear();
    assert!(screen(&app, 80, 24).contains("Nothing matches these filters."));
    // Esc clears them.
    let effects = key(&mut app, KeyCode::Esc);
    assert!(matches!(
        &effects[..],
        [Effect::Request(Request::History { filter, .. }, _)] if filter.is_empty()
    ));
    assert_eq!(app.page, Page::History);
    // From another page, f opens History with the form.
    app.page = Page::Home;
    let effects = key(&mut app, KeyCode::Char('f'));
    assert!(matches!(
        &effects[..],
        [Effect::Request(Request::History { .. }, _)]
    ));
    assert_eq!(app.page, Page::History);
    assert!(matches!(editor(&app).target, EditTarget::HistoryFilter));
}
#[test]
fn history_details_show_the_absolute_time() {
    assert_eq!(options::when(1790859600, true), "Thu 1 Oct 2026, 13:00");
    assert_eq!(options::start_of_day(1790859600 + 59, true), 1790812800);
    // Local midnight, whatever this machine's zone, including DST change weeks.
    for t in (1_774_000_000..1_775_000_000).step_by(43_200) {
        let midnight = options::start_of_day(t, false);
        assert!(
            options::when(midnight, false).ends_with(", 00:00")
                || options::when(midnight, false).ends_with(", 01:00")
        );
        assert!((0..=25 * 3600).contains(&(t - midnight)), "{t} {midnight}");
    }
    let (_dir, mut app) = populated();
    app.page = Page::History;
    assert!(screen(&app, 80, 24).contains("Thu 1 Oct 2026, 13:00"));
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
    // Its rules go with it, and the app says how many.
    app.set("dictionary.words", r#"["Veronica", "Ratatui"]"#)
        .unwrap();
    app.set("dictionary.learned", r#"["Veronica"]"#).unwrap();
    app.set(
        "dictionary.replacements",
        r#"[{ from = "Veronika", to = "Veronica" }, { from = "gonna", to = "going to" }]"#,
    )
    .unwrap();
    app.selected[Page::Dictionary.index()] = 2;
    key_enter(&mut app);
    key(&mut app, KeyCode::Down);
    key_enter(&mut app);
    key_enter(&mut app);
    assert_eq!(app.config.dictionary.words, ["Ratatui"]);
    assert_eq!(app.config.dictionary.replacements.len(), 1);
    assert_eq!(app.config.dictionary.replacements[0].from, "gonna");
    assert!(app
        .toast
        .as_deref()
        .is_some_and(|t| t.contains("1 correction rule")));
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
    let error = editor_error(&app).unwrap();
    assert!(
        screen(&app, 80, 24).contains(&format!("! {error}")),
        "editor errors need a text marker, not just colour"
    );
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
        "Automatic"
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

#[test]
fn backup_picker_only_offers_known_stored_keys_and_displays_the_current_choice() {
    let (_dir, mut app) = app();
    app.backups = vec!["groq".into(), "openai".into()];
    app.page = Page::Settings;
    app.selected[Page::Settings.index()] = app
        .settings()
        .iter()
        .position(|(key, _)| key == "stt.fallback")
        .unwrap();
    key_enter(&mut app);
    assert_eq!(editor(&app).fields[0].choices, vec!["", "openai"]);
    assert_eq!(options::value_text("stt.fallback", "[]"), "None");
    assert_eq!(
        options::value_text("stt.fallback", r#"["openai"]"#),
        "OpenAI"
    );
    assert_eq!(options::value_text("stt.fallback", "openai"), "OpenAI");
    app.saving = true;
    key_enter(&mut app);
    assert!(editor_error(&app).unwrap().contains("saving"));
}
#[test]
fn kept_failed_audio_offers_a_retry_that_copies_instead_of_pasting_into_the_terminal() {
    let (_dir, mut app) = populated();
    assert!(key(&mut app, KeyCode::Char('R')).is_empty());
    assert!(!screen(&app, 80, 24).contains("R retry"));
    app.event(Response {
        failed_audio: Some(Default::default()),
        ..Response::status(State::Error, 0.0)
    });
    assert!(screen(&app, 80, 24).contains("R retry"));
    assert!(matches!(
        key(&mut app, KeyCode::Char('R')).as_slice(),
        [Effect::Request(
            Request::Retry {
                full: false,
                delivery: Delivery::Clipboard,
                discard: false
            },
            Tag::Done(_)
        )]
    ));
}

#[test]
fn preferred_languages_picker_uses_names_and_rejects_unsupported_save() {
    let (_dir, mut app) = app();
    app.page = Page::Settings;
    let rows = app.settings();
    app.selected[Page::Settings.index()] =
        rows.iter().position(|(k, _)| k == "stt.languages").unwrap();
    key_enter(&mut app);
    let Some(Modal::Editor(editor)) = app.modal.as_mut() else {
        panic!("preferred languages editor")
    };
    assert_eq!(editor.fields.len(), 3);
    assert!(editor.intro.contains("Whisper"));
    editor.fields[0].value = "hi".to_owned().into();
    editor.fields[1].value = "en".to_owned().into();
    key(&mut app, KeyCode::F(2));
    assert_eq!(app.config.stt.languages, ["hi", "en"]);
    assert_eq!(
        options::value_text("stt.languages", r#"["hi", "en"]"#),
        "Hindi + English"
    );
    assert_eq!(
        options::value_text("stt.languages", r#"["hi-IN", "en"]"#),
        "Hindi + English"
    );
    assert!(app.set("stt.provider", "mistral").is_err());
    assert_eq!(app.config.stt.provider, "groq");
    app.set("stt.language", "fr").unwrap();
    assert!(app.config.stt.languages.is_empty());
    app.set("stt.languages", r#"["hi", "en"]"#).unwrap();
    assert!(app.config.stt.language.is_none());
}
/// The TUI's check that config.toml is still what it loaded happens under the
/// config lock: an edit another writer saves while holding the lock (a word
/// xflowd learned, a CLI command) is seen and never overwritten.
#[test]
fn save_compares_and_writes_under_the_config_lock() {
    let (_dir, app) = app();
    let file = app.file.as_ref().unwrap();
    let path = file.path().to_owned();
    let held = crate::config_edit::lock(&path).unwrap();
    let (ready, started) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        ready.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::write(&path, "[ui]\ntheme = 'midnight'\n").unwrap();
        drop(held);
    });
    started.recv().unwrap();
    let error = super::runtime::save(file, None).unwrap_err();
    assert!(
        format!("{error:#}").contains("changed outside"),
        "{error:#}"
    );
    writer.join().unwrap();
    assert!(std::fs::read_to_string(file.path())
        .unwrap()
        .contains("midnight"));
}

fn app_with(colors: ColorMode) -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let file = ConfigFile::load(&path).unwrap();
    let mut app = App::new(path, colors);
    app.load(file, None).unwrap();
    app.now = Some(1790859600 + 120);
    (dir, app)
}

#[test]
fn compact_nav_shows_full_labels_and_keys_are_bold_without_color() {
    for colors in [ColorMode::TrueColor, ColorMode::Ansi, ColorMode::None] {
        let (_dir, mut app) = app_with(colors);
        app.disk = Some(app.file.as_ref().unwrap().to_string());
        app.connected = true;
        app.event(Response::status(State::Idle, 0.0));
        app.page = Page::Home;
        let mode = match colors {
            ColorMode::TrueColor => "truecolor",
            ColorMode::Ansi => "ansi",
            ColorMode::None => "none",
        };
        let text = screen(&app, 40, 16);
        assert!(
            text.contains(">1  Home"),
            "selected page needs a text marker in {mode}:\n{text}"
        );
        assert!(
            text.contains(" 3  Dictionary"),
            "compact nav clipped Dictionary in {mode}:\n{text}"
        );
        assert!(
            text.contains(" 8  Your stats"),
            "compact nav clipped Your stats in {mode}:\n{text}"
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render::draw(frame, &app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let y = 23;
        let row: String = (0..80).map(|x| buffer[(x, y)].symbol()).collect();
        let space = row.find("Space").expect(&row);
        let dictate = row.find("dictate").expect(&row);
        assert!(
            buffer[(space as u16, y)].modifier.contains(Modifier::BOLD),
            "key should be bold: {row}"
        );
        assert!(
            !buffer[(dictate as u16, y)]
                .modifier
                .contains(Modifier::BOLD),
            "action should not be bold: {row}"
        );
        if matches!(colors, ColorMode::None) {
            let mark = (0..24).find_map(|row| {
                (0..80).find_map(|x| {
                    let cell = &buffer[(x, row)];
                    (cell.symbol() == ">").then_some((cell.fg, cell.bg))
                })
            });
            assert_eq!(
                mark,
                Some((ratatui::style::Color::Reset, ratatui::style::Color::Reset)),
                "NO_COLOR selection must not depend on a painted cell"
            );
        }
    }
}

fn home_sample(colors: ColorMode, transcript: &str) -> (tempfile::TempDir, App) {
    let (dir, mut app) = app_with(colors);
    app.disk = Some(app.file.as_ref().unwrap().to_string());
    app.connected = true;
    app.event(Response {
        text: Some(transcript.into()),
        provider: Some("groq".into()),
        model: Some("whisper-large-v3-turbo".into()),
        ..Response::status(State::Success, 0.0)
    });
    (dir, app)
}

#[test]
fn short_home_keeps_the_dictation_and_the_status_word() {
    let long = "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu. The rest of this dictation keeps going so a short window cannot show the ending omega.";
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../.reports/ui/home");
    std::fs::create_dir_all(&root).unwrap();
    for (width, height) in [(40u16, 16u16), (80, 24), (120, 35)] {
        for (mode, colors) in [("color", ColorMode::TrueColor), ("none", ColorMode::None)] {
            for (len, transcript) in [
                ("short", "The best ideas start with a conversation."),
                ("long", long),
            ] {
                let (_dir, app) = home_sample(colors, transcript);
                let text = screen(&app, width, height);
                std::fs::write(
                    root.join(format!("{width}x{height}-{mode}-{len}.txt")),
                    &text,
                )
                .unwrap();
                assert!(
                    text.contains("DONE"),
                    "{mode} {width}x{height} {len}:\n{text}"
                );
                assert!(
                    text.contains("✓") || text.contains("v  DONE"),
                    "status icon missing:\n{text}"
                );
                assert!(text.contains("Last dictation"), "{text}");
                assert!(text.contains("Shortcut"), "{text}");
                assert!(text.contains("Groq"), "{text}");
                assert!(text.contains("Microphone"), "{text}");
                if len == "short" {
                    assert!(text.contains("The best ideas"), "{text}");
                } else {
                    assert!(text.contains("Alpha"), "{text}");
                    if (width, height) == (40, 16) {
                        assert!(!text.contains("omega"), "{text}");
                    }
                }
            }
        }
    }
    let (_dir, app) = home_sample(ColorMode::None, "The best ideas start with a conversation.");
    let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();
    terminal
        .draw(|frame| {
            render::draw(frame, &app);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let done = (0..16u16).find_map(|y| {
        (0..37u16).find_map(|x| {
            let word: String = (0..4).map(|i| buffer[(x + i, y)].symbol()).collect();
            (word == "DONE").then_some((x, y))
        })
    });
    let (x, y) = done.expect("DONE");
    for i in 0..4 {
        let cell = &buffer[(x + i, y)];
        assert!(
            cell.modifier.contains(Modifier::BOLD),
            "status word should be bold"
        );
        assert_eq!(cell.fg, ratatui::style::Color::Reset);
    }
    assert_ne!(buffer[(x - 3, y)].symbol(), " ", "status icon");
    let (_dir, mut listening) = home_sample(
        ColorMode::TrueColor,
        "The best ideas start with a conversation.",
    );
    listening.event(Response::status(State::Listening, 0.4));
    let text = screen(&listening, 80, 19);
    assert!(text.contains("LISTENING"), "{text}");
    assert!(text.contains("Last dictation"), "{text}");
}

fn ai_status(sharing: bool) -> super::ai_apps::Status {
    use super::ai_apps::{ClientStatus, Registration};
    let clients = crate::mcp::setup::CLIENTS
        .iter()
        .map(|&client| ClientStatus {
            client,
            detected: matches!(client, "claude-code" | "codex" | "cursor"),
            registration: match client {
                "claude-code" => Registration::Command {
                    command: "/usr/local/bin/xflow".into(),
                    exists: true,
                    version: format!("xflow {}", env!("CARGO_PKG_VERSION")),
                },
                "cursor" => Registration::Command {
                    command: "/opt/old/xflow".into(),
                    exists: true,
                    version: "xflow 0.0.1".into(),
                },
                _ => Registration::Missing,
            },
            manual: match client {
                "zed" => Some("{context_servers: xflow}".into()),
                "chatgpt" => Some("Use Codex in ChatGPT desktop.".into()),
                _ => None,
            },
        })
        .collect();
    super::ai_apps::Status {
        exe: "/usr/local/bin/xflow".into(),
        version: "1.0.0",
        sources: crate::context::Sources {
            dictations: sharing,
            meetings: false,
        },
        clients,
    }
}
fn ai_row(app: &mut App, client: &str) {
    app.selected[Page::AiApps.index()] = 2 + super::ai_apps::CLIENTS
        .iter()
        .position(|(id, _)| *id == client)
        .unwrap();
}
fn mcp_job(effects: &[Effect]) -> Option<super::ai_apps::Job> {
    match effects {
        [Effect::Mcp(job, _)] => Some(job.clone()),
        _ => None,
    }
}
#[test]
fn ai_apps_screen_shows_status_and_its_privacy_lines() {
    use super::ai_apps::{Job, INJECTION, LEAVES};
    let (_dir, mut app) = populated();
    // 0 opens the screen and asks for status off the UI loop, once at a time.
    assert_eq!(
        mcp_job(&key(&mut app, KeyCode::Char('0'))),
        Some(Job::Status)
    );
    assert_eq!(app.page, Page::AiApps);
    assert!(key(&mut app, KeyCode::Char('r')).is_empty());
    assert!(app.ai.busy);
    let text = screen(&app, 120, 35);
    assert!(text.contains("Checking…"), "{text}");
    super::ai_apps::done(&mut app, Ok(super::ai_apps::Done::Status(ai_status(false))));
    assert!(!app.ai.busy);
    for (width, height) in [(120, 35), (80, 24)] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("src/tui/snapshots/ai-apps-{width}x{height}.txt"));
        let actual = screen(&app, width, height);
        if std::env::var_os("XFLOW_UPDATE_TUI_SNAPSHOTS").is_some() {
            std::fs::write(&path, &actual).unwrap();
        }
        assert_eq!(actual, std::fs::read_to_string(&path).unwrap());
        assert_plain(&actual);
        assert!(actual.contains("Dictation history:"));
        assert!(!actual.contains("Meetings:"));
    }
    let text = screen(&app, 160, 40);
    for line in [LEAVES, INJECTION] {
        assert!(text.contains(line), "{text}");
    }
    // A different registered version is flagged with its path.
    ai_row(&mut app, "cursor");
    let text = screen(&app, 160, 40);
    assert!(text.contains("/opt/old/xflow"), "{text}");
    assert!(text.contains("different version (xflow 0.0.1"), "{text}");
    // Print-only clients show their snippet; Enter only explains.
    ai_row(&mut app, "zed");
    assert!(screen(&app, 160, 40).contains("{context_servers: xflow}"));
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert!(app.modal.is_none());
    assert!(app.toast.as_deref().unwrap().contains("Zed"));
    ai_row(&mut app, "chatgpt");
    assert!(screen(&app, 160, 40).contains("Use Codex in ChatGPT desktop."));
}
#[test]
fn ai_apps_sharing_needs_explicit_consent_and_off_needs_none() {
    use super::ai_apps::{Done, Job, CONSENT};
    let (_dir, mut app) = populated();
    app.page = Page::AiApps;
    super::ai_apps::done(&mut app, Ok(Done::Status(ai_status(false))));
    // Turning it on shows the consent text; Esc changes nothing.
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert!(matches!(&app.modal, Some(Modal::Confirm { prompt, .. }) if prompt.contains(CONSENT)));
    assert!(screen(&app, 120, 35).contains("that app's AI provider"));
    assert!(key(&mut app, KeyCode::Esc).is_empty());
    assert!(app.modal.is_none());
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        mcp_job(&key(&mut app, KeyCode::Enter)),
        Some(Job::Share(true))
    );
    // A finished change refreshes status.
    assert_eq!(
        mcp_job(&super::ai_apps::done(&mut app, Ok(Done::Shared(true)))),
        Some(Job::Status)
    );
    super::ai_apps::done(&mut app, Ok(Done::Status(ai_status(true))));
    assert!(screen(&app, 120, 35).contains("Dictation history:    on"));
    // Turning it off needs no confirmation.
    assert_eq!(
        mcp_job(&key(&mut app, KeyCode::Enter)),
        Some(Job::Share(false))
    );
    assert!(app.modal.is_none());
}
#[test]
fn ai_apps_install_confirms_a_replacement_and_remove_runs_the_cli_code() {
    use super::ai_apps::{Done, Job, Report};
    let (_dir, mut app) = populated();
    app.page = Page::AiApps;
    super::ai_apps::done(&mut app, Ok(Done::Status(ai_status(true))));
    ai_row(&mut app, "cursor");
    key(&mut app, KeyCode::Enter);
    assert!(matches!(&editor(&app).target, EditTarget::Menu(_)));
    let install = Job::Install {
        client: "cursor",
        force: false,
    };
    assert_eq!(mcp_job(&key(&mut app, KeyCode::Enter)), Some(install));
    // While it runs, nothing else starts.
    key(&mut app, KeyCode::Enter);
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    // A differing entry asks before replacing; yes reinstalls with force.
    let replace = Done::Replace {
        client: "cursor",
        existing: "/opt/old/xflow".into(),
    };
    assert!(super::ai_apps::done(&mut app, Ok(replace)).is_empty());
    assert!(
        matches!(&app.modal, Some(Modal::Confirm { prompt, .. }) if prompt.contains("/opt/old/xflow"))
    );
    assert_eq!(
        mcp_job(&key(&mut app, KeyCode::Enter)),
        Some(Job::Install {
            client: "cursor",
            force: true
        })
    );
    super::ai_apps::done(&mut app, Ok(Done::Installed("cursor", Report::default())));
    assert!(app.toast.as_deref().unwrap().contains("Restart"));
    super::ai_apps::done(&mut app, Ok(Done::Status(ai_status(true))));
    // Remove; a missing client CLI leaves the command to run by hand.
    ai_row(&mut app, "gemini");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Down);
    assert_eq!(
        mcp_job(&key(&mut app, KeyCode::Enter)),
        Some(Job::Remove("gemini"))
    );
    let report = Report {
        manual: Some("gemini mcp remove -s user xflow".into()),
        ..Report::default()
    };
    super::ai_apps::done(&mut app, Ok(Done::Removed("gemini", report)));
    super::ai_apps::done(&mut app, Ok(Done::Status(ai_status(true))));
    assert!(screen(&app, 160, 40).contains("gemini mcp remove -s user xflow"));
    // Errors become a plain toast and free the screen.
    super::ai_apps::done(&mut app, Err(anyhow::anyhow!("Unsafe client config file")));
    assert!(!app.ai.busy);
    assert!(app
        .toast
        .as_deref()
        .unwrap()
        .contains("Unsafe client config file"));
}

// On this Mac (offline): facts pane, install job and the snapshot-keeping switch.
fn mac() -> crate::local_models::Facts {
    let host = crate::local_models::Host {
        platform: "macos",
        process_isa: "aarch64",
        native_isa: Some("aarch64".into()),
        os_version: Some("15.1".into()),
        ..Default::default()
    };
    // This test machine can't run it; the pane is what matters here.
    crate::local_models::Facts {
        supported: true,
        ..crate::local_models::facts_for(&host, false)
    }
}
fn linux() -> crate::local_models::Facts {
    crate::local_models::facts_for(
        &crate::local_models::Host {
            platform: "linux",
            ..Default::default()
        },
        false,
    )
}
fn on_device_pane(app: &App) -> bool {
    matches!(app.modal, Some(Modal::OnDevice { .. }))
}
fn open_provider_setting(app: &mut App) {
    app.page = Page::Settings;
    app.selected[Page::Settings.index()] = app
        .settings()
        .iter()
        .position(|(k, _)| k == "stt.provider")
        .unwrap();
    key_enter(app);
}
fn pick_value(app: &mut App, value: &str) {
    key(app, KeyCode::Home);
    while editor(app).fields[0].value.as_str() != value {
        let before = editor(app).fields[0].value.to_string();
        key(app, KeyCode::Down);
        assert_ne!(
            before,
            editor(app).fields[0].value.as_str(),
            "{value} not offered"
        );
    }
}

#[test]
fn setup_on_this_mac_skips_the_key_and_downloads_only_on_install() {
    let (_dir, mut app) = app();
    app.probe = mac;
    app.connected = true;
    key_enter(&mut app); // Welcome
    key_enter(&mut app); // Speech service
    pick_value(&mut app, xflow_providers::ON_DEVICE);
    // The three kinds are all offered: API services, this Mac, a local server.
    let labels = editor(&app).fields[0].labels.join("\n");
    assert!(labels.contains("Groq") && labels.contains("On this Mac (offline)"));
    assert!(editor(&app).fields[0]
        .choices
        .contains(&"local".to_string()));
    let effects = key_enter(&mut app);
    assert!(effects.is_empty(), "choosing the row never downloads");
    assert!(on_device_pane(&app));
    assert_eq!(app.config.stt.provider, "groq");
    for (width, height) in [(80, 24), (120, 35)] {
        let text = screen(&app, width, height);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("src/tui/snapshots/on-device-{width}x{height}.txt"));
        if std::env::var_os("XFLOW_UPDATE_TUI_SNAPSHOTS").is_some() {
            std::fs::write(&path, &text).unwrap();
        }
        assert_eq!(
            text,
            std::fs::read_to_string(&path).unwrap(),
            "{}",
            path.display()
        );
        assert!(!text.contains("Paste your"));
    }
    let text = app.on_device_text(false, None);
    for expected in [
        "NVIDIA Parakeet TDT 0.6B v3",
        "Languages (25)",
        "Hindi",
        "Japanese",
        "484 MB",
        "CC BY 4.0 · NVIDIA",
        "FluidInference",
        "This Mac can run it",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    // Install, cancel (staging kept), resume, fail, retry, done.
    assert!(matches!(
        &key_enter(&mut app)[..],
        [Effect::InstallModel(_)]
    ));
    app.install_progress(120_000_000, 483_105_645);
    assert!(app
        .on_device_text(false, None)
        .contains("Downloading… 120 MB of 484 MB"));
    assert!(render::keys(&app).iter().any(|(k, _)| *k == "x cancel"));
    assert!(key_enter(&mut app).is_empty(), "one download at a time");
    key(&mut app, KeyCode::Char('x'));
    assert!(
        key_enter(&mut app).is_empty(),
        "no second download while stopping"
    );
    app.install_done(Err(anyhow::anyhow!("cancelled")));
    assert!(app
        .toast
        .as_deref()
        .unwrap()
        .contains("picks up where it stopped"));
    assert!(matches!(
        &key_enter(&mut app)[..],
        [Effect::InstallModel(_)]
    ));
    app.install_done(Err(anyhow::anyhow!("network is unreachable")));
    assert!(app
        .on_device_text(false, None)
        .contains("network is unreachable"));
    assert!(render::keys(&app).iter().any(|(k, _)| *k == "Enter retry"));
    assert_eq!(
        app.config.stt.provider, "groq",
        "a failed download changes nothing"
    );
    assert!(matches!(
        &key_enter(&mut app)[..],
        [Effect::InstallModel(_)]
    ));
    app.install_done(Ok(true));
    // Use it: no key step, straight to the microphone.
    let effects = key_enter(&mut app);
    assert!(matches!(&effects[..], [Effect::Devices]));
    assert_eq!(app.config.stt.provider, xflow_providers::ON_DEVICE);
    assert_eq!(app.setup, Some(Step::Microphone));
    assert_eq!(
        app.config.on_device.previous_api.as_ref().unwrap().provider,
        "groq"
    );
}

#[test]
fn unsupported_machine_explains_and_never_installs() {
    let (_dir, mut app) = app();
    app.probe = linux;
    open_provider_setting(&mut app);
    pick_value(&mut app, xflow_providers::ON_DEVICE);
    key_enter(&mut app);
    let text = screen(&app, 80, 24);
    assert!(text.contains("Not supported yet on Linux"), "{text}");
    assert!(render::keys(&app)
        .iter()
        .all(|(k, _)| *k != "Enter install"));
    assert!(key_enter(&mut app).is_empty());
    assert!(!on_device_pane(&app));
    assert_eq!(app.config.stt.provider, "groq");
}

#[test]
fn api_services_keep_working_while_the_model_is_missing_or_downloading() {
    let (_dir, mut app) = app();
    app.probe = mac;
    open_provider_setting(&mut app);
    pick_value(&mut app, xflow_providers::ON_DEVICE);
    key_enter(&mut app);
    assert!(app.on_device_text(false, None).contains("Not installed"));
    assert!(matches!(
        &key_enter(&mut app)[..],
        [Effect::InstallModel(_)]
    ));
    key(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    open_provider_setting(&mut app);
    pick_value(&mut app, "deepgram");
    key_enter(&mut app);
    assert_eq!(app.config.stt.provider, "deepgram");
    // The download finishing doesn't switch anything by itself.
    app.install_done(Ok(true));
    assert_eq!(app.config.stt.provider, "deepgram");
}

#[test]
fn switching_api_to_on_device_and_back_keeps_the_custom_route() {
    let (_dir, mut app) = app();
    app.probe = || crate::local_models::Facts {
        installed: true,
        ..mac()
    };
    app.set(
        "stt",
        r#"{ provider = "custom", endpoint = "https://stt.example.com/v1/audio/transcriptions", model = "whisper-x", api_key_env = "MY_STT_KEY", languages = ["en", "fr"], vocabulary = ["xflow"], domain_hints = false }"#,
    )
    .unwrap();
    let before = app.config.stt.clone();
    open_provider_setting(&mut app);
    pick_value(&mut app, xflow_providers::ON_DEVICE);
    key_enter(&mut app);
    assert!(render::keys(&app).iter().any(|(k, _)| *k == "Enter use it"));
    key_enter(&mut app);
    assert_eq!(app.config.stt.provider, xflow_providers::ON_DEVICE);
    assert_eq!(app.config.on_device.previous_api.as_ref(), Some(&before));
    assert_eq!(
        app.settings()
            .iter()
            .find(|(k, _)| k == "action.stt_key")
            .unwrap()
            .1,
        "Not needed"
    );
    // The saved custom server is offered to go back to, and comes back whole.
    open_provider_setting(&mut app);
    pick_value(&mut app, "custom");
    key_enter(&mut app);
    assert_eq!(app.config.stt, before);
    assert!(app.config.on_device.previous_api.is_none());
}

#[test]
fn remove_asks_first_and_switches_back_to_the_saved_route() {
    let (_dir, mut app) = app();
    app.probe = || crate::local_models::Facts {
        installed: true,
        ..mac()
    };
    open_provider_setting(&mut app);
    pick_value(&mut app, xflow_providers::ON_DEVICE);
    key_enter(&mut app);
    key_enter(&mut app); // Use it
    assert_eq!(app.config.stt.provider, xflow_providers::ON_DEVICE);
    open_provider_setting(&mut app);
    pick_value(&mut app, xflow_providers::ON_DEVICE);
    key_enter(&mut app);
    assert!(key(&mut app, KeyCode::Char('d')).is_empty());
    assert!(screen(&app, 80, 24).contains("Remove the model"));
    key(&mut app, KeyCode::Esc);
    assert!(!app.on_device_text(false, None).contains("Remove the model"));
    assert!(on_device_pane(&app));
    key(&mut app, KeyCode::Char('d'));
    assert!(matches!(&key_enter(&mut app)[..], [Effect::RemoveModel]));
    assert_eq!(app.config.stt.provider, "groq");
    app.model_removed(Ok(true));
    assert!(app.on_device_text(false, None).contains("Not installed"));
}

#[test]
fn meeting_sharing_confirms_and_off_is_immediate() {
    use super::ai_apps::{Done, Job, CONSENT};
    let (_dir, mut app) = populated();
    app.page = Page::AiApps;
    super::ai_apps::done(&mut app, Ok(Done::Status(ai_status(false))));
    app.selected[Page::AiApps.index()] = 1;
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    match &app.modal {
        Some(Modal::Confirm {
            action: Action::ShareMeetings,
            prompt,
        }) => {
            assert!(prompt.contains("meeting transcripts"));
            assert!(prompt.contains(CONSENT));
        }
        _ => panic!("meeting consent confirmation"),
    }
    assert_eq!(
        mcp_job(&key(&mut app, KeyCode::Char('y'))),
        Some(Job::ShareMeetings(true))
    );
    let mut status = ai_status(false);
    status.sources.meetings = true;
    super::ai_apps::done(&mut app, Ok(Done::Status(status)));
    assert_eq!(
        mcp_job(&key(&mut app, KeyCode::Enter)),
        Some(Job::ShareMeetings(false))
    );
}

#[test]
fn dictionary_layout_keeps_selection_and_hits_together() {
    let (_dir, mut app) = app();
    app.page = Page::Dictionary;
    app.config.dictionary.words = vec![
        "Kubernetes".into(),
        "PostgreSQL".into(),
        "useEffect".into(),
        "Siobhán O’Connor".into(),
        "Nguyễn Minh".into(),
        "नमस्ते team, deployment कल करेंगे".into(),
        "Review OAuth token rotation before the next Kubernetes deployment".into(),
    ];
    app.config.dictionary.replacements = vec![xflow_core::config::Replacement {
        from: "post grass".into(),
        to: "PostgreSQL".into(),
    }];
    for (width, height) in [(120, 32), (80, 24), (40, 16)] {
        for (name, colors) in [
            ("midnight", ColorMode::TrueColor),
            ("paper", ColorMode::TrueColor),
            ("paper", ColorMode::None),
        ] {
            app.theme = Theme::resolve(name, &BTreeMap::new(), colors).unwrap();
            for selected in 0..app.rows().len() {
                app.select(selected as isize - app.selected() as isize);
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut hits = render::HitMap::default();
                terminal.draw(|f| hits = render::draw(f, &app)).unwrap();
                let (rect, _) = hits
                    .rows
                    .iter()
                    .find(|(_, i)| *i == selected)
                    .expect("selected row visible");
                let buffer = terminal.backend().buffer();
                assert_eq!(buffer[(rect.x, rect.y)].symbol(), ">");
                assert!(buffer[(rect.x, rect.y)]
                    .modifier
                    .contains(Modifier::REVERSED | Modifier::BOLD));
                for (row, index) in &hits.rows {
                    assert_eq!(row.height, 1);
                    assert!(row.bottom() < height - 2);
                    assert!(*index < app.rows().len());
                }
                key(&mut app, KeyCode::Enter);
                assert!(matches!(app.modal, Some(Modal::Editor(_))));
                if selected >= app.adds() {
                    key(&mut app, KeyCode::Enter);
                    assert!(matches!(
                        editor(&app).target,
                        EditTarget::Word(_) | EditTarget::Replacement(_)
                    ));
                }
                key(&mut app, KeyCode::Esc);
                if selected >= app.adds() {
                    key(&mut app, KeyCode::Delete);
                    assert!(matches!(app.modal, Some(Modal::Confirm { .. })));
                    key(&mut app, KeyCode::Esc);
                }
            }
            app.select(-(app.selected() as isize));
            app.config.dictionary.words.clear();
            app.config.dictionary.replacements.clear();
            let text = screen(&app, width, height);
            assert!(text.contains("No saved words yet."));
            assert!(text.contains("+ Add a word"));
            assert!(text.contains("+ Add a correction"));
            assert!(text.contains("q quit"));
            // Restore fixtures for the next theme without persisting any edits.
            app.config.dictionary.words = vec!["Kubernetes".into(), "नमस्ते team".into()];
        }
    }
}

#[test]
fn styles_keep_selection_actions_and_details_in_short_terminals() {
    let (_dir, mut app) = app();
    app.page = Page::Styles;
    app.config.styles = [
        ("Ada: concise", "com.apple.mail", "Use concise sentences."),
        (
            "Kubernetes PostgreSQL useEffect git rebase OAuth kubectl — development notes",
            "code",
            "José, Mei, Fatima and Chinedu: preserve identifiers, punctuation, 14:30 and v2.",
        ),
        (
            "Priya और José: कल API deploy करना है",
            "slack",
            "Hindi-English: technical terms वैसे ही रखो। 42, 3.14, 2026.",
        ),
    ]
    .into_iter()
    .map(|(name, id, prompt)| xflow_core::config::Style {
        name: name.into(),
        apps: vec![id.into()],
        mode: None,
        prompt: Some(prompt.into()),
    })
    .collect();
    let capture = std::env::var_os("XFLOW_STYLES_CAPTURES").map(PathBuf::from);
    for (theme, mode) in [
        ("midnight", ColorMode::TrueColor),
        ("paper", ColorMode::TrueColor),
        ("none", ColorMode::None),
    ] {
        app.theme = Theme::resolve(
            if theme == "none" { "paper" } else { theme },
            &BTreeMap::new(),
            mode,
        )
        .unwrap();
        for (width, height) in [(120, 32), (80, 24), (40, 16), (40, 12)] {
            for selected in 0..=app.config.styles.len() {
                app.selected[Page::Styles.index()] = selected;
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut hits = render::HitMap::default();
                terminal.draw(|f| hits = render::draw(f, &app)).unwrap();
                let text = screen(&app, width, height);
                assert!(text.contains("Styles (3)"), "{text}");
                assert!(text.contains("PgUp/PgDn details"), "{text}");
                assert!(
                    text.contains(if selected == 0 {
                        "Enter add"
                    } else {
                        "Enter options"
                    }),
                    "{text}"
                );
                assert!(text.contains("q quit"), "{text}");
                assert!(hits.rows.iter().any(|(_, i)| *i == selected), "{text}");
                for (rect, index) in &hits.rows {
                    assert!(*index <= app.config.styles.len());
                    assert_eq!(rect.height, 1);
                    for x in rect.x..rect.right() {
                        assert!(!["─", "╰", "╯"]
                            .contains(&terminal.backend().buffer()[(x, rect.y)].symbol()));
                    }
                }
                let selected_rect = hits.rows.iter().find(|(_, i)| *i == selected).unwrap().0;
                let cell = &terminal.backend().buffer()[(selected_rect.x, selected_rect.y)];
                assert_eq!(cell.symbol(), ">");
                assert!(cell.modifier.contains(Modifier::REVERSED | Modifier::BOLD));
                if let Some(root) = &capture {
                    std::fs::create_dir_all(root).unwrap();
                    std::fs::write(
                        root.join(format!("{width}x{height}-{theme}-{selected}.txt")),
                        text,
                    )
                    .unwrap();
                }
            }
        }
    }
    app.selected[Page::Styles.index()] = 0;
    key(&mut app, KeyCode::Enter);
    assert!(matches!(editor(&app).target, EditTarget::Style(None)));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.selected(), 1);
    key(&mut app, KeyCode::PageDown);
    assert_eq!(app.scroll, 8);
    assert!(screen(&app, 40, 12).contains("Apps: Mail"));
    key(&mut app, KeyCode::PageUp);
    assert_eq!(app.scroll, 0);
    key(&mut app, KeyCode::Enter);
    assert!(matches!(editor(&app).target, EditTarget::Menu(_)));
    key(&mut app, KeyCode::Enter);
    assert!(matches!(editor(&app).target, EditTarget::Style(Some(0))));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Delete);
    assert!(matches!(app.modal, Some(Modal::Confirm { .. })));
    key(&mut app, KeyCode::Esc);
    app.config.styles.clear();
    app.selected[Page::Styles.index()] = 0;
    assert!(screen(&app, 40, 16).contains("No styles yet."));
    app.toast = Some("Could not save: synthetic read-only config".into());
    assert!(screen(&app, 80, 24).contains("Could not save"));
    let start = std::time::Instant::now();
    for _ in 0..30 {
        screen(&app, 120, 32);
    }
    eprintln!("Styles: 30 synthetic redraws: {:?}", start.elapsed());
}

#[test]
fn meeting_notes_and_ask_are_keyboard_accessible_without_colour() {
    let (_dir, mut app) = app();
    app.colors = ColorMode::None;
    app.theme = Theme::resolve("paper", &app.config.ui.themes, ColorMode::None).unwrap();
    let effects = key(&mut app, KeyCode::Char('='));
    assert!(matches!(effects.as_slice(), [Effect::Meetings(_)]));
    assert_eq!(app.page, Page::Meetings);
    let id = xflow_core::meeting::MeetingId {
        store_id: "a".repeat(32),
        id: 1,
    };
    app.meetings
        .apply(super::meetings::Done::Notes(id.clone(), None));
    let shown = screen(&app, 120, 35);
    assert!(shown.contains("Ask about this meeting"));
    assert!(shown.contains("Generate cited notes"));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.page, Page::Chat);
    assert_eq!(app.ask_meeting, Some(id));
    assert!(app.chat.input.contains("Summarize this meeting"));
}

#[test]
fn settings_selection_and_model_status_fit_small_terminals() {
    use super::on_device::Job;
    let (_dir, mut app) = populated();
    app.page = Page::Settings;
    app.advanced = true;
    app.set(
        "recording.device",
        "\"Priya और José — Kubernetes PostgreSQL useEffect git rebase OAuth kubectl 14:30\"",
    )
    .unwrap();
    let settings = app.settings();
    let capture = std::env::var_os("XFLOW_SETTINGS_CAPTURES").map(PathBuf::from);
    for (name, mode) in [
        ("midnight", ColorMode::TrueColor),
        ("paper", ColorMode::TrueColor),
        ("none", ColorMode::None),
    ] {
        app.theme = Theme::resolve(
            if name == "none" { "paper" } else { name },
            &BTreeMap::new(),
            mode,
        )
        .unwrap();
        for (width, height) in [(120, 32), (80, 24), (40, 16), (40, 12)] {
            for pos in [0, 3, 7, settings.len() - 1] {
                app.selected[Page::Settings.index()] = pos;
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut hits = render::HitMap::default();
                terminal
                    .draw(|frame| hits = render::draw(frame, &app))
                    .unwrap();
                assert!(
                    hits.rows.iter().any(|(_, i)| *i == pos),
                    "selected row must have a hit region at {width}x{height}"
                );
                for (rect, index) in &hits.rows {
                    assert!(rect.height == 1 && rect.right() <= width && rect.bottom() <= height);
                    let row = (rect.x..rect.right())
                        .map(|x| terminal.backend().buffer()[(x, rect.y)].symbol())
                        .collect::<String>();
                    assert!(
                        row.contains(
                            &options::row(&settings[*index].0)
                                .unwrap()
                                .2
                                .chars()
                                .take(10)
                                .collect::<String>()
                        ),
                        "hit region must match row: {row}"
                    );
                }
                let text = screen(&app, width, height);
                assert!(text.contains("Enter") && text.contains("q quit"));
                if let Some(root) = &capture {
                    std::fs::create_dir_all(root).unwrap();
                    std::fs::write(
                        root.join(format!("settings-{width}x{height}-{name}-{pos}.txt")),
                        text,
                    )
                    .unwrap();
                }
            }
            app.facts = Some(mac());
            for (state, job, removing, error, expected) in [
                ("empty", Job::Idle, false, None, "Not installed"),
                (
                    "download",
                    Job::Running {
                        done: 123_000_000,
                        total: 484_000_000,
                        started: std::time::Instant::now(),
                        cancel: std::sync::Arc::new(tokio::sync::Notify::new()),
                    },
                    false,
                    None,
                    "Downloading",
                ),
                (
                    "failed",
                    Job::Failed("Kubernetes OAuth: Priya और José 503".into()),
                    false,
                    None,
                    "Download stopped",
                ),
                ("cancelling", Job::Cancelling, false, None, "Stopping"),
                ("remove", Job::Idle, true, None, "Remove the model"),
                (
                    "error",
                    Job::Idle,
                    false,
                    Some("Helper unavailable: retry after setup".into()),
                    "Error:",
                ),
            ] {
                app.install = job;
                app.modal = Some(Modal::OnDevice {
                    setup: false,
                    removing,
                    error,
                });
                let text = screen(&app, width, height);
                assert!(text.contains(expected), "{state} {width}x{height}:\n{text}");
                assert!(text.contains("Esc"));
                if let Some(root) = &capture {
                    std::fs::write(
                        root.join(format!("model-{width}x{height}-{name}-{state}.txt")),
                        text,
                    )
                    .unwrap();
                }
            }
            app.modal = None;
        }
    }
    let start = std::time::Instant::now();
    for _ in 0..100 {
        std::hint::black_box(screen(&app, 80, 24));
    }
    eprintln!(
        "settings: 100 TestBackend draws in {:?} (includes allocation)",
        start.elapsed()
    );
}

#[test]
fn pill_keeps_focus_values_hits_and_actions_at_every_supported_size() {
    let started = std::time::Instant::now();
    use crate::gsettings::{Kind, Setting};
    let (_dir, mut app) = populated();
    app.page = Page::Overlay;
    app.overlay = [
        ("Show pill", "true"),
        (
            "Kubernetes PostgreSQL useEffect git rebase OAuth kubectl",
            "false",
        ),
        ("José Mei Fatima Chinedu", "true"),
        ("Priya और José: कल API deploy करना है 42 3.14 14:30", "false"),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (summary, value))| Setting {
        key: format!("option-{i}"),
        summary: summary.into(),
        value: value.into(),
        kind: Kind::Bool,
    })
    .collect();
    let original = app.overlay.clone();
    for (theme, mode) in [
        ("midnight", ColorMode::TrueColor),
        ("paper", ColorMode::TrueColor),
        ("paper", ColorMode::None),
    ] {
        app.theme = Theme::resolve(theme, &BTreeMap::new(), mode).unwrap();
        for (w, h) in [(120, 32), (80, 24), (40, 16), (40, 12)] {
            for selected in 0..app.overlay.len() {
                app.selected[Page::Overlay.index()] = selected;
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                let mut hits = render::HitMap::default();
                terminal.draw(|f| hits = render::draw(f, &app)).unwrap();
                let text = screen(&app, w, h);
                assert!(text.contains(&format!("{}/4", selected + 1)), "{text}");
                assert!(text.contains("Current value"), "{text}");
                assert!(
                    text.contains(if selected % 2 == 0 { "On" } else { "Off" }),
                    "{text}"
                );
                assert!(
                    text.contains("Enter change") && text.contains("q quit"),
                    "{text}"
                );
                let (rect, _) = hits.rows.iter().find(|(_, i)| *i == selected).unwrap();
                let cell = &terminal.backend().buffer()[(rect.x, rect.y)];
                assert_eq!(cell.symbol(), ">");
                assert!(cell.modifier.contains(Modifier::BOLD));
                for (rect, _) in &hits.rows {
                    assert!(rect.right() <= w && rect.bottom() <= h - 2);
                }
                assert!(hits
                    .actions
                    .iter()
                    .any(|(_, a)| matches!(a, Action::Choose)));
                assert!(hits.actions.iter().any(|(_, a)| matches!(a, Action::Quit)));
            }
        }
    }
    eprintln!(
        "Pill: 96 TestBackend draws plus assertions in {:?}",
        started.elapsed()
    );
    assert_eq!(app.overlay, original);
    app.selected[Page::Overlay.index()] = 0;
    key(&mut app, KeyCode::Down);
    assert_eq!(app.selected(), 1);
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.modal, Some(Modal::Editor(_))));
    key(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    assert!(matches!(
        key(&mut app, KeyCode::Char('r')).as_slice(),
        [Effect::Overlay]
    ));
    app.overlay.clear();
    app.selected[Page::Overlay.index()] = 0;
    for failed in [false, true] {
        app.overlay_error = failed;
        let text = screen(&app, 40, 12);
        assert!(
            text.contains(if failed { "unavailable" } else { "Loading" }),
            "{text}"
        );
        assert!(
            text.contains("r refresh") && text.contains("q quit"),
            "{text}"
        );
    }
}

#[test]
fn stats_metrics_remain_readable_by_keyboard_in_short_and_narrow_terminals() {
    let (_dir, mut app) = app();
    app.connected = true;
    key(&mut app, KeyCode::Char('8'));
    assert_eq!(app.page, Page::Stats);
    assert!(key(&mut app, KeyCode::Char('r'))
        .iter()
        .any(|e| matches!(e, Effect::Request(Request::Stats, Tag::Stats))));
    for (theme, mode) in [
        ("midnight", ColorMode::TrueColor),
        ("paper", ColorMode::TrueColor),
        ("paper", ColorMode::None),
    ] {
        app.theme = Theme::resolve(theme, &BTreeMap::new(), mode).unwrap();
        for (width, height) in [(120, 32), (80, 24), (40, 16), (40, 12)] {
            for n in [0, 3, 9876543] {
                app.stats = xflow_core::ipc::Stats {
                    sessions: n,
                    words: n * 7,
                    audio_ms: n * 1000,
                    sessions_today: n / 2,
                    words_today: n * 3,
                    streak_days: u32::from(n > 0),
                    wpm: (n > 0).then_some(137.0),
                    latency_p50_ms: (n > 0).then_some(1234),
                    latency_p95_ms: (n > 0).then_some(987654321),
                };
                let fixture = app.stats.clone();
                app.stats_loaded = true;
                app.activity = if n == 0 { vec![] } else { vec![2, 9, 4, 12] };
                app.scroll = 0;
                let mut pages = String::new();
                let mut compact = String::new();
                for _ in 0..20 {
                    let view = screen(&app, width, height);
                    let nav = if width >= 70 { 18 } else { 16 };
                    compact.extend(
                        view.lines()
                            .skip(2)
                            .take_while(|line| line.chars().nth(nav) == Some('│'))
                            .flat_map(|line| {
                                line.chars().skip(nav + 1).take(width as usize - nav - 2)
                            })
                            .filter(|c| !c.is_whitespace()),
                    );
                    pages.push_str(&view);
                    key(&mut app, KeyCode::PageDown);
                }
                for metric in [
                    "Dictations",
                    "Today",
                    "Streak",
                    "Speakingpace",
                    "Timesaved",
                    "Typicalwait",
                    "40words/min",
                    "(p50)",
                    "(p95)",
                ] {
                    assert!(
                        compact.contains(metric),
                        "{width}x{height}: missing {metric}\n{pages}"
                    );
                }
                assert!(compact.contains(&(n * 7).to_string()), "{pages}");
                if n == 0 {
                    assert!(compact.contains("Nodictationsyet."), "{pages}");
                    assert!(compact.contains("Notavailable"), "{pages}");
                    assert!(pages.contains("No recent dictations"), "{pages}");
                } else {
                    assert!(compact.contains("987654.3s"), "{pages}");
                    assert!(compact.contains("137wordsaminute"), "{pages}");
                }
                assert_eq!(app.stats, fixture);
                key(&mut app, KeyCode::Up); // Existing list navigation returns to the top.
                let top = screen(&app, width, height);
                key(&mut app, KeyCode::PageDown);
                key(&mut app, KeyCode::PageUp);
                assert_eq!(screen(&app, width, height), top);
                assert!(top.contains("PgUp/PgDn"), "{top}");
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut hits = render::HitMap::default();
                terminal.draw(|f| hits = render::draw(f, &app)).unwrap();
                assert!(hits.tabs.iter().any(|(_, p)| *p == Page::Stats));
                assert!(terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .any(|c| c.modifier.contains(Modifier::BOLD)));
                if width == 120 {
                    assert!(hits.actions.iter().any(|(_, a)| *a == Action::Refresh));
                }
            }
            app.stats_loaded = false;
            app.scroll = 0;
            let unavailable = screen(&app, width, height);
            assert!(unavailable.contains("Stats unavailable"), "{unavailable}");
            assert!(!unavailable.contains("No dictations yet"));
        }
    }
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.page, Page::Home);
    key(&mut app, KeyCode::Char('8'));
    assert_eq!(app.page, Page::Stats);
    key(&mut app, KeyCode::Char('q'));
    assert!(app.quit);
}

#[test]
fn checkup_readable_status_focus_and_details_at_all_sizes() {
    let started = std::time::Instant::now();
    let (_dir, mut app) = app();
    app.page = Page::Doctor;
    app.connected = true;
    app.diagnostics = vec![
        "✓ Settings valid".into(),
        "! Kubernetes PostgreSQL OAuth — José, Priya and Mei: a deliberately long diagnostic with recovery instructions at the end: retry-setup".into(),
        "+ Clipboard tool found".into(),
        "• Microphone not checked".into(),
    ];
    for (theme, colors) in [
        ("midnight", ColorMode::TrueColor),
        ("paper", ColorMode::TrueColor),
        ("paper", ColorMode::None),
    ] {
        app.theme = Theme::resolve(theme, &BTreeMap::new(), colors).unwrap();
        for (width, height) in [(120, 32), (80, 24), (40, 16), (40, 12)] {
            key(&mut app, KeyCode::Home);
            // Find by label so macOS's permission rows keep their indices/actions.
            let index = app
                .rows()
                .iter()
                .position(|(r, _)| r.starts_with("! Kubernetes"))
                .unwrap();
            for _ in 0..index {
                key(&mut app, KeyCode::Down);
            }
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut hits = render::HitMap::default();
            terminal
                .draw(|frame| hits = render::draw(frame, &app))
                .unwrap();
            let text = screen(&app, width, height);
            assert!(text.contains("! Check"), "{width}x{height}: {text}");
            assert!(
                text.contains("r refresh") && text.contains("q quit"),
                "{text}"
            );
            assert!(
                text.contains("Details") && text.contains("Kubernetes"),
                "{text}"
            );
            let (rect, _) = hits
                .rows
                .iter()
                .find(|(_, i)| *i == index)
                .expect("selected row has a hit region");
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(rect.x, rect.y)].symbol(), ">");
            assert!(buffer[(rect.x, rect.y)]
                .modifier
                .contains(Modifier::REVERSED | Modifier::BOLD));
            assert!(hits
                .rows
                .iter()
                .all(|(r, _)| r.width > 0 && r.height == 1 && r.bottom() <= height));
            assert!(hits
                .actions
                .iter()
                .any(|(_, a)| matches!(a, Action::Refresh)));
            assert!(hits.actions.iter().any(|(_, a)| matches!(a, Action::Quit)));
            let mut tail = false;
            for _ in 0..8 {
                tail |= screen(&app, width, height).contains("retry-setup");
                key(&mut app, KeyCode::PageDown);
            }
            assert!(tail, "detail must be reachable at {width}x{height}");
            for _ in 0..8 {
                key(&mut app, KeyCode::PageUp);
            }
            assert_eq!(app.scroll, 0);
            assert!(key(&mut app, KeyCode::Char('r'))
                .iter()
                .any(|e| matches!(e, Effect::Doctor)));
            assert_eq!(app.selected(), index);
            key(&mut app, KeyCode::End);
            let text = screen(&app, width, height);
            assert!(text.contains("Not checked"), "{text}");
        }
    }
    eprintln!(
        "Check-up: 12 theme/size variants, keyboard, hit regions and redraws in {:?}",
        started.elapsed()
    );
}

#[test]
fn checkup_loading_and_helper_down_keep_recovery_visible() {
    let (_dir, mut app) = app();
    app.page = Page::Doctor;
    app.diagnostics.clear();
    for (width, height) in [(120, 32), (80, 24), (40, 16), (40, 12)] {
        let text = screen(&app, width, height);
        assert!(text.contains("Checking local setup"), "{text}");
        assert!(
            text.contains("! Check") && text.contains("r refresh") && text.contains("q quit"),
            "{text}"
        );
    }
}
