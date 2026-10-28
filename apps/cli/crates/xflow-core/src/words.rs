//! Common-word guard shared by dictionary snapping and correction learning.
//!
//! SCOWL 2020.12.07, English + American + British `*-words` levels <= 40,
//! ASCII alphabetic entries, lowercased and sorted; plus `login` from level 50
//! to protect the log-in/login regression. See SCOWL-LICENSE.txt.
//! Source: https://downloads.sourceforge.net/project/wordlist/SCOWL/2020.12.07/scowl-2020.12.07.tar.gz
//! words.txt is front-coded: each line is `shared-prefix-bytes:suffix` relative
//! to the previous word. This keeps the embedded list below 240 KB.
use std::{collections::HashSet, sync::OnceLock};

/// Whether a lowercase word belongs to the everyday-English frequency list.
pub fn common(word: &str) -> bool {
    static WORDS: OnceLock<HashSet<String>> = OnceLock::new();
    WORDS
        .get_or_init(|| {
            let mut previous = String::new();
            include_str!("words.txt")
                .lines()
                .map(|line| {
                    let (prefix, suffix) = line.split_once(':').expect("word-list separator");
                    previous.truncate(prefix.parse().expect("word-list prefix"));
                    previous.push_str(suffix);
                    previous.clone()
                })
                .collect()
        })
        .contains(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_sorted_bounded_and_protects_real_words() {
        let mut previous = String::new();
        let mut count = 0;
        for line in include_str!("words.txt").lines() {
            let (prefix, suffix) = line.split_once(':').unwrap();
            let word = format!("{}{suffix}", &previous[..prefix.parse::<usize>().unwrap()]);
            assert!(word > previous && word.bytes().all(|c| c.is_ascii_lowercase()));
            previous = word;
            count += 1;
        }
        assert_eq!(count, 45_129);
        assert!(include_str!("words.txt").len() < 240_000);
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
