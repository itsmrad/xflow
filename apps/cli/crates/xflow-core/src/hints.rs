//! Hint-only term lexicon: public product names and terms, one canonical
//! spelling per line, ranked within sections (`@aws dev`: section, then the
//! `[vocab]` group that turns it on). Built by `scripts/lexicon/build.py`.
//! Sent to the STT provider after the user's own words (`stt.domain_hints`);
//! never used to rewrite text.
//!
//! Each dictation picks at most `MAX` terms from local signals: the focused
//! app's groups, the sections the user's recent dictations, dictionary words
//! and window title point at, and terms the user recently dictated. The data
//! stays in the binary's read-only pages; the only heap is a lazily built
//! sorted index (8 bytes a term) for lookups.
use crate::config::Config;
use std::sync::OnceLock;

/// Most shipped terms one dictation offers; no provider takes more (AssemblyAI
/// and Mistral cap hints at 100), so a longer list could never be sent.
/// Providers cut further: about 20 fill the 100-token share of a Whisper prompt,
/// and keyword protocols take 30 until their limits are verified.
pub const MAX: usize = 100;
/// Recently dictated lexicon terms kept for hints (`Recent`).
pub const RECENT: usize = 24;
/// Recent terms offered first, before section heads.
const RECENT_FIRST: usize = MAX / 5;

const LEXICON: &str = include_str!("hints/lexicon.txt");
/// The `[vocab]` groups, in the same order as the text packs (`cues`).
const GROUPS: [&str; 4] = ["dev", "corp", "sheet", "finance"];

struct Section {
    name: &'static str,
    group: usize,
    /// Terms, one per line, best first.
    body: &'static str,
}

fn sections() -> &'static [Section] {
    static SECTIONS: OnceLock<Vec<Section>> = OnceLock::new();
    SECTIONS.get_or_init(|| {
        // (name, group, body start) per header, then each body ends at the next.
        let mut headers = Vec::new();
        let mut at = 0;
        for line in LEXICON.split_inclusive('\n') {
            at += line.len();
            if let Some(header) = line.strip_prefix('@') {
                let header = header.trim_end();
                let (name, group) = header.split_once(' ').unwrap_or((header, "dev"));
                let group = GROUPS.iter().position(|g| *g == group).unwrap_or(0);
                headers.push((name, group, at, at - line.len()));
            }
        }
        (0..headers.len())
            .map(|i| {
                let (name, group, start, _) = headers[i];
                let end = headers.get(i + 1).map_or(LEXICON.len(), |h| h.3);
                Section {
                    name,
                    group,
                    body: &LEXICON[start..end],
                }
            })
            .collect()
    })
}

/// (byte offset of a term in `LEXICON`, section), sorted by lowercase term.
fn index() -> &'static [(u32, u32)] {
    static INDEX: OnceLock<Vec<(u32, u32)>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let base = LEXICON.as_ptr() as usize;
        let mut out: Vec<(u32, u32)> = sections()
            .iter()
            .enumerate()
            .flat_map(|(s, section)| {
                section
                    .body
                    .lines()
                    .map(move |t| ((t.as_ptr() as usize - base) as u32, s as u32))
            })
            .collect();
        out.sort_by(|a, b| key(term_at(a.0)).cmp(key(term_at(b.0))));
        out
    })
}

fn term_at(offset: u32) -> &'static str {
    let rest = &LEXICON[offset as usize..];
    &rest[..rest.find('\n').unwrap_or(rest.len())]
}

fn key(term: &str) -> impl Iterator<Item = u8> + '_ {
    term.bytes().map(|b| b.to_ascii_lowercase())
}

/// The shipped spelling and section of a term, ignoring ASCII case.
fn lookup(term: &str) -> Option<(&'static str, usize)> {
    let index = index();
    let i = index
        .binary_search_by(|&(at, _)| key(term_at(at)).cmp(key(term)))
        .ok()?;
    Some((term_at(index[i].0), index[i].1 as usize))
}

/// Local context for one dictation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Signals<'a> {
    pub terminal: bool,
    pub app: Option<&'a str>,
    /// Window-title terms (`title::terms`). They only pick sections: no shipped
    /// term that shares a word with them is sent.
    pub title: &'a [String],
    /// Lexicon terms from recent dictations, most recent first (`Recent`).
    pub recent: &'a [&'static str],
}

/// Shipped terms for one dictation, best first, at most `MAX`:
/// 1. terms the user recently dictated;
/// 2. the heads of the sections the signals point at, by weight;
/// 3. the focused app's groups (up to two thirds of `MAX`), then the other
///    `[vocab]` groups, each group's hand-kept head first.
///
/// A term that ends one already picked (Key Vault after Azure Key Vault) is
/// skipped. The app turns groups on exactly as for the text packs. Empty with
/// `stt.domain_hints` off.
pub fn select(config: &Config, signals: &Signals) -> Vec<&'static str> {
    if !config.stt.domain_hints {
        return vec![];
    }
    let v = &config.vocab;
    let (on, focus) = (
        [v.dev, v.corp, v.sheet, v.finance],
        crate::cues::focused(signals.terminal, signals.app),
    );
    let sections = sections();
    let eligible = |s: usize| on[sections[s].group] || focus[sections[s].group];
    // Every lowercase word and identifier part of the title: never sent.
    let mut private: Vec<String> = Vec::new();
    for term in signals.title {
        private.push(term.to_lowercase());
        private.extend(
            term.split(['-', '_', '.', '/', ' '])
                .filter(|p| !p.is_empty())
                .map(str::to_lowercase),
        );
    }
    let mut picks = Picks {
        out: Vec::with_capacity(MAX),
        private: &private,
    };

    // 1. The user's own recent terms.
    let mut votes = vec![0u32; sections.len()];
    for &term in signals.recent {
        if let Some((term, s)) = lookup(term) {
            votes[s] += 3;
            if eligible(s) && picks.out.len() < RECENT_FIRST {
                picks.push(term);
            }
        }
    }
    // 2. Sections the user's words and the title point at.
    let config_words = config
        .stt
        .vocabulary
        .iter()
        .chain(&config.dictionary.words)
        .chain(&config.dictionary.learned);
    for word in config_words.map(String::as_str) {
        if let Some((_, s)) = lookup(word) {
            votes[s] += 2;
        }
    }
    for word in &private {
        if let Some((_, s)) = lookup(word) {
            votes[s] += 2;
        }
    }
    let mut voted: Vec<usize> = (0..sections.len())
        .filter(|&s| votes[s] > 0 && eligible(s))
        .collect();
    voted.sort_by_key(|&s| std::cmp::Reverse(votes[s]));
    if let Some(&top) = voted.first() {
        // Rounds: a section takes up to 4 terms a round, scaled by its votes.
        let mut heads: Vec<_> = voted
            .iter()
            .map(|&s| {
                (
                    sections[s].body.lines(),
                    (4 * votes[s]).div_ceil(votes[top]),
                )
            })
            .collect();
        let mut dry = false;
        while !dry && picks.out.len() < MAX * 2 / 3 {
            dry = true;
            for (lines, take) in &mut heads {
                for term in lines.take(*take as usize) {
                    dry = false;
                    picks.push(term);
                }
            }
        }
    }
    // 3. The focused app's groups, then the rest; #117's order when no signal.
    let (focused, rest): (Vec<usize>, Vec<usize>) = (0..4)
        .filter(|&g| on[g] || focus[g])
        .partition(|&g| focus[g]);
    let share = if rest.is_empty() { MAX } else { MAX * 2 / 3 };
    for term in interleave(group_terms(&focused)) {
        if picks.out.len() >= share {
            break;
        }
        picks.push(term);
    }
    for term in interleave(group_terms(&rest)) {
        if picks.out.len() >= MAX {
            break;
        }
        picks.push(term);
    }
    picks.out
}

struct Picks<'a> {
    out: Vec<&'static str>,
    private: &'a [String],
}

impl Picks<'_> {
    fn push(&mut self, term: &'static str) {
        // A formal name and its short form (Azure Key Vault, Key Vault) are one
        // hint; Claude and Claude Code are two.
        // Lexicon terms are ASCII, so byte slicing is safe.
        let same = |a: &str, b: &str| {
            let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
            let cut = long.len() - short.len();
            long[cut..].eq_ignore_ascii_case(short)
                && (cut == 0 || long.as_bytes()[cut - 1] == b' ')
        };
        if self.out.len() < MAX
            && !term
                .split(' ')
                .any(|w| self.private.iter().any(|p| p.eq_ignore_ascii_case(w)))
            && !self.out.iter().any(|t| same(t, term))
        {
            self.out.push(term);
        }
    }
}

/// Each group's terms: its hand-kept head (the section named after it), then
/// its other sections round robin.
fn group_terms(groups: &[usize]) -> Vec<Box<dyn Iterator<Item = &'static str>>> {
    groups
        .iter()
        .map(|&g| {
            let sections = sections();
            let head = sections.iter().filter(move |s| s.name == GROUPS[g]);
            let others: Vec<_> = sections
                .iter()
                .filter(|s| s.group == g && s.name != GROUPS[g])
                .map(|s| Box::new(s.body.lines()) as Box<dyn Iterator<Item = &'static str>>)
                .collect();
            Box::new(head.flat_map(|s| s.body.lines()).chain(interleave(others)))
                as Box<dyn Iterator<Item = &'static str>>
        })
        .collect()
}

/// Round robin over iterators. No parsed copy: walking the `include_str!`
/// lines per dictation costs microseconds and no heap.
fn interleave(
    mut lines: Vec<Box<dyn Iterator<Item = &'static str>>>,
) -> impl Iterator<Item = &'static str> {
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

/// Lexicon terms in a transcript, in order: the longest match (up to three
/// words) wins at each word, ignoring ASCII case.
pub fn mentions(text: &str) -> Vec<&'static str> {
    let words: Vec<&str> = text
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| {
                matches!(
                    c,
                    ',' | '.'
                        | ';'
                        | ':'
                        | '!'
                        | '?'
                        | '"'
                        | '\''
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '*'
                        | '`'
                )
            })
        })
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let found = (1..=3).rev().find_map(|n| {
            let phrase = words.get(i..i + n)?;
            if phrase.iter().any(|w| w.is_empty()) {
                return None;
            }
            let phrase = phrase.join(" ");
            (phrase.len() <= 24)
                .then(|| lookup(&phrase))
                .flatten()
                .map(|(t, _)| (t, n))
        });
        match found {
            Some((term, n)) => {
                if !out.contains(&term) {
                    out.push(term);
                }
                i += n;
            }
            None => i += 1,
        }
    }
    out
}

/// Lexicon terms from the last dictations, most recent first, in memory only.
/// Updated after delivery, never on the stop path.
#[derive(Debug, Default)]
pub struct Recent {
    terms: Vec<&'static str>,
    /// History has been read once since the daemon started.
    pub seeded: bool,
}

impl Recent {
    /// Adds the terms a delivered transcript mentions.
    pub fn note(&mut self, text: &str) {
        for term in mentions(text).into_iter().rev() {
            self.terms.retain(|t| *t != term);
            self.terms.insert(0, term);
        }
        self.terms.truncate(RECENT);
    }
    pub fn terms(&self) -> &[&'static str] {
        &self.terms
    }
    pub fn clear(&mut self) {
        self.terms.clear();
    }
}

#[cfg(test)]
mod tests;
