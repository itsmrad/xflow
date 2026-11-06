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
    /// Where the span sat in the last read (bytes): picks between repeats of an
    /// anchor only when one of them is exactly there.
    near: (usize, usize),
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
        // Only blank around it (an empty contenteditable reads "\n"): the field's edge.
        let edge = |anchor: &str| if anchor.trim().is_empty() { "" } else { anchor }.to_owned();
        Some(Self {
            before: edge(before),
            inserted: field[start..end].into(),
            after: edge(after),
            near: (start, end),
        })
    }
    /// Build the receipt before delivery from the exact selected range. No late
    /// substring search can accidentally adopt another field or an early edit.
    pub fn delivery(field: &str, selection: (usize, usize), inserted: &str) -> Option<Self> {
        let (start, len) = selection;
        let end = start.checked_add(len)?;
        let a = utf16_to_byte(field, start)?;
        let b = utf16_to_byte(field, end)?;
        let predicted = format!("{}{}{}", &field[..a], inserted, &field[b..]);
        Self::new(
            &predicted,
            inserted,
            Some(start.checked_add(inserted.encode_utf16().count())?),
        )
    }
    pub fn inserted(&self) -> &str {
        &self.inserted
    }
    /// The span in the field as it reads now, while it is still recognisably the
    /// dictation (see `similar`). See `span`.
    pub fn corrected<'a>(&self, field: &'a str) -> Option<&'a str> {
        self.span(field)
            .filter(|span| similar(&self.inserted, span))
    }
    /// The span in the field as it reads now, whether or not it still reads as
    /// the dictation. An empty anchor means the field's start or end. Any other
    /// anchor, or the part of it nearest the span when the user edited the rest,
    /// must occur once, or exactly where the span last sat; failing that, the
    /// dictation's own first or last words may mark that edge. Nothing else is
    /// searched for: anything ambiguous is no span at all.
    pub fn span<'a>(&self, field: &'a str) -> Option<&'a str> {
        let start = if self.before.is_empty() {
            0
        } else {
            self.start(field)?
        };
        let end = if self.after.is_empty() {
            field.len()
        } else {
            self.end(field)?
        };
        field.get(start..end)
    }
    fn start(&self, field: &str) -> Option<usize> {
        let chars = self.before.chars().count();
        anchor_parts(chars)
            .find_map(|n| {
                let cut = self.before.char_indices().nth(chars - n)?.0;
                let part = &self.before[cut..];
                once(field, part, self.near.0.checked_sub(part.len())).map(|at| at + part.len())
            })
            .or_else(|| {
                once(
                    field,
                    edge(self.inserted.trim_end(), false),
                    Some(self.near.0),
                )
            })
    }
    fn end(&self, field: &str) -> Option<usize> {
        anchor_parts(self.after.chars().count())
            .find_map(|n| {
                let cut = self
                    .after
                    .char_indices()
                    .nth(n)
                    .map_or(self.after.len(), |c| c.0);
                once(field, &self.after[..cut], Some(self.near.1))
            })
            .or_else(|| {
                let tail = edge(self.inserted.trim_end(), true);
                once(field, tail, None).map(|at| at + tail.len())
            })
    }
    /// Remember where the span sits now; call after each read that found it.
    pub fn track(&mut self, field: &str) {
        let start = if self.before.is_empty() {
            Some(0)
        } else {
            self.start(field)
        };
        let end = if self.after.is_empty() {
            Some(field.len())
        } else {
            self.end(field)
        };
        if let (Some(start), Some(end)) = (start, end) {
            self.near = (start, end);
        }
    }
    /// The field's own text of a delivery: what the app made of the text just
    /// sent, typography and a dropped trailing space included. Not an edit.
    pub fn delivered(&self, span: &str) -> bool {
        let keys = |s: &str| -> Vec<char> { folded(s.trim()).into_iter().map(|c| c.0).collect() };
        keys(&self.inserted) == keys(span)
    }
    /// The span the app shows for the text sent becomes the baseline.
    pub fn landed(&mut self, span: &str) {
        self.inserted = span.into();
    }
    /// A read that no longer looks like the dictation (see `corrected`) may be
    /// the user retyping part of it, or the next message. It counts as an edit
    /// in progress only while a context anchor is still found, or, for a
    /// dictation that filled the field, while the field keeps a whole word at
    /// the start or end of `last` (the last span that looked like the dictation):
    /// "R, call me" while "Rimpas or Kar, call me" is retyped; or reads as the
    /// start of it (see `retyping`). A placeholder or a different message keeps
    /// neither.
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
        (remnant > 0 && remnant + MAX_HUNK_WORDS + 1 >= a.len()) || retyping(span, last)
    }
    /// The dictation filled the field: nothing around it tells it apart from
    /// the next message typed there.
    pub fn whole_field(&self) -> bool {
        self.before.is_empty() && self.after.is_empty()
    }
}
/// How much of an anchor of `chars` characters to look for, nearest the span.
fn anchor_parts(chars: usize) -> impl Iterator<Item = usize> {
    std::iter::once(chars).chain([12, 8, 4].into_iter().filter(move |&n| n < chars))
}
/// The dictation's first or last words, at least six bytes: the edge of the span
/// when its anchor is gone. Empty when the text is too short to mean anything.
fn edge(text: &str, last: bool) -> &str {
    const MIN: usize = 6;
    if text.len() < MIN {
        return "";
    }
    let cuts = text
        .char_indices()
        .filter(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i);
    if last {
        let mut at = None;
        for cut in cuts {
            if text.len() - cut > MIN {
                at = Some(cut);
            }
        }
        at.map_or(text, |cut| text[cut..].trim_start())
    } else {
        cuts.into_iter()
            .find(|&cut| cut >= MIN)
            .map_or(text, |cut| &text[..cut])
    }
}
/// `needle`'s only occurrence, or, among several, the one exactly at `near`.
fn once(field: &str, needle: &str, near: Option<usize>) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let first = field.find(needle)?;
    let next = first + field[first..].chars().next()?.len_utf8();
    if !field[next..].contains(needle) {
        return Some(first);
    }
    near.filter(|&at| field.get(at..).is_some_and(|rest| rest.starts_with(needle)))
}
/// The field so far reads as the start of `last`, letter for letter but for the
/// fixes in progress: a select-all retype. Never proof by itself: the watch ends
/// on an empty field, and on a change that leaves such a read as it was.
fn retyping(span: &str, last: &str) -> bool {
    let letters = |s: &str| -> Vec<char> {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (typed, last) = (letters(span), letters(last));
    if typed.is_empty() || typed.len() > MAX_LETTER_TOTAL || last.len() > MAX_LETTER_TOTAL {
        return false;
    }
    let n = typed.len().min(last.len());
    distance(&typed, &last[..n]) * 3 <= n
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
                        let source = c.from.to_lowercase();
                        if !list.iter().any(|f| {
                            f.to.to_lowercase() == lower && f.from.to_lowercase() == source
                        }) {
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
    token.trim_matches(|c: char| {
        !c.is_alphanumeric() && unicode_normalization::char::canonical_combining_class(c) == 0
    })
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
    // Combining marks (virama, nukta, accents) belong to their word.
    let mark = |c: char| unicode_normalization::char::canonical_combining_class(c) != 0;
    let plain = from
        .chars()
        .all(|c| c.is_alphabetic() || mark(c) || matches!(c, ' ' | '\'' | '’' | '-'));
    let term = to.chars().all(|c| {
        c.is_alphanumeric() || mark(c) || matches!(c, ' ' | '\'' | '’' | '-' | '.' | '_' | '/')
    });
    // Canonically equal spellings are not a correction.
    let fold = |s: &str| -> String {
        use unicode_normalization::UnicodeNormalization;
        s.nfc()
            .filter(|c| c.is_alphanumeric() || mark(*c) || *c == ' ')
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
    } else if (to.split(' ').all(common) || everyday) && !uncommon_source(&from) {
        // Tier 3: both common. A swap between real words is a choice, not a spelling.
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
/// Tier 1 rather than Tier 2: the dictated words hold one no everyday list or
/// name list knows (three letters or more, so a stray "d" does not count). Absence
/// from a list is not proof of a misspelling; `DictionaryConfig::adopt` keeps
/// such a save reversible and says so.
pub fn uncommon_source(from: &str) -> bool {
    from.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .any(|w| {
            w.chars().count() >= 3
                && !crate::words::common(&w)
                && !crate::words::name(&w)
                && !common(&w)
                && !crate::snap::FUNCTION_WORDS.contains(&w.as_str())
        })
}
/// Everyday words, very common names and acronyms are never learned as a choice
/// between two of them (Wispr skips them too).
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

/// Correction rules learning may add for one term, and in all. Counted
/// conservatively: every replacement that targets a dictionary word counts,
/// whoever wrote it.
const MAX_RULES_PER_TERM: usize = 3;
const MAX_RULES: usize = 100;
/// Corrections kept in memory, waiting to repeat (see `DictionaryConfig::adopt`).
pub const MAX_SEEN: usize = 32;

/// What one reviewed correction changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Adopted {
    /// Newly saved words, each once.
    pub added: Vec<String>,
    /// Literal rules added.
    pub rules: Vec<Replacement>,
    /// Known words moved to the front (see `promote`).
    pub promoted: Vec<String>,
    /// A rule was earned but a cap or a conflict refused it; the word still counts.
    pub refused: bool,
}
impl Adopted {
    /// What of this is still earned by `kept`, the corrections staying saved.
    pub fn keeping(&self, kept: &[Replacement]) -> Self {
        let targeted = |word: &String| {
            kept.iter()
                .any(|c| c.to.to_lowercase() == word.to_lowercase())
        };
        Self {
            added: self.added.iter().filter(|w| targeted(w)).cloned().collect(),
            rules: self
                .rules
                .iter()
                .filter(|r| kept.iter().any(|c| same_pair(c, r)))
                .cloned()
                .collect(),
            promoted: self
                .promoted
                .iter()
                .filter(|w| targeted(w))
                .cloned()
                .collect(),
            refused: false,
        }
    }
}
/// The same correction, however its case was stored.
pub fn same_pair(a: &Replacement, b: &Replacement) -> bool {
    a.from.to_lowercase() == b.from.to_lowercase() && a.to.to_lowercase() == b.to.to_lowercase()
}
/// Of the corrections a watch already saved, those the field no longer shows as
/// saved: edited again at the same words (`now`, what it shows now, has another
/// correction of them), or taken back (the dictated words are there again and the
/// saved ones are not). A correction merely edited away, by a rewrite or a
/// deletion, stays: nothing says its spelling was wrong.
pub fn superseded(saved: &[Replacement], now: &[Replacement], span: &str) -> Vec<Replacement> {
    let lower = |s: &str| -> Vec<String> {
        crate::text::words(s)
            .iter()
            .map(|w| w.text.to_lowercase())
            .collect()
    };
    let shown = lower(span);
    saved
        .iter()
        .filter(|d| {
            if now.iter().any(|c| same_pair(c, d)) {
                return false;
            }
            let from = lower(&d.from);
            now.iter()
                .any(|c| lower(&c.from).iter().any(|w| from.contains(w)))
                || (from.iter().all(|w| shown.contains(w))
                    && !lower(&d.to).iter().all(|w| shown.contains(w)))
        })
        .cloned()
        .collect()
}
impl DictionaryConfig {
    /// Apply again what an earlier save did, without judging it again.
    pub fn replay(&mut self, adopted: &Adopted) {
        for word in &adopted.promoted {
            if let Some(i) = self.words.iter().position(|w| w == word) {
                let word = self.words.remove(i);
                self.words.insert(0, word);
            }
        }
        for word in &adopted.added {
            if !self.words.contains(word) {
                self.words.insert(0, word.clone());
            }
            if !self.learned.contains(word) {
                self.learned.push(word.clone());
            }
        }
        self.replacements.extend(adopted.rules.iter().cloned());
    }
    /// Save what a reviewed correction earned, by tier:
    /// 1. An uncommon source (see `uncommon_source`) and a close spelling: the
    ///    word and the literal rule at once, so the next dictation is right
    ///    whatever the recognizer does with hints.
    /// 2. A common source and an uncommon destination ("cloud" -> "Claude"): the
    ///    word only. A rule would rewrite an everyday word in every transcript,
    ///    so it waits for the same correction to repeat. `seen` is the memory of
    ///    those first corrections; it is never persisted, so a restart only
    ///    means one more repeat.
    /// 3. Everything else never reaches here (see `candidate`).
    pub fn adopt(&mut self, found: &Corrections, seen: &mut Vec<Replacement>) -> Adopted {
        let mut done = Adopted {
            promoted: self.promote(&found.known),
            ..Adopted::default()
        };
        for c in found.known.iter().chain(&found.new) {
            let known = self.words.contains(&c.to);
            if !known {
                if self.words.len() >= MAX_DICTIONARY_ITEMS {
                    continue;
                }
                self.words.insert(0, c.to.clone());
                if !self.learned.contains(&c.to) {
                    self.learned.push(c.to.clone());
                }
            }
            if !known && !done.added.contains(&c.to) {
                done.added.push(c.to.clone());
            }
            let pair =
                |s: &Replacement| s.from.to_lowercase() == c.from.to_lowercase() && s.to == c.to;
            let earned = uncommon_source(&c.from) || (known && seen.iter().any(pair));
            if !earned {
                if !seen.iter().any(pair) {
                    if seen.len() >= MAX_SEEN {
                        seen.remove(0);
                    }
                    seen.push(c.clone());
                }
                continue;
            }
            seen.retain(|s| !pair(s));
            match self.rule_conflict(c) {
                None => {
                    self.replacements.push(c.clone());
                    done.rules.push(c.clone());
                }
                Some(Conflict::Present) => {}
                Some(Conflict::Refused) => done.refused = true,
            }
        }
        done
    }
    /// Why `c` may not become a literal rule. Rules have no provenance, so every
    /// existing rule counts and none is ever replaced. Never a source that is a
    /// dictionary word, or that any rule writes; never a target another rule
    /// rewrites; never overlapping another rule's source: an output is never
    /// rewritten, whichever pass reaches it (see `text::reapply`).
    fn rule_conflict(&self, c: &Replacement) -> Option<Conflict> {
        let words = |s: &str| -> Vec<String> {
            crate::text::words(s)
                .iter()
                .map(|w| w.text.to_lowercase())
                .collect()
        };
        let (from, to) = (words(&c.from), words(&c.to));
        if from.is_empty() {
            return Some(Conflict::Refused);
        }
        let inside = |needle: &[String], hay: &[String]| {
            !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
        };
        let lower = c.to.to_lowercase();
        if self
            .replacements
            .iter()
            .any(|r| words(&r.from) == from && r.to.to_lowercase() == lower)
        {
            return Some(Conflict::Present);
        }
        let per_term = self
            .replacements
            .iter()
            .filter(|r| r.to.to_lowercase() == lower)
            .count();
        let targets = |r: &Replacement| {
            self.words
                .iter()
                .any(|w| w.to_lowercase() == r.to.to_lowercase())
        };
        if inside(&from, &to) {
            return Some(Conflict::Refused);
        }
        let clash = self.replacements.iter().any(|r| {
            let (f, t) = (words(&r.from), words(&r.to));
            inside(&from, &f)
                || inside(&f, &from)
                || from.iter().any(|w| t.contains(w))
                || f.iter().any(|w| to.contains(w))
        });
        // A word the dictionary keeps is never rewritten, even inside a phrase.
        let holds_word = self
            .words
            .iter()
            .any(|w| w.to_lowercase() != lower && inside(&words(w), &from));
        let removed = self.never_learn.iter().any(|w| w.to_lowercase() == lower);
        (clash
            || holds_word
            || removed
            || per_term >= MAX_RULES_PER_TERM
            || self.replacements.iter().filter(|r| targets(r)).count() >= MAX_RULES
            || self.replacements.len() >= MAX_DICTIONARY_ITEMS)
            .then_some(Conflict::Refused)
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
    /// Remove a word and every rule that writes it, whoever added them, and say
    /// how many rules went. A learned word is remembered so it is never learned
    /// again.
    pub fn forget(&mut self, word: &str) -> usize {
        self.words.retain(|w| w != word);
        let lower = word.to_lowercase();
        let rules = self.replacements.len();
        self.replacements.retain(|r| r.to.to_lowercase() != lower);
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
        rules - self.replacements.len()
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
}
enum Conflict {
    /// The same rule exists.
    Present,
    Refused,
}

/// How soon a correction may be saved after its words stopped changing.
pub const COMMIT_QUIET_MS: u64 = 2_000;
/// With the caret still on the corrected word, nothing says it is finished. A
/// word at least as long as the one it corrects, left alone this long, is. A
/// shorter one might be `Veronic` on its way to `Veronica`: it waits for the
/// caret to move or for the user to leave. Calibration knob.
pub const COMMIT_ON_WORD_MS: u64 = 4_000;
/// A word left this recently, caret still on it, may be half typed. One shorter
/// than what it corrects is held back however long it sat (`complete_on_caret`).
pub const LEAVE_QUIET_MS: u64 = 500;

/// What the corrections found between `original` and `span` are, for timing
/// only: the dictionary still decides what is saved.
pub fn pending(original: &str, span: &str) -> Vec<Replacement> {
    match review(original, span, &DictionaryConfig::default()) {
        Ok(found) => found.new,
        Err(_) => vec![],
    }
}
/// The word the caret touches in `field`, lowercase; None off any word.
pub fn word_at(field: &str, caret: usize) -> Option<String> {
    let at = utf16_to_byte(field, caret)?;
    let (before, after) = field.split_at(at);
    let inside = |c: &char| {
        c.is_alphanumeric() || unicode_normalization::char::canonical_combining_class(*c) != 0
    };
    let head: String = before.chars().rev().take_while(inside).collect();
    let tail: String = after.chars().take_while(inside).collect();
    let word: String = head
        .chars()
        .rev()
        .chain(tail.chars())
        .flat_map(char::to_lowercase)
        .collect();
    (!word.is_empty()).then_some(word)
}
/// Whether the caret is on one of `found`'s corrected words.
pub fn caret_on(found: &[Replacement], caret: Option<&str>) -> bool {
    caret.is_some_and(|word| {
        found.iter().any(|c| {
            c.to.to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| w == word)
        })
    })
}
/// A corrected word on which the caret still rests may be saved after
/// COMMIT_ON_WORD_MS only if it is not shorter than what it corrects.
pub fn complete_on_caret(found: &[Replacement]) -> bool {
    found
        .iter()
        .all(|c| c.to.chars().count() >= c.from.chars().count())
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
    fn adopt(dictionary: &mut DictionaryConfig, original: &str, corrected: &str) -> Adopted {
        let found = review(original, corrected, dictionary).unwrap();
        dictionary.adopt(&found, &mut vec![])
    }
    #[test]
    fn a_saved_word_is_known_and_removed_words_stay_removed() {
        let mut dictionary = DictionaryConfig::default();
        let done = adopt(&mut dictionary, "use kubernetis", "use Kubernetes");
        assert_eq!(done.added, ["Kubernetes"]);
        assert_eq!(dictionary.words, ["Kubernetes"]);
        assert_eq!(dictionary.learned, ["Kubernetes"]);
        assert_eq!(
            adopt(&mut dictionary, "we use dev terms", "we use DevTerms").added,
            ["DevTerms"]
        );
        assert_eq!(
            corrections("use kubernetis", "use Kubernetes", &dictionary).unwrap_err(),
            "known"
        );
        assert_eq!(dictionary.forget("Kubernetes"), 1);
        assert_eq!(dictionary.words, ["DevTerms"]);
        assert_eq!(dictionary.learned, ["DevTerms"]);
        assert!(corrections("use kubernetis", "use Kubernetes", &dictionary).is_err());
        assert!(corrections("use Kubernetes", "use kubernetis", &dictionary).is_err());
    }
    /// The approved policy, on the baseline's words. Tier 3 returns no candidate.
    #[test]
    fn corrections_earn_by_tier() {
        // Tier 1: uncommon source, close spelling: word and rule at once.
        for (from, to) in [
            ("Veronika", "Veronica"),
            ("Zorblax", "Zorblex"),
            ("definately", "definitely"),
            ("Rimpas or Kar", "Rimpa Sarkar"),
            ("post gres", "Postgres"),
            ("kubernetis", "Kubernetes"),
        ] {
            let mut d = DictionaryConfig::default();
            let done = adopt(
                &mut d,
                &format!("ask {from} today"),
                &format!("ask {to} today"),
            );
            assert_eq!(done.added, [to], "{from}");
            assert_eq!(done.rules, [rule(from, to)], "{from}");
            assert_eq!(d.replacements, [rule(from, to)], "{from}");
        }
        // Tier 2: common source, uncommon destination: word only, rule on repeat.
        for (from, to) in [
            ("cloud", "Claude"),
            ("Linear", "Linea"),
            ("Brian", "Bryan"),
            ("re act", "React"),
        ] {
            let mut d = DictionaryConfig::default();
            let mut seen = vec![];
            let (a, b) = (format!("ask {from} today"), format!("ask {to} today"));
            let done = d.adopt(&review(&a, &b, &d).unwrap(), &mut seen);
            assert_eq!(done.added, [to], "{from}");
            assert!(done.rules.is_empty() && d.replacements.is_empty(), "{from}");
            // The same correction again: its own rule.
            let done = d.adopt(&review(&a, &b, &d).unwrap(), &mut seen);
            assert!(done.added.is_empty(), "{from}");
            assert_eq!(done.rules, [rule(from, to)], "{from}");
            assert!(seen.is_empty(), "{from}");
        }
        // A different source for the same word is not a repeat.
        let mut d = DictionaryConfig::default();
        let mut seen = vec![];
        d.adopt(&review("ask cloud", "ask Claude", &d).unwrap(), &mut seen);
        let done = d.adopt(&review("ask clued", "ask Claude", &d).unwrap(), &mut seen);
        assert!(done.rules.is_empty());
        // A word added by hand: its first correction from a common source is a first.
        let mut d = known(&["Claude"]);
        let mut seen = vec![];
        assert!(d
            .adopt(&review("ask cloud", "ask Claude", &d).unwrap(), &mut seen)
            .rules
            .is_empty());
        assert_eq!(
            d.adopt(&review("ask cloud", "ask Claude", &d).unwrap(), &mut seen)
                .rules
                .len(),
            1
        );
    }
    /// Tier 3: never a word, never a rule.
    #[test]
    fn hard_negatives_save_nothing() {
        for (original, corrected) in [
            // Rewrite, deletion, addition.
            (
                "Please send the report before lunch",
                "We should postpone everything until Monday",
            ),
            (
                "Please bring the small cat home tomorrow",
                "Please bring the cat home tomorrow",
            ),
            (
                "Please bring the cat home tomorrow",
                "Please bring the small cat home tomorrow",
            ),
            // Both common, or not close.
            (
                "Please bring the cat home tomorrow",
                "Please bring the dog home tomorrow",
            ),
            (
                "Please bring the cat home tomorrow",
                "Please bring the car home tomorrow",
            ),
            ("we deploy it", "we deployed it"),
            ("it will affect us", "it will effect us"),
            ("we went their", "we went there"),
            ("meet Rimpa circa soon", "meet Rimpa Sarkar soon"),
            // Case and punctuation only, also canonically equal spellings.
            (
                "Please ask veronica about tomorrow plans",
                "Please ask Veronica, about tomorrow plans!",
            ),
            ("send the e-mail", "send the email"),
            ("visit the cafe\u{301} today", "visit the caf\u{e9} today"),
            // Unchanged, or a completion typed after the dictation.
            ("Please ask Veronika", "Please ask Veronika"),
            ("Please ask Veronika", "Please ask Veronika about Veronica"),
            ("a very long", "a very long term of Kubernetes"),
        ] {
            let mut d = DictionaryConfig::default();
            if let Ok(found) = review(original, corrected, &d) {
                let done = d.adopt(&found, &mut vec![]);
                assert_eq!(done, Adopted::default(), "{original} -> {corrected}");
            }
            assert!(d.words.is_empty() && d.replacements.is_empty());
        }
    }
    #[test]
    fn several_corrections_all_count_and_aliases_of_one_word_are_kept() {
        let mut d = DictionaryConfig::default();
        let done = adopt(
            &mut d,
            "Please ask Veronika and Zorblax tomorrow",
            "Please ask Veronica and Zorblex tomorrow",
        );
        assert_eq!(done.added.len(), 2);
        assert_eq!(done.rules.len(), 2);
        let mut d = DictionaryConfig::default();
        let done = adopt(
            &mut d,
            "Kuberntes and Kubernetis today",
            "Kubernetes and Kubernetes today",
        );
        assert_eq!(done.added, ["Kubernetes"]);
        assert_eq!(done.rules.len(), 2);
    }
    #[test]
    fn rules_are_capped_and_never_collide() {
        let pair = rule("Rimpas or Kar", "Rimpa Sarkar");
        let refused = |d: &DictionaryConfig, c: &Replacement| {
            assert!(
                matches!(d.rule_conflict(c), Some(Conflict::Refused)),
                "{c:?}"
            );
        };
        let d = known(&["Rimpa Sarkar"]);
        assert!(d.rule_conflict(&pair).is_none());
        // The same rule is no change.
        let mut same = known(&["Rimpa Sarkar"]);
        same.replacements
            .push(rule("rimpas  or kar", "Rimpa Sarkar"));
        assert!(matches!(same.rule_conflict(&pair), Some(Conflict::Present)));
        // A manual rule for the phrase is never replaced, whatever its target.
        let mut manual = known(&["Rimpa Sarkar"]);
        manual.replacements.push(rule("rimpas  or kar", "Rimpa S."));
        refused(&manual, &pair);
        for from in ["kar", "or Kar", "Hi Rimpas or Kar"] {
            let mut manual = known(&["Rimpa Sarkar"]);
            manual.replacements.push(rule(from, "Manual spelling"));
            refused(&manual, &pair);
        }
        // Chains: a source some rule writes, a target some rule rewrites, a cycle.
        let mut chain = known(&["Beta"]);
        chain.replacements.push(rule("Alpha", "Zorblax"));
        refused(&chain, &rule("Zorblax", "Beta"));
        chain.replacements = vec![rule("Zorblex", "Gamma")];
        refused(&chain, &rule("Zorblax", "Zorblex"));
        // A phrase holding another dictionary word is left to that word; a word
        // the user removed stays removed.
        refused(&known(&["Rimpa Sarkar", "Kar"]), &pair);
        let mut removed = known(&["Rimpa Sarkar"]);
        removed.never_learn.push("rimpa sarkar".into());
        refused(&removed, &pair);
        // At most three rules per term and a hundred that target dictionary words.
        let mut capped = known(&["Rimpa Sarkar"]);
        for from in ["Rimpaz Sarkaz", "Rimpos Sarkor", "Rumpas Sarkur"] {
            capped.replacements.push(rule(from, "Rimpa Sarkar"));
        }
        refused(&capped, &pair);
        let mut full = known(&["Rimpa Sarkar"]);
        for i in 0..MAX_RULES {
            let word = format!("Term{i}");
            full.words.push(word.clone());
            full.replacements
                .push(rule(&format!("termz{i} termx"), &word));
        }
        refused(&full, &pair);
        // Refused still saves the word and says so.
        let mut d = DictionaryConfig {
            words: vec!["Zorblex".into()],
            replacements: (0..3)
                .map(|i| rule(&format!("zorb{i}x"), "Zorblex"))
                .collect(),
            ..DictionaryConfig::default()
        };
        let done = d.adopt(
            &review("ask Zorblax", "ask Zorblex", &d).unwrap(),
            &mut vec![],
        );
        assert!(done.refused && done.rules.is_empty());
    }
    #[test]
    fn non_latin_and_combining_spellings_are_compared_whole() {
        // Hindi with a virama: the mark is part of the word, not punctuation.
        let (from, to) = ("कृष्णन", "कृष्णा");
        let found = review(
            &format!("ask {from} now"),
            &format!("ask {to} now"),
            &DictionaryConfig::default(),
        );
        assert_eq!(found.unwrap().new, [rule(from, to)]);
        assert!(uncommon_source("Sarkaar"));
        assert!(!uncommon_source("the car"));
        assert!(!uncommon_source("an d or"));
    }
    #[test]
    fn the_caret_decides_when_a_correction_is_finished() {
        let found = pending("Please ask Veronika today", "Please ask Veronica today");
        assert_eq!(found, [rule("Veronika", "Veronica")]);
        let field = "Please ask Veronica today";
        let on = |caret| caret_on(&found, word_at(field, caret).as_deref());
        assert!(on(19), "at its end");
        assert!(on(11), "at its start");
        assert!(on(15), "inside");
        assert!(!on(20), "on the next word");
        assert!(!on(5));
        assert!(!on(25));
        assert_eq!(word_at("a  b", 2), None);
        // Typing on after the fix is not on the fixed word.
        assert!(!caret_on(
            &pending("ask Veronika", "ask Veronica and din"),
            word_at("ask Veronica and din", 20).as_deref()
        ));
        // A shorter word may be half typed; one at least as long is not held back.
        assert!(complete_on_caret(&found));
        assert!(!complete_on_caret(&pending(
            "ask Veronika today",
            "ask Veronic today"
        )));
        assert!(!pending("ask Veronika today", "ask Veronic today").is_empty());
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
        // A short dictation that filled the field is still told from another text.
        let receipt = Receipt::new("use postgress", "use postgress", None).unwrap();
        assert!(receipt.corrected("lunch with Postgres").is_none());
        assert_eq!(receipt.corrected("use Postgres"), Some("use Postgres"));
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
        for field in ["", "Message #general", "Hi there"] {
            assert!(!receipt.editing(field, pasted), "{field:?}");
        }
        // A first key may start a retype; the watch's continuity rules decide
        // what happens next (see `a_select_all_retype_learns_unless_the_field_emptied`).
        assert!(receipt.editing("R", pasted));
        let pasted = "I think the deploy of kubernetis failed";
        let receipt = Receipt::new(pasted, pasted, None).unwrap();
        for field in ["I need lunch", "The next message failed"] {
            assert!(!receipt.editing(field, pasted), "{field:?}");
        }
        assert!(receipt.editing("I thi", pasted));
        // An anchored dictation is being edited while its anchors are found once.
        let receipt = Receipt::new("Notes: we use postgress", "we use postgress", None).unwrap();
        assert!(receipt.editing("Notes: ", "we use postgress"));
        assert!(!receipt.editing("Other text", "we use postgress"));
        // A short dictation is compared like any other.
        let receipt = Receipt::new("Rimpas or Kar", "Rimpas or Kar", None).unwrap();
        assert_eq!(receipt.span("Rimpa Sarkar"), Some("Rimpa Sarkar"));
        assert_eq!(receipt.corrected("lunch at noon"), None);
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
    fn a_saved_correction_is_superseded_only_at_its_own_words() {
        let saved = [rule("kubernetis", "Kubernete"), rule("Zorblax", "Zorblex")];
        let check =
            |span: &str| superseded(&saved, &pending("use kubernetis and Zorblax", span), span);
        // Unchanged.
        assert!(check("use Kubernete and Zorblex").is_empty());
        // Edited again at the same words.
        assert_eq!(check("use Kubernetes and Zorblex"), [saved[0].clone()]);
        // Taken back.
        assert_eq!(check("use kubernetis and Zorblex"), [saved[0].clone()]);
        assert_eq!(check("use kubernetis and Zorblax"), saved);
        // Deleted or rewritten away: nothing says the spelling was wrong.
        assert!(check("use and Zorblex").is_empty());
        assert!(check("something else entirely").is_empty());
        // Phrases overlap on any word.
        let saved = [rule("Rimpas", "Rimpa")];
        let now = pending("Rimpas or Kar call", "Rimpa Sarkar call");
        assert_eq!(superseded(&saved, &now, "Rimpa Sarkar call"), saved);
        // Case does not make it another correction.
        assert!(same_pair(
            &rule("cloud", "Claude"),
            &rule("Cloud", "claude")
        ));
    }
    #[test]
    fn replaying_what_stays_saved_reproduces_it() {
        let mut d = known(&["Base"]);
        let mut seen = vec![];
        let a = d.adopt(
            &review("ask Veronika", "ask Veronica", &d).unwrap(),
            &mut seen,
        );
        let b = d.adopt(
            &review("ask Zorblax", "ask Zorblex", &d).unwrap(),
            &mut seen,
        );
        let kept = [rule("Zorblax", "Zorblex")];
        let mut rebuilt = known(&["Base"]);
        rebuilt.replay(&a.keeping(&kept));
        rebuilt.replay(&b.keeping(&kept));
        assert_eq!(rebuilt.words, ["Zorblex", "Base"]);
        assert_eq!(rebuilt.learned, ["Zorblex"]);
        assert_eq!(rebuilt.replacements, kept);
        // An alias keeps a word its sibling also earned.
        let adopted = Adopted {
            added: vec!["Kubernetes".into()],
            rules: vec![
                rule("Kubernetis", "Kubernetes"),
                rule("Kuberntes", "Kubernetes"),
            ],
            ..Adopted::default()
        };
        let kept = adopted.keeping(&[rule("Kuberntes", "Kubernetes")]);
        assert_eq!(kept.added, ["Kubernetes"]);
        assert_eq!(kept.rules, [rule("Kuberntes", "Kubernetes")]);
    }
    #[test]
    fn deleting_a_word_takes_every_rule_that_writes_it_and_says_how_many() {
        let mut dictionary = known(&["Rimpa Sarkar"]);
        dictionary.learned = vec!["Rimpa Sarkar".into()];
        dictionary.replacements = vec![
            rule("Rimpas or Kar", "Rimpa Sarkar"),
            rule("Rim pass", "rimpa sarkar"),
            rule("gonna", "going to"),
        ];
        assert_eq!(dictionary.forget("rimpa sarkar"), 2);
        assert!(dictionary.words.is_empty() && dictionary.learned.is_empty());
        assert_eq!(dictionary.never_learn, ["rimpa sarkar"]);
        assert_eq!(dictionary.replacements, [rule("gonna", "going to")]);
        // A word added by hand goes with its rules too, and may be learned again.
        let mut dictionary = known(&["Kubernetes"]);
        dictionary.replacements = vec![rule("kube", "Kubernetes")];
        assert_eq!(dictionary.forget("Kubernetes"), 1);
        assert!(dictionary.never_learn.is_empty() && dictionary.replacements.is_empty());
        assert_eq!(DictionaryConfig::default().forget("absent"), 0);
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

#[cfg(test)]
mod delivery_tests {
    use super::*;
    #[test]
    fn prepared_receipt_survives_early_edit_and_uses_selected_utf16_span() {
        let r = Receipt::delivery("😀 header old tail", (10, 3), "Veronika").unwrap();
        assert_eq!(r.corrected("😀 header Veronica tail"), Some("Veronica"));
        assert!(Receipt::delivery("😀", (1, 0), "x").is_none());
        assert!(Receipt::delivery("x", (usize::MAX, 1), "x").is_none());
    }
    const DICTATED: &str = "Please ask Veronika about tomorrow plans ";
    /// T14: a change inside the 16-character anchor, plus the spelling fix.
    #[test]
    fn an_edited_anchor_still_finds_the_span_by_what_is_left_of_it() {
        let r = Receipt::delivery("Some context: ", (14, 0), DICTATED).unwrap();
        assert_eq!(r.inserted(), DICTATED);
        let fixed = "Please ask Veronica about tomorrow plans ";
        assert_eq!(r.corrected(&format!("Other context: {fixed}")), Some(fixed));
        // Nothing of the anchor left: the dictation's own first word marks the edge.
        assert_eq!(r.corrected(&format!("Rewritten> {fixed}")), Some(fixed));
        // The first word fixed too: no edge at all.
        assert_eq!(
            r.corrected("Rewritten> Pleese ask Veronica about tomorrow plans "),
            None
        );
        // The end works the same way, also after a fixed last word.
        let r =
            Receipt::delivery("Before.  After the text", (8, 0), "ask Veronika tomorrow").unwrap();
        assert_eq!(
            r.corrected("Before. ask Veronica tomorrow  after the text"),
            Some("ask Veronica tomorrow")
        );
        assert_eq!(
            r.corrected("Before. ask Veronica tomorrow  xxxxxxxxxxxxxx"),
            Some("ask Veronica tomorrow")
        );
    }
    /// T03: a short dictation that filled the field is compared like any other.
    #[test]
    fn a_short_field_is_found_but_a_different_text_is_not_it() {
        let r = Receipt::delivery("", (0, 0), "Hi Veronika ").unwrap();
        assert!(r.whole_field());
        assert_eq!(r.corrected("Hi Veronica "), Some("Hi Veronica "));
        assert_eq!(r.corrected("lunch at noon anyone"), None);
    }
    /// An empty contenteditable reads as a lone newline; the paste then has no
    /// real neighbour, and a fixed last word must not lose the span.
    #[test]
    fn blank_context_is_the_field_edge() {
        let r = Receipt::delivery("\n", (0, 0), "Hi Veronika ").unwrap();
        assert!(r.whole_field());
        assert!(r.delivered("Hi Veronika\u{a0}"));
        assert_eq!(r.corrected("Hi Veronica\u{a0}"), Some("Hi Veronica\u{a0}"));
        assert!(r.editing("Hi Ver", "Hi Veronika "));
    }
    /// A repeated anchor only resolves exactly where the span last sat.
    #[test]
    fn repeated_context_needs_the_span_where_it_was() {
        let filler = "synthetic filler ".repeat(4);
        let mut r = Receipt::delivery(&filler, (filler.len(), 0), "ask Veronika today").unwrap();
        let field = format!("{filler}ask Veronica today");
        assert_eq!(r.corrected(&field), Some("ask Veronica today"));
        r.track(&field);
        // The same anchor text moved: ambiguous, so no span.
        assert_eq!(r.corrected(&format!("x{filler}ask Veronica today")), None);
    }
    /// Typography an app applied to the delivered text is the delivery, not an edit.
    #[test]
    fn what_the_app_made_of_the_text_is_the_baseline() {
        let r = Receipt::delivery("", (0, 0), "don't ask Veronika ").unwrap();
        assert!(r.delivered("don’t ask Veronika\u{a0}"));
        assert!(r.delivered("Don't ask Veronika"));
        assert!(!r.delivered("don't ask Veronica "));
    }
}
