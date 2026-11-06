use super::{
    chat::{wrap, Entry, CONSENT_GUARD},
    render,
    runtime::{dispatch, drive_turn, handle, Update},
    state::{App, EditTarget, Effect, Modal, Page},
    theme::ColorMode,
};
use crate::{chat::TurnEnv, chat::TurnEvent, config_edit::ConfigFile, context::ContextEnv};
use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use std::{
    path::PathBuf,
    sync::{atomic::AtomicU64, Arc},
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, task::JoinSet};
use unicode_width::UnicodeWidthStr;
use xflow_providers::{
    llm::{ChatProvider, ChatRequest, Event, LlmError, Message, Stop},
    KeySource,
};

fn blank() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let file = ConfigFile::load(&path).unwrap();
    let mut app = App::new(path, ColorMode::TrueColor);
    app.load(file, None).unwrap();
    app.page = Page::Chat;
    (dir, app)
}
/// Set up with OpenRouter, already allowed.
fn ready() -> (tempfile::TempDir, App) {
    let (dir, mut app) = blank();
    app.set("ai.provider", "openrouter").unwrap();
    app.set("ai.consented", r#"["openrouter"]"#).unwrap();
    app.dirty = false;
    (dir, app)
}
fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE))
}
fn ctrl(app: &mut App, c: char) -> Vec<Effect> {
    app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}
fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        assert!(press(app, KeyCode::Char(c)).is_empty());
    }
}
/// Types and sends; returns the started turn.
fn send(app: &mut App, text: &str) -> super::chat::Turn {
    typed(app, text);
    match press(app, KeyCode::Enter).pop() {
        Some(Effect::ChatTurn(run)) => run.turn,
        _ => panic!("nothing was sent"),
    }
}
/// Puts the open consent modal `age` past its opening, or just-opened when `None`.
fn age_consent(app: &mut App, age: Option<Duration>) {
    let Some(Modal::ChatConsent { opened_at, .. }) = &mut app.modal else {
        panic!("consent modal");
    };
    // A future instant has zero elapsed time however slow the machine is.
    *opened_at = match age {
        Some(age) => Instant::now().checked_sub(age).unwrap(),
        None => Instant::now() + Duration::from_secs(60),
    };
}
/// The user has had time to read the consent modal.
fn settle(app: &mut App) {
    age_consent(app, Some(CONSENT_GUARD));
}
fn text_event(app: &mut App, id: u64, text: &str) {
    let mut redraw = true;
    handle(
        app,
        Update::ChatEvent(id, TurnEvent::Model(Event::Text(text.into()))),
        &mut redraw,
    );
    assert!(!redraw, "a streamed token never draws by itself");
}
fn done(app: &mut App, id: u64, result: Result<Stop, LlmError>, history: Vec<Message>) {
    handle(app, Update::ChatDone(id, result, history), &mut true);
}
fn draw(terminal: &mut Terminal<TestBackend>, app: &App) -> String {
    terminal
        .draw(|frame| {
            render::draw(frame, app);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        let row: String = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        text.push_str(row.trim_end());
        text.push('\n');
    }
    text
}
fn screen(app: &App, width: u16, height: u16) -> String {
    draw(
        &mut Terminal::new(TestBackend::new(width, height)).unwrap(),
        app,
    )
}
fn snapshot(name: &str, actual: &str) {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("src/tui/snapshots/{name}.txt"));
    if std::env::var_os("XFLOW_UPDATE_TUI_SNAPSHOTS").is_some() {
        std::fs::write(&path, actual).unwrap();
    }
    assert_eq!(actual, std::fs::read_to_string(&path).unwrap(), "{name}");
}

#[test]
fn a_turn_streams_finishes_and_becomes_history() {
    let (_dir, mut app) = ready();
    let turn = send(&mut app, "what did I say");
    assert_eq!(turn.question, "what did I say");
    assert!(turn.dictations);
    assert!(app.chat.input.is_empty());
    assert!(app.chat.running());
    text_event(&mut app, turn.id, "You said ");
    text_event(&mut app, turn.id, "hello");
    assert_eq!(app.chat.entries[1], Entry::Answer("You said hello".into()));
    let history = vec![Message::User("what did I say".into())];
    done(&mut app, turn.id, Ok(Stop::End), history.clone());
    assert!(!app.chat.running());
    assert!(!app.chat.can_retry());
    let next = send(&mut app, "and then");
    assert_eq!(next.history, history);
}
#[test]
fn enter_while_answering_keeps_the_text_and_sends_nothing() {
    let (_dir, mut app) = ready();
    send(&mut app, "first");
    typed(&mut app, "second");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.chat.input, "second");
}
#[test]
fn esc_cancels_a_running_turn_then_leaves_like_other_pages() {
    let (_dir, mut app) = ready();
    let turn = send(&mut app, "question");
    text_event(&mut app, turn.id, "partial");
    assert!(matches!(
        press(&mut app, KeyCode::Esc)[..],
        [Effect::ChatCancel]
    ));
    assert!(!app.chat.running());
    assert_eq!(app.chat.entries[1], Entry::Answer("partial".into()));
    assert!(
        matches!(&app.chat.entries[2], Entry::Error(e) if e.starts_with("Interrupted") && e.contains("Ctrl+R"))
    );
    // Late events from the aborted task change nothing.
    app.chat.take_dirty();
    text_event(&mut app, turn.id, " more");
    done(
        &mut app,
        turn.id,
        Ok(Stop::End),
        vec![Message::User("x".into())],
    );
    assert_eq!(app.chat.entries[1], Entry::Answer("partial".into()));
    assert!(app.chat.can_retry());
    assert!(!app.chat.take_dirty());

    typed(&mut app, "draft");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.page, Page::Home);
    assert_eq!(app.chat.input, "draft", "leaving keeps what was typed");
}
#[test]
fn an_interrupted_answer_keeps_its_text_and_one_key_resends_the_turn() {
    let (_dir, mut app) = ready();
    let first = send(&mut app, "question");
    done(
        &mut app,
        first.id,
        Ok(Stop::End),
        vec![Message::User("question".into())],
    );
    let turn = send(&mut app, "second question");
    text_event(&mut app, turn.id, "half an ans");
    done(
        &mut app,
        turn.id,
        Err(LlmError::Interrupted { partial: true }),
        vec![],
    );
    let text = screen(&app, 80, 24);
    assert!(text.contains("half an ans"), "{text}");
    assert!(text.contains("Interrupted"), "{text}");
    assert!(text.contains("Ctrl+R retry"), "{text}");
    typed(&mut app, "draft kept");
    let Some(Effect::ChatTurn(run)) = ctrl(&mut app, 'r').pop() else {
        panic!("retry resends");
    };
    assert_eq!(run.turn.question, "second question");
    assert_eq!(run.turn.history, vec![Message::User("question".into())]);
    assert_eq!(app.chat.input, "draft kept");
    assert!(app
        .chat
        .entries
        .contains(&Entry::Answer("half an ans".into())));
    assert!(app.chat.running());
    assert!(ctrl(&mut app, 'r').is_empty(), "already running");
    text_event(&mut app, run.turn.id, "whole answer");
    done(&mut app, run.turn.id, Ok(Stop::End), vec![]);
    assert!(app
        .chat
        .entries
        .contains(&Entry::Answer("whole answer".into())));
    assert!(ctrl(&mut app, 'r').is_empty(), "nothing left to retry");
}
#[test]
fn errors_are_one_plain_line_with_the_next_step() {
    let (_dir, mut app) = ready();
    let turn = send(&mut app, "q");
    done(&mut app, turn.id, Err(LlmError::Auth), vec![]);
    let Some(Entry::Error(line)) = app.chat.entries.last() else {
        panic!("inline error");
    };
    assert!(line.contains("OpenRouter rejected the API key"), "{line}");
    assert!(line.contains("xflow key set openrouter"), "{line}");
    assert!(!line.contains('\n'));
}
#[test]
fn one_key_starts_a_new_chat_and_cancels_a_running_turn() {
    let (_dir, mut app) = ready();
    let turn = send(&mut app, "secret question");
    text_event(&mut app, turn.id, "secret answer");
    typed(&mut app, "draft");
    assert!(matches!(ctrl(&mut app, 'n')[..], [Effect::ChatCancel]));
    assert!(app.chat.entries.is_empty());
    assert!(!app.chat.running());
    assert_eq!(app.chat.input, "draft");
    press(&mut app, KeyCode::End);
    typed(&mut app, "!");
    assert_eq!(app.chat.input, "draft!");
    let text = screen(&app, 80, 24);
    assert!(!text.contains("secret"), "{text}");
    let next = send(&mut app, "");
    assert!(next.history.is_empty());
    assert!(next.id > turn.id, "the old task's events stay stale");
}
#[test]
fn consent_yes_saves_through_the_config_path_and_is_not_asked_again() {
    let (_dir, mut app) = blank();
    app.set("ai.provider", "openrouter").unwrap();
    typed(&mut app, "question");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    settle(&mut app);
    let Some(Modal::ChatConsent { name, .. }) = &app.modal else {
        panic!("first send asks");
    };
    assert_eq!(name, "OpenRouter");
    let text = screen(&app, 80, 24);
    for line in ["This leaves your computer", "OpenRouter", "Allow it?"] {
        assert!(text.contains(line), "{line}: {text}");
    }
    assert!(crate::chat::consent_text("X")
        .starts_with("This leaves your computer: transcript excerpts"));
    app.dirty = false;
    let Some(Effect::ChatTurn(run)) = press(&mut app, KeyCode::Char('y')).pop() else {
        panic!("yes sends");
    };
    assert_eq!(run.turn.question, "question");
    assert_eq!(app.config.ai.consented, ["openrouter"]);
    assert!(app.dirty, "the run loop's usual save picks this up");
    assert!(app.file.as_ref().unwrap().to_string().contains("consented"));
    done(&mut app, run.turn.id, Ok(Stop::End), vec![]);
    send(&mut app, "again");
    assert!(app.modal.is_none());
}
#[test]
fn keys_typed_right_after_enter_cannot_answer_consent() {
    let (_dir, mut app) = blank();
    app.set("ai.provider", "openrouter").unwrap();
    typed(&mut app, "question");
    press(&mut app, KeyCode::Enter);
    let Some(Modal::ChatConsent { opened_at, .. }) = &app.modal else {
        panic!("first send asks");
    };
    assert!(
        opened_at.elapsed() < Duration::from_secs(60),
        "stamped on opening"
    );
    age_consent(&mut app, None);
    for early in [
        KeyCode::Char('y'),
        KeyCode::Char('Y'),
        KeyCode::Char('n'),
        KeyCode::Enter,
        KeyCode::Esc,
    ] {
        assert!(press(&mut app, early).is_empty(), "{early:?}");
        assert!(
            matches!(app.modal, Some(Modal::ChatConsent { .. })),
            "{early:?}"
        );
    }
    assert!(!app.chat.running());
    assert!(app.config.ai.consented.is_empty());
    assert_eq!(app.chat.input, "question");
    // Once the guard has passed, the same key answers.
    settle(&mut app);
    assert!(matches!(
        press(&mut app, KeyCode::Char('y'))[..],
        [Effect::ChatTurn(_)]
    ));
    assert_eq!(app.config.ai.consented, ["openrouter"]);
}
#[test]
fn consent_no_sends_nothing_and_keeps_the_question() {
    let (_dir, mut app) = blank();
    app.set("ai.provider", "anthropic").unwrap();
    typed(&mut app, "question");
    for no in [
        KeyCode::Char('n'),
        KeyCode::Esc,
        // Like `[y/N]`: a stray Enter is not a yes.
        KeyCode::Enter,
    ] {
        press(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert!(matches!(app.modal, Some(Modal::ChatConsent { .. })));
        assert!(press(&mut app, KeyCode::Char('z')).is_empty());
        assert!(ctrl(&mut app, 'y').is_empty());
        assert!(matches!(app.modal, Some(Modal::ChatConsent { .. })));
        assert!(press(&mut app, no).is_empty());
        assert!(app.modal.is_none());
        assert!(!app.chat.running());
        assert_eq!(app.chat.input, "question");
        assert!(
            matches!(app.chat.entries.last(), Some(Entry::Note(n)) if n.starts_with("Nothing was sent"))
        );
    }
    assert!(app.config.ai.consented.is_empty());
}
#[test]
fn consent_waits_while_a_save_holds_the_file() {
    let (_dir, mut app) = blank();
    app.set("ai.provider", "openai").unwrap();
    typed(&mut app, "question");
    press(&mut app, KeyCode::Enter);
    settle(&mut app);
    app.saving = true;
    assert!(press(&mut app, KeyCode::Char('y')).is_empty());
    assert!(matches!(app.modal, Some(Modal::ChatConsent { .. })));
    app.saving = false;
    assert!(matches!(
        press(&mut app, KeyCode::Char('y'))[..],
        [Effect::ChatTurn(_)]
    ));
}
#[test]
fn offline_refuses_cloud_providers_inline_before_asking_anything() {
    let (_dir, mut app) = blank();
    app.set("ai.provider", "openai").unwrap();
    app.set("privacy.offline", "true").unwrap();
    typed(&mut app, "question");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(app.modal.is_none());
    assert!(!app.chat.running());
    assert_eq!(app.chat.input, "question");
    assert!(
        matches!(app.chat.entries.last(), Some(Entry::Error(e)) if e.contains("OpenAI is refused") && e.contains("Settings"))
    );
    // A local server stays allowed.
    app.set("ai.provider", "custom").unwrap();
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.modal, Some(Modal::ChatConsent { .. })));
}
#[test]
fn with_history_off_the_page_says_so_and_chat_works_without_transcripts() {
    let (_dir, mut app) = blank();
    app.set("ai.provider", "openai").unwrap();
    app.set("privacy.history", "false").unwrap();
    assert!(screen(&app, 80, 24).contains("History is off"));
    let turn = send(&mut app, "question");
    assert!(app.modal.is_none(), "nothing of the user's history leaves");
    assert!(!turn.dictations);
}
#[test]
fn earlier_excerpts_never_follow_a_changed_provider_or_history_setting() {
    let (_dir, mut app) = ready();
    let excerpt = vec![Message::User("<untrusted_transcript>secret".into())];
    let turn = send(&mut app, "first");
    done(&mut app, turn.id, Ok(Stop::End), excerpt.clone());
    let turn = send(&mut app, "same scope");
    assert_eq!(turn.history, excerpt);
    done(&mut app, turn.id, Ok(Stop::End), excerpt);
    app.set("privacy.history", "false").unwrap();
    app.set("ai.provider", "openai").unwrap();
    let turn = send(&mut app, "history off, another provider");
    assert!(!turn.dictations);
    assert!(turn.history.is_empty());
    assert!(app
        .chat
        .entries
        .iter()
        .any(|e| matches!(e, Entry::Note(n) if n.contains("starts fresh"))));
}
#[test]
fn reads_name_the_provider_they_went_to() {
    let (_dir, mut app) = ready();
    app.set("ai.fallback", r#"["openai","anthropic"]"#).unwrap();
    let turn = send(&mut app, "q");
    let ctx = app.chat_ctx();
    let read = |query: serde_json::Value, bytes| TurnEvent::Read {
        tool: "search_dictations".into(),
        arguments: serde_json::json!({ "query": query }),
        bytes,
        ms: 1,
    };
    app.chat.event(turn.id, read("kubernetes".into(), 90), &ctx);
    assert!(app.chat.entries.contains(&Entry::Note(
        "Searched dictations for 'kubernetes': sent to OpenRouter".into()
    )));
    app.chat
        .event(turn.id, read("nope\u{1b}[31m".into(), 0), &ctx);
    assert!(app.chat.entries.contains(&Entry::Note(
        "Searched dictations for 'nope[31m': nothing found, so nothing was sent".into()
    )));
    let got = TurnEvent::Read {
        tool: "get_dictation".into(),
        arguments: serde_json::json!({"id": 7}),
        bytes: 9,
        ms: 1,
    };
    app.chat.event(turn.id, got.clone(), &ctx);
    assert!(app
        .chat
        .entries
        .contains(&Entry::Note("Read dictation 7: sent to OpenRouter".into())));
    // A consented fallback may receive it too, and the line says so.
    app.set("ai.consented", r#"["openrouter","anthropic"]"#)
        .unwrap();
    let ctx = app.chat_ctx();
    app.chat.event(turn.id, got, &ctx);
    assert!(app.chat.entries.contains(&Entry::Note(
        "Read dictation 7: sent to OpenRouter or Anthropic".into()
    )));
    app.chat.event(
        turn.id,
        TurnEvent::Notice("History is disabled".into()),
        &ctx,
    );
    assert!(app
        .chat
        .entries
        .contains(&Entry::Note("History is disabled".into())));
}
#[test]
fn setup_lists_the_catalog_with_key_state_and_saves_key_and_model() {
    let (_dir, mut app) = blank();
    app.page = Page::Home;
    assert!(matches!(
        press(&mut app, KeyCode::Char('-'))[..],
        [Effect::ChatKeys(false)]
    ));
    assert_eq!(app.page, Page::Chat);
    handle(
        &mut app,
        Update::ChatKeys(vec![
            ("groq".into(), KeySource::Missing),
            ("openai".into(), KeySource::Missing),
            ("anthropic".into(), KeySource::Keystore),
            ("openrouter".into(), KeySource::Environment("X".into())),
        ]),
        &mut true,
    );
    let text = screen(&app, 80, 24);
    for row in [
        "OpenAI",
        "No key yet",
        "Anthropic",
        "Key saved",
        "OpenRouter",
        "Key from your environment",
        "Local",
        "No key needed",
    ] {
        assert!(text.contains(row), "{row}: {text}");
    }
    snapshot("chat-setup-80x24", &text);
    // Choosing reuses the secure key form with a model field.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    let Some(Modal::Editor(editor)) = &app.modal else {
        panic!("key form");
    };
    assert!(matches!(&editor.target, EditTarget::ChatProvider(id) if id == "openai"));
    assert!(editor.fields[0].secret);
    assert_eq!(editor.fields[1].value.as_str(), "gpt-6-luna");
    // No key anywhere: the form says so and stays.
    press(&mut app, KeyCode::Enter);
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(!app.chat_ready());
    let Some(Modal::Editor(editor)) = &mut app.modal else {
        panic!("still open");
    };
    assert!(editor.error.is_some());
    editor.focus = 0;
    app.paste("  sk-synthetic-123  ");
    let text = screen(&app, 80, 24);
    assert!(!text.contains("sk-synthetic"), "{text}");
    press(&mut app, KeyCode::Enter);
    let effects = press(&mut app, KeyCode::Enter);
    let [Effect::SaveKey(id, secret)] = &effects[..] else {
        panic!("key payload");
    };
    assert_eq!(
        (id.as_str(), secret.as_str()),
        ("openai", "sk-synthetic-123")
    );
    assert!(app.modal.is_none());
    assert_eq!(app.config.ai.provider, "openai");
    assert_eq!(app.config.ai.model.as_deref(), Some("gpt-6-luna"));
    assert!(app.dirty, "saved by the usual path");
    assert!(!app
        .file
        .as_ref()
        .unwrap()
        .to_string()
        .contains("sk-synthetic"));
    // Set up: the page is the chat now, and typing goes to its input.
    assert!(screen(&app, 80, 24).contains("Ask"));
    typed(&mut app, "q");
    assert_eq!(app.chat.input, "q");
}
#[test]
fn a_saved_key_is_kept_and_local_needs_a_loopback_address_and_a_model() {
    let (_dir, mut app) = blank();
    app.chat.keys = vec![("anthropic".into(), KeySource::Keystore)];
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    assert!(
        press(&mut app, KeyCode::Enter).is_empty(),
        "no new key to save"
    );
    assert_eq!(app.config.ai.provider, "anthropic");

    let (_dir, mut app) = blank();
    app.set("ai.fallback", r#"["openai"]"#).unwrap();
    app.set("ai.consented", r#"["openai"]"#).unwrap();
    app.selected[Page::Chat.index()] = 4;
    press(&mut app, KeyCode::Enter);
    let form = |app: &mut App| {
        let Some(Modal::Editor(editor)) = &mut app.modal else {
            panic!("form");
        };
        editor.fields[0].cursor = 0;
        editor.fields[1].cursor = 0;
        editor.fields[0].value.clear();
        editor.fields[1].value.clear();
        editor.focus = 0;
    };
    form(&mut app);
    app.paste("http://example.com/v1/chat/completions");
    press(&mut app, KeyCode::Enter);
    app.paste("local-model");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(!app.chat_ready(), "a remote address is refused");
    assert!(screen(&app, 80, 24).contains("on this computer"));
    form(&mut app);
    app.paste("http://127.0.0.1:8080/v1/chat/completions");
    press(&mut app, KeyCode::Enter);
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(!app.chat_ready(), "a local server needs a model");
    form(&mut app);
    app.paste("http://127.0.0.1:8080/v1/chat/completions");
    press(&mut app, KeyCode::Enter);
    app.paste("local-model");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.config.ai.provider, "custom");
    assert!(
        app.config.ai.fallback.is_empty(),
        "Local must never fall back to a cloud provider"
    );
    assert_eq!(app.chat_ctx().receivers, "Local");
    assert_eq!(app.config.ai.model.as_deref(), Some("local-model"));
    assert_eq!(
        app.config.ai.endpoint.as_deref(),
        Some("http://127.0.0.1:8080/v1/chat/completions")
    );
    assert!(screen(&app, 80, 24).contains("Local"));
}
#[test]
fn command_letters_are_typing_on_a_ready_chat_page() {
    let (_dir, mut app) = ready();
    let letters = "qxcprnNkjhdCRK?:/ 1234567890aAtTbmeiu";
    typed(&mut app, letters);
    assert_eq!(app.chat.input, letters);
    assert!(!app.quit);
    assert!(app.modal.is_none());
    assert_eq!(app.page, Page::Chat);
    // Getting away and quitting still work.
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.page, Page::Meetings);
    press(&mut app, KeyCode::BackTab);
    assert_eq!(app.page, Page::Chat);
    ctrl(&mut app, 'p');
    assert!(matches!(app.modal, Some(Modal::Palette { .. })));
    press(&mut app, KeyCode::Esc);
    ctrl(&mut app, 'c');
    assert!(app.quit);
}
#[test]
fn failed_helper_start_never_swallows_enter_on_the_chat_page() {
    let (_dir, mut app) = ready();
    app.start_failed = true;
    assert!(matches!(
        send(&mut app, "question").question.as_str(),
        "question"
    ));
}
#[test]
fn non_ascii_wraps_to_width_and_never_splits_a_grapheme() {
    let hindi = "नमस्ते दुनिया क्षत्रिय की कहानी";
    for (text, width) in [
        (hindi, 8),
        ("日本語のテキストを折り返します", 7),
        ("👩‍💻👨‍👩‍👧‍👦🇮🇳 mixed कि text 日本", 5),
        ("plain words wrap between words", 10),
        ("日本語", 1),
    ] {
        let rows = wrap(text, width);
        let joined: String = rows.concat();
        assert_eq!(joined, text);
        for row in &rows {
            assert!(row.width() <= width.max(2), "{row:?} at {width}");
        }
    }
    assert_eq!(wrap("one two three", 7), ["one ", "two ", "three"]);
    assert_eq!(wrap("a\n\nb", 7), ["a", "", "b"]);
    let family = "👨‍👩‍👧‍👦";
    assert_eq!(wrap(&format!("{family}{family}"), 3), [family, family]);
}
#[test]
fn non_ascii_input_edits_by_grapheme() {
    let (_dir, mut app) = ready();
    app.paste("a👩‍💻क्षि日");
    press(&mut app, KeyCode::Backspace);
    assert_eq!(app.chat.input, "a👩‍💻क्षि");
    press(&mut app, KeyCode::Backspace);
    assert_eq!(app.chat.input, "a👩‍💻");
    press(&mut app, KeyCode::Left);
    typed(&mut app, "न");
    assert_eq!(app.chat.input, "aन👩‍💻");
    press(&mut app, KeyCode::Delete);
    assert_eq!(app.chat.input, "aन");
    press(&mut app, KeyCode::Home);
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Right);
    typed(&mut app, "é");
    assert_eq!(app.chat.input, "aनé");
    ctrl(&mut app, 'u');
    assert!(app.chat.input.is_empty());
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Delete);
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Right);

    // Wrapped, the cursor sits past the last cell and never on half a wide character.
    app.paste(&"日本語".repeat(10));
    let view = app.chat.input_view(7, 100);
    assert!(view.rows.iter().all(|r| r.width() <= 7));
    assert_eq!(view.rows.concat(), "日本語".repeat(10));
    assert_eq!(view.cursor, (view.rows.len() - 1, 6));
    let text = screen(&app, 80, 24);
    assert!(text.contains("日"), "{text}");
    // Pasted line ends and tabs keep their meaning; terminal controls don't get in.
    ctrl(&mut app, 'u');
    app.paste("one\r\ntwo\rthree\tfour\u{1b}[2J\u{7}");
    assert_eq!(app.chat.input, "one\ntwo\nthree    four[2J");
    let view = app.chat.input_view(20, 100);
    assert_eq!(view.rows, ["one", "two", "three    four[2J"]);
    assert_eq!(view.cursor, (2, 16));
    // Non-ASCII answers come through whole at any width.
    let turn = send(&mut app, "");
    for piece in ["日本語の", "答え क्षत्रिय 👩‍💻", "\u{1b}]52;c;evil\u{7}"]
    {
        text_event(&mut app, turn.id, piece);
    }
    let text = screen(&app, 44, 14);
    // (Wide characters occupy two cells of this text dump.)
    assert!(text.contains('答'), "{text}");
    assert!(!text.contains('\u{1b}'), "{text}");
}
#[test]
fn a_100_kb_paste_stays_whole_and_responsive_and_is_refused_at_send() {
    let (_dir, mut app) = ready();
    let paste = "नमस्ते 日本語 hello ".repeat(100 * 1024 / 34 + 1);
    assert!(paste.len() >= 100 * 1024);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let started = Instant::now();
    app.paste(&paste);
    draw(&mut terminal, &app);
    typed(&mut app, "!");
    draw(&mut terminal, &app);
    let elapsed = started.elapsed();
    // A debug build on a shared machine: far above any real target, far below a hang.
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
    assert_eq!(app.chat.input, format!("{paste}!"));
    // The field shows the cursor's end, not the whole paste.
    assert!(app.chat.input_view(60, 7).rows.len() <= 7);
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(!app.chat.running());
    assert_eq!(app.chat.input.len(), paste.len() + 1);
    assert!(matches!(app.chat.entries.last(), Some(Entry::Error(e)) if e.contains("over 64 KiB")));
    // Over the box's limit: refused whole, with word, and nothing else changes.
    let before = app.chat.input.len();
    app.paste(&"x".repeat(2 * 1024 * 1024));
    assert_eq!(app.chat.input.len(), before);
    assert!(matches!(app.chat.entries.last(), Some(Entry::Note(n)) if n.contains("too long")));
}
fn rows_text(app: &App, width: usize, height: usize) -> String {
    app.chat
        .window(width, height)
        .rows
        .into_iter()
        .map(|r| r.text)
        .collect::<Vec<_>>()
        .join("\n")
}
#[test]
fn resizing_mid_stream_rewraps_and_loses_nothing() {
    let (_dir, mut app) = ready();
    let turn = send(&mut app, "question");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut answer = String::new();
    for i in 0..40 {
        let piece = format!("word{i} 語क्ष ");
        answer.push_str(&piece);
        text_event(&mut app, turn.id, &piece);
        if i == 20 {
            draw(&mut terminal, &app);
            terminal.backend_mut().resize(44, 14);
            terminal
                .resize(ratatui::layout::Rect::new(0, 0, 44, 14))
                .unwrap();
            draw(&mut terminal, &app);
            terminal.backend_mut().resize(20, 5);
            terminal
                .resize(ratatui::layout::Rect::new(0, 0, 20, 5))
                .unwrap();
            assert!(draw(&mut terminal, &app).contains("window"));
            terminal.backend_mut().resize(100, 30);
            terminal
                .resize(ratatui::layout::Rect::new(0, 0, 100, 30))
                .unwrap();
        }
    }
    draw(&mut terminal, &app);
    for width in [96, 30, 11] {
        let flat: String = rows_text(&app, width, 10_000).replace('\n', "");
        assert_eq!(
            flat.replace(' ', ""),
            format!("› question{}", answer).replace(' ', ""),
            "{width}"
        );
    }
    let narrow = app.chat.window(11, 10_000);
    assert!(narrow.rows.iter().all(|r| r.text.width() <= 11));
}
#[test]
fn only_the_live_answer_is_wrapped_again() {
    let (_dir, mut app) = ready();
    let first = send(&mut app, "first");
    text_event(&mut app, first.id, "finished answer");
    done(&mut app, first.id, Ok(Stop::End), vec![]);
    let turn = send(&mut app, "second");
    text_event(&mut app, turn.id, "live");
    app.chat.window(30, 5);
    assert_eq!(app.chat.frozen_entries(), 3, "all but the live answer");
    // A finished row is not rebuilt: changing the entry would not show until a resize.
    app.chat.entries[1] = Entry::Answer("edited".into());
    assert!(rows_text(&app, 30, 100).contains("finished answer"));
    text_event(&mut app, turn.id, " more");
    assert!(rows_text(&app, 30, 100).contains("live more"));
    assert!(
        rows_text(&app, 31, 100).contains("edited"),
        "a resize wraps all"
    );
}
#[test]
fn scrolling_follows_the_bottom_unless_the_user_scrolled_up() {
    let (_dir, mut app) = ready();
    let turn = send(&mut app, "question");
    let many = (0..40)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    text_event(&mut app, turn.id, &many);
    let at_end = app.chat.window(30, 10);
    assert!(!at_end.below);
    assert!(at_end.rows.iter().any(|r| r.text == "line39"));
    press(&mut app, KeyCode::Up);
    let up = rows_text(&app, 30, 10);
    assert!(!up.contains("line39") && up.contains("line38"), "{up}");
    press(&mut app, KeyCode::PageUp);
    let held = rows_text(&app, 30, 10);
    text_event(&mut app, turn.id, "new\nnew\nnew\n");
    assert_eq!(
        rows_text(&app, 30, 10),
        held,
        "new text doesn't move the view"
    );
    assert!(app.chat.window(30, 10).below);
    for _ in 0..200 {
        press(&mut app, KeyCode::PageUp);
    }
    assert!(rows_text(&app, 30, 10).contains("question"));
    for _ in 0..200 {
        press(&mut app, KeyCode::PageDown);
    }
    text_event(&mut app, turn.id, "fresh");
    assert!(
        rows_text(&app, 30, 10).ends_with("fresh"),
        "following again"
    );
    // Sending a new question also returns to the bottom.
    press(&mut app, KeyCode::PageUp);
    done(&mut app, turn.id, Ok(Stop::End), vec![]);
    send(&mut app, "next");
    assert!(rows_text(&app, 30, 10).contains("next"));
}
#[test]
fn nothing_ticks_or_draws_when_idle() {
    let (_dir, mut app) = ready();
    assert!(!app.chat.running(), "the frame timer's guard is off");
    assert!(!app.chat.take_dirty());
    let turn = send(&mut app, "q");
    assert!(app.chat.running(), "on while streaming");
    text_event(&mut app, turn.id, "a");
    text_event(&mut app, turn.id, "b");
    assert!(app.chat.take_dirty());
    assert!(!app.chat.take_dirty(), "coalesced into one frame");
    done(&mut app, turn.id, Ok(Stop::End), vec![]);
    assert!(!app.chat.running(), "off again");
    send(&mut app, "q");
    press(&mut app, KeyCode::Esc);
    assert!(!app.chat.running(), "off after a cancel");
    let turn = send(&mut app, "q");
    done(&mut app, turn.id, Err(LlmError::Timeout), vec![]);
    assert!(!app.chat.running(), "off after a failure");
}
#[test]
fn tiny_terminals_never_panic_and_never_offer_a_letter_to_quit() {
    let (_dir, mut app) = ready();
    let turn = send(&mut app, "question");
    text_event(&mut app, turn.id, "日本語 क्ष answer ".repeat(50).as_str());
    app.paste(&"👩‍💻".repeat(300));
    let sizes = [
        (20, 5),
        (1, 1),
        (2, 2),
        (34, 11),
        (39, 12),
        (40, 11),
        (40, 12),
        (41, 13),
        (300, 12),
        (40, 200),
    ];
    for (width, height) in sizes {
        screen(&app, width, height);
    }
    assert!(screen(&app, 20, 12).contains("Ctrl+C"));
    press(&mut app, KeyCode::Esc);
    let modal = Modal::ChatConsent {
        name: "A provider with a long name".into(),
        opened_at: Instant::now(),
    };
    app.modal = Some(modal);
    for (width, height) in sizes {
        screen(&app, width, height);
    }
    app.modal = None;
    // Setup, too.
    let (_dir, app) = blank();
    for (width, height) in sizes {
        screen(&app, width, height);
    }
}
#[test]
fn chat_snapshots_with_history_off_and_interrupted() {
    let (_dir, mut app) = ready();
    app.connected = true;
    let turn = send(
        &mut app,
        "What did I decide about the kubernetes migration?",
    );
    let ctx = app.chat_ctx();
    app.chat.event(
        turn.id,
        TurnEvent::Read {
            tool: "search_dictations".into(),
            arguments: serde_json::json!({"query":"kubernetes"}),
            bytes: 120,
            ms: 1,
        },
        &ctx,
    );
    text_event(
        &mut app,
        turn.id,
        "You decided to move the staging cluster first, 日本語 क्षत्रिय, and then",
    );
    press(&mut app, KeyCode::Esc);
    typed(&mut app, "and production?");
    snapshot("chat-80x24", &screen(&app, 80, 24));
    snapshot("chat-120x35", &screen(&app, 120, 35));
}

/// Streams `tokens` at a fixed rate, then ends or fails.
struct Fake {
    tokens: usize,
    every: Duration,
    fail: Option<LlmError>,
}
#[async_trait]
impl ChatProvider for Fake {
    async fn warm(&self) -> Result<(), LlmError> {
        Ok(())
    }
    async fn stream(&self, _: &ChatRequest, tx: &mpsc::Sender<Event>) -> Result<(), LlmError> {
        let mut tick = tokio::time::interval(self.every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
        for i in 0..self.tokens {
            tick.tick().await;
            let _ = tx.send(Event::Text(format!("token{i} "))).await;
        }
        if let Some(error) = &self.fail {
            return Err(error.clone());
        }
        let _ = tx.send(Event::Done(Stop::End)).await;
        Ok(())
    }
}
fn env(dir: &std::path::Path) -> TurnEnv {
    TurnEnv {
        context: Arc::new(ContextEnv {
            database: dir.join("history.db"),
            config: dir.join("config.toml"),
            executable: "/test/xflow".into(),
            client: "test".into(),
        }),
        model: "m".into(),
        max_context_tokens: 32_000,
        max_output_tokens: 100,
    }
}
#[tokio::test]
async fn the_task_feeds_updates_and_aborting_it_stops_them() {
    let (dir, mut app) = ready();
    let env = Arc::new(env(dir.path()));
    let (tx, mut rx) = mpsc::channel(8);
    let mut jobs = JoinSet::new();
    let turn = send(&mut app, "question");
    let fake = Fake {
        tokens: 1_000_000,
        every: Duration::from_millis(1),
        fail: None,
    };
    let task_env = env.clone();
    let task_tx = tx.clone();
    let mut task = Some(jobs.spawn(async move {
        drive_turn(&fake, &task_env, turn, &task_tx).await;
    }));
    // The bounded channel holds the provider back while the UI isn't reading.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(tx.capacity() == 0, "backpressure, not an unbounded queue");
    for _ in 0..3 {
        let update = rx.recv().await.unwrap();
        let mut redraw = true;
        handle(&mut app, update, &mut redraw);
        assert!(!redraw);
    }
    press(&mut app, KeyCode::Esc);
    let generation = Arc::new(AtomicU64::new(0));
    dispatch(
        vec![Effect::ChatCancel],
        &tx,
        &mut jobs,
        &generation,
        &mut task,
    );
    assert!(task.is_none());
    let joined = jobs.join_next().await.unwrap().unwrap_err();
    assert!(joined.is_cancelled());
    // What was queued before the abort arrives late and is ignored.
    let shown = app.chat.entries.clone();
    while let Ok(update) = rx.try_recv() {
        handle(&mut app, update, &mut true);
    }
    assert_eq!(app.chat.entries, shown);
    assert!(app.chat.can_retry());
    assert_ne!(app.chat.entries.len(), 0);
}
#[tokio::test]
async fn a_failed_stream_keeps_the_partial_text_and_the_model_history() {
    let (dir, mut app) = ready();
    let env = env(dir.path());
    let (tx, mut rx) = mpsc::channel(64);
    let turn = send(&mut app, "question");
    let fake = Fake {
        tokens: 2,
        every: Duration::from_millis(1),
        fail: Some(LlmError::Interrupted { partial: true }),
    };
    let task = async {
        drive_turn(&fake, &env, turn, &tx).await;
        drop(tx);
    };
    let ui = async {
        while let Some(update) = rx.recv().await {
            handle(&mut app, update, &mut true);
        }
    };
    tokio::join!(task, ui);
    assert!(app
        .chat
        .entries
        .contains(&Entry::Answer("token0 token1 ".into())));
    assert!(app.chat.can_retry());
    let Some(Effect::ChatTurn(run)) = ctrl(&mut app, 'r').pop() else {
        panic!("retry");
    };
    assert!(run.turn.history.is_empty());
}
#[tokio::test]
async fn transcripts_reach_the_page_as_inline_reads() {
    let (dir, mut app) = ready();
    std::fs::write(dir.path().join("config.toml"), "").unwrap();
    let env = env(dir.path());
    let conn = rusqlite::Connection::open(&env.context.database).unwrap();
    conn.execute_batch("CREATE TABLE history(id INTEGER PRIMARY KEY, created_at INTEGER NOT NULL DEFAULT(unixepoch()), text TEXT NOT NULL, provider TEXT NOT NULL); PRAGMA user_version=3; INSERT INTO history(text,provider) VALUES('kubernetes rollout notes','t');").unwrap();
    drop(conn);
    let (tx, mut rx) = mpsc::channel(64);
    let turn = send(&mut app, "kubernetes");
    let fake = Fake {
        tokens: 1,
        every: Duration::from_millis(1),
        fail: None,
    };
    let task = async {
        drive_turn(&fake, &env, turn, &tx).await;
        drop(tx);
    };
    let ui = async {
        while let Some(update) = rx.recv().await {
            handle(&mut app, update, &mut true);
        }
    };
    tokio::join!(task, ui);
    assert!(
        app.chat.entries.contains(&Entry::Note(
            "Searched dictations for 'kubernetes': sent to OpenRouter".into()
        )),
        "{:?}",
        app.chat.entries
    );
    assert!(!app.chat.running());
}

fn percentile(samples: &mut [Duration], at: f64) -> Duration {
    samples.sort();
    samples
        .get(((samples.len() as f64 * at) as usize).min(samples.len().saturating_sub(1)))
        .copied()
        .unwrap_or_default()
}
/// The budget test: 200 tokens a second into a 50,000-character conversation, with the
/// run loop's own frame timer and a key every 100 ms. Timings print with `--nocapture`;
/// only the redraw rate is asserted, because the rest is guidance on a shared machine.
#[tokio::test]
async fn streaming_stays_within_the_draw_budget() {
    let (dir, mut app) = ready();
    for i in 0..250 {
        let line = format!("Conversation line {i}: नमस्ते 日本語 and some ordinary words to wrap around the panel, again and again until it is long. ");
        app.chat.entries.push(if i % 5 == 0 {
            Entry::You(line)
        } else {
            Entry::Answer(line)
        });
    }
    let chars: usize = app
        .chat
        .entries
        .iter()
        .map(|e| match e {
            Entry::You(t) | Entry::Answer(t) | Entry::Note(t) | Entry::Error(t) => {
                t.chars().count()
            }
        })
        .sum();
    assert!(chars >= 25_000, "{chars}");
    let filler = "x y z ".repeat((50_000 - chars.min(50_000)) / 6 + 1);
    app.chat.entries.push(Entry::Answer(filler));
    let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
    draw(&mut terminal, &app);

    let env = env(dir.path());
    let (tx, mut rx) = mpsc::channel(64);
    let turn = send(&mut app, "stream please");
    let fake = Fake {
        tokens: 600,
        every: Duration::from_millis(5),
        fail: None,
    };
    let task = async {
        drive_turn(&fake, &env, turn, &tx).await;
        drop(tx);
    };
    let ui = async {
        let (mut frames, mut keys) = (vec![], vec![]);
        let mut frame = tokio::time::interval(super::chat::FRAME);
        frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut key = tokio::time::interval(Duration::from_millis(100));
        let started = Instant::now();
        while app.chat.running() {
            tokio::select! {
                update = rx.recv() => {
                    let Some(update) = update else { break };
                    let mut redraw = true;
                    handle(&mut app, update, &mut redraw);
                }
                _ = frame.tick(), if app.chat.running() => {
                    if app.chat.take_dirty() {
                        let at = Instant::now();
                        draw(&mut terminal, &app);
                        frames.push(at.elapsed());
                    }
                }
                _ = key.tick() => {
                    let at = Instant::now();
                    press(&mut app, KeyCode::Char('k'));
                    draw(&mut terminal, &app);
                    keys.push(at.elapsed());
                }
            }
        }
        (frames, keys, started.elapsed())
    };
    let ((), (mut frames, mut keys, elapsed)) = tokio::join!(task, ui);
    let rate = frames.len() as f64 / elapsed.as_secs_f64();
    println!(
        "chat budget: {chars} chars + filler, {} frames in {elapsed:.2?} = {rate:.1} redraws/s; draw p50 {:.2?} p95 {:.2?} max {:.2?}; key-to-draw p95 {:.2?} ({} keys)",
        frames.len(),
        percentile(&mut frames, 0.50),
        percentile(&mut frames, 0.95),
        frames.last().copied().unwrap_or_default(),
        percentile(&mut keys, 0.95),
        keys.len(),
    );
    assert!(rate <= 30.0, "{rate} redraws a second");
    assert!(frames.len() > 10, "frames were drawn");
    assert!(!app.chat.running());
}

#[test]
fn meetings_need_new_consent_even_with_saved_provider_consent() {
    let (_dir, mut app) = ready();
    ctrl(&mut app, 's');
    typed(&mut app, "What did Priya decide in the meeting?");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(matches!(app.modal, Some(Modal::ChatConsent { .. })));
    settle(&mut app);
    let effect = press(&mut app, KeyCode::Char('y')).pop().unwrap();
    let Effect::ChatTurn(run) = effect else {
        panic!("turn")
    };
    assert!(run.turn.meetings);
    assert!(!run.turn.dictations);
}
