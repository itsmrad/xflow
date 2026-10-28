//! Common-word and proper-name guards shared by dictionary snapping and
//! correction learning. See SCOWL-LICENSE.txt.
//!
//! Source: SCOWL 2020.12.07,
//! https://downloads.sourceforge.net/project/wordlist/SCOWL/2020.12.07/scowl-2020.12.07.tar.gz
//! - words.txt: English + American + British `*-words` levels <= 40, plus
//!   `login` from level 50 to protect the log-in/login regression.
//! - names.txt: `english-upper` + `english-proper-names` levels <= 50, at least
//!   five letters, minus words.txt (only these can be sound-alike inputs).
//!
//! Entries are ASCII alphabetic, lowercased, sorted and front-coded: each line
//! is one letter `A`..`Z` giving the bytes shared with the previous entry
//! (0..25), then the lowercase suffix.
use std::sync::OnceLock;

/// A decoded list: one newline-joined string plus offsets (well under 1 MB,
/// where a `HashSet<String>` costs about 4 MB).
type List = OnceLock<(String, Vec<u32>)>;

fn contains(list: &'static List, data: &str, word: &str) -> bool {
    let (text, starts) = list.get_or_init(|| {
        let (mut text, mut starts, mut previous) = (String::new(), Vec::new(), 0);
        for line in data.lines() {
            let prefix = usize::from(line.as_bytes()[0] - b'A');
            let suffix = &line[1..];
            let start = text.len();
            text.extend_from_within(previous..previous + prefix);
            text.push_str(suffix);
            text.push('\n');
            starts.push(start as u32);
            previous = start;
        }
        (text, starts)
    });
    starts
        .binary_search_by(|&start| {
            let rest = &text[start as usize..];
            rest[..rest.find('\n').unwrap_or(rest.len())].cmp(word)
        })
        .is_ok()
}

/// Whether a lowercase word belongs to the everyday-English frequency list.
pub fn common(word: &str) -> bool {
    static WORDS: List = OnceLock::new();
    contains(&WORDS, include_str!("words.txt"), word)
}

/// Whether a lowercase word is a known proper name ("austen", "clyde").
pub fn name(word: &str) -> bool {
    static NAMES: List = OnceLock::new();
    contains(&NAMES, include_str!("names.txt"), word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_sorted_bounded_and_protects_real_words() {
        for (data, entries, max_bytes) in [
            (include_str!("words.txt"), 45_129, 200_000),
            (include_str!("names.txt"), 8_175, 50_000),
        ] {
            let mut previous = String::new();
            let mut count = 0;
            for line in data.lines() {
                let prefix = usize::from(line.as_bytes()[0] - b'A');
                let word = format!("{}{}", &previous[..prefix], &line[1..]);
                assert!(word > previous && word.bytes().all(|c| c.is_ascii_lowercase()));
                previous = word;
                count += 1;
            }
            assert_eq!(count, entries);
            assert!(data.len() < max_bytes);
        }
        for word in ["austen", "clyde", "merton", "austin", "claude"] {
            assert!(name(word), "{word}");
        }
        assert!(!name("kubernetes") && !name("everyday"));
        for word in [
            "everyday",
            "anyway",
            "react",
            "login",
            "deploy",
            "deployed",
            "affect",
            "effect",
            "form",
            "forms",
            "loose",
            "lose",
            "accept",
            "except",
            "principal",
            "principle",
        ] {
            assert!(common(word), "{word}");
        }
        for word in [
            "devterms",
            "next.js",
            "useeffect",
            "kubectl",
            "supabase",
            "xflow",
        ] {
            assert!(!common(word), "{word}");
        }
    }
}
