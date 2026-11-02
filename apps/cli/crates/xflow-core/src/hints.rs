//! Hint-only domain lexicon: public product names and terms, one canonical
//! spelling per line, ranked by importance. Sent to the STT provider after the
//! user's own words (`stt.domain_hints`); never used to rewrite text.
use crate::config::Config;

/// Most shipped terms one dictation offers; no provider takes more (AssemblyAI
/// and Mistral cap hints at 100), so a longer domain list could never be sent.
/// Providers cut further: about 20 fill the 100-token share of a Whisper prompt,
/// and keyword protocols take 30 until their limits are verified.
pub const MAX: usize = 100;

/// [dev, corp, sheet, finance], the same order as the text packs (`cues`).
const DOMAINS: [&str; 4] = [
    include_str!("hints/dev.txt"),
    include_str!("hints/corp.txt"),
    include_str!("hints/sheet.txt"),
    include_str!("hints/finance.txt"),
];

/// Shipped terms for one dictation, best first: the focused app's domains take
/// up to two thirds of `MAX`, the other `[vocab]` domains the rest, each group
/// interleaved by rank. The app turns domains on exactly as for the text packs.
/// Empty with `stt.domain_hints` off.
pub fn select(config: &Config, terminal: bool, app: Option<&str>) -> Vec<&'static str> {
    if !config.stt.domain_hints {
        return vec![];
    }
    let v = &config.vocab;
    let (on, focus) = (
        [v.dev, v.corp, v.sheet, v.finance],
        crate::cues::focused(terminal, app),
    );
    let (focus, rest): (Vec<usize>, Vec<usize>) = (0..4)
        .filter(|&i| on[i] || focus[i])
        .partition(|&i| focus[i]);
    let share = if rest.is_empty() { MAX } else { MAX * 2 / 3 };
    let mut out: Vec<_> = interleave(&focus).take(share).collect();
    out.extend(interleave(&rest).take(MAX - out.len()));
    out
}

/// Round robin over the domains' lines, best first. No parsed copy: walking
/// a few KB of `include_str!` per dictation costs microseconds and no heap.
fn interleave(domains: &[usize]) -> impl Iterator<Item = &'static str> {
    let mut lines: Vec<_> = domains.iter().map(|&d| DOMAINS[d].lines()).collect();
    let (mut next, mut dry) = (0, 0);
    std::iter::from_fn(move || {
        let len = lines.len();
        while dry < len {
            let term = lines[next % len].next();
            next += 1;
            match term {
                Some(term) => {
                    dry = 0;
                    return Some(term);
                }
                None => dry += 1,
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms() -> [Vec<&'static str>; 4] {
        DOMAINS.map(|data| data.lines().collect())
    }
    use std::collections::HashSet;

    fn domain(term: &str) -> usize {
        (0..4).find(|&d| terms()[d].contains(&term)).unwrap()
    }

    #[test]
    fn lexicon_is_small_unique_plain_and_never_ordinary_english() {
        let mut seen = HashSet::new();
        let mut bytes = 0;
        for (d, list) in terms().iter().enumerate() {
            assert!(list.len() <= MAX, "domain {d} has unreachable terms");
            for &term in list {
                bytes += term.len() + 1;
                assert!(seen.insert(term.to_lowercase()), "duplicate {term}");
                assert!(
                    !term.is_empty()
                        && term.len() <= 24
                        && term.trim() == term
                        && term.split(' ').count() <= 3
                        && term.chars().all(|c| c.is_ascii_graphic() || c == ' ')
                        // `, ` joins the Whisper prompt; OpenAI rejects <, >.
                        && !term.contains([',', '<', '>', '"']),
                    "{term:?}"
                );
                // Transcript-like terms only: no sentences or instructions.
                assert!(!term.ends_with(['.', ':', '!', '?']), "{term}");
                assert!(
                    !term.split(' ').any(|w| matches!(
                        w.to_lowercase().as_str(),
                        "spell"
                            | "write"
                            | "use"
                            | "not"
                            | "never"
                            | "always"
                            | "instead"
                            | "ignore"
                            | "transcribe"
                            | "glossary"
                            | "terms"
                    )),
                    "{term}"
                );
                // A one-word hint biases casing too: everyday English stays out
                // unless camelCase sets it apart (TypeScript).
                let camel = term
                    .as_bytes()
                    .windows(2)
                    .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
                assert!(
                    term.contains(' ') || camel || !crate::words::common(&term.to_lowercase()),
                    "{term} is everyday English"
                );
            }
        }
        assert!(bytes < 4096, "{bytes} bytes");
    }

    #[test]
    fn defaults_hint_dev_and_corp_and_the_focused_app_first() {
        let config = Config::default();
        // Mail, or focus not known yet: dev and corp interleaved; no sheet, no finance.
        let mail = select(&config, false, Some("com.apple.mail"));
        assert_eq!(
            mail[..4],
            ["Claude", "Salesforce", "Claude Code", "HubSpot"]
        );
        assert_eq!(select(&config, false, None), mail);
        assert!(mail.iter().all(|t| matches!(domain(t), 0 | 1)));
        // Terminals and editors: dev first.
        for (terminal, app) in [
            (true, Some("com.apple.Terminal")),
            (false, Some("com.microsoft.VSCode")),
            (false, Some("code.desktop")),
        ] {
            let got = select(&config, terminal, app);
            assert!(got[..MAX * 2 / 3].iter().all(|t| domain(t) == 0), "{app:?}");
            assert!(got[MAX * 2 / 3..].iter().all(|t| domain(t) == 1), "{app:?}");
            assert_eq!(got[0], "Claude");
        }
        // Spreadsheets: every sheet term first, then dev and corp; finance stays off.
        for app in [
            "com.microsoft.Excel",
            "com.apple.iWork.Numbers",
            "libreoffice-calc",
        ] {
            let got = select(&config, false, Some(app));
            let sheet = terms()[2].len();
            assert_eq!(got[..sheet], terms()[2][..], "{app}");
            assert!(got[sheet..].iter().all(|t| matches!(domain(t), 0 | 1)));
            assert!(got.contains(&"XLOOKUP") && !got.contains(&"EBITDA"));
        }
        for app in [None, Some("com.apple.mail"), Some("com.microsoft.Excel")] {
            let got = select(&config, true, app);
            assert_eq!(got.len(), MAX);
            assert_eq!(got.iter().collect::<HashSet<_>>().len(), MAX);
        }
    }

    #[test]
    fn config_turns_domains_and_hints_on_and_off() {
        let mut config = Config::default();
        config.vocab.finance = true;
        assert!(select(&config, false, None)[..3].contains(&"EBITDA"));
        config.vocab = crate::config::VocabConfig {
            dev: false,
            corp: false,
            sheet: false,
            finance: false,
        };
        assert!(select(&config, false, None).is_empty());
        // The focused app still turns its own domain on.
        assert_eq!(select(&config, true, None), terms()[0]);
        config.stt.domain_hints = false;
        for terminal in [false, true] {
            assert!(select(&config, terminal, Some("com.microsoft.Excel")).is_empty());
        }
    }
}
