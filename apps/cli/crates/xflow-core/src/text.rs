//! Local corrections precede model cleanup; literal snippets follow it.
use crate::config::{Config, Style};

#[derive(Clone)]
struct Word<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}
fn word_char(c: char) -> bool {
    c.is_alphanumeric()
        || c == '_'
        || matches!(c as u32, 0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0xfe20..=0xfe2f)
}
fn words(text: &str) -> Vec<Word<'_>> {
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
    for c in text.chars() {
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
                && !matches!(c, ',' | '.' | '!' | '?' | ':' | ';' | ')')
            {
                out.push(' ');
            }
            out.push(c);
            space = false;
        }
    }
    out.trim().to_owned()
}
pub fn before_cleanup(text: &str, config: &Config) -> String {
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
    let pairs: Vec<_> = config
        .dictionary
        .replacements
        .iter()
        .map(|r| (r.from.as_str(), r.to.as_str()))
        .collect();
    // Tidy before replacement: dictionary values, like snippets, stay literal.
    replace(&text, &pairs, false)
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
            assert_eq!(before_cleanup(input, &c), expected, "{input}");
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
            before_cleanup("POST GRES école café cafétéria", &c),
            "Postgres School Coffee cafétéria"
        );
    }
    #[test]
    fn punctuation_is_opt_in_and_boundaries_hold() {
        let mut c = Config::default();
        assert_eq!(
            before_cleanup("hello comma world new line next question mark", &c),
            "hello comma world new line next question mark"
        );
        c.formatting.spoken_punctuation = true;
        assert_eq!(
            before_cleanup("hello comma world new line next question mark", &c),
            "hello, world\nnext?"
        );
        assert_eq!(
            before_cleanup("commander periodical", &c),
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
        assert_eq!(before_cleanup("um uh", &Config::default()), "");
    }
    #[test]
    fn configurable_phrase_fillers_and_opt_out() {
        let mut c = Config::default();
        c.formatting.fillers = vec!["you know".into(), "um".into()];
        assert_eq!(
            before_cleanup("YOU KNOW, hello um, world", &c),
            "hello world"
        );
        c.formatting.remove_fillers = false;
        assert_eq!(before_cleanup("um, you know", &c), "um, you know");
    }
    #[test]
    fn combining_marks_stay_inside_word_boundaries() {
        let mut c = Config::default();
        c.dictionary.replacements = vec![Replacement {
            from: "cafe".into(),
            to: "coffee".into(),
        }];
        assert_eq!(before_cleanup("cafe\u{301} cafe", &c), "cafe\u{301} coffee");
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
