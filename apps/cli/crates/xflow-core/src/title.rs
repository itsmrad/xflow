//! File, project and branch names from an editor or terminal window title, for
//! one dictation's local snap pass (`privacy.window_terms`). Never stored, logged
//! or sent to a provider.
use crate::words::common;

/// Most terms one title contributes.
pub const MAX_TERMS: usize = 8;
/// Editor and shell names that fill titles but are never what the user says.
const CHROME: [&str; 13] = [
    "bash", "fish", "zsh", "sh", "ssh", "tmux", "xcode", "iterm2", "ghostty", "zed", "intellij",
    "idea", "untitled",
];

/// Identifier-shaped words first (`useAuth.ts`, `useAuth`, `title-hints`),
/// then other rare words such as a project name; never everyday words.
pub fn terms(title: &str) -> Vec<String> {
    let (mut shaped, mut plain) = (Vec::new(), Vec::new());
    let separators = |c: char| c.is_whitespace() || "—–|•●:()[]{}<>\"'`,;*!?#$%&=+".contains(c);
    for chunk in title.split(separators) {
        // user@host and emails are not names the user dictates.
        if chunk.contains('@') {
            continue;
        }
        // A path or branch contributes its last two components.
        let parts: Vec<_> = chunk.split(['/', '\\']).collect();
        for part in &parts[parts.len().saturating_sub(2)..] {
            let part = part.trim_matches(|c: char| !c.is_alphanumeric());
            if part.chars().count() < 2
                || part.len() > 64
                || !part.chars().any(char::is_alphabetic)
                || part.contains(|c: char| !c.is_alphanumeric() && !matches!(c, '.' | '-' | '_'))
            {
                continue;
            }
            let camel = part
                .chars()
                .zip(part.chars().skip(1))
                .any(|(a, b)| a.is_lowercase() && b.is_uppercase());
            if camel || part.contains(['.', '-', '_']) {
                shaped.push(part.to_owned());
                // `useAuth.ts` is also said without its extension.
                if let Some((stem, ext)) = part.rsplit_once('.') {
                    if (1..=5).contains(&ext.len()) && stem.contains(char::is_alphabetic) {
                        shaped.push(stem.to_owned());
                    }
                }
            } else {
                plain.push(part.to_owned());
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    for term in shaped.into_iter().chain(plain) {
        let lower = term.to_lowercase();
        if out.len() == MAX_TERMS {
            break;
        }
        if !common(&lower)
            && !CHROME.contains(&lower.as_str())
            && !out.iter().any(|t| t.to_lowercase() == lower)
        {
            out.push(term);
        }
    }
    out
}

/// camelCase title names said as their parts: "use task store" -> `useTaskStore`.
/// Exact words and three or more humps only: no sound-alikes, case-only or partial
/// matches, so prose ("the release notes", "read me") never changes. Snake, kebab
/// and file names need "dot ext" (`code::known_file`).
pub fn apply(text: &str, config: &crate::config::Config) -> String {
    let humps: Vec<(Vec<String>, &str)> = (config.title_terms.iter())
        .filter(|_| config.formatting.snap_to_dictionary)
        .filter(|t| t.starts_with(|c: char| c.is_ascii_lowercase()))
        .filter(|t| t.chars().all(|c| c.is_ascii_alphabetic()))
        .filter_map(|t| {
            let mut parts = vec![String::new()];
            for c in t.chars() {
                if c.is_ascii_uppercase() {
                    parts.push(String::new());
                }
                parts.last_mut()?.push(c.to_ascii_lowercase());
            }
            (parts.len() >= 3).then_some((parts, t.as_str()))
        })
        .collect();
    if humps.is_empty() {
        return text.to_owned();
    }
    let w = crate::text::words(text);
    // Spoken words only: not part of a path, file name, flag or identifier.
    let edge = |c: Option<char>| c.is_none_or(|c| c.is_whitespace() || ",;:!?()\"'".contains(c));
    let (mut out, mut cursor, mut i) = (String::with_capacity(text.len()), 0, 0);
    while i < w.len() {
        let found = humps.iter().filter(|(parts, _)| {
            let Some(range) = w.get(i..i + parts.len()) else {
                return false;
            };
            let (start, end) = (range[0].start, range[parts.len() - 1].end);
            let mut after = text[end..].chars();
            range
                .iter()
                .zip(parts)
                .all(|(x, p)| x.text.eq_ignore_ascii_case(p))
                && range.windows(2).all(|p| {
                    let gap = &text[p[0].end..p[1].start];
                    !gap.is_empty() && gap.chars().all(|c| c == ' ' || c == '\t')
                })
                && edge(text[..start].chars().next_back())
                && match after.next() {
                    Some('.') => !after.next().is_some_and(char::is_alphanumeric),
                    c => edge(c),
                }
        });
        if let Some((parts, term)) = found.max_by_key(|(parts, _)| parts.len()) {
            out.push_str(&text[cursor..w[i].start]);
            out.push_str(term);
            cursor = w[i + parts.len() - 1].end;
            i += parts.len();
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

    #[test]
    fn real_title_formats() {
        for (title, expected) in [
            // VS Code and Cursor: file — folder (● marks unsaved).
            (
                "useTaskStore.ts — myapp",
                &["useTaskStore.ts", "useTaskStore", "myapp"][..],
            ),
            (
                "● useAuthSession.ts — xflow",
                &["useAuthSession.ts", "useAuthSession", "xflow"],
            ),
            (
                "snap.rs — xflow [SSH: devbox]",
                &["snap.rs", "xflow", "devbox"],
            ),
            // Zed: project — file.
            (
                "xflow — title_terms.rs",
                &["title_terms.rs", "title_terms", "xflow"],
            ),
            // Xcode and JetBrains.
            (
                "MyApp — ContentView.swift",
                &["MyApp", "ContentView.swift", "ContentView"],
            ),
            (
                "xflow – src/main/kotlin/UserService.kt",
                &["UserService.kt", "UserService", "xflow", "kotlin"],
            ),
            // Terminals: user@host, cwd, git branch, shell.
            ("mrad@mbp: ~/code/projects/xflow", &["xflow"]),
            (
                "xflow (itsmrad/title-hints-65) — -zsh — 120×40",
                &["title-hints-65", "xflow", "itsmrad"],
            ),
            ("✳ Fix login redirect", &[]),
            (
                "Terminal — vim README.md — 80×24",
                &["README.md", "README", "vim"],
            ),
        ] {
            assert_eq!(terms(title), expected, "{title}");
        }
    }

    #[test]
    fn everyday_words_are_dropped_and_terms_capped() {
        assert!(terms("Welcome — Visual Studio Code").is_empty());
        assert!(terms("Settings - Untitled").is_empty());
        let many = (0..20)
            .map(|i| format!("module_{i}.rs"))
            .collect::<Vec<_>>();
        let found = terms(&many.join(" "));
        assert_eq!(found.len(), MAX_TERMS);
        assert_eq!(found[..2], ["module_0.rs", "module_0"]);
        assert_eq!(terms("Foo.ts foo.ts FOO.ts"), ["Foo.ts", "Foo"]);
    }

    fn said(text: &str, title: &str) -> String {
        let config = crate::config::Config {
            title_terms: terms(title),
            ..Default::default()
        };
        crate::text::before_cleanup(text, &config, false)
    }

    #[test]
    fn spoken_forms_match_title_terms_exactly() {
        for (title, text, expected) in [
            (
                "useTaskStore.ts — myapp",
                "open use task store",
                "open useTaskStore",
            ),
            (
                "useTaskStore.ts — myapp",
                "Use task store, then save.",
                "useTaskStore, then save.",
            ),
            (
                "useAuthSession.ts — xflow",
                "open use auth session dot ts",
                "open useAuthSession.ts",
            ),
            (
                "user_service.py — api",
                "open user service dot py",
                "open user_service.py",
            ),
            (
                "release-notes.md — docs",
                "edit release notes dot md",
                "edit release-notes.md",
            ),
        ] {
            assert_eq!(said(text, title), expected, "{title}: {text}");
            assert_ne!(said(text, ""), expected, "{text}");
        }
        for (title, text) in [
            // A shorter open file never claims part of a longer spoken name.
            ("useAuth.ts — xflow", "open use auth session dot ts"),
            // Typed paths and files, and partial or reordered parts, are left alone.
            ("useTaskStore.ts", "see src/use task store"),
            ("useTaskStore.ts", "use task store.ts"),
            ("useTaskStore.ts", "use task"),
            ("useTaskStore.ts", "task store use"),
        ] {
            assert_eq!(said(text, title), said(text, ""), "{title}: {text}");
        }
    }

    /// The PR #115 review probe: everyday sentences under real-looking titles.
    const PROBE_TITLES: [&str; 16] = [
        "README.md — notes",
        "meeting-notes.md — docs",
        "grocery-list.txt — home",
        "xflow (new-login-flow) — -zsh",
        "Makefile — xflow",
        "App.tsx — weather-app",
        "useAuth.ts — myapp",
        "release-notes.md — xflow",
        "draft-letter.md — letters",
        "UserService.kt — user-service",
        "test-utils.ts — web",
        "WeatherApp.swift — WeatherApp",
        "CHANGELOG.md — change-log",
        "back-end — front-end",
        "NewLoginFlow.tsx — app",
        "useTaskStore.ts user_id getUserName — myapp",
    ];
    const PROBE_SENTENCES: [&str; 16] = [
        "can you read me the list again",
        "I took meeting notes",
        "add milk to the grocery list",
        "the new login flow is confusing",
        "make file copies",
        "the app crashed",
        "use auth tokens carefully",
        "read the release notes",
        "write a draft letter",
        "ask the user service team",
        "fix the test utils",
        "open the weather app",
        "update the change log",
        "the back end team and the front end team",
        "use the task store",
        "get the user name and user id",
    ];

    #[test]
    fn title_terms_never_change_prose() {
        let lines = include_str!("snap-prose.txt")
            .lines()
            .chain(PROBE_SENTENCES);
        let mut changed = Vec::new();
        for text in lines {
            for title in PROBE_TITLES {
                let out = said(text, title);
                if out != said(text, "") {
                    changed.push(format!("{title}: {text} -> {out}"));
                }
            }
        }
        assert!(changed.is_empty(), "{changed:#?}");
    }
}
