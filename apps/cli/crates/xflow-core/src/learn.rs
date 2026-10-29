//! Learn dictionary entries from the user's own corrections of a dictation.
//! Pure functions: the daemon passes text read from the field it pasted into, or
//! an edited history entry. Nothing here stores, logs or sends that text.
use crate::config::{DictionaryConfig, Replacement, MAX_DICTIONARY_ITEMS};

/// Context kept on each side of a pasted span to find it again later.
const ANCHOR_CHARS: usize = 16;
/// Wispr and VoiceInk both cap learned candidates per dictation at four.
pub const MAX_CANDIDATES: usize = 4;
const MAX_HUNK_WORDS: usize = 3;
/// Longer texts are never compared, so a longer dictation is not watched at all.
pub const MAX_DIFF_WORDS: usize = 400;

/// Where a dictation landed in a text field: the field's own text of the paste
/// plus up to 16 characters on each side. Never the rest of the field.
pub struct Receipt {
    before: String,
    inserted: String,
    after: String,
}
impl Receipt {
    /// Locate `inserted` in the field read just after the paste: right before
    /// the caret (UTF-16 units, as the field reports it), else its only
    /// occurrence. Apps' typographic substitutions are tolerated (see
    /// `fold_char`), and so is a trailing space the app dropped.
    pub fn new(field: &str, inserted: &str, caret: Option<usize>) -> Option<Self> {
        let chars = folded(field);
        let keys: Vec<char> = chars.iter().map(|c| c.0).collect();
        // The folded field as text, for a linear-time substring search.
        let hay: String = keys.iter().collect();
        let offsets: Vec<usize> = hay.char_indices().map(|(b, _)| b).collect();
        let (i, n) = [inserted, inserted.trim_end()]
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .find_map(|s| {
                let needle: Vec<char> = folded(s).into_iter().map(|c| c.0).collect();
                let n = needle.len();
                if n == 0 {
                    return None;
                }
                let at_caret = caret.and_then(|c| utf16_to_byte(field, c)).and_then(|end| {
                    let j = chars.partition_point(|c| c.1 < end);
                    (j >= n && keys[j - n..j] == needle[..]).then(|| j - n)
                });
                // The only occurrence; a second one, even overlapping, is ambiguous.
                let unique = || {
                    let needle: String = needle.iter().collect();
                    let first = hay.find(&needle)?;
                    let next = first + hay[first..].chars().next()?.len_utf8();
                    hay[next..]
                        .find(&needle)
                        .is_none()
                        .then(|| offsets.partition_point(|&b| b < first))
                };
                at_caret.or_else(unique).map(|i| (i, n))
            })?;
        // Combining marks sorted into canonical order may swap byte ranges.
        let span = &chars[i..i + n];
        let start = span.iter().map(|c| c.1).min()?;
        let end = span.iter().map(|c| c.2).max()?;
        let before = &field[..start];
        let after = &field[end..];
        let before = &before[before
            .char_indices()
            .rev()
            .nth(ANCHOR_CHARS - 1)
            .map_or(0, |(i, _)| i)..];
        let after = &after[..after
            .char_indices()
            .nth(ANCHOR_CHARS)
            .map_or(after.len(), |(i, _)| i)];
        Some(Self {
            before: before.into(),
            inserted: field[start..end].into(),
            after: after.into(),
        })
    }
    pub fn inserted(&self) -> &str {
        &self.inserted
    }
    /// The span between the anchors in the field as it reads now, while it is
    /// still recognisably the dictation (see `similar`). An empty anchor means
    /// the field's start or end; any other anchor must occur exactly once. A
    /// short dictation that filled an empty field has nothing to tell it apart
    /// from the next message typed there, so it is not trusted.
    pub fn corrected<'a>(&self, field: &'a str) -> Option<&'a str> {
        if self.before.is_empty()
            && self.after.is_empty()
            && self.inserted.split_whitespace().count() < 4
        {
            return None;
        }
        let unique = |anchor: &str| {
            (field.matches(anchor).count() == 1)
                .then(|| field.find(anchor))
                .flatten()
        };
        let start = if self.before.is_empty() {
            0
        } else {
            unique(&self.before)? + self.before.len()
        };
        let end = if self.after.is_empty() {
            field.len()
        } else {
            unique(&self.after)?
        };
        field
            .get(start..end)
            .filter(|span| similar(&self.inserted, span))
    }
}
/// The comparison form of a character. Smart quotes and dashes, no-break
/// spaces and auto-capitalisation fold away; CR and zero-width characters are
/// dropped (CRLF reads as LF).
fn fold_char(c: char) -> Option<char> {
    Some(match c {
        '\r' | '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}' => return None,
        '‘' | '’' | '‚' | '‛' | '′' => '\'',
        '“' | '”' | '„' | '‟' | '″' => '"',
        '\u{2010}'..='\u{2015}' | '−' => '-',
        '\u{A0}' | '\u{2007}' | '\u{202F}' => ' ',
        c => c.to_lowercase().next().unwrap_or(c),
    })
}
/// Folded characters of `s` in canonical decomposition (NFD), each with the
/// byte range of the character it came from, so "é" and "e\u{301}" compare
/// equal and a match still maps back to the field's own text.
fn folded(s: &str) -> Vec<(char, usize, usize)> {
    use unicode_normalization::char::{canonical_combining_class, decompose_canonical};
    let mut out: Vec<(char, usize, usize)> = Vec::with_capacity(s.len());
    for (i, c) in s.char_indices() {
        decompose_canonical(c, |d| {
            if let Some(f) = fold_char(d) {
                out.push((f, i, i + c.len_utf8()));
            }
        });
    }
    // Canonical ordering: runs of combining marks sort by combining class.
    let mut start = 0;
    while start < out.len() {
        let end = start
            + out[start..]
                .iter()
                .take_while(|c| canonical_combining_class(c.0) != 0)
                .count();
        out[start..end].sort_by_key(|c| canonical_combining_class(c.0));
        start = end + 1;
    }
    out
}
/// Byte offset of a UTF-16 offset; None when it is past the end or splits a character.
fn utf16_to_byte(s: &str, units: usize) -> Option<usize> {
    let mut seen = 0;
    for (i, c) in s.char_indices() {
        if seen == units {
            return Some(i);
        }
        seen += c.len_utf16();
    }
    (seen == units).then_some(s.len())
}
/// Words compared case-insensitively, without the punctuation around them.
fn words(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| key(w).to_lowercase())
        .collect()
}
/// Suffix LCS lengths of two word lists.
fn lcs(a: &[String], b: &[String]) -> Vec<Vec<u16>> {
    let mut lcs = vec![vec![0u16; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    lcs
}
/// At least half of the original words survive, in order: an edit of the
/// dictation, not a different text such as the next message. Words are split
/// at punctuation and camelCase humps, so "get user name" survives as
/// getUserName.
fn similar(original: &str, corrected: &str) -> bool {
    let parts = |s: &str| {
        let mut parts = vec![];
        let mut part = String::new();
        let mut lower = false;
        for c in s.chars() {
            if (!c.is_alphanumeric() || (lower && c.is_uppercase())) && !part.is_empty() {
                parts.push(std::mem::take(&mut part));
            }
            if c.is_alphanumeric() {
                part.extend(c.to_lowercase());
            }
            lower = c.is_lowercase() || c.is_numeric();
        }
        if !part.is_empty() {
            parts.push(part);
        }
        parts
    };
    let (a, b) = (parts(original), parts(corrected));
    a.len() <= MAX_DIFF_WORDS
        && b.len() <= MAX_DIFF_WORDS
        && usize::from(lcs(&a, &b)[0][0]) * 2 >= a.len()
}

/// Close word corrections from `original` to `corrected` (at most four), skipping
/// words the dictionary already has or the user removed. Shared by the paste
/// observer and history edits. Nothing is learned from a mostly different text.
/// Err names why nothing was learned, for a log line without the text.
pub fn corrections(
    original: &str,
    corrected: &str,
    dictionary: &DictionaryConfig,
) -> Result<Vec<Replacement>, &'static str> {
    let a: Vec<&str> = original.split_whitespace().map(key).collect();
    let b: Vec<&str> = corrected.split_whitespace().map(key).collect();
    // Bounds the LCS matrix below; punctuation-only tokens count here even
    // though `similar` skips them.
    if a.len() > MAX_DIFF_WORDS || b.len() > MAX_DIFF_WORDS {
        return Err("too-long");
    }
    // Case-insensitive, so recapitalizing the next word is not part of a fix.
    if !similar(original, corrected) {
        return Err("dissimilar");
    }
    let (la, lb) = (words(original), words(corrected));
    // Edits are the unmatched runs between matched words.
    let lcs = lcs(&la, &lb);
    let known = |word: &str| {
        let word = word.to_lowercase();
        dictionary
            .words
            .iter()
            .chain(&dictionary.never_learn)
            .any(|known| known.to_lowercase() == word)
    };
    let mut found: Vec<Replacement> = vec![];
    let mut rejected = "no-edit";
    let (mut i, mut j, mut from, mut to) = (0, 0, 0, 0);
    loop {
        let matched = i < a.len() && j < b.len() && la[i] == lb[j];
        if matched || (i == a.len() && j == b.len()) {
            if from < i || to < j {
                match hunk(&a[from..i], &b[to..j]) {
                    // A removed word is not learned back in either direction.
                    Ok(c)
                        if known(&c.to)
                            || dictionary.never_learn.contains(&c.from.to_lowercase()) =>
                    {
                        rejected = "known";
                    }
                    Ok(c) => {
                        if !found
                            .iter()
                            .any(|f| f.to.to_lowercase() == c.to.to_lowercase())
                        {
                            found.push(c);
                        }
                    }
                    Err(reason) => rejected = reason,
                }
            }
            if !matched {
                break;
            }
            (i, j) = (i + 1, j + 1);
            (from, to) = (i, j);
        } else if j == b.len() || (i < a.len() && lcs[i + 1][j] >= lcs[i][j + 1]) {
            i += 1;
        } else {
            j += 1;
        }
    }
    found.truncate(MAX_CANDIDATES);
    if found.is_empty() {
        Err(rejected)
    } else {
        Ok(found)
    }
}
/// An edit hunk, or its aligned start or end when the user also typed or
/// deleted words right next to the correction.
fn hunk(from: &[&str], to: &[&str]) -> Result<Replacement, &'static str> {
    let (n, m) = (from.len(), to.len());
    let k = n.min(m);
    candidate(from, to)
        .or_else(|e| candidate(&from[..k], &to[..k]).map_err(|_| e))
        .or_else(|e| candidate(&from[n - k..], &to[m - k..]).map_err(|_| e))
}
/// A word without the punctuation around it.
fn key(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric())
}
/// One edit is learnable when it replaces 1-3 plain dictated words with a term
/// of 1-3 words that looks or sounds like them. Learning only adds vocabulary,
/// so the term may be code-like (`useEffect`, `Next.js`, `k8s`, `shadcn/ui`).
/// Insertions, deletions, case or punctuation changes, numbers, URLs, spoken
/// punctuation and swaps between everyday words (their/there, deploy/deployed,
/// every day/everyday) are not.
fn candidate(from: &[&str], to: &[&str]) -> Result<Replacement, &'static str> {
    if from.is_empty() || to.is_empty() || from.len() > MAX_HUNK_WORDS || to.len() > MAX_HUNK_WORDS
    {
        return Err("shape");
    }
    let (from, to) = (from.join(" "), to.join(" "));
    let plain = from
        .chars()
        .all(|c| c.is_alphabetic() || matches!(c, ' ' | '\'' | '’' | '-'));
    let term = to
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '\'' | '’' | '-' | '.' | '_' | '/'));
    let fold = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ')
            .flat_map(char::to_lowercase)
            .collect()
    };
    // Digits read as their names, so "kay eights" and "k8s" compare.
    const DIGITS: [&str; 10] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    ];
    let letters = |s: &str| -> Vec<char> {
        s.chars()
            .flat_map(|c| match c.to_digit(10) {
                Some(d) => DIGITS[d as usize].chars().collect(),
                None if c.is_alphabetic() => c.to_lowercase().collect(),
                None => vec![],
            })
            .collect()
    };
    let (x, y) = (letters(&from), letters(&to));
    // At most a third of the letters changed, or the same consonant skeleton.
    let close = distance(&x, &y) * 3 <= x.len().max(y.len())
        || (x.len().min(y.len()) >= 5 && sounds(&x).len() >= 3 && sounds(&x) == sounds(&y));
    let spoken = |name: &str, symbol: char| {
        from.split(' ').any(|w| w.eq_ignore_ascii_case(name)) && to.contains(symbol)
    };
    let url = to.matches('/').count() >= 2
        || spoken("dot", '.')
        || spoken("slash", '/')
        || spoken("underscore", '_')
        || spoken("dash", '-')
        || spoken("hyphen", '-')
        || to.rsplit_once('.').is_some_and(|(_, tld)| {
            ["com", "org", "net", "edu", "gov"].contains(&tld.to_lowercase().as_str())
        });
    // Everyday words in both texts with no new capital: a grammar or spelling
    // fix, not a term. "re act" -> "React" adds a capital and is kept.
    let capitals = |s: &str| s.chars().filter(|c| c.is_uppercase()).count();
    let everyday = to
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .all(|w| crate::words::common(&w.to_lowercase()))
        && capitals(&to) <= capitals(&from);
    if !plain || !term {
        Err("symbols")
    } else if to.chars().count() > 40 {
        Err("long")
    } else if !to.chars().any(char::is_alphabetic) || y.len() < 2 {
        Err("number")
    } else if fold(&from) == fold(&to) {
        Err("case-or-punctuation")
    } else if !close {
        Err("not-close")
    } else if url {
        Err("url")
    } else if to.split(' ').all(common) || everyday {
        Err("everyday-word")
    } else {
        Ok(Replacement { from, to })
    }
}
pub(crate) fn distance(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (diagonal + usize::from(ca != cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}
/// Consonant skeleton with Soundex classes, vowels dropped and repeats merged:
/// "coopernetties" and "kubernetes" agree.
pub(crate) fn sounds(letters: &[char]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in letters {
        let class = match c {
            'b' | 'f' | 'p' | 'v' => 1,
            'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => 2,
            'd' | 't' => 3,
            'l' => 4,
            'm' | 'n' => 5,
            'r' => 6,
            _ => continue,
        };
        if out.last() != Some(&class) {
            out.push(class);
        }
    }
    out
}
/// Everyday words, very common names and acronyms are never learned (Wispr skips them too).
fn common(word: &str) -> bool {
    // ponytail: a short fixed list; swap for a frequency table if false positives show up.
    const COMMON: &str = "\
        a able about above after again against ago all almost also always am an and another any \
        anything are around as asap ask at away back bad be because been before being best better \
        between big both bring but buy by call came can car case ceo change check child city close \
        come could country data david day did different do does doing done door down during each \
        early easy end even ever every eye face fact family far feel few find fine first for found \
        free friend from front full game get give go going good got great group had hand happy hard \
        has have he head hear help her here high him his home hope hour house how however i idea if \
        important in into is issue it its it's james job john just keep kind know large last late \
        later leave left less let life like line list little live long look lot love made make man \
        many mary may maybe me mean meeting might mike mind minute money month more morning most move \
        much must my name need never new news next nice night no not note nothing now number of off \
        office often ok okay old on once one only open or order other our out over own page part \
        people pdf person place plan play please point post power problem product program project put \
        question quite read ready real really reason right room run said same sarah saw say school \
        see seem send set she should show side since small so some something soon sorry start state \
        still stop story study such sure system take talk team tell test than thank thanks that the \
        their them then there these they thing think this those though thought three through time \
        to today together told too took top try turn two under understand until up us use used very \
        wait want was water way we week well went were what when where which while who whole why will \
        with without word work world would write year yes yet you your";
    let word = word.to_lowercase();
    COMMON.split_whitespace().any(|common| common == word)
}

impl DictionaryConfig {
    /// Save learned corrections as vocabulary only: the corrected word becomes a
    /// recognition hint (first, so provider hint limits keep it). Learning never
    /// writes replacements, which would rewrite every future transcript.
    /// Returns the newly learned words.
    pub fn learn(&mut self, found: &[Replacement]) -> Vec<String> {
        let mut added = Vec::new();
        for c in found {
            if self.words.len() >= MAX_DICTIONARY_ITEMS {
                break;
            }
            if !self.words.contains(&c.to) {
                self.words.insert(0, c.to.clone());
            }
            if !self.learned.contains(&c.to) {
                self.learned.push(c.to.clone());
            }
            added.push(c.to.clone());
        }
        added
    }
    /// Remove a word. A learned word is remembered so it is never learned again.
    pub fn forget(&mut self, word: &str) {
        self.words.retain(|w| w != word);
        let lower = word.to_lowercase();
        let learned = self.learned.len();
        self.learned.retain(|w| w.to_lowercase() != lower);
        if self.learned.len() < learned {
            self.words.retain(|w| w.to_lowercase() != lower);
            if !self.never_learn.contains(&lower) {
                if self.never_learn.len() >= MAX_DICTIONARY_ITEMS {
                    self.never_learn.remove(0);
                }
                self.never_learn.push(lower);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn learn(original: &str, corrected: &str) -> Vec<(String, String)> {
        corrections(original, corrected, &DictionaryConfig::default())
            .unwrap_or_default()
            .into_iter()
            .map(|r| (r.from, r.to))
            .collect()
    }
    fn pair(from: &str, to: &str) -> Vec<(String, String)> {
        vec![(from.into(), to.into())]
    }

    #[test]
    fn learns_close_corrections_only() {
        assert_eq!(
            learn(
                "We deploy on kubernetis today.",
                "We deploy on Kubernetes today."
            ),
            pair("kubernetis", "Kubernetes")
        );
        // Multi-word: two dictated words become one name, matched by sound.
        assert_eq!(
            learn("ship it to cooper netties now", "ship it to Kubernetes now"),
            pair("cooper netties", "Kubernetes")
        );
        assert_eq!(
            learn("please ask jon smyth today", "please ask John Smith today"),
            pair("jon smyth", "John Smith")
        );
        // The user fixed a word and kept typing.
        assert_eq!(
            learn("use postgress today", "use Postgres today and more"),
            pair("postgress", "Postgres")
        );
        // Recapitalizing the next word is not part of the fix.
        assert_eq!(
            learn("it uses postgress. then we", "it uses Postgres. Then we"),
            pair("postgress", "Postgres")
        );
        // Nothing changed, case or punctuation only, insertion, deletion.
        assert!(learn("hello postgres world", "hello postgres world").is_empty());
        assert!(learn("hello postgres world", "Hello Postgres world!").is_empty());
        assert!(learn("send the e-mail", "send the email").is_empty());
        assert!(learn("hello world", "hello brave new world").is_empty());
        assert!(learn("hello brave new world", "hello world").is_empty());
        // Unrelated rewrites.
        assert!(learn("the cat sat", "the dog sat").is_empty());
        assert!(learn("check its done", "check it's done").is_empty());
        assert!(learn("buy a apple", "buy an apple").is_empty());
        // A different text is not a correction of this one.
        assert!(learn("deploy kubernetis now", "lunch at noon with Kubernetes").is_empty());
        // At most four per dictation.
        assert_eq!(
            learn(
                "we use kubernetis and postgress and grafanna and terraforn and ansibel",
                "we use Kubernetes and Postgres and Grafana and Terraform and Ansible"
            )
            .len(),
            MAX_CANDIDATES
        );
    }

    #[test]
    fn developer_terms_are_learnable_vocabulary() {
        for (original, corrected, from, to) in [
            (
                "we use dev terms",
                "we use DevTerms",
                "dev terms",
                "DevTerms",
            ),
            (
                "call use effect here",
                "call useEffect here",
                "use effect",
                "useEffect",
            ),
            (
                "built with next js",
                "built with Next.js",
                "next js",
                "Next.js",
            ),
            (
                "run cube cuddle get pods",
                "run kubectl get pods",
                "cube cuddle",
                "kubectl",
            ),
            ("deploy to kay eights", "deploy to k8s", "kay eights", "k8s"),
            (
                "we should try gpt four o now",
                "we should try gpt-4o now",
                "gpt four o",
                "gpt-4o",
            ),
            (
                "please add shad cn ui here",
                "please add shadcn/ui here",
                "shad cn ui",
                "shadcn/ui",
            ),
            ("we like re act", "we like React", "re act", "React"),
            // pr47 rejected camelCase over 6 chars; vocabulary-only learning allows it.
            (
                "call get user name",
                "call getUserName",
                "get user name",
                "getUserName",
            ),
        ] {
            assert_eq!(learn(original, corrected), pair(from, to), "{original}");
        }
    }

    #[test]
    fn grammar_numbers_urls_and_code_edits_are_not_learned() {
        for (original, corrected, reason) in [
            ("we deploy it", "we deployed it", "everyday-word"),
            ("it will affect us", "it will effect us", "everyday-word"),
            ("I went their", "I went there", "not-close"),
            ("Their plan works", "There plan works", "not-close"),
            ("we use the form", "we use the forms", "everyday-word"),
            ("we ship every day", "we ship everyday", "everyday-word"),
            (
                "we always use an d or here",
                "we always use and/or here",
                "everyday-word",
            ),
            ("call me at five", "call me at 5", "number"),
            ("open example dot com", "open example.com", "url"),
            ("open example com now", "open example.com now", "url"),
            ("see docs slash api slash v1", "see docs/api/v1", "shape"),
            ("we use src app main now", "we use src/app/main now", "url"),
            ("mail bob at example", "mail bob@example", "symbols"),
            ("fix my_var", "fix my_vat", "symbols"),
            ("send the e-mail", "send the email", "case-or-punctuation"),
            ("the cat sat", "the dog sat", "not-close"),
            ("hello postgres world", "hello postgres world", "no-edit"),
            (
                "deploy kubernetis now",
                "lunch at noon with Kubernetes",
                "dissimilar",
            ),
        ] {
            assert_eq!(
                corrections(original, corrected, &DictionaryConfig::default()).unwrap_err(),
                reason,
                "{original} -> {corrected}"
            );
        }
        // Punctuation-only tokens count toward the diff bound.
        let dots = " .".repeat(600);
        assert_eq!(
            corrections(
                &format!("we use post gres{dots}"),
                &format!("we use Postgres{dots}"),
                &DictionaryConfig::default()
            )
            .unwrap_err(),
            "too-long"
        );
        let long = "averyveryveryveryverylongidentifiername42x";
        assert!(learn("use a very long name", &format!("use {long}")).is_empty());
    }

    #[test]
    fn learning_adds_vocabulary_only_and_removed_words_stay_removed() {
        let mut dictionary = DictionaryConfig::default();
        let found = corrections("use kubernetis", "use Kubernetes", &dictionary).unwrap();
        assert_eq!(dictionary.learn(&found), ["Kubernetes"]);
        assert_eq!(dictionary.words, ["Kubernetes"]);
        assert_eq!(dictionary.learned, ["Kubernetes"]);
        let found = corrections("we use dev terms", "we use DevTerms", &dictionary).unwrap();
        assert_eq!(dictionary.learn(&found), ["DevTerms"]);
        // Never a replacement, even for a join that #64's snap pass now handles.
        assert!(dictionary.replacements.is_empty());
        assert_eq!(
            corrections("use kubernetis", "use Kubernetes", &dictionary).unwrap_err(),
            "known"
        );
        dictionary.forget("Kubernetes");
        assert_eq!(dictionary.words, ["DevTerms"]);
        assert_eq!(dictionary.learned, ["DevTerms"]);
        assert!(corrections("use kubernetis", "use Kubernetes", &dictionary).is_err());
        assert!(corrections("use Kubernetes", "use kubernetis", &dictionary).is_err());
    }

    #[test]
    fn receipt_refinds_the_pasted_span_by_its_anchors() {
        let inserted = "We deploy on kubernetis daily. ";
        let field = format!("Intro paragraph here. {inserted}Closing words stay.");
        let receipt = Receipt::new(&field, inserted, None).unwrap();
        // Edits elsewhere in the field do not matter.
        let edited =
            "Changed intro paragraph here. We deploy on Kubernetes daily. Closing words stay!";
        let span = receipt.corrected(edited).unwrap();
        assert_eq!(span, "We deploy on Kubernetes daily. ");
        assert_eq!(
            corrections(receipt.inserted(), span, &DictionaryConfig::default()).unwrap()[0].to,
            "Kubernetes"
        );
        // Pasted at the end of the field; the app dropped the trailing space; the
        // user typed more afterwards (an insertion, not learned).
        let receipt = Receipt::new("Notes: we use postgress", "we use postgress ", None).unwrap();
        let span = receipt.corrected("Notes: we use Postgres today").unwrap();
        assert_eq!(
            learn(receipt.inserted(), span),
            pair("postgress", "Postgres")
        );
        // Ambiguous without a caret: the text was pasted twice, or an anchor now occurs twice.
        assert!(Receipt::new("hi there. hi there.", "hi there.", None).is_none());
        let receipt = Receipt::new(&field, inserted, None).unwrap();
        assert!(receipt
            .corrected("Intro paragraph here. Intro paragraph here. We deploy. Closing words stay.")
            .is_none());
        // Anchor gone.
        assert!(receipt.corrected("Something else entirely").is_none());
        // A short dictation that filled an empty field has no anchors to trust.
        let receipt = Receipt::new("use postgress", "use postgress", None).unwrap();
        assert!(receipt.corrected("lunch with Postgres").is_none());
    }

    #[test]
    fn receipt_uses_the_caret_and_tolerates_app_substitutions() {
        // The caret picks the second of two identical pastes (UTF-16 offsets).
        let field = "hi there. hi there.";
        let receipt = Receipt::new(field, "hi there.", Some(19)).unwrap();
        assert_eq!(receipt.before, "hi there. ");
        assert_eq!(receipt.after, "");
        let receipt = Receipt::new(field, "hi there.", Some(9)).unwrap();
        assert_eq!(receipt.after, " hi there.");
        // Emoji and CJK before the paste: the caret counts UTF-16 units, not bytes.
        let field = "😀 日本 ok we use postgress now";
        let caret = "😀 日本 ok we use postgress".encode_utf16().count();
        assert_eq!(caret, 25);
        let receipt = Receipt::new(field, "we use postgress", Some(caret)).unwrap();
        assert_eq!(receipt.inserted(), "we use postgress");
        assert_eq!(receipt.before, "😀 日本 ok ");
        // A caret inside a surrogate pair or past the end falls back to the unique match.
        assert!(Receipt::new(field, "we use postgress", Some(1)).is_some());
        assert!(Receipt::new(field, "we use postgress", Some(999)).is_some());
        assert_eq!(utf16_to_byte("😀a", 1), None);
        assert_eq!(utf16_to_byte("😀a", 2), Some(4));
        assert_eq!(utf16_to_byte("😀a", 3), Some(5));
        // Smart quotes and dashes, NBSP, CRLF, a zero-width space and an
        // auto-capitalised first letter: the receipt keeps the field's own text.
        for (field, inserted) in [
            ("Note: “Don’t” — ok", "\"don't\" - ok"),
            ("x\u{a0}y and more", "x y and more"),
            ("line one\r\nline two", "line one\nline two"),
            ("we\u{200b} use it", "we use it"),
            ("Ship it today", "ship it today"),
        ] {
            let receipt =
                Receipt::new(field, inserted, None).unwrap_or_else(|| panic!("{field:?}"));
            assert!(field.ends_with(receipt.inserted()), "{field:?}");
        }
        // Canonically equivalent text: composed in one, decomposed in the other,
        // marks in either order; the caret counts the field's own UTF-16 units.
        let field = "we use cafe\u{301} daily";
        let receipt = Receipt::new(field, "we use café daily", None).unwrap();
        assert_eq!(receipt.inserted(), field);
        let field = "Menu: café au lait";
        let caret = field.encode_utf16().count();
        let receipt = Receipt::new(field, "cafe\u{301} au lait", Some(caret)).unwrap();
        assert_eq!(receipt.inserted(), "café au lait");
        assert_eq!(receipt.before, "Menu: ");
        let field = "x a\u{323}\u{301} y";
        let receipt = Receipt::new(
            field,
            "a\u{301}\u{323} y",
            Some(field.encode_utf16().count()),
        );
        assert_eq!(receipt.unwrap().inserted(), "a\u{323}\u{301} y");
        // An overlapping second occurrence is ambiguous too.
        assert!(Receipt::new("x a a a y", "a a", None).is_none());
        // Text that folds away entirely is never searched for.
        assert!(Receipt::new("abc", "\u{200b} ", None).is_none());
        // The anchored span must still be the dictation, not the next message.
        let receipt = Receipt::new("we use dev terms", "we use dev terms", Some(16)).unwrap();
        assert_eq!(
            receipt.corrected("we use DevTerms"),
            Some("we use DevTerms")
        );
        assert_eq!(receipt.corrected(""), None);
        assert_eq!(receipt.corrected("lunch at noon anyone"), None);
    }

    /// Time per call at typical, 20-minute (#85) and pathological sizes:
    /// `cargo test --release -p xflow-core learn::tests::cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement; run explicitly in release"]
    fn cost() {
        fn time(label: &str, mut f: impl FnMut()) {
            let mut runs: Vec<std::time::Duration> = (0..30)
                .map(|_| {
                    let start = std::time::Instant::now();
                    f();
                    start.elapsed()
                })
                .collect();
            runs.sort();
            println!("{label}: p50 {:?} p95 {:?}", runs[15], runs[28]);
        }
        let dictionary = DictionaryConfig::default();
        let words = |n: usize, salt: &str| -> String {
            (0..n)
                .map(|i| format!("word{} ", (i * 7919) % 997))
                .collect::<String>()
                + salt
        };
        let lines =
            |n: usize, tag: &str| -> String { (0..n).map(|i| format!("{tag} {i}. ")).collect() };
        // Typical: a 40-word dictation in a 2 KB message.
        let dictation = words(40, "post gres ");
        let field = format!("{}{dictation}{}", lines(70, "before"), lines(70, "after"));
        let edited = field.replace("post gres", "Postgres");
        let receipt = Receipt::new(&field, &dictation, None).unwrap();
        time("typical receipt (no caret)", || {
            Receipt::new(&field, &dictation, None).unwrap();
        });
        time("typical read", || {
            receipt.corrected(&edited).unwrap();
        });
        let span = receipt.corrected(&edited).unwrap().to_owned();
        time("typical corrections", || {
            corrections(receipt.inserted(), &span, &dictionary).unwrap();
        });
        // 20-minute dictation (~3,000 words, 18 KB) at the end of a 64 KB field.
        let dictation = words(3000, "post gres ");
        let field = format!("{}{dictation}", lines(4_000, "line"));
        assert!(field.encode_utf16().count() > 50_000);
        let caret = field.encode_utf16().count();
        time("20-min receipt (caret)", || {
            Receipt::new(&field, &dictation, Some(caret)).unwrap();
        });
        time("20-min receipt (no caret)", || {
            Receipt::new(&field, &dictation, None).unwrap();
        });
        let receipt = Receipt::new(&field, &dictation, Some(caret)).unwrap();
        let edited = field.replace("post gres", "Postgres");
        time("20-min read (over the diff bound)", || {
            assert!(receipt.corrected(&edited).is_none());
        });
        // Pathological: a periodic 64 KB field and a near-match needle.
        let field = "a ".repeat(32_000);
        let needle = format!("{}b", "a ".repeat(9_000));
        time("periodic 64 KB, 18 KB needle", || {
            assert!(Receipt::new(&field, &needle, None).is_none());
        });
    }
}
