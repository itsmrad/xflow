//! Local corrections precede model cleanup; literal snippets follow it.
use crate::config::{Config, Style};

#[derive(Clone)]
pub(crate) struct Word<'a> {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) text: &'a str,
}
fn word_char(c: char) -> bool {
    c.is_alphanumeric()
        || c == '_'
        || matches!(c as u32, 0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0xfe20..=0xfe2f)
}
pub(crate) fn words(text: &str) -> Vec<Word<'_>> {
    let mut result = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        let apostrophe = matches!(c, '\'' | '’')
            && start.is_some()
            && text[i + c.len_utf8()..]
                .chars()
                .next()
                .is_some_and(word_char);
        if word_char(c) || apostrophe {
            start.get_or_insert(i);
        } else if let Some(start) = start.take() {
            result.push(Word {
                start,
                end: i,
                text: &text[start..i],
            });
        }
    }
    if let Some(start) = start {
        result.push(Word {
            start,
            end: text.len(),
            text: &text[start..],
        });
    }
    result
}
fn folded(text: &str) -> String {
    text.split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}
/// Match original input once, longest phrase first. Inserted text is never scanned.
fn replace(text: &str, pairs: &[(&str, &str)], punctuation_insensitive: bool) -> String {
    let input = words(text);
    let lower: Vec<_> = input.iter().map(|w| w.text.to_lowercase()).collect();
    let mut patterns: Vec<_> = pairs
        .iter()
        .map(|(from, to)| {
            (
                words(from)
                    .iter()
                    .map(|w| w.text.to_lowercase())
                    .collect::<Vec<_>>(),
                folded(from),
                *to,
            )
        })
        .filter(|(w, _, _)| !w.is_empty())
        .collect();
    patterns.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(b.1.len().cmp(&a.1.len())));
    let mut output = String::with_capacity(text.len());
    let (mut cursor, mut i) = (0, 0);
    while i < input.len() {
        let matched = patterns.iter().find(|(pattern, from, _)| {
            let end = i + pattern.len();
            end <= input.len()
                && pattern.iter().zip(&lower[i..end]).all(|(a, b)| a == b)
                && (punctuation_insensitive
                    || folded(&text[input[i].start..input[end - 1].end]) == *from)
        });
        if let Some((pattern, _, to)) = matched {
            output.push_str(&text[cursor..input[i].start]);
            output.push_str(to);
            i += pattern.len();
            cursor = input[i - 1].end;
            if to.is_empty() {
                for (offset, c) in text[cursor..].char_indices() {
                    if !matches!(c, ',' | ';' | ':') {
                        break;
                    }
                    cursor = input[i - 1].end + offset + c.len_utf8();
                }
            }
        } else {
            i += 1;
        }
    }
    output.push_str(&text[cursor..]);
    output
}
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push(c);
            space = false;
        } else if c.is_whitespace() {
            space = true;
        } else {
            if space
                && !out.is_empty()
                && !out.ends_with('\n')
                && (!matches!(c, ',' | '.' | '!' | '?' | ':' | ';' | ')')
                    // A dotfile or extension keeps its space: "in .env", "a .md file".
                    || c == '.' && text[i + 1..].starts_with(char::is_alphanumeric))
            {
                out.push(' ');
            }
            out.push(c);
            space = false;
        }
    }
    out.trim().to_owned()
}
/// `terminal`: the target is a terminal, where a newline can run a command.
pub fn before_cleanup(text: &str, config: &Config, terminal: bool) -> String {
    let mut text = text.trim().to_owned();
    if config.formatting.spoken_punctuation {
        text = replace(
            &text,
            &[
                ("new paragraph", "\n\n"),
                ("new line", "\n"),
                ("question mark", "?"),
                ("exclamation mark", "!"),
                ("full stop", "."),
                ("comma", ","),
                ("period", "."),
                ("semicolon", ";"),
                ("colon", ":"),
            ],
            false,
        );
    }
    if config.formatting.remove_fillers {
        let pairs: Vec<_> = config
            .formatting
            .fillers
            .iter()
            .map(|s| (s.as_str(), ""))
            .collect();
        text = replace(&text, &pairs, false);
    }
    text = tidy(&text);
    text = crate::code::apply(&text, config, |s| {
        crate::cues::apply(&crate::snap::apply(s, config, terminal), config, terminal)
    });
    let pairs: Vec<_> = config
        .dictionary
        .replacements
        .iter()
        .map(|r| (r.from.as_str(), r.to.as_str()))
        .collect();
    // Tidy before replacement: dictionary values, like snippets, stay literal.
    let text = replace(&text, &pairs, false);
    // After replacements, so list items carry the corrected terms.
    if config.formatting.lists && !terminal {
        crate::lists::format(&text, &config.dictionary).unwrap_or(text)
    } else {
        text
    }
}
pub fn after_cleanup(text: &str, config: &Config) -> String {
    let pairs: Vec<_> = config
        .snippets
        .iter()
        .map(|s| (s.trigger.as_str(), s.text.as_str()))
        .collect();
    let mut text = replace(text.trim(), &pairs, true);
    if config.injection.trailing_space && !text.is_empty() && !text.ends_with(char::is_whitespace) {
        text.push(' ');
    }
    text
}
/// Scripts written without spaces between words (CJK, kana, fullwidth forms, Thai).
fn unspaced(c: char) -> bool {
    matches!(c as u32, 0x0e00..=0x0e7f | 0x3000..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0xff00..=0xffef | 0x20000..=0x3ffff)
}
fn closing(c: char) -> bool {
    matches!(
        c,
        ',' | '.' | '!' | '?' | ';' | ':' | ')' | ']' | '}' | '…' | '”' | '’' | '%'
    )
}
/// A period after these doesn't end a sentence.
const ABBREVIATIONS: &[&str] = &[
    "approx", "ave", "capt", "co", "col", "dept", "dr", "etc", "fig", "gen", "gov", "inc", "jr",
    "lt", "ltd", "mr", "mrs", "ms", "mt", "prof", "rev", "sgt", "sr", "st", "vs",
];
/// Function words, which never start a name, so mid-sentence they are lowercased.
/// SCOWL's common list can't decide this: "john", "notes", "ok" and "smith" are on it.
const CONTINUING: &[&str] = &[
    "a",
    "about",
    "after",
    "again",
    "all",
    "also",
    "although",
    "always",
    "am",
    "an",
    "and",
    "another",
    "any",
    "anyone",
    "anything",
    "are",
    "aren't",
    "around",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "between",
    "both",
    "but",
    "by",
    "can't",
    "could",
    "couldn't",
    "did",
    "didn't",
    "do",
    "does",
    "doesn't",
    "don't",
    "during",
    "each",
    "either",
    "even",
    "every",
    "everyone",
    "everything",
    "for",
    "from",
    "had",
    "has",
    "have",
    "he",
    "he's",
    "her",
    "here",
    "here's",
    "him",
    "his",
    "how",
    "if",
    "in",
    "into",
    "is",
    "isn't",
    "it",
    "it's",
    "its",
    "just",
    "let's",
    "maybe",
    "me",
    "might",
    "more",
    "most",
    "much",
    "must",
    "my",
    "neither",
    "never",
    "no",
    "nor",
    "not",
    "nothing",
    "now",
    "of",
    "off",
    "often",
    "on",
    "once",
    "only",
    "onto",
    "or",
    "our",
    "out",
    "over",
    "perhaps",
    "probably",
    "rather",
    "really",
    "she",
    "she's",
    "should",
    "shouldn't",
    "since",
    "so",
    "some",
    "someone",
    "something",
    "sometimes",
    "still",
    "such",
    "than",
    "that",
    "that's",
    "the",
    "their",
    "them",
    "then",
    "there",
    "there's",
    "these",
    "they",
    "they're",
    "this",
    "those",
    "though",
    "through",
    "to",
    "too",
    "under",
    "unless",
    "until",
    "up",
    "us",
    "very",
    "was",
    "wasn't",
    "we",
    "we're",
    "we've",
    "were",
    "weren't",
    "what",
    "what's",
    "when",
    "where",
    "whether",
    "which",
    "while",
    "who",
    "whom",
    "whose",
    "why",
    "with",
    "within",
    "without",
    "won't",
    "would",
    "wouldn't",
    "yet",
    "you",
    "you're",
    "your",
];
/// Whether text typed after `prev`, the last character of `before`, needs a space.
fn space_after(prev: char, before: &str) -> bool {
    match prev {
        '"' | '\'' => {
            // A straight quote opens when nothing or a space/bracket precedes it.
            let mut chars = before.chars().rev().skip(1);
            chars
                .next()
                .is_some_and(|c| !c.is_whitespace() && !matches!(c, '(' | '[' | '{'))
        }
        '(' | '[' | '{' | '“' | '‘' | '«' | '¿' | '¡' | '/' | '@' | '#' | '$' | '_' | '\\'
        | '~' => false,
        c => !c.is_whitespace() && !unspaced(c),
    }
}
/// Spacing and first-letter case for `text` pasted between `before` and `after`,
/// the field's text around the caret (only the nearest characters matter).
/// Words in `keep` (dictionary words) never change case. Neither side is ever
/// logged or uploaded.
pub fn adjust(before: &str, after: &str, text: &str, keep: &[String]) -> String {
    // A trailing space came from `injection.trailing_space`; the neighbours decide it here.
    let body = text.trim_end_matches(' ');
    let (prev, next) = (before.chars().next_back(), after.chars().next());
    let Some(first) = body.chars().next() else {
        return text.to_owned();
    };
    // Inside a word: insert exactly what was said.
    let latin = |c: char| word_char(c) && !unspaced(c);
    if prev.is_some_and(latin) && next.is_some_and(latin) {
        return body.to_owned();
    }
    // Next to CJK, spacing stays as dictated: those scripts mix with Latin either way.
    let cjk = prev.is_some_and(unspaced) || next.is_some_and(unspaced);
    let mut out = String::with_capacity(text.len() + 2);
    if !cjk
        && prev.is_some_and(|p| space_after(p, before))
        && !first.is_whitespace()
        && !closing(first)
        && !unspaced(first)
    {
        out.push(' ');
    }
    let line_break = |c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    let trimmed = before.trim_end_matches(|c: char| c.is_whitespace() && !line_break(c));
    // Some(true): a sentence starts here; Some(false): one continues; None: leave the case.
    let capitalize = match trimmed.chars().next_back() {
        None | Some('!' | '?' | '。' | '！' | '？') => Some(true),
        Some(c) if line_break(c) => Some(true),
        // "done. ", "don't. " and "done... " end a sentence; "Dr. ", "e.g. ",
        // "example.com" and "v1." don't.
        Some('.' | '…') => (trimmed.len() < before.len() && {
            let token = trimmed
                .trim_end_matches(['.', '…'])
                .rsplit(char::is_whitespace)
                .next()
                .unwrap_or_default();
            token.chars().filter(|c| c.is_alphabetic()).count() >= 2
                && token
                    .chars()
                    .all(|c| c.is_alphabetic() || matches!(c, '\'' | '’'))
                && !ABBREVIATIONS.contains(&token.to_lowercase().as_str())
        })
        .then_some(true),
        Some(c) if unspaced(c) => None,
        Some(c) if word_char(c) || matches!(c, ',' | ';') => Some(false),
        _ => None,
    };
    // Only plain words change case: never ALLCAPS, camelCase, file names or dictionary words.
    let end = body.find(char::is_whitespace).unwrap_or(body.len());
    let word = body[..end].trim_end_matches([',', '.', '!', '?', ';', ':']);
    let plain = word
        .chars()
        .skip(1)
        .all(|c| c.is_lowercase() || matches!(c, '\'' | '’'))
        && !keep.iter().any(|k| k == word);
    let head = match capitalize {
        Some(true) if plain && first.is_lowercase() => Some(first.to_uppercase().to_string()),
        // Lowercased only when it can't be a name, so "John", "Notes" and "Monday" stay.
        Some(false)
            if plain
                && first.is_uppercase()
                && CONTINUING.contains(&word.to_lowercase().replace('’', "'").as_str()) =>
        {
            Some(first.to_lowercase().to_string())
        }
        _ => None,
    };
    match head {
        Some(head) => {
            out.push_str(&head);
            out.push_str(&body[first.len_utf8()..]);
        }
        None => out.push_str(body),
    }
    match next {
        Some(c) if !cjk && word_char(c) && !out.ends_with(unspaced) => out.push(' '),
        // Whitespace or punctuation follows; field end and CJK keep `trailing_space`.
        Some(_) if !cjk => {}
        _ => out.push_str(&text[body.len()..]),
    }
    out
}
pub fn style<'a>(config: &'a Config, app_id: Option<&str>) -> Option<&'a Style> {
    let app = app_id?.to_lowercase();
    config.styles.iter().find(|s| {
        s.apps
            .iter()
            .any(|pattern| app.contains(&pattern.to_lowercase()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Replacement, Snippet};
    #[test]
    fn fillers_boundaries_unicode_and_punctuation() {
        let c = Config::default();
        for (input, expected) in [
            ("Um, hello uh world.", "hello world."),
            ("human umbrella hmm", "human umbrella"),
            ("um's word", "um's word"),
            ("éum um中文", "éum um中文"),
            ("um\nhello", "hello"),
            ("hello, um, world", "hello, world"),
        ] {
            assert_eq!(before_cleanup(input, &c, false), expected, "{input}");
        }
    }
    #[test]
    fn longest_nonrecursive_dictionary_and_unicode() {
        let mut c = Config::default();
        c.dictionary.replacements = [
            ("post gres", "Postgres"),
            ("post", "wrong"),
            ("Postgres", "recursive"),
            ("ÉCOLE", "School"),
            ("café", "Coffee"),
        ]
        .into_iter()
        .map(|(from, to)| Replacement {
            from: from.into(),
            to: to.into(),
        })
        .collect();
        assert_eq!(
            before_cleanup("POST GRES école café cafétéria", &c, false),
            "Postgres School Coffee cafétéria"
        );
    }
    #[test]
    fn punctuation_is_opt_in_and_boundaries_hold() {
        let mut c = Config::default();
        assert_eq!(
            before_cleanup("hello comma world new line next question mark", &c, false),
            "hello comma world new line next question mark"
        );
        c.formatting.spoken_punctuation = true;
        assert_eq!(
            before_cleanup("hello comma world new line next question mark", &c, false),
            "hello, world\nnext?"
        );
        assert_eq!(
            before_cleanup("commander periodical", &c, false),
            "commander periodical"
        );
    }
    #[test]
    fn snippets_are_literal_and_not_recursive() {
        let mut c = Config {
            snippets: vec![Snippet {
                trigger: "my signature".into(),
                text: "my signature\n  Cheers, Éva".into(),
            }],
            ..Config::default()
        };
        assert_eq!(
            after_cleanup("MY, SIGNATURE!", &c),
            "my signature\n  Cheers, Éva! "
        );
        c.injection.trailing_space = false;
        assert_eq!(
            after_cleanup("signatured my signature", &c),
            "signatured my signature\n  Cheers, Éva"
        );
    }
    #[test]
    fn empty_text_never_becomes_a_space() {
        assert_eq!(after_cleanup("   ", &Config::default()), "");
        assert_eq!(before_cleanup("um uh", &Config::default(), false), "");
    }
    #[test]
    fn configurable_phrase_fillers_and_opt_out() {
        let mut c = Config::default();
        c.formatting.fillers = vec!["you know".into(), "um".into()];
        assert_eq!(
            before_cleanup("YOU KNOW, hello um, world", &c, false),
            "hello world"
        );
        c.formatting.remove_fillers = false;
        assert_eq!(before_cleanup("um, you know", &c, false), "um, you know");
    }
    #[test]
    fn combining_marks_stay_inside_word_boundaries() {
        let mut c = Config::default();
        c.dictionary.replacements = vec![Replacement {
            from: "cafe".into(),
            to: "coffee".into(),
        }];
        assert_eq!(
            before_cleanup("cafe\u{301} cafe", &c, false),
            "cafe\u{301} coffee"
        );
    }
    #[test]
    fn adjust_spacing_and_case_to_the_caret() {
        let keep = ["Postgres".to_owned(), "xflow".to_owned()];
        for (before, after, text, expected) in [
            // Mid-sentence: space, function words lowercase; field end keeps the trailing space.
            ("I think", "", "That we should. ", " that we should. "),
            ("hello", "", "There it is", " there it is"),
            ("a list,", "", "Then more", " then more"),
            ("so", "", "A cat", " a cat"),
            ("and", "", "Don't stop", " don't stop"),
            ("and", "", "Don’t stop", " don’t stop"),
            // Anything that could be a name keeps its case (review probes).
            ("hello", "", "World", " World"),
            ("met", "", "John ", " John "),
            ("met", "", "John's idea", " John's idea"),
            ("on", "", "Monday we", " Monday we"),
            ("open", "", "Notes app", " Notes app"),
            ("and", "", "Ok fine", " Ok fine"),
            ("met ", " today", "Sarah ", "Sarah"),
            ("ok", "🎉", "Fine", " Fine"),
            // Sentence and field starts capitalize.
            ("It's done. ", "", "hello there", "Hello there"),
            ("Really?  ", "", "yes", "Yes"),
            ("Really?", "", "yes", " Yes"),
            ("Fertig. ", "", "über", "Über"),
            ("", "", "hello", "Hello"),
            ("first line\n", "", "hello", "Hello"),
            ("first line\n  ", "", "hello", "Hello"),
            // A period ends a sentence only after a word, before a space, and not
            // after an abbreviation (review probes); otherwise only spacing changes.
            ("It's done.", "", "hello there", " hello there"),
            ("See e.g.", "", "the docs", " the docs"),
            ("See e.g. ", "", "the docs", "the docs"),
            ("Ask Dr.", "", "who called", " who called"),
            ("Ask Dr. ", "", "who called", "who called"),
            ("Mr.", "", "smith", " smith"),
            ("the U.S. ", "", "army", "army"),
            ("Version 2. ", "", "next", "next"),
            ("example.", "", "com", " com"),
            ("file.", "", "txt", " txt"),
            // A trailing period is never deleted, and text is never emptied.
            ("ask", " smith", "Dr. ", " Dr."),
            ("see", " the", "e.g. ", " e.g."),
            ("the", " army", "U.S. ", " U.S."),
            ("hello", " world", ". ", "."),
            ("I think", " we should", "That.", " that."),
            ("I think ", "we should", "That", "that "),
            // After ':' the case stays; a space is still due.
            ("Note:", "", "Remember this", " Remember this"),
            // Brackets and quotes.
            ("call (", "", "Maybe later", "Maybe later"),
            ("he said \"", "", "Wait", "Wait"),
            ("he said “", "", "Wait", "Wait"),
            ("he said \"stop\"", "", "Then", " Then"),
            // Mid-word: inserted exactly, no spaces.
            ("hel", "lo", "Xyz ", "Xyz"),
            // Punctuation after the caret: no space of our own.
            ("I think", ".", "So ", " so"),
            ("see (", ")", "Here ", "Here"),
            // ALLCAPS, "I", camelCase, file names and dictionary words keep their case.
            ("we use", "", "Postgres now", " Postgres now"),
            ("and", "", "NASA launched", " NASA launched"),
            ("and", "", "I'm here", " I'm here"),
            ("and", "", "I said", " I said"),
            ("open", "", "README.md", " README.md"),
            ("", "", "xflow rocks", "xflow rocks"),
            ("", "", "userId is", "userId is"),
            ("", "", "main.rs", "main.rs"),
            // "No." is not an abbreviation; contractions and ellipses end sentences.
            ("No. ", "", "we left ", "We left "),
            ("Don't. ", "", "hello ", "Hello "),
            ("done... ", "", "hello ", "Hello "),
            ("done… ", "", "hello", "Hello"),
            ("etc... ", "", "and more", "and more"),
            ("Wait...", "", "and more", " and more"),
            // CJK: no spaces, no case changes after ideographs.
            ("我们使用", "", "数据库", "数据库"),
            ("我们使用", "", "Postgres", "Postgres"),
            ("完成。", "", "hello", "Hello"),
            ("and", "", "数据库", "数据库"),
            // A CJK neighbour leaves spacing as dictated.
            ("使用", "Postgres", "和 ", "和 "),
            ("hello", "数据", "World ", "World "),
            ("使用", " Postgres", "和", "和"),
            // Emoji and surrogate pairs on either side.
            ("Great 🎉", "", "Thanks", " Thanks"),
            ("😀", "😀", "hi", " hi"),
            // Other scripts keep their case mid-sentence.
            ("Привет,", "", "Мир", " Мир"),
            // Leading punctuation never gets a space.
            ("hello", "", ", world", ", world"),
            ("", "", "", ""),
        ] {
            assert_eq!(
                adjust(before, after, text, &keep),
                expected,
                "{before:?} | {after:?} <- {text:?}"
            );
        }
    }
    #[test]
    fn first_app_style_wins() {
        let c = Config {
            styles: vec![
                Style {
                    name: "first".into(),
                    apps: vec!["editor".into()],
                    mode: None,
                    prompt: None,
                },
                Style {
                    name: "second".into(),
                    apps: vec!["editor".into()],
                    mode: None,
                    prompt: None,
                },
            ],
            ..Config::default()
        };
        assert_eq!(style(&c, Some("Org.EDITOR")).unwrap().name, "first");
        assert!(style(&c, None).is_none());
    }
}
