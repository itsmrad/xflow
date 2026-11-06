//! Replay a terminal line from the keys pressed after a paste.
//!
//! The line is the pasted text and the cursor starts at its end. A paste leaves
//! the cursor there. This is only a proposal: before saving, a separate guarded
//! visible-screen read must confirm it. Scrollback is never requested.
//! Keystrokes exist only inside this value and are cleared by [`Line::stop`].
//!
//! These are not modeled, and tracking stops instead of guessing: vi-mode,
//! wrapped or multi-line prompts, a line editor that rewrites the buffer
//! (autosuggestion accept, bracketed paste), and IME composition. Ctrl-U clears
//! the line only when the cursor is at the end, where bash and zsh agree; in
//! the middle they do not. Ctrl-W uses the same word characters as Option-left
//! (zsh). bash's whitespace Ctrl-W can differ when the word contains punctuation.
//! A command that would move or delete past the start of the paste stops
//! tracking: text before the paste was never seen.
//!
//! Confirmation requests only AXVisibleCharacterRange via AXStringForRange;
//! unavailable, ambiguous or stale text saves nothing.

use crate::learn::{
    caret_on, complete_on_caret, pending, word_at, COMMIT_ON_WORD_MS, COMMIT_QUIET_MS,
    LEAVE_QUIET_MS,
};
use async_trait::async_trait;
use std::time::Duration;

/// Keys from the terminal watch. The platform enqueues these and returns at once.
#[async_trait]
pub trait KeyReplay: Send {
    async fn next(&mut self) -> Option<ReplayIn>;
    /// A fresh, guarded visible-screen read must confirm every proposed save.
    async fn confirm(&mut self, _original: &str, _corrected: &str) -> Result<(), &'static str> {
        Err("unconfirmed")
    }
    /// Drop anything still queued and stop listening.
    fn stop(&mut self) {}
}

/// Only the visible range is exposed; there is no whole-value fallback.
pub trait VisibleScreen {
    fn safe(&self) -> bool;
    fn visible_range(&self) -> Option<(usize, usize)>;
    fn in_viewport(&self, start: usize, len: usize) -> bool;
    fn range(&self, start: usize, len: usize) -> Option<String>;
}

pub const MAX_VISIBLE_UTF16: usize = 16_384;
pub const MAX_CONFIRM_UTF16: usize = 4_096;

fn placeholder(text: &str) -> bool {
    ["[Pasted text", "[Pasted Content"]
        .iter()
        .any(|s| text.contains(s))
}

/// Read once, compare in memory, then drop the visible text. Errors are tags only.
pub fn confirm(
    screen: &impl VisibleScreen,
    original: &str,
    corrected: &str,
) -> Result<(), &'static str> {
    if original.is_empty()
        || corrected.is_empty()
        || original == corrected
        || original.chars().any(char::is_control)
        || corrected.chars().any(char::is_control)
        || placeholder(corrected)
    {
        return Err("unconfirmed");
    }
    if !screen.safe() {
        return Err("unsafe-screen");
    }
    let visible_range = screen.visible_range().ok_or("visible-range")?;
    let (start, len) = visible_range;
    let end = start.checked_add(len).ok_or("visible-bound")?;
    let proposed_len = corrected.encode_utf16().count();
    let len = len.min(proposed_len.saturating_mul(4).clamp(512, MAX_CONFIRM_UTF16));
    if len == 0 || proposed_len > len {
        return Err("visible-bound");
    }
    let start = end - len;
    if !screen.safe() || !screen.in_viewport(start, len) {
        return Err("unsafe-screen");
    }
    let visible = screen.range(start, len).ok_or("visible-read")?;
    if !screen.safe()
        || screen.visible_range() != Some(visible_range)
        || !screen.in_viewport(start, len)
    {
        return Err("unsafe-screen");
    }
    if visible.encode_utf16().count() != len {
        return Err("visible-count");
    }
    let mut matches = visible
        .char_indices()
        .filter(|(i, _)| visible[*i..].starts_with(corrected));
    let at = matches.next().map(|(i, _)| i).ok_or("unconfirmed")?;
    let suffix = visible[at + corrected.len()..].trim_start_matches(' ');
    if matches.next().is_none()
        && (suffix.is_empty() || suffix.starts_with('\n'))
        && !visible.contains(original)
        && !placeholder(&visible)
    {
        Ok(())
    } else {
        Err("unconfirmed")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayIn {
    Key(Key),
    /// The front app or its window changed.
    Leave,
    /// Secure input is on. The model must be discarded.
    Secure,
    /// An event was lost. The cursor is no longer known.
    Missed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Insert(String),
    Backspace,
    Delete,
    Left,
    Right,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    KillWord,
    /// Clear the whole line. Only applied at the end of the line.
    KillLine,
    Submit,
    /// The cursor would be a guess.
    Abstain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub cmd: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Tab,
    Escape,
    Return,
    Grave,
    Letter(char),
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Edit(Key),
    Leave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// `changed` is whether the text changed. A cursor move alone is false.
    Edit {
        changed: bool,
    },
    Submit,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moment {
    Quiet(Duration),
    Submit,
    Leave(Duration),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to commit, and nothing to wait for.
    Wait,
    /// The caret hold is still open. Look again later.
    Recheck,
    Save,
    Drop(&'static str),
}

/// The pasted line. Not `Debug`: the text must not show up in a log or a panic.
pub struct Line {
    text: String,
    /// Byte offset, always on a char boundary.
    cursor: usize,
    live: bool,
}

impl Line {
    /// Cursor at the end of `pasted`.
    pub fn new(pasted: &str) -> Self {
        let live = !pasted.is_empty()
            && pasted.encode_utf16().count() <= MAX_VISIBLE_UTF16
            && !pasted.chars().any(char::is_control);
        Self {
            cursor: if live { pasted.len() } else { 0 },
            text: if live {
                pasted.to_owned()
            } else {
                String::new()
            },
            live,
        }
    }

    pub fn apply(&mut self, key: Key) -> Applied {
        if !self.live {
            return Applied::Stop;
        }
        match key {
            Key::Abstain => self.halt(),
            Key::Submit => Applied::Submit,
            Key::Insert(s) => self.insert(&s),
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete(),
            Key::Left => self.left(),
            Key::Right => self.right(),
            Key::WordLeft => self.word_left(),
            Key::WordRight => self.word_right(),
            Key::LineStart => self.place(0),
            Key::LineEnd if self.cursor == self.text.len() => self.halt(),
            Key::LineEnd => self.place(self.text.len()),
            Key::KillWord => self.kill_word(),
            Key::KillLine => self.kill_line(),
        }
    }

    pub fn text(&self) -> Option<&str> {
        self.live.then_some(self.text.as_str())
    }

    /// Caret in UTF-16 units, the unit [`word_at`] uses.
    pub fn caret_utf16(&self) -> Option<usize> {
        self.live
            .then(|| self.text[..self.cursor].encode_utf16().count())
    }

    /// Forget the line. Safe to call again.
    pub fn stop(&mut self) {
        self.text.clear();
        self.text.shrink_to_fit();
        self.cursor = 0;
        self.live = false;
    }
}
impl Drop for Line {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Line {
    fn halt(&mut self) -> Applied {
        self.stop();
        Applied::Stop
    }

    fn insert(&mut self, s: &str) -> Applied {
        if s.chars().any(char::is_control)
            || self.text.encode_utf16().count() + s.encode_utf16().count() > MAX_VISIBLE_UTF16
        {
            return self.halt();
        }
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        Applied::Edit {
            changed: !s.is_empty(),
        }
    }

    fn backspace(&mut self) -> Applied {
        let Some((at, _)) = self.prev() else {
            return self.halt();
        };
        self.text.replace_range(at..self.cursor, "");
        self.cursor = at;
        Applied::Edit { changed: true }
    }

    fn delete(&mut self) -> Applied {
        let Some((at, _)) = self.next_char() else {
            return Applied::Edit { changed: false };
        };
        self.text.replace_range(self.cursor..at, "");
        Applied::Edit { changed: true }
    }

    fn left(&mut self) -> Applied {
        let Some((at, _)) = self.prev() else {
            return self.halt();
        };
        self.cursor = at;
        Applied::Edit { changed: false }
    }

    fn right(&mut self) -> Applied {
        let Some((at, _)) = self.next_char() else {
            return self.halt();
        };
        self.cursor = at;
        Applied::Edit { changed: false }
    }

    fn word_left(&mut self) -> Applied {
        let Some(at) = self.word_start() else {
            return self.halt();
        };
        self.cursor = at;
        Applied::Edit { changed: false }
    }

    fn word_right(&mut self) -> Applied {
        if self.cursor == self.text.len() {
            return self.halt();
        }
        self.cursor = self.word_end();
        Applied::Edit { changed: false }
    }

    fn place(&mut self, at: usize) -> Applied {
        self.cursor = at;
        Applied::Edit { changed: false }
    }

    fn kill_word(&mut self) -> Applied {
        let Some(at) = self.word_start() else {
            return self.halt();
        };
        if at == self.cursor {
            return Applied::Edit { changed: false };
        }
        self.text.replace_range(at..self.cursor, "");
        self.cursor = at;
        Applied::Edit { changed: true }
    }

    fn kill_line(&mut self) -> Applied {
        if self.cursor != self.text.len() {
            return self.halt();
        }
        if self.text.is_empty() {
            return Applied::Edit { changed: false };
        }
        self.text.clear();
        self.cursor = 0;
        Applied::Edit { changed: true }
    }

    /// Start of the previous word, or None if the cursor is already at the start.
    fn word_start(&self) -> Option<usize> {
        if self.cursor == 0 {
            return None;
        }
        let mut c = self.cursor;
        if !self
            .text
            .get(..c)
            .and_then(|s| s.chars().next_back())
            .is_some_and(word_char)
        {
            c = retreat(&self.text, c, |ch| !word_char(ch));
        }
        Some(retreat(&self.text, c, word_char))
    }

    fn word_end(&self) -> usize {
        let mut c = self.cursor;
        if !self
            .text
            .get(c..)
            .and_then(|s| s.chars().next())
            .is_some_and(word_char)
        {
            c = advance(&self.text, c, |ch| !word_char(ch));
        }
        advance(&self.text, c, word_char)
    }

    fn prev(&self) -> Option<(usize, char)> {
        let ch = self.text.get(..self.cursor)?.chars().next_back()?;
        Some((self.cursor - ch.len_utf8(), ch))
    }

    fn next_char(&self) -> Option<(usize, char)> {
        let ch = self.text.get(self.cursor..)?.chars().next()?;
        Some((self.cursor + ch.len_utf8(), ch))
    }
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || unicode_normalization::char::canonical_combining_class(c) != 0
}

fn retreat(text: &str, mut c: usize, pred: impl Fn(char) -> bool) -> usize {
    while let Some(ch) = text.get(..c).and_then(|s| s.chars().next_back()) {
        if !pred(ch) {
            break;
        }
        c -= ch.len_utf8();
    }
    c
}

fn advance(text: &str, mut c: usize, pred: impl Fn(char) -> bool) -> usize {
    while let Some(ch) = text.get(c..).and_then(|s| s.chars().next()) {
        if !pred(ch) {
            break;
        }
        c += ch.len_utf8();
    }
    c
}

/// Map one key to a replay action. `text` is the event's unicode string.
pub fn classify(kind: Kind, text: &str, mods: Mods) -> Action {
    if mods.cmd && !mods.ctrl && !mods.alt && !mods.shift && matches!(kind, Kind::Tab | Kind::Grave)
    {
        return Action::Leave;
    }
    // Shift changes the character we insert. With anything else it selects or
    // runs a shortcut, and the caret is no longer a single point.
    if mods.shift && !printable(text) {
        return Action::Edit(Key::Abstain);
    }
    if matches!(kind, Kind::Up | Kind::Down | Kind::Tab | Kind::Escape) {
        return Action::Edit(Key::Abstain);
    }
    if mods.cmd || mods.ctrl || mods.alt {
        return chord(kind, text, mods);
    }
    let key = match kind {
        Kind::Backspace => Key::Backspace,
        Kind::Delete => Key::Delete,
        Kind::Left => Key::Left,
        Kind::Right => Key::Right,
        Kind::Home => Key::LineStart,
        Kind::End => Key::LineEnd,
        Kind::Return => Key::Submit,
        _ if printable(text) => Key::Insert(text.to_owned()),
        _ => Key::Abstain,
    };
    Action::Edit(key)
}

fn chord(kind: Kind, text: &str, mods: Mods) -> Action {
    if [mods.ctrl, mods.alt, mods.cmd]
        .iter()
        .filter(|on| **on)
        .count()
        != 1
        || mods.shift
    {
        return Action::Edit(Key::Abstain);
    }
    if mods.cmd {
        return Action::Edit(match kind {
            Kind::Left | Kind::Home => Key::LineStart,
            Kind::Right | Kind::End => Key::LineEnd,
            Kind::Letter('v') => Key::Abstain,
            _ => Key::Abstain,
        });
    }
    if mods.alt {
        return Action::Edit(match kind {
            Kind::Left => Key::WordLeft,
            Kind::Right => Key::WordRight,
            _ if printable(text) => Key::Insert(text.to_owned()),
            _ => Key::Abstain,
        });
    }
    let letter = ctrl_letter(text).or(match kind {
        Kind::Letter(c) => Some(c),
        _ => None,
    });
    Action::Edit(match letter {
        Some('a') => Key::LineStart,
        Some('e') => Key::LineEnd,
        Some('f') => Key::Right,
        Some('w') => Key::KillWord,
        Some('u') => Key::KillLine,
        Some('k' | 'y' | 'v') => Key::Abstain,
        _ => Key::Abstain,
    })
}

fn ctrl_letter(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let code = u32::from(c);
    (1..=26)
        .contains(&code)
        .then(|| char::from(b'a' + (code as u8 - 1)))
}

fn printable(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| !c.is_control())
}

/// Whether the replayed line may be saved. The dictionary still decides the tier.
pub fn verdict(original: &str, edited: &str, caret: Option<usize>, moment: Moment) -> Verdict {
    let found = pending(original, edited);
    if found.is_empty() {
        return match moment {
            Moment::Quiet(_) => Verdict::Wait,
            Moment::Submit | Moment::Leave(_) => Verdict::Drop("no-edit"),
        };
    }
    let word = caret.and_then(|c| word_at(edited, c));
    let on = caret.is_none() || caret_on(&found, word.as_deref());
    match moment {
        Moment::Submit => Verdict::Save,
        Moment::Quiet(quiet) => {
            let ripe =
                quiet >= Duration::from_millis(COMMIT_ON_WORD_MS) && complete_on_caret(&found);
            if quiet >= Duration::from_millis(COMMIT_QUIET_MS) && !(on && !ripe) {
                Verdict::Save
            } else if quiet >= Duration::from_millis(COMMIT_QUIET_MS) {
                Verdict::Recheck
            } else {
                Verdict::Wait
            }
        }
        Moment::Leave(quiet) => {
            if on && (quiet < Duration::from_millis(LEAVE_QUIET_MS) || !complete_on_caret(&found)) {
                Verdict::Drop("partial")
            } else {
                Verdict::Save
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DictionaryConfig, Replacement};
    use crate::learn::{review, Adopted};
    use std::cell::Cell;
    use Kind::*;

    struct Screen {
        text: Option<String>,
        len: usize,
        reads: Cell<usize>,
        secure: Cell<bool>,
        secure_during_read: bool,
        in_viewport: bool,
        bounds: Cell<Option<(usize, usize)>>,
    }
    impl Screen {
        fn new(text: &str) -> Self {
            Self {
                text: Some(text.into()),
                len: text.encode_utf16().count(),
                reads: Cell::new(0),
                secure: Cell::new(false),
                secure_during_read: false,
                in_viewport: true,
                bounds: Cell::new(None),
            }
        }
    }
    impl VisibleScreen for Screen {
        fn safe(&self) -> bool {
            !self.secure.get()
        }
        fn visible_range(&self) -> Option<(usize, usize)> {
            Some((500_000, self.len))
        }
        fn in_viewport(&self, start: usize, len: usize) -> bool {
            self.bounds.set(Some((start, len)));
            self.in_viewport
        }
        fn range(&self, start: usize, len: usize) -> Option<String> {
            assert_eq!(
                self.bounds.get(),
                Some((start, len)),
                "bounds checked on the tail being read"
            );
            assert!(start >= 500_000 && len <= MAX_CONFIRM_UTF16);
            assert_eq!(start + len, 500_000 + self.len, "tail only");
            self.reads.set(self.reads.get() + 1);
            if self.secure_during_read {
                self.secure.set(true);
            }
            let text = self.text.as_ref()?;
            let units: Vec<_> = text.encode_utf16().collect();
            let offset = start - 500_000;
            String::from_utf16(units.get(offset..offset + len).unwrap_or(&units)).ok()
        }
    }
    #[test]
    fn visible_confirmation_is_unique_literal_and_bounded() {
        let cases = [
            (
                "kubectl rolaut status deployment/api",
                "kubectl rollout status deployment/api",
                "$ kubectl rollout status deployment/api",
                true,
            ),
            (
                "git rebsae origin/main",
                "git rebase origin/main",
                "$ git rebase origin/main\n$ git rebase origin/main",
                false,
            ),
            (
                "Aisha fixed Postgress",
                "Aisha fixed PostgreSQL",
                "Aisha fixed Postgress\nAisha fixed PostgreSQL",
                false,
            ),
            (
                "Kal Kubernetis deploy karna",
                "Kal Kubernetes deploy karna",
                "❯ [Pasted text #1 +4 lines]",
                false,
            ),
            (
                "ask Claud to review",
                "ask Claude to review",
                "ask Claude\nto review",
                false,
            ),
            (
                "use kubernetis",
                "use Kubernetes",
                "use kuberneKubernetes",
                false,
            ),
            (
                "Priyanca approved OAuth",
                "Priyanka approved OAuth",
                "❯ Priyanka approved OAuth",
                true,
            ),
            (
                "post gres",
                "[Pasted Content 900 chars]",
                "[Pasted Content 900 chars]",
                false,
            ),
        ];
        for (original, corrected, visible, expected) in cases {
            let screen = Screen::new(visible);
            assert_eq!(confirm(&screen, original, corrected).is_ok(), expected);
            assert!(screen.reads.get() <= 1);
        }
        let mut screen = Screen::new("José fixed useEffect");
        let oversized = "x".repeat(MAX_CONFIRM_UTF16 + 1);
        assert_eq!(
            confirm(&screen, "old phrase", &oversized),
            Err("visible-bound")
        );
        assert_eq!(screen.reads.get(), 0);
        screen.len += 1;
        assert_eq!(
            confirm(&screen, "Jose fixed useEffect", "José fixed useEffect"),
            Err("visible-count")
        );
        screen.text = None;
        assert_eq!(
            confirm(&screen, "Jose fixed useEffect", "José fixed useEffect"),
            Err("visible-read")
        );
    }
    #[test]
    fn confirmation_requires_a_line_end_and_no_placeholder_elsewhere() {
        let original = "kubectl rolaut deploym";
        let corrected = "kubectl rollout deploym";
        for text in [
            "$ kubectl rollout deployment-api",
            "$ kubectl rollout deploym --namespace staging",
            "[Pasted text #1 +4 lines]\n$ kubectl rollout deploym",
            "[Pasted Content 900 chars]\n$ kubectl rollout deploym",
        ] {
            assert_eq!(
                confirm(&Screen::new(text), original, corrected),
                Err("unconfirmed")
            );
        }
        for text in [
            "$ kubectl rollout deploym   ",
            "$ kubectl rollout deploym  \nnext prompt",
        ] {
            assert_eq!(confirm(&Screen::new(text), original, corrected), Ok(()));
        }
    }
    #[test]
    fn only_the_proposal_sized_tail_is_read_after_its_own_bounds_check() {
        let text = format!(
            "{}\n$ git rebase origin/main",
            "unrelated screen output\n".repeat(300)
        );
        let mut screen = Screen::new(&text);
        assert_eq!(
            confirm(&screen, "git rebsae origin/main", "git rebase origin/main"),
            Ok(())
        );
        assert_eq!(screen.bounds.get(), Some((500_000 + screen.len - 512, 512)));
        assert_eq!(screen.reads.get(), 1);
        screen.in_viewport = false;
        screen.reads.set(0);
        assert_eq!(
            confirm(&screen, "git rebsae origin/main", "git rebase origin/main"),
            Err("unsafe-screen")
        );
        assert_eq!(screen.reads.get(), 0, "an off-screen tail is never fetched");
        let long = format!("{} Priyanka", "a".repeat(1_100));
        let screen = Screen::new(&format!("{}\n{long}", "padding\n".repeat(1_000)));
        assert_eq!(confirm(&screen, "unrelated original", &long), Ok(()));
        assert_eq!(screen.bounds.get().unwrap().1, MAX_CONFIRM_UTF16);
        assert_eq!(
            confirm(
                &screen,
                "unrelated original",
                &"b".repeat(MAX_CONFIRM_UTF16 + 1)
            ),
            Err("visible-bound")
        );
    }
    #[test]
    fn completion_keys_at_the_known_end_discard_the_model() {
        let actions = [
            classify(Right, "", plain()),
            classify(End, "", plain()),
            classify(Letter('e'), "\u{5}", hold(true, false, false)),
            classify(Letter('f'), "\u{6}", hold(true, false, false)),
            classify(Right, "", hold(false, true, false)),
        ];
        for action in actions {
            let Action::Edit(key) = action else {
                panic!("edit expected");
            };
            let mut line = Line::new("kubectl rollout deploym");
            assert_eq!(line.apply(key), Applied::Stop);
            assert!(
                line.text().is_none(),
                "an accepted deployment-api suffix cannot leave a proposal"
            );
        }
    }
    #[test]
    fn secure_input_before_or_during_confirmation_never_confirms() {
        let original = "check Postgress";
        let corrected = "check PostgreSQL";
        let mut screen = Screen::new(corrected);
        // The watch was armed while safe; secure input turns on during the quiet period.
        screen.secure.set(true);
        assert_eq!(confirm(&screen, original, corrected), Err("unsafe-screen"));
        assert_eq!(screen.reads.get(), 0);
        screen.secure.set(false);
        screen.secure_during_read = true;
        assert_eq!(confirm(&screen, original, corrected), Err("unsafe-screen"));
        assert_eq!(screen.reads.get(), 1);
    }
    #[test]
    fn multiline_and_oversized_models_abstain() {
        for pasted in [
            "git rebase\nkubectl rollout".to_owned(),
            "a".repeat(MAX_VISIBLE_UTF16 + 1),
            String::new(),
        ] {
            assert!(Line::new(&pasted).text().is_none());
        }
        let mut line = Line::new(&"a".repeat(MAX_VISIBLE_UTF16));
        assert_eq!(line.apply(Key::Insert("x".into())), Applied::Stop);
    }

    fn plain() -> Mods {
        Mods {
            shift: false,
            ctrl: false,
            alt: false,
            cmd: false,
        }
    }
    fn hold(ctrl: bool, alt: bool, cmd: bool) -> Mods {
        Mods {
            shift: false,
            ctrl,
            alt,
            cmd,
        }
    }
    fn run(start: &str, keys: &[Key]) -> Option<String> {
        let mut line = Line::new(start);
        for key in keys {
            match line.apply(key.clone()) {
                Applied::Stop => return None,
                Applied::Submit | Applied::Edit { .. } => {}
            }
        }
        line.text().map(str::to_owned)
    }
    fn ins(s: &str) -> Key {
        Key::Insert(s.into())
    }
    fn typed(s: &str) -> Vec<Key> {
        s.chars().map(|c| Key::Insert(c.to_string())).collect()
    }
    fn adopt(original: &str, edited: &str) -> Adopted {
        let found = review(original, edited, &DictionaryConfig::default()).unwrap();
        let mut seen = Vec::new();
        let mut dictionary = DictionaryConfig::default();
        dictionary.adopt(&found, &mut seen)
    }

    #[test]
    fn each_key_class_edits_the_line() {
        let start = "refactor the kubernetis deploy";
        assert_eq!(run(start, &[Key::Left, Key::Right]), Some(start.into()));
        assert_eq!(
            run(start, &[Key::LineStart, ins("x"), Key::LineEnd, ins("!")]),
            Some(format!("x{start}!"))
        );
        // Cmd-left / cmd-right and Home / End are the ends of the line.
        assert_eq!(
            classify(Left, "", hold(false, false, true)),
            Action::Edit(Key::LineStart)
        );
        assert_eq!(
            classify(Right, "", hold(false, false, true)),
            Action::Edit(Key::LineEnd)
        );
        assert_eq!(
            run("José", &[Key::Backspace]),
            Some("Jos".into()),
            "one scalar, including é"
        );
        assert_eq!(run("Søren", &[Key::Delete]), Some("Søren".into()));
        let mut line = Line::new("Søren");
        line.apply(Key::LineStart);
        assert_eq!(line.apply(Key::Delete), Applied::Edit { changed: true });
        assert_eq!(line.text(), Some("øren"));
        assert_eq!(
            run("Nguyễn", &typed(" ")),
            Some("Nguyễn ".into()),
            "a name is just text"
        );
        assert_eq!(
            run("ask Priyanka", &[Key::Backspace]),
            Some("ask Priyank".into())
        );
        // Ctrl-A / Ctrl-E from the control character, layout-independent.
        assert_eq!(
            classify(Other, "\u{1}", hold(true, false, false)),
            Action::Edit(Key::LineStart)
        );
        assert_eq!(
            classify(Other, "\u{5}", hold(true, false, false)),
            Action::Edit(Key::LineEnd)
        );
        assert_eq!(
            classify(Letter('w'), "\u{17}", hold(true, false, false)),
            Action::Edit(Key::KillWord)
        );
        assert_eq!(classify(Return, "", plain()), Action::Edit(Key::Submit));
        assert_eq!(
            classify(
                Letter('k'),
                "K",
                Mods {
                    shift: true,
                    ..plain()
                }
            ),
            Action::Edit(ins("K"))
        );
        assert_eq!(
            classify(Letter('e'), "é", hold(false, true, false)),
            Action::Edit(ins("é"))
        );
    }

    #[test]
    fn word_jumps_stop_on_boundaries() {
        let line = "she sells seashells by the seashore";
        let mut at = Line::new(line);
        let mut lands = vec![];
        for _ in 0..6 {
            at.apply(Key::WordLeft);
            lands.push(
                line[at.cursor..]
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .to_owned(),
            );
        }
        assert_eq!(
            lands,
            ["seashore", "the", "by", "seashells", "sells", "she"]
        );
        at.apply(Key::WordRight);
        assert_eq!(&line[at.cursor..], " sells seashells by the seashore");
        // Punctuation and a combining mark stay with the word they belong to.
        let named = "José, call Priyanka.";
        let mut at = Line::new(named);
        at.apply(Key::WordLeft);
        assert_eq!(&named[at.cursor..], "Priyanka.");
        at.apply(Key::WordLeft);
        assert_eq!(&named[at.cursor..], "call Priyanka.");
        at.apply(Key::WordLeft);
        assert_eq!(&named[at.cursor..], "José, call Priyanka.");
        let devanagari = "प्रियंका ने क्ष कहा";
        let mut at = Line::new(devanagari);
        at.apply(Key::WordLeft);
        assert_eq!(&devanagari[at.cursor..], "कहा");
        at.apply(Key::WordLeft);
        assert_eq!(&devanagari[at.cursor..], "क्ष कहा");
        // क्ष is क + virama + ष. The caret is UTF-16, and the emoji is two units.
        let mixed = "क्ष😀";
        let mut at = Line::new(mixed);
        assert_eq!(at.caret_utf16(), Some(mixed.encode_utf16().count()));
        at.apply(Key::Left);
        assert_eq!(at.caret_utf16(), Some(3), "emoji is two UTF-16 units");
        at.apply(Key::WordLeft);
        assert_eq!(at.caret_utf16(), Some(0));
        assert_eq!(at.text().unwrap().encode_utf16().count(), 5);
    }

    #[test]
    fn ctrl_w_kills_the_word_and_ctrl_u_clears_only_at_the_end() {
        assert_eq!(
            run("she sells seashells", &[Key::KillWord]),
            Some("she sells ".into())
        );
        assert_eq!(run("foo-bar", &[Key::KillWord]), Some("foo-".into()));
        assert_eq!(run("use kubernetis", &[Key::KillLine]), Some(String::new()));
        assert_eq!(
            run("use kubernetis", &[Key::Left, Key::KillLine]),
            None,
            "mid-line Ctrl-U is bash one way and zsh the other"
        );
    }

    #[test]
    fn a_correction_reaches_the_tier_the_dictionary_already_uses() {
        let original = "ship the kubernetis chart today";
        // The cursor starts at the end of the paste. Step back onto the word, then replace it.
        let mut keys = vec![
            Key::WordLeft,
            Key::WordLeft,
            Key::WordLeft,
            Key::WordRight,
            Key::KillWord,
        ];
        keys.extend(typed("Kubernetes"));
        let edited = run(original, &keys).unwrap();
        assert_eq!(edited, "ship the Kubernetes chart today");
        let saved = adopt(original, &edited);
        assert_eq!(saved.added, ["Kubernetes"]);
        assert_eq!(
            saved.rules,
            [Replacement {
                from: "kubernetis".into(),
                to: "Kubernetes".into()
            }]
        );
        // Tier 2: a common source and an uncommon destination. The word is saved, the rule waits.
        let saved = adopt("the cloud client", "the Claude client");
        assert_eq!(saved.added, ["Claude"]);
        assert!(
            saved.rules.is_empty(),
            "a rule would rewrite an everyday word"
        );
        // Tier 3: an everyday swap is not a correction, so a submit saves nothing.
        assert!(matches!(
            verdict("the cat sat", "the dog sat", Some(0), Moment::Submit),
            Verdict::Drop("no-edit")
        ));
        let end = "use Kubernetes".encode_utf16().count();
        assert_eq!(
            verdict(
                "use kubernetis",
                "use Kubernetes",
                Some(end),
                Moment::Quiet(Duration::from_millis(2_000))
            ),
            Verdict::Recheck,
            "the caret is still on the word"
        );
        assert_eq!(
            verdict(
                "use kubernetis",
                "use Kubernetes",
                Some(0),
                Moment::Quiet(Duration::from_millis(2_000))
            ),
            Verdict::Save
        );
        assert_eq!(
            verdict(
                "use kubernetis",
                "use Kubernetes",
                Some(end),
                Moment::Quiet(Duration::from_millis(4_000))
            ),
            Verdict::Save
        );
        // A shorter word under the caret is held back on a leave and on the quiet timer.
        let short = "Priyank".encode_utf16().count();
        assert_eq!(
            verdict(
                "Priyanca",
                "Priyank",
                Some(short),
                Moment::Leave(Duration::from_secs(5))
            ),
            Verdict::Drop("partial")
        );
        assert_eq!(
            verdict(
                "Priyanca",
                "Priyank",
                Some(short),
                Moment::Quiet(Duration::from_secs(5))
            ),
            Verdict::Recheck
        );
        assert_eq!(
            verdict("Priyanca", "Priyank", Some(short), Moment::Submit),
            Verdict::Save,
            "Return finishes the word"
        );
        assert_eq!(
            verdict(
                "use kubernetis",
                "use Kubernetes",
                Some(end),
                Moment::Leave(Duration::from_millis(100))
            ),
            Verdict::Drop("partial")
        );
        assert_eq!(
            verdict(
                "use kubernetis",
                "use Kubernetes",
                Some(end),
                Moment::Leave(Duration::from_millis(500))
            ),
            Verdict::Save
        );
    }

    #[test]
    fn every_abstain_trigger_drops_the_line() {
        let start = "she sells seashells by the seashore";
        let abstain = [
            classify(Up, "", plain()),
            classify(Down, "", plain()),
            classify(Tab, "", plain()),
            classify(Escape, "", plain()),
            classify(Other, "", plain()),
            classify(Letter('k'), "", hold(true, false, false)),
            classify(Letter('y'), "", hold(true, false, false)),
            classify(Letter('v'), "", hold(false, false, true)),
            classify(Letter('v'), "\u{16}", hold(true, false, false)),
            classify(
                Left,
                "",
                Mods {
                    shift: true,
                    ..plain()
                },
            ),
            classify(Left, "", hold(true, true, false)),
            classify(Letter('c'), "", hold(false, false, true)),
            classify(Letter('e'), "", hold(false, true, false)),
            classify(
                Return,
                "",
                Mods {
                    shift: true,
                    ..plain()
                },
            ),
        ];
        for action in abstain {
            let Action::Edit(key) = action else {
                panic!("expected an abstain, got {action:?}");
            };
            assert_eq!(key, Key::Abstain);
            assert_eq!(run(start, &[key]), None);
        }
        assert_eq!(
            run(start, &[Key::LineStart, Key::Left]),
            None,
            "past the paste"
        );
        assert_eq!(run(start, &[Key::LineStart, Key::Backspace]), None);
        assert_eq!(run(start, &[Key::LineStart, Key::WordLeft]), None);
        assert_eq!(run(start, &[Key::LineStart, Key::KillWord]), None);
        assert_eq!(run(start, &[Key::Abstain, ins("x")]), None);
    }

    #[test]
    fn secure_input_and_stop_clear_the_line() {
        let mut line = Line::new("the password is swordfish");
        assert_eq!(line.apply(Key::Abstain), Applied::Stop);
        assert_eq!(line.text(), None);
        assert_eq!(line.caret_utf16(), None);
        line.stop();
        assert_eq!(line.text(), None);
        assert_eq!(line.apply(ins("x")), Applied::Stop);
        assert_eq!(line.text(), None);
    }

    #[test]
    fn a_wrong_cursor_does_not_produce_a_save() {
        // A click moved the real cursor. Later keystrokes must not be applied
        // at the old one, and the line is gone so nothing can be saved from it.
        let mut line = Line::new("ship it to cooper netties now");
        line.apply(Key::Abstain);
        assert_eq!(line.apply(ins("Kubernetes")), Applied::Stop);
        assert_eq!(line.text(), None);
        // The same insertion, had the cursor been trusted at the end, is not a
        // correction either: review refuses an added word.
        assert!(matches!(
            verdict(
                "ship it to cooper netties now",
                "ship it to cooper netties now Kubernetes",
                Some(0),
                Moment::Submit
            ),
            Verdict::Drop("no-edit")
        ));
        // Walking off the front of the paste is the same refusal.
        let steps: Vec<_> = "kubernetis".chars().map(|_| Key::Left).collect();
        assert_eq!(run("kubernetis", &steps), Some("kubernetis".into()));
        let mut off = steps;
        off.push(Key::Left);
        assert_eq!(run("kubernetis", &off), None, "one step past the paste");
        assert_eq!(
            run("kubernetis", &[Key::LineStart, Key::Left, ins("X")]),
            None
        );
    }
}
