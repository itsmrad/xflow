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
        self.span(field)
            .filter(|span| similar(&self.inserted, span))
    }
    /// The span between the anchors, whether or not it still reads as the
    /// dictation; the same short-dictation guard as `corrected`.
    pub fn span<'a>(&self, field: &'a str) -> Option<&'a str> {
        if self.whole_field() && self.inserted.split_whitespace().count() < 4 {
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
        field.get(start..end)
    }
    /// The dictation filled the field: nothing around it tells it apart from
    /// the next message typed there.
    fn whole_field(&self) -> bool {
        self.before.is_empty() && self.after.is_empty()
    }
    /// A read that no longer looks like the dictation (see `corrected`) may be
    /// the user retyping part of it, or the next message. It counts as an edit
    /// in progress only while a context anchor is still found exactly once, or,
    /// for a dictation that filled the field, while the field keeps a whole
    /// word at the start or end of `last` (the last span that looked like the
    /// dictation): "R, call me" while "Rimpas or Kar, call me" is retyped. A
    /// placeholder or a new message after a send keeps neither.
    pub fn editing(&self, field: &str, last: &str) -> bool {
        let Some(span) = self.span(field) else {
            return false;
        };
        if !self.whole_field() {
            return true;
        }
        let (a, b) = (parts(last), parts(span));
        let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        let suffix = a
            .iter()
            .rev()
            .zip(b.iter().rev())
            .take_while(|(x, y)| x == y)
            .count();
        let remnant = prefix + suffix.min(b.len() - prefix);
        remnant > 0 && remnant + MAX_HUNK_WORDS + 1 >= a.len()
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
/// Lowercase words, split at punctuation and camelCase humps.
fn parts(s: &str) -> Vec<String> {
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
}
/// At least half of the original words survive, in order: an edit of the
/// dictation, not a different text such as the next message. Words are split
/// at punctuation and camelCase humps, so "get user name" survives as
/// getUserName. A short text where the user respelled a run of words passes on
/// its letters instead (see `similar_letters`).
fn similar(original: &str, corrected: &str) -> bool {
    let (a, b) = (parts(original), parts(corrected));
    if a.len() > MAX_DIFF_WORDS || b.len() > MAX_DIFF_WORDS {
        return false;
    }
    let lcs = lcs(&a, &b);
    usize::from(lcs[0][0]) * 2 >= a.len() || similar_letters(&a, &b, &lcs)
}
/// The letter fallback runs on short texts only, so its edit distances stay
/// small: at most this many words and letters in the original, and runs of at
/// most this many letters on each side.
const MAX_LETTER_PARTS: usize = 40;
const MAX_LETTER_TOTAL: usize = 400;
const MAX_RUN_LETTERS: usize = 64;
/// At least half of the original's letters survive: each kept word counts its
/// letters, and each respelled run of up to four words against up to three
/// counts the letters that stayed ("Rimpas or Kar, call me" against "Rimpa
/// Sarkar, call me" keeps 16 of 17). A short message where one name was
/// respelled is still the same message; "Hi Rimpas or Kar" against "Hi there"
/// keeps 3 of 13 and is not.
fn similar_letters(a: &[String], b: &[String], lcs: &[Vec<u16>]) -> bool {
    let total: usize = a.iter().map(|w| w.chars().count()).sum();
    if total == 0
        || total > MAX_LETTER_TOTAL
        || a.len() > MAX_LETTER_PARTS
        || b.len() > MAX_LETTER_PARTS
    {
        return false;
    }
    let respelled = |x: &[String], y: &[String]| -> usize {
        let (x, y): (Vec<char>, Vec<char>) =
            (x.concat().chars().collect(), y.concat().chars().collect());
        if x.len() > MAX_RUN_LETTERS || y.len() > MAX_RUN_LETTERS {
            return 0;
        }
        x.len().saturating_sub(distance(&x, &y))
    };
    let (mut i, mut j, mut from, mut to, mut kept) = (0, 0, 0, 0, 0);
    loop {
        let matched = i < a.len() && j < b.len() && a[i] == b[j];
        if matched || (i == a.len() && j == b.len()) {
            if (from < i || to < j) && i - from <= MAX_HUNK_WORDS + 1 && j - to <= MAX_HUNK_WORDS {
                kept += respelled(&a[from..i], &b[to..j]);
            }
            if !matched {
                break;
            }
            kept += a[i].chars().count();
            (i, j) = (i + 1, j + 1);
            (from, to) = (i, j);
        } else if j == b.len() || (i < a.len() && lcs[i + 1][j] >= lcs[i][j + 1]) {
            i += 1;
        } else {
            j += 1;
        }
    }
    kept * 2 >= total
}

/// What one correction teaches: terms to learn, and corrections back to a term
/// the dictionary already had, whose recognition hint did not take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Corrections {
    /// New terms, at most four.
    pub new: Vec<Replacement>,
    /// At most four; `to` is the dictionary's own spelling of the term.
    pub known: Vec<Replacement>,
    /// Why nothing new was learned, when `new` is empty.
    pub rejected: &'static str,
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
    let found = review(original, corrected, dictionary)?;
    if found.new.is_empty() {
        Err(found.rejected)
    } else {
        Ok(found.new)
    }
}
/// `corrections`, plus the hunks that respell a term the dictionary already
/// has (see `DictionaryConfig::promote` and `derive`). Err only when there is
/// neither.
pub fn review(
    original: &str,
    corrected: &str,
    dictionary: &DictionaryConfig,
) -> Result<Corrections, &'static str> {
    // Bounds the work below, punctuation-only tokens included.
    if original.split_whitespace().count() > MAX_DIFF_WORDS
        || corrected.split_whitespace().count() > MAX_DIFF_WORDS
    {
        return Err("too-long");
    }
    // Punctuation-only tokens ("->", "=") are not words: they take no part in
    // the alignment and never end up inside a learned term.
    let tokens = |s| -> Vec<&str> {
        str::split_whitespace(s)
            .map(key)
            .filter(|w| !w.is_empty())
            .collect()
    };
    let (a, b) = (tokens(original), tokens(corrected));
    if !similar(original, corrected) {
        return Err("dissimilar");
    }
    // Case-insensitive, so recapitalizing the next word is not part of a fix.
    let lower = |w: &[&str]| -> Vec<String> { w.iter().map(|w| w.to_lowercase()).collect() };
    let (la, lb) = (lower(&a), lower(&b));
    // Edits are the unmatched runs between matched words.
    let lcs = lcs(&la, &lb);
    let never = |word: &str| {
        let word = word.to_lowercase();
        dictionary
            .never_learn
            .iter()
            .any(|w| w.to_lowercase() == word)
    };
    let mut found = Corrections {
        rejected: "no-edit",
        ..Corrections::default()
    };
    let (mut i, mut j, mut from, mut to) = (0, 0, 0, 0);
    loop {
        let matched = i < a.len() && j < b.len() && la[i] == lb[j];
        if matched || (i == a.len() && j == b.len()) {
            if from < i || to < j {
                match hunk(&a[from..i], &b[to..j]) {
                    // A removed word is not learned back in either direction.
                    Ok(c) if never(&c.to) || never(&c.from) => found.rejected = "known",
                    Ok(c) => {
                        let lower = c.to.to_lowercase();
                        let stored = dictionary.words.iter().find(|w| w.to_lowercase() == lower);
                        let (list, term) = match stored {
                            Some(word) => {
                                found.rejected = "known";
                                (&mut found.known, word.clone())
                            }
                            None => (&mut found.new, c.to),
                        };
                        if !list.iter().any(|f| f.to.to_lowercase() == lower) {
                            list.push(Replacement {
                                from: c.from,
                                to: term,
                            });
                        }
                    }
                    Err(reason) => found.rejected = reason,
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
    found.new.truncate(MAX_CANDIDATES);
    found.known.truncate(MAX_CANDIDATES - found.new.len());
    if found.new.is_empty() && found.known.is_empty() {
        Err(found.rejected)
    } else {
        Ok(found)
    }
}
/// An edit hunk, or its aligned start or end when the user also typed or
/// deleted words right next to the correction. Words typed right after or
/// before the fix ("DevTerms 2", "DevTerms done") are not part of it: a
/// shorter start or end of the new text that already matches wins.
fn hunk(from: &[&str], to: &[&str]) -> Result<Replacement, &'static str> {
    let (n, m) = (from.len(), to.len());
    for len in 1..m.min(MAX_HUNK_WORDS + 1) {
        for part in [&to[..len], &to[m - len..]] {
            if let Ok(c) = candidate(from, part) {
                return Ok(c);
            }
        }
    }
    let k = n.min(m);
    candidate(from, to)
        .or_else(|e| candidate(&from[..k], &to[..k]).map_err(|_| e))
        .or_else(|e| candidate(&from[n - k..], &to[m - k..]).map_err(|_| e))
}
/// A word without the punctuation around it.
fn key(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric())
}
/// One edit is learnable when it replaces 1-3 plain dictated words (four for a
/// term of one or two) with a term of 1-3 words that looks or sounds like them.
/// Learning only adds vocabulary, so the term may be code-like (`useEffect`,
/// `Next.js`, `k8s`, `shadcn/ui`).
/// Insertions, deletions, case or punctuation changes, numbers, URLs, spoken
/// punctuation and swaps between everyday words (their/there, deploy/deployed,
/// every day/everyday) are not.
fn candidate(from: &[&str], to: &[&str]) -> Result<Replacement, &'static str> {
    // Four dictated words may make a name of one or two ("Rim pass or car" ->
    // "Rimpa Sarkar").
    let heard = from.len();
    if from.is_empty()
        || to.is_empty()
        || to.len() > MAX_HUNK_WORDS
        || heard > MAX_HUNK_WORDS + usize::from(to.len() <= 2)
    {
        return Err("shape");
    }
    // Each word of a multi-word term is a real word: "DevTerms 2", "DevTerms v2"
    // or "Postgres 1" adds a counter or version, not a spelling.
    if to.len() > 1
        && to
            .iter()
            .any(|w| w.chars().filter(|c| c.is_alphabetic()).count() < 2)
    {
        return Err("counter");
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
    // Four words joined letter for letter ("a lot of fun" -> "alotoffun") are a
    // typing choice, not a misrecognition; snap never joins four words either.
    if heard > MAX_HUNK_WORDS && x == y {
        return Err("shape");
    }
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

/// Correction rules `derive` may add for one term, and in all. Counted
/// conservatively: every replacement that targets a dictionary word counts,
/// whoever wrote it.
const MAX_DERIVED_PER_TERM: usize = 3;
const MAX_DERIVED: usize = 100;

impl DictionaryConfig {
    /// Save learned corrections as vocabulary: the corrected word becomes a
    /// recognition hint (first, so provider hint limits keep it). A first
    /// correction never writes a replacement, which would rewrite every future
    /// transcript (see `derive` for the second). Returns the newly learned words.
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
    /// Move each corrected term the dictionary already has (its stored
    /// spelling, `known` from `review`) to the front, so provider hint limits
    /// keep the words the user keeps correcting. Returns the terms that moved.
    pub fn promote(&mut self, known: &[Replacement]) -> Vec<String> {
        let mut moved = Vec::new();
        for c in known {
            if let Some(i) = self
                .words
                .iter()
                .position(|w| *w == c.to)
                .filter(|&i| i > 0)
            {
                let word = self.words.remove(i);
                self.words.insert(0, word);
                moved.push(c.to.clone());
            }
        }
        moved
    }
    /// A second correction of a term the dictionary already had (`known` from
    /// `review`): its recognition hint did not take, and snap cannot restore a
    /// name misheard with a function word inside ("Rimpas or Kar"), so that
    /// exact phrase becomes an ordinary replacement to the stored spelling.
    /// Only a narrow shape: 2-3 ASCII words of two letters or more, at least one
    /// of five; the first and last uncommon (not everyday words, names or
    /// function words), a middle one a function word. Never a single word
    /// ("cloud" stays "cloud"), a phrase snap already restores, a phrase over an
    /// existing rule or holding another dictionary word, or past the caps.
    /// Replacements match whole words with spaces between them only, so
    /// "Rimpas, or Kar" is left alone. Returns the rules added.
    pub fn derive(&mut self, known: &[Replacement]) -> Vec<Replacement> {
        let mut added = Vec::new();
        for c in known {
            if let Some(rule) = self.phrase(c) {
                self.replacements.push(rule.clone());
                added.push(rule);
            }
        }
        added
    }
    fn phrase(&self, c: &Replacement) -> Option<Replacement> {
        let tokens: Vec<String> = c.from.split(' ').map(str::to_ascii_lowercase).collect();
        let function = |w: &str| crate::snap::FUNCTION_WORDS.contains(&w);
        let uncommon = |w: &str| {
            !crate::words::common(w) && !crate::words::name(w) && !common(w) && !function(w)
        };
        let (first, last) = (tokens.first()?, tokens.last()?);
        let shape = (2..=3).contains(&tokens.len())
            && tokens
                .iter()
                .all(|t| t.len() >= 2 && t.bytes().all(|b| b.is_ascii_alphabetic()))
            && tokens.iter().any(|t| t.len() >= 5)
            && uncommon(first.as_str())
            && uncommon(last.as_str())
            && tokens[1..tokens.len() - 1]
                .iter()
                .all(|t| function(t.as_str()));
        // The stored spelling of an active term the user did not remove.
        let to = self.words.iter().find(|w| **w == c.to)?;
        let lower = to.to_lowercase();
        if !shape || self.never_learn.iter().any(|w| w.to_lowercase() == lower) {
            return None;
        }
        let words =
            |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_lowercase).collect() };
        let letters = |s: &str| -> String {
            s.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect()
        };
        let targets = |r: &Replacement| {
            self.words
                .iter()
                .any(|w| w.to_lowercase() == r.to.to_lowercase())
        };
        // Over an existing rule, whatever its target (a manual one is never replaced).
        let ruled = self.replacements.iter().any(|r| {
            let existing = words(&r.from);
            !existing.is_empty()
                && (tokens
                    .windows(existing.len())
                    .any(|window| window == existing)
                    || existing
                        .windows(tokens.len())
                        .any(|window| window == tokens))
        });
        let holds_word = self.words.iter().any(|w| {
            let w = words(w);
            !w.is_empty() && tokens.windows(w.len()).any(|window| window == w.as_slice())
        });
        let per_term = self
            .replacements
            .iter()
            .filter(|r| r.to.to_lowercase() == lower)
            .count();
        let derived = self.replacements.iter().filter(|r| targets(r)).count();
        if ruled
            || holds_word
            || letters(&c.from) == letters(to)
            || per_term >= MAX_DERIVED_PER_TERM
            || derived >= MAX_DERIVED
            || self.replacements.len() >= MAX_DICTIONARY_ITEMS
        {
            return None;
        }
        // Snap already restores it, so the rule would be redundant.
        let config = crate::config::Config {
            dictionary: self.clone(),
            ..Default::default()
        };
        if crate::snap::apply(&c.from, &config, false) == *to {
            return None;
        }
        Some(Replacement {
            from: c.from.clone(),
            to: to.clone(),
        })
    }
    /// Remove a word. A learned word is remembered so it is never learned again.
    /// Replacements are left alone, whoever wrote them: `targeting` lists those
    /// that still produce the word, for `dictionary unreplace`.
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
    /// A stable fingerprint of every list and the auto-learn switch (each entry
    /// length-prefixed, see `stable_hash`), to tell whether the daemon's
    /// dictionary matches config.toml without sending a word.
    pub fn fingerprint(&self) -> u64 {
        let mut bytes = vec![u8::from(self.auto_learn)];
        let lists = [&self.words, &self.learned, &self.never_learn];
        let pairs = self.replacements.iter().flat_map(|r| [&r.from, &r.to]);
        for (i, list) in lists.into_iter().enumerate() {
            bytes.extend((list.len() as u64).to_le_bytes());
            bytes.push(i as u8);
            for entry in list {
                bytes.extend((entry.len() as u64).to_le_bytes());
                bytes.extend(entry.as_bytes());
            }
        }
        bytes.extend((self.replacements.len() as u64).to_le_bytes());
        for entry in pairs {
            bytes.extend((entry.len() as u64).to_le_bytes());
            bytes.extend(entry.as_bytes());
        }
        crate::stable_hash(&bytes)
    }
    /// Replacements whose result is `word` (case-insensitive).
    pub fn targeting(&self, word: &str) -> Vec<&Replacement> {
        let lower = word.to_lowercase();
        self.replacements
            .iter()
            .filter(|r| r.to.to_lowercase() == lower)
            .collect()
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
            // Punctuation, counters and words typed next to a fix are not part
            // of the term (independent test B1).
            (
                "set dev terms",
                "set DevTerms -> 2",
                "dev terms",
                "DevTerms",
            ),
            (
                "rename it to dev terms",
                "rename it to DevTerms = v2",
                "dev terms",
                "DevTerms",
            ),
            (
                "we use dev terms",
                "we use \"DevTerms\" = 1",
                "dev terms",
                "DevTerms",
            ),
            (
                "we use dev terms",
                "we use DevTerms 2",
                "dev terms",
                "DevTerms",
            ),
            (
                "set dev terms",
                "set DevTerms done",
                "dev terms",
                "DevTerms",
            ),
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
    fn learned_terms_are_well_formed_words() {
        // Never a counter, version or punctuation debris inside a term.
        assert_eq!(
            candidate(&["dev", "terms"], &["DevTerms", "2"]).unwrap_err(),
            "counter"
        );
        assert_eq!(
            candidate(&["dev", "terms"], &["DevTerms", "v2"]).unwrap_err(),
            "counter"
        );
        for (original, corrected) in [
            ("set dev terms", "set DevTerms -> 2"),
            ("we use dev terms", "we use \"DevTerms\" = 1"),
            ("rename to dev terms", "rename to DevTerms = v2 !!"),
            ("we use dev terms", "we use Dev  Terms -- x"),
        ] {
            for r in
                corrections(original, corrected, &DictionaryConfig::default()).unwrap_or_default()
            {
                assert!(
                    !r.to.contains("  ")
                        && r.to
                            .split(' ')
                            .all(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 2)
                        && r.to.chars().next().is_some_and(char::is_alphanumeric)
                        && r.to.chars().last().is_some_and(char::is_alphanumeric),
                    "{corrected:?} learned {:?}",
                    r.to
                );
            }
        }
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

    #[test]
    fn short_messages_with_a_respelled_name_are_still_the_dictation() {
        let name = pair("Rimpas or Kar", "Rimpa Sarkar");
        for (original, corrected) in [
            ("Rimpas or Kar, call me", "Rimpa Sarkar, call me"),
            ("Hi Rimpas or Kar", "Hi Rimpa Sarkar"),
            ("Call Rimpas or Kar", "Call Rimpa Sarkar"),
            ("Rimpas or Kar here", "Rimpa Sarkar here"),
            // Long sentences already passed on their words.
            ("Ask Rimpas or Kar to join", "Ask Rimpa Sarkar to join"),
            (
                "Thanks Rimpas or Kar for the update",
                "Thanks Rimpa Sarkar for the update",
            ),
        ] {
            assert_eq!(learn(original, corrected), name, "{original}");
        }
        // Four dictated words may make a name of one or two.
        assert_eq!(
            learn(
                "Thanks Rim pass or car for the update",
                "Thanks Rimpa Sarkar for the update"
            ),
            pair("Rim pass or car", "Rimpa Sarkar")
        );
        // A different short reply is not a correction of the dictation.
        for (original, corrected) in [
            ("Hi Rimpas or Kar", "Hi there"),
            ("Call Rimpas or Kar", "Call mom"),
            ("deploy kubernetis now", "lunch at noon with Kubernetes"),
        ] {
            assert!(!similar(original, corrected), "{corrected}");
            assert_eq!(
                corrections(original, corrected, &DictionaryConfig::default()).unwrap_err(),
                "dissimilar"
            );
        }
        // Ordinary edits keep their old results.
        assert_eq!(
            corrections(
                "Thanks Rimpa circa for the update",
                "Thanks Rimpa Sarkar for the update",
                &DictionaryConfig::default()
            )
            .unwrap_err(),
            "not-close"
        );
        assert_eq!(
            corrections(
                "we went their",
                "we went there",
                &DictionaryConfig::default()
            )
            .unwrap_err(),
            "not-close"
        );
    }

    #[test]
    fn four_word_names_only_and_never_a_plain_join() {
        assert_eq!(
            candidate(&["Rim", "pass", "or", "car"], &["Rimpa", "Sar", "Kar"]).unwrap_err(),
            "shape"
        );
        assert_eq!(
            candidate(&["a", "lot", "of", "fun"], &["alotoffun"]).unwrap_err(),
            "shape"
        );
        assert_eq!(
            candidate(&["see", "docs", "slash", "api", "slash"], &["docs/api"]).unwrap_err(),
            "shape"
        );
        assert!(learn("we had a lot of fun today", "we had alotoffun today").is_empty());
    }

    #[test]
    fn letter_fallback_is_bounded() {
        // One huge token (CJK text, an identifier) is never compared letter by letter.
        let long = "日".repeat(10_000);
        let started = std::time::Instant::now();
        assert!(!similar(&format!("{long} x"), &format!("{long}y z")));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        // Over 40 words, the word rule alone decides.
        let many: Vec<String> = (0..41).map(|i| format!("w{i}")).collect();
        assert!(!similar_letters(&many, &many, &lcs(&many, &many)));
        assert!(similar_letters(
            &many[..40],
            &many[..40],
            &lcs(&many[..40], &many[..40])
        ));
        // A respelled run over 64 letters keeps none of them.
        for (n, kept) in [(64, true), (65, false)] {
            let (a, b) = (["a".repeat(n)], [format!("{}b", "a".repeat(n - 1))]);
            assert_eq!(similar_letters(&a, &b, &lcs(&a, &b)), kept, "{n}");
        }
    }

    #[test]
    fn receipt_tells_a_retype_from_the_next_message() {
        let pasted = "Rimpas or Kar, call me ";
        let receipt = Receipt::new(pasted, pasted, None).unwrap();
        // Retyping the name keeps the rest of the message.
        assert_eq!(receipt.corrected("R, call me "), None);
        assert!(receipt.editing("R, call me ", pasted));
        assert!(receipt.editing("Rimpas or Kar, ", pasted));
        // Sent: a cleared field, a placeholder or a new message keeps no remnant.
        for field in ["", "Message #general", "Hi there", "R"] {
            assert!(!receipt.editing(field, pasted), "{field:?}");
        }
        let pasted = "I think the deploy of kubernetis failed";
        let receipt = Receipt::new(pasted, pasted, None).unwrap();
        for field in ["I ", "I need lunch", "The next message failed"] {
            assert!(!receipt.editing(field, pasted), "{field:?}");
        }
        let pasted = "ok so kubernetis is down ok";
        let receipt = Receipt::new(pasted, pasted, None).unwrap();
        assert!(!receipt.editing("ok", pasted));
        // An anchored dictation is being edited while its anchors are found once.
        let receipt = Receipt::new("Notes: we use postgress", "we use postgress", None).unwrap();
        assert!(receipt.editing("Notes: ", "we use postgress"));
        assert!(!receipt.editing("Other text", "we use postgress"));
        // The short-dictation guard holds for edits too.
        let receipt = Receipt::new("Rimpas or Kar", "Rimpas or Kar", None).unwrap();
        assert_eq!(receipt.span("Rimpa Sarkar"), None);
        assert!(!receipt.editing("Rimpa", "Rimpas or Kar"));
    }

    fn known(words: &[&str]) -> DictionaryConfig {
        DictionaryConfig {
            words: words.iter().map(|w| w.to_string()).collect(),
            ..DictionaryConfig::default()
        }
    }

    #[test]
    fn a_known_term_is_reported_with_its_stored_spelling_and_moves_first() {
        let mut dictionary = known(&["Postgres", "xflow", "Rimpa Sarkar"]);
        let original = "Hi Rimpas or Kar, how are you?";
        let corrected = "Hi rimpa sarkar, how are you?";
        assert_eq!(
            corrections(original, corrected, &dictionary).unwrap_err(),
            "known"
        );
        let found = review(original, corrected, &dictionary).unwrap();
        assert!(found.new.is_empty());
        assert_eq!(
            found.known,
            [Replacement {
                from: "Rimpas or Kar".into(),
                to: "Rimpa Sarkar".into()
            }]
        );
        assert_eq!(dictionary.promote(&found.known), ["Rimpa Sarkar"]);
        assert_eq!(dictionary.words, ["Rimpa Sarkar", "Postgres", "xflow"]);
        // Already first: nothing moves, nothing to save.
        assert!(dictionary.promote(&found.known).is_empty());
        // A removed word stays removed: not known, not new.
        dictionary.never_learn.push("kubernetes".into());
        assert_eq!(
            review("use kubernetis", "use Kubernetes", &dictionary).unwrap_err(),
            "known"
        );
    }

    fn rule(from: &str, to: &str) -> Replacement {
        Replacement {
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn a_second_correction_adds_only_a_narrow_exact_rule() {
        let mut dictionary = known(&["Rimpa Sarkar"]);
        let found = review("Hi Rimpas or Kar", "Hi Rimpa Sarkar", &dictionary).unwrap();
        assert_eq!(
            dictionary.derive(&found.known),
            [rule("Rimpas or Kar", "Rimpa Sarkar")]
        );
        assert_eq!(
            dictionary.replacements,
            [rule("Rimpas or Kar", "Rimpa Sarkar")]
        );
        // The same phrase again: already a rule.
        assert!(dictionary.derive(&found.known).is_empty());
        assert_eq!(dictionary.replacements.len(), 1);

        let refused = |dictionary: &DictionaryConfig, from: &str, to: &str| {
            assert_eq!(dictionary.phrase(&rule(from, to)), None, "{from} -> {to}");
        };
        let dictionary = known(&[
            "Rimpa Sarkar",
            "Claud",
            "adaws",
            "Rimpasorkar",
            "Sarkarion",
            "Kasa Bianca",
        ]);
        // Never a single word: "cloud" stays "cloud" everywhere.
        refused(&dictionary, "cloud", "Claud");
        refused(&dictionary, "casa", "Kasa Bianca");
        // Everyday or name outer words, a middle word that is not a function word,
        // no word of five letters, or not plain ASCII letters.
        refused(&dictionary, "Rimpas or car", "Rimpa Sarkar");
        refused(&dictionary, "ad AWS", "adaws");
        refused(&dictionary, "kar do", "Rimpa Sarkar");
        refused(&dictionary, "Rimpas aur Kar", "Rimpa Sarkar");
        refused(&dictionary, "Rimp or Kar", "Rimpa Sarkar");
        refused(&dictionary, "Rímpas or Kar", "Rimpa Sarkar");
        refused(&dictionary, "Rimpas, or Kar", "Rimpa Sarkar");
        // The same letters, or a phrase snap already restores.
        refused(&dictionary, "Rimpas or Kar", "Rimpasorkar");
        refused(&dictionary, "Sarkar ian", "Sarkarion");
        // Only a term the dictionary has, and never one the user removed.
        refused(&dictionary, "Rimpas or Kar", "Rimpa Sarkhar");
        let mut removed = known(&["Rimpa Sarkar"]);
        removed.never_learn.push("rimpa sarkar".into());
        refused(&removed, "Rimpas or Kar", "Rimpa Sarkar");
        // A manual rule for the phrase is never replaced, whatever its target.
        let mut manual = known(&["Rimpa Sarkar"]);
        manual.replacements.push(rule("rimpas  or kar", "Rimpa S."));
        refused(&manual, "Rimpas or Kar", "Rimpa Sarkar");
        for from in ["kar", "or Kar", "Hi Rimpas or Kar"] {
            let mut manual = known(&["Rimpa Sarkar"]);
            manual.replacements.push(rule(from, "Manual spelling"));
            refused(&manual, "Rimpas or Kar", "Rimpa Sarkar");
        }
        // A phrase holding another dictionary word is left to that word.
        refused(
            &known(&["Rimpa Sarkar", "Kar"]),
            "Rimpas or Kar",
            "Rimpa Sarkar",
        );
        // At most three rules per term and a hundred that target dictionary words.
        let mut capped = known(&["Rimpa Sarkar"]);
        for from in ["Rimpaz Sarkaz", "Rimpos Sarkor", "Rumpas Sarkur"] {
            capped.replacements.push(rule(from, "Rimpa Sarkar"));
        }
        refused(&capped, "Rimpas or Kar", "Rimpa Sarkar");
        let mut full = known(&["Rimpa Sarkar"]);
        for i in 0..MAX_DERIVED {
            let word = format!("Term{i}");
            full.words.push(word.clone());
            full.replacements
                .push(rule(&format!("termz{i} termx"), &word));
        }
        refused(&full, "Rimpas or Kar", "Rimpa Sarkar");
    }

    #[test]
    fn removing_a_word_keeps_and_names_the_rules_that_write_it() {
        let mut dictionary = known(&["Rimpa Sarkar"]);
        dictionary.learned = vec!["Rimpa Sarkar".into()];
        dictionary.replacements = vec![
            rule("Rimpas or Kar", "Rimpa Sarkar"),
            rule("gonna", "going to"),
        ];
        dictionary.forget("rimpa sarkar");
        assert!(dictionary.words.is_empty() && dictionary.learned.is_empty());
        assert_eq!(dictionary.never_learn, ["rimpa sarkar"]);
        assert_eq!(dictionary.replacements.len(), 2);
        assert_eq!(
            dictionary.targeting("Rimpa Sarkar"),
            [&rule("Rimpas or Kar", "Rimpa Sarkar")]
        );
    }

    #[test]
    fn fingerprint_is_stable_and_covers_every_list() {
        let base = known(&["Postgres"]);
        assert_eq!(base.fingerprint(), known(&["Postgres"]).fingerprint());
        let mut changed = vec![];
        let mut d = base.clone();
        d.learned.push("Postgres".into());
        changed.push(d);
        let mut d = base.clone();
        d.never_learn.push("postgres".into());
        changed.push(d);
        let mut d = base.clone();
        d.replacements.push(rule("post gres", "Postgres"));
        changed.push(d);
        let mut d = base.clone();
        d.auto_learn = false;
        changed.push(d);
        // Moving a word between lists changes it too.
        let mut d = DictionaryConfig::default();
        d.learned.push("Postgres".into());
        changed.push(d);
        for d in changed {
            assert_ne!(d.fingerprint(), base.fingerprint(), "{d:?}");
        }
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
