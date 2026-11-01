//! Small, exact heard forms with local context; never part of snap's fuzzy index.
use crate::config::Config;
use std::sync::OnceLock;

const DATA: &str = include_str!("cues.txt");

struct Cue {
    term: &'static str,
    heard: &'static str,
    left: &'static str,
    right: &'static str,
    block_left: &'static str,
    block_right: &'static str,
    after_left: &'static str,
}

fn entries() -> &'static [Cue] {
    static CUES: OnceLock<Vec<Cue>> = OnceLock::new();
    CUES.get_or_init(|| {
        let mut cues = Vec::new();
        for line in DATA
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let fields: Vec<_> = line
                .split('\t')
                .map(|field| if field == "-" { "" } else { field })
                .collect();
            for heard in fields[1].split('|') {
                cues.push(Cue {
                    term: fields[0],
                    heard,
                    left: fields[2],
                    right: fields[3],
                    block_left: fields[4],
                    block_right: fields[5],
                    after_left: fields[6],
                });
            }
        }
        cues.sort_by_key(|c| std::cmp::Reverse(c.heard.len()));
        cues
    })
}

// Conservatively protect a whole user phrase when a shorter shipped form overlaps it.
fn overlaps(a: &str, b: &str) -> bool {
    a.split_whitespace()
        .any(|a| b.split_whitespace().any(|b| a.eq_ignore_ascii_case(b)))
}

struct Token<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    eligible: bool,
}

fn gap(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| matches!(c, ' ' | '\t'))
}

fn neighbour(text: &str, tokens: &[Token<'_>], i: usize, patterns: &str, left: bool) -> bool {
    if i == 0 || i == tokens.len() || !gap(&text[tokens[i - 1].end..tokens[i].start]) {
        return false;
    }
    patterns.split('|').filter(|s| !s.is_empty()).any(|p| {
        let count = p.split_whitespace().count();
        let start = if left {
            let Some(start) = i.checked_sub(count) else {
                return false;
            };
            start
        } else {
            i
        };
        let end = start + count;
        end <= tokens.len() && matches(text, &tokens[start..end], p)
    })
}

fn matches(text: &str, tokens: &[Token<'_>], phrase: &str) -> bool {
    tokens.iter().all(|t| t.eligible)
        && tokens.iter().map(|t| t.text).eq(phrase.split_whitespace())
        && tokens.windows(2).all(|w| gap(&text[w[0].end..w[1].start]))
}

/// Blocked terms and explicit user spellings always outrank shipped corrections.
pub(crate) fn apply(text: &str, config: &Config, terminal: bool) -> String {
    if !config.formatting.snap_to_dictionary {
        return text.to_owned();
    }
    let lower = text.to_ascii_lowercase();
    let tokens: Vec<_> = lower
        .split_whitespace()
        .map(|chunk| {
            let word = chunk.trim_matches(|c: char| !c.is_alphanumeric());
            let start = word.as_ptr() as usize - lower.as_ptr() as usize;
            Token {
                text: word,
                start,
                end: start + word.len(),
                eligible: !word.is_empty()
                    && !chunk.contains(['@', '/', '\\', '_', '`', '-'])
                    && !chunk.starts_with("www.")
                    && !crate::code::literal(chunk),
            }
        })
        .collect();
    let (mut cursor, mut i) = (0, 0);
    let mut out = String::with_capacity(text.len());
    while i < tokens.len() {
        if !tokens[i].eligible {
            i += 1;
            continue;
        }
        let found = entries().iter().find_map(|cue| {
            if !cue.heard.starts_with(tokens[i].text) {
                return None;
            }
            let count = cue.heard.split_whitespace().count();
            let end = i + count;
            if end > tokens.len() || !matches(&lower, &tokens[i..end], cue.heard) {
                return None;
            }
            Some((cue, end))
        });
        // A blocked longer phrase must not fall back to a partial correction.
        let found = found.and_then(|(cue, end)| {
            let after = lower[tokens[end - 1].end..].trim_start();
            let boundary = after.is_empty() || after.starts_with([',', '.', ':', ';', '!', '?']);
            let left = neighbour(&lower, &tokens, i, cue.left, true)
                && (cue.after_left.is_empty()
                    || boundary
                    || neighbour(&lower, &tokens, end, cue.after_left, false));
            if neighbour(&lower, &tokens, i, cue.block_left, true)
                || neighbour(&lower, &tokens, end, cue.block_right, false)
                || !(left || neighbour(&lower, &tokens, end, cue.right, false))
            {
                return None;
            }
            let input = &text[tokens[i].start..tokens[end - 1].end];
            let target = config
                .dictionary
                .words
                .iter()
                .chain(&config.stt.vocabulary)
                .find(|w| w.eq_ignore_ascii_case(cue.term))
                .map_or(cue.term, String::as_str);
            if terminal && input.eq_ignore_ascii_case(target)
                || config
                    .dictionary
                    .never_learn
                    .iter()
                    .any(|w| overlaps(cue.term, w) || overlaps(input, w))
                || config
                    .dictionary
                    .words
                    .iter()
                    .chain(&config.stt.vocabulary)
                    .any(|w| overlaps(input, w))
                || config
                    .dictionary
                    .replacements
                    .iter()
                    .any(|r| overlaps(input, &r.from))
            {
                return None;
            }
            Some((end, target))
        });
        if let Some((end, term)) = found {
            out.push_str(&text[cursor..tokens[i].start]);
            let input = &text[tokens[i].start..tokens[end - 1].end];
            if input.chars().any(char::is_uppercase) && !input.chars().any(char::is_lowercase) {
                out.push_str(&term.to_ascii_uppercase());
            } else {
                out.push_str(term);
            }
            cursor = tokens[end - 1].end;
            i = end;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Replacement, text::before_cleanup};

    #[test]
    fn dev_terms_need_neighbouring_evidence() {
        let config = Config::default();
        let cases = [
            ("ask cloud to review", "ask Claude to review"),
            ("hey cloud code", "hey Claude Code"),
            ("tell cloud to explain", "tell Claude to explain"),
            ("cloud opus", "Claude opus"),
            ("cloud sonnet", "Claude sonnet"),
            ("codecs agent", "Codex agent"),
            ("cursor editor", "Cursor editor"),
            ("cube cuddle get pods", "kubectl get pods"),
            ("cube control apply", "kubectl apply"),
            ("post gress database", "Postgres database"),
            ("post gress sql server", "PostgreSQL server"),
            ("next JS app", "Next.js app"),
            ("versal deployment", "Vercel deployment"),
            ("super base auth", "Supabase auth"),
            ("n p m install", "npm install"),
            ("p n p m install", "pnpm install"),
            ("git hub pull request", "GitHub pull request"),
            ("git lab merge request", "GitLab merge request"),
            ("oh auth token", "OAuth token"),
            ("jason payload", "JSON payload"),
            ("yammel config", "YAML config"),
            ("engine x proxy", "nginx proxy"),
            ("sequel lite database", "SQLite database"),
            ("reddis cache", "Redis cache"),
            ("kafka topic", "Kafka topic"),
            ("terra form plan", "Terraform plan"),
            ("anthro pic api", "Anthropic api"),
            ("open ai sdk", "OpenAI sdk"),
            ("gemini model", "Gemini model"),
            ("llama weights", "Llama weights"),
            ("oh llama serve", "Ollama serve"),
            ("hugging face hub", "Hugging Face hub"),
            ("pie torch tensor", "PyTorch tensor"),
            ("type script compiler", "TypeScript compiler"),
            ("tail wind css", "Tailwind css"),
            ("shad cn components", "shadcn components"),
            ("t r p c router", "tRPC router"),
            ("g r p c service", "gRPC service"),
            ("web socket connection", "WebSocket connection"),
            ("dogger container", "Docker container"),
            ("use snake case cloud code", "use cloud_code"),
        ];
        for (input, expected) in cases {
            assert_eq!(before_cleanup(input, &config, false), expected, "{input}");
        }
        // Every shipped alias needs a cue, even uncommon spellings.
        for cue in entries() {
            assert_eq!(apply(cue.heard, &config, false), cue.heard, "{}", cue.heard);
        }
        for input in [
            "cloud storage",
            "the cloud",
            "google cloud code",
            "deploy to the cloud in Cursor",
            "cloud",
            "ask cloud storage",
            "hey google cloud code",
            "hey cloud code storage",
            "audio codecs agent",
            "video codecs",
            "mouse cursor editor",
            "cursor position",
            "franz kafka topic",
            "kafka novel",
            "gemini zodiac",
            "llama farm",
            "dogger bank",
            "the dogger sailed",
            "so late",
            "we have a super base",
            "jason bourne",
            "a tail wind helps",
            "ask cloud.service to review",
            "ask /cloud to review",
            "ask cloud@example.com to review",
            "ask `cloud` to review",
            "ask cloud_name to review",
            "ask\ncloud to review",
            "ask. cloud to review",
            "next. js app",
            "post, gress database",
            "my web socket is broken",
            "pie torch café",
            "école cloud storage",
        ] {
            assert_eq!(before_cleanup(input, &config, false), input, "{input}");
        }
    }

    #[test]
    fn review_cloud_prose_is_unchanged() {
        eprintln!("cue tests source: {}", env!("CARGO_MANIFEST_DIR"));
        let config = Config::default();
        for input in [
            "ask cloud providers for a quote",
            "ask cloud vendors",
            "ask cloud support to help",
            "tell cloud engineers to scale",
            "ask cloud architects",
            "ask cloud native folks",
            "hey cloud team, any update",
            "the bug is in cloud code not client code",
            "try cloud code for vs code",
            "use cloud code to debug kubernetes",
            "we deploy with cloud code generation",
            "ask cloud code reviewers",
            "cloud code generation",
            "cloud code reviewers",
            "we need an open ai model that anyone can run",
            "an anthropic model of cosmology",
            "a codecs review",
        ] {
            assert_eq!(before_cleanup(input, &config, false), input, "{input}");
        }
    }

    #[test]
    fn addressing_cues_need_a_safe_continuation() {
        let config = Config::default();
        for (input, expected) in [
            ("ask cloud to review", "ask Claude to review"),
            ("ask cloud about this", "ask Claude about this"),
            ("ask cloud for help", "ask Claude for help"),
            ("ask cloud opus", "ask Claude opus"),
            ("hey cloud", "hey Claude"),
            ("hey cloud!", "hey Claude!"),
            ("hey cloud, review this", "hey Claude, review this"),
            ("hey cloud code", "hey Claude Code"),
            ("ask cloud code to review", "ask Claude Code to review"),
            ("tell cloud code about this", "tell Claude Code about this"),
            ("ask cloud code for help", "ask Claude Code for help"),
            ("hey cloud code!", "hey Claude Code!"),
        ] {
            assert_eq!(before_cleanup(input, &config, false), expected, "{input}");
        }
    }

    #[test]
    fn all_caps_corrections_keep_the_speakers_case() {
        let config = Config::default();
        for (input, expected) in [
            ("ASK CLOUD TO REVIEW", "ASK CLAUDE TO REVIEW"),
            ("HEY CLOUD CODE", "HEY CLAUDE CODE"),
        ] {
            assert_eq!(before_cleanup(input, &config, false), expected, "{input}");
        }
    }

    #[test]
    fn dictionary_phrases_protect_overlapping_cue_words() {
        let mut config = Config::default();
        for term in ["Google Cloud", "VS Code"] {
            config.dictionary.words = vec![term.into()];
            assert_eq!(
                before_cleanup("hey cloud code", &config, false),
                "hey cloud code"
            );
        }
    }

    #[test]
    fn punctuation_tokens_skip_the_cue_search() {
        let text = "... ".repeat(10_000);
        let start = std::time::Instant::now();
        assert_eq!(apply(&text, &Config::default(), false), text);
        let elapsed = start.elapsed();
        eprintln!("punctuation-only 40KB: {elapsed:?}");
        assert!(elapsed.as_millis() < if cfg!(debug_assertions) { 100 } else { 20 });
    }

    #[test]
    fn controls_and_user_spellings_win() {
        let mut config = Config::default();
        config.formatting.snap_to_dictionary = false;
        assert_eq!(
            before_cleanup("ask cloud to review", &config, false),
            "ask cloud to review"
        );
        config.formatting.snap_to_dictionary = true;
        for blocked in ["Claude", "cloud"] {
            config.dictionary.never_learn = vec![blocked.into()];
            for input in ["ask cloud to review", "hey cloud code"] {
                assert_eq!(before_cleanup(input, &config, false), input);
            }
        }
        config.dictionary.never_learn = vec!["Claude Code".into()];
        assert_eq!(
            before_cleanup("hey cloud code", &config, false),
            "hey cloud code"
        );
        config.dictionary.never_learn.clear();
        for spelling in ["cloud", "cloud code"] {
            config.dictionary.words = vec![spelling.into()];
            assert_eq!(
                before_cleanup("hey cloud code", &config, false),
                "hey cloud code"
            );
        }
        config.dictionary.words = vec!["cLaUdE".into()];
        assert_eq!(
            before_cleanup("ask claude to review", &config, false),
            "ask cLaUdE to review"
        );
        assert_eq!(
            before_cleanup("ask cloud to review", &config, false),
            "ask cLaUdE to review"
        );
        config.dictionary.words.clear();
        config.stt.vocabulary = vec!["cloud".into()];
        assert_eq!(
            before_cleanup("ask cloud to review", &config, false),
            "ask cloud to review"
        );
        config.stt.vocabulary.clear();
        config.dictionary.replacements = vec![Replacement {
            from: "Claude".into(),
            to: "Assistant".into(),
        }];
        assert_eq!(
            before_cleanup("ask cloud to review", &config, false),
            "ask Assistant to review"
        );
        config.dictionary.replacements = vec![Replacement {
            from: "cloud".into(),
            to: "AWS".into(),
        }];
        assert_eq!(
            before_cleanup("ask cloud to review", &config, false),
            "ask AWS to review"
        );
    }

    #[test]
    fn terminals_skip_only_case_changes() {
        let config = Config::default();
        for input in [
            "cursor editor",
            "docker compose",
            "json payload",
            "claude code",
        ] {
            assert_eq!(before_cleanup(input, &config, true), input);
        }
        assert_eq!(
            before_cleanup("ask cloud to review", &config, true),
            "ask Claude to review"
        );
        assert_eq!(
            before_cleanup("cube cuddle get pods", &config, true),
            "kubectl get pods"
        );
    }

    #[test]
    fn shipped_data_is_small_unique_and_gated() {
        use std::collections::BTreeSet;
        assert!(DATA.len() < 8 * 1024);
        let mut terms = BTreeSet::new();
        let mut forms = BTreeSet::new();
        for line in DATA
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let fields: Vec<_> = line
                .split('\t')
                .map(|field| if field == "-" { "" } else { field })
                .collect();
            assert_eq!(fields.len(), 7, "{line}");
            assert!(terms.insert(fields[0].to_ascii_lowercase()));
            assert!(!fields[2].is_empty() || !fields[3].is_empty(), "{line}");
            for form in fields[1].split('|') {
                assert!(!form.is_empty() && form.is_ascii());
                assert_eq!(form, form.to_ascii_lowercase());
                assert!(forms.insert(form), "duplicate: {form}");
            }
        }
        assert_eq!(terms.len(), 36);
        assert!(entries().len() < 100);
    }

    #[test]
    fn prose_control_is_unchanged() {
        let config = Config::default();
        for text in include_str!("snap-prose.txt").lines() {
            assert_eq!(apply(text, &config, false), text);
        }
    }

    #[test]
    fn pipeline_latency() {
        use std::{
            hint::black_box,
            time::{Duration, Instant},
        };
        let config = Config::default();
        let text =
            "ask cloud to review the pull request with next js app and post gress database. "
                .repeat(26);
        for (label, text) in [
            ("short", "ask cloud to review the pull request"),
            ("2k", text.as_str()),
        ] {
            let mut samples = Vec::new();
            for _ in 0..200 {
                let start = Instant::now();
                black_box(before_cleanup(black_box(text), &config, false));
                samples.push(start.elapsed());
            }
            samples.sort_unstable();
            eprintln!(
                "before_cleanup {label}: p50 {:?}, p95 {:?}",
                samples[100], samples[190]
            );
            assert!(
                samples[100]
                    < if cfg!(debug_assertions) {
                        Duration::from_millis(100)
                    } else {
                        Duration::from_millis(2)
                    }
            );
        }
    }
}
