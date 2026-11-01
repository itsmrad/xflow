//! Spoken self-correction ("backtrack", #75): "at 3, sorry, at 5" keeps only "at 5".
//! Deterministic and local, and only when certain: anything unclear stays verbatim
//! for the model (`Outcome::Ambiguous`); traps such as "I mean it" are no cue.
use crate::text::{tidy, words, Word};

const CUES: [&[&str]; 12] = [
    &["scratch", "that"],
    &["never", "mind"],
    &["no", "wait"],
    &["wait", "no"],
    &["no", "no"],
    &["i", "mean"],
    &["i", "meant"],
    &["make", "that"],
    &["correction"],
    &["sorry"],
    &["actually"],
    &["no"],
];
const NUMBERS: [&str; 31] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "thousand",
    "million",
];
const DETERMINERS: [&str; 16] = [
    "the", "a", "an", "this", "that", "my", "your", "his", "her", "our", "their", "its", "any",
    "every", "each", "no",
];
const FUNCTION: [&str; 26] = [
    "i", "a", "an", "the", "to", "of", "in", "on", "at", "and", "or", "it", "is", "we", "you",
    "he", "she", "they", "will", "be", "for", "with", "that", "this", "so", "but",
];
/// "the phrase scratch that", "I said never mind": the cue is mentioned, not meant.
const MENTIONS: [&str; 10] = [
    "phrase", "word", "words", "say", "says", "said", "saying", "called", "term", "like",
];
/// "no, no one came", "no, no problem": negation, not a retraction.
const NO_TRAPS: [&str; 10] = [
    "one", "body", "thing", "where", "longer", "more", "way", "problem", "doubt", "matter",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Unchanged,
    Edited,
    /// A cue remains whose retracted words aren't certain: model cleanup should
    /// run even for a short dictation.
    Ambiguous,
    /// The dictation ended with "scratch that": deliver nothing.
    Discarded,
}

/// The text with every certain self-correction applied.
pub fn apply(text: &str) -> (String, Outcome) {
    let mut edited = text.to_owned();
    for _ in 0..8 {
        match pass(&edited) {
            (Some(next), _) => edited = next,
            (None, ambiguous) => {
                let outcome = if ambiguous {
                    Outcome::Ambiguous
                } else if edited.is_empty() && !text.trim().is_empty() {
                    Outcome::Discarded
                } else if edited != text {
                    Outcome::Edited
                } else {
                    Outcome::Unchanged
                };
                return (edited, outcome);
            }
        }
    }
    // More repairs than any real dictation has: not certain, leave it to the model.
    (text.to_owned(), Outcome::Ambiguous)
}
pub fn ambiguous(text: &str) -> bool {
    apply(text).1 == Outcome::Ambiguous
}

/// A quote mark; an apostrophe inside a word ("o'clock", "don’t") isn't one.
fn quote_mark(text: &str, at: usize, c: char) -> bool {
    match c {
        '"' | '“' | '”' | '‘' | '`' => true,
        '\'' | '’' => {
            let before = text[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
            let after = text[at + c.len_utf8()..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric);
            before != after
        }
        _ => false,
    }
}
fn quote_marks(text: &str, from: usize, to: usize) -> usize {
    text[from..to]
        .char_indices()
        .filter(|&(i, c)| quote_mark(text, from + i, c))
        .count()
}

/// Words, with digit groups joined: "10,000" and "3:30" are one token.
fn tokens(text: &str) -> Vec<Word<'_>> {
    let mut out: Vec<Word> = Vec::new();
    for w in words(text) {
        if let Some(last) = out.last_mut() {
            if matches!(&text[last.end..w.start], "," | "." | ":")
                && last.text.ends_with(|c: char| c.is_ascii_digit())
                && w.text.starts_with(|c: char| c.is_ascii_digit())
            {
                last.end = w.end;
                last.text = &text[last.start..w.end];
                continue;
            }
        }
        out.push(w);
    }
    out
}
fn lower(w: &Word) -> String {
    w.text.to_lowercase().replace('’', "'")
}
/// For matching only: "three" and "3" are the same word.
fn norm(w: &Word) -> String {
    let l = lower(w);
    match NUMBERS[..21].iter().position(|n| *n == l) {
        Some(n) => n.to_string(),
        None => l,
    }
}
fn number(w: &Word) -> bool {
    w.text.starts_with(|c: char| c.is_ascii_digit()) || NUMBERS.contains(&lower(w).as_str())
}

struct Ctx<'a> {
    text: &'a str,
    t: Vec<Word<'a>>,
    low: Vec<String>,
    /// Token `i` is inside a quotation: an odd number of quote marks precede it.
    quoted: Vec<bool>,
}
impl Ctx<'_> {
    /// Text between token `i - 1` (or the start) and token `i` (or the end).
    fn gap(&self, i: usize) -> &str {
        let a = if i == 0 { 0 } else { self.t[i - 1].end };
        let b = self.t.get(i).map_or(self.text.len(), |w| w.start);
        &self.text[a..b]
    }
    fn initial(&self, i: usize) -> bool {
        i == 0 || self.gap(i).contains(['.', '!', '?', '\n'])
    }
    fn sentence(&self, mut i: usize) -> usize {
        while !self.initial(i) {
            i -= 1;
        }
        i
    }
    fn lower(&self, i: usize) -> &str {
        self.low.get(i).map_or("", String::as_str)
    }
    /// Capitalized and not "I": mid-sentence, or an uncommon word ("Thursday").
    fn name(&self, i: usize) -> bool {
        let w = self.t[i].text;
        w.starts_with(char::is_uppercase)
            && w.len() > 1
            && !w.contains(['\'', '’'])
            && (!self.initial(i) || !crate::words::common(&w.to_lowercase()))
    }
    /// 1 number or time, 2 name, 0 neither.
    fn slot(&self, i: usize) -> u8 {
        if number(&self.t[i]) {
            1
        } else if self.name(i) {
            2
        } else {
            0
        }
    }
    /// A cue phrase whose words stand alone ("--no-wait" is a flag): its end.
    fn phrase(&self, i: usize) -> Option<(usize, &'static [&'static str])> {
        let standalone =
            |c: Option<char>| c.is_none_or(|c| c.is_whitespace() || ",;:.!?()\"“”—".contains(c));
        CUES.iter().find_map(|cue| {
            let end = i + cue.len();
            (end <= self.t.len()
                && cue
                    .iter()
                    .enumerate()
                    .all(|(k, word)| self.lower(i + k) == *word)
                && (i + 1..end).all(|k| self.gap(k).chars().all(|c| c.is_whitespace() || c == ','))
                && standalone(self.gap(i).chars().next_back())
                && standalone(self.gap(end).chars().next()))
            .then_some((end, *cue))
        })
    }
    /// A cue at `i`, with any stacked cues after it ("actually, scratch that"): its end.
    fn cue(&self, i: usize) -> Option<usize> {
        let (end, cue) = self.phrase(i)?;
        let prior = i > 0;
        let punct = self
            .gap(i)
            .contains([',', '.', ';', ':', '!', '?', '—', '\n']);
        let (prev, next) = (if prior { self.lower(i - 1) } else { "" }, self.lower(end));
        let slot_next = end < self.t.len() && self.slot(end) > 0;
        let num = |k: usize| self.t.get(k).is_some_and(number);
        // Quoted or literal text ("she said 'at 3, actually 5'") is never a cue.
        let valid = !self.quoted[i]
            && match cue {
                // Its own clause: "Thursday, scratch that", not "the phrase scratch that".
                ["scratch", "that"] | ["never", "mind"] => {
                    (!prior || punct) && !DETERMINERS.contains(&prev) && !MENTIONS.contains(&prev)
                }
                ["no", "wait"] | ["wait", "no"] => prior,
                ["no", "no"] => prior && !NO_TRAPS.contains(&next),
                ["i", _] => {
                    prior && prev != "what" && !["it", "to", "well", "business"].contains(&next)
                }
                ["make", "that"] => prior && slot_next && (punct || num(i - 1)),
                ["correction"] => prior && punct,
                ["sorry"] => {
                    prior && punct && (self.gap(end).contains(',') || self.phrase(end).is_some())
                }
                ["actually"] => prior && (punct || num(end)),
                _ => prior && num(i - 1) && num(end),
            };
        if !valid {
            return None;
        }
        Some(self.stacked(end))
    }
    fn stacked(&self, mut end: usize) -> usize {
        // A stacked bare "no" may be negation: "actually, no one came".
        while let Some((next, _)) = self
            .phrase(end)
            .filter(|(n, cue)| *cue != ["no"] || !NO_TRAPS.contains(&self.lower(*n)))
        {
            end = next;
        }
        end
    }
}

/// The first clear repair, and whether an unclear cue was seen before it.
fn pass(text: &str) -> (Option<String>, bool) {
    if text.is_empty() {
        return (None, false);
    }
    let t = tokens(text);
    let low = t.iter().map(lower).collect();
    let mut marks = 0;
    let quoted = (0..t.len())
        .map(|i| {
            marks += quote_marks(text, if i == 0 { 0 } else { t[i - 1].end }, t[i].start);
            marks % 2 == 1
        })
        .collect();
    let c = Ctx {
        text,
        t,
        low,
        quoted,
    };
    let mut ambiguous = false;
    let mut i = 0;
    while i < c.t.len() {
        let Some(end) = c.cue(i) else {
            // Skip a refused run whole: "Sorry, I mean Monday." has nothing to retract.
            i = c.phrase(i).map_or(i + 1, |(end, _)| c.stacked(end));
            continue;
        };
        if let Some(edited) = repair(&c, i, end) {
            return (Some(tidy(&edited)), ambiguous);
        }
        // Content before and after, but no clear span: leave it to the model.
        ambiguous |= i > 0 && end < c.t.len() && !c.initial(end);
        i = end;
    }
    (None, ambiguous)
}

fn repair(c: &Ctx, i: usize, end: usize) -> Option<String> {
    let text = c.text;
    let start = c.sentence(i);
    // A cue that opens a sentence corrects the previous one.
    let window = if start == i && i > 0 {
        c.sentence(i - 1)
    } else {
        start
    };
    // An edit never reaches into or out of a quotation.
    let cut = |from: usize, to: usize, keep: &str| {
        (quote_marks(text, from, to) == 0).then(|| {
            let mut keep = keep.to_owned();
            if text[from..].starts_with(char::is_uppercase) {
                if let Some(first) = keep.chars().next() {
                    keep.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
                }
            }
            format!("{}{keep}", &text[..from])
        })
    };
    let cancel = (i..end).any(|k| matches!(c.lower(k), "scratch" | "never"));
    if end == c.t.len() || c.initial(end) {
        if !cancel {
            return None;
        }
        // #75: ending the dictation discards all of it; mid-dictation it takes
        // back the sentence before it, through its own full stop.
        if end == c.t.len() {
            return Some(String::new());
        }
        let to = c.t[end].start;
        return cut(c.t[window].start, to, &text[to..]);
    }
    if window == i {
        return None;
    }
    // Prefix restart: the repair repeats an earlier opening ("I will ... no no I will ...").
    for j in (window..i).rev() {
        let m = (0..)
            .take_while(|&k| {
                j + k < i && end + k < c.t.len() && norm(&c.t[j + k]) == norm(&c.t[end + k])
            })
            .count();
        let content = (0..m).all(|k| !FUNCTION.contains(&c.lower(j + k)));
        if m >= 3 || m == 2 && content {
            return cut(c.t[j].start, c.t[end].start, &text[c.t[end].start..]);
        }
    }
    // Same slot, only when the repair is set off by commas, is nothing but a new
    // number, time or name (after an optional repeated frame, "to Mike") and
    // stays in the cue's sentence: "buy 3 apples, I mean 4" -> "buy 4 apples".
    if start == i || !c.gap(i).contains([',', ';', '—']) {
        return None;
    }
    let k = (0..=2).find(|&k| {
        end + k < c.t.len() && (1..=k).all(|f| !c.initial(end + f)) && c.slot(end + k) > 0
    })?;
    let new = end + k;
    let boundary = new + 1 == c.t.len()
        || c.gap(new + 1)
            .contains([',', '.', ';', ':', '!', '?', '\n']);
    let kind = c.slot(new);
    let old = (start.max(i.saturating_sub(6))..i).rev().find(|&o| {
        o >= start + k
            && c.slot(o) == kind
            && (0..k).all(|f| norm(&c.t[o - k + f]) == norm(&c.t[end + f]))
    })?;
    if !boundary || norm(&c.t[old]) == norm(&c.t[new]) {
        return None;
    }
    let kept = &text[c.t[old].end..c.t[i - 1].end];
    cut(
        c.t[old - k].start,
        c.t[new].end,
        &format!(
            "{}{kept}{}",
            &text[c.t[end].start..c.t[new].end],
            &text[c.t[new].end..]
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_cancels_and_traps() {
        use Outcome::{Ambiguous, Discarded, Edited, Unchanged};
        // Ambiguous rows keep their words and force model cleanup.
        for (input, output, outcome) in [
            // The research table for #75 and backtrack.
            ("I will do the meeting at 3 pm no no I will do the meeting at 5 pm", "I will do the meeting at 5 pm", Edited),
            ("send it to John, sorry, to Mike", "send it to Mike", Edited),
            ("let's use Postgres, actually, use MySQL", "let's use MySQL", Edited),
            ("buy 3 apples, I mean 4", "buy 4 apples", Edited),
            ("buy 3 apples, I mean 4 oranges", "buy 3 apples, I mean 4 oranges", Ambiguous),
            ("Let's meet at 2, actually 3 PM on Friday.", "Let's meet at 2, actually 3 PM on Friday.", Ambiguous),
            ("Call me tomorrow at 9, sorry, at 10 in the morning.", "Call me tomorrow at 9, sorry, at 10 in the morning.", Ambiguous),
            ("The budget for the project is 10,000 dollars, I mean 12,000.", "The budget for the project is 12,000 dollars.", Edited),
            ("We'll need 50, no, 60 seats for the launch.", "We'll need 50, no, 60 seats for the launch.", Ambiguous),
            ("three o'clock, make that four", "four o'clock", Edited),
            ("Thursday, scratch that, Friday", "Friday", Edited),
            ("Push it to the main branch. Actually, scratch that, push it to the staging branch.", "Push it to the staging branch.", Edited),
            // Same-slot repairs never cross a sentence boundary.
            ("The planning meeting is on Tuesday. Sorry, I mean Wednesday.", "The planning meeting is on Tuesday. Sorry, I mean Wednesday.", Ambiguous),
            // #75: a dictation that ends with "scratch that" is discarded whole.
            ("Meet at noon. Bring notes. Scratch that.", "", Discarded),
            ("scratch that", "", Discarded),
            ("I mean it", "I mean it", Unchanged),
            ("actually it works", "actually it works", Unchanged),
            ("I actually think the new design is better than the old one.", "I actually think the new design is better than the old one.", Unchanged),
            ("no, no one came", "no, no one came", Unchanged),
            ("I'm sorry for the late reply, I was out sick most of last week.", "I'm sorry for the late reply, I was out sick most of last week.", Unchanged),
            ("the scratch that I made", "the scratch that I made", Unchanged),
            ("she said \"scratch that\" and left", "she said \"scratch that\" and left", Unchanged),
            ("pass --no-wait to curl", "pass --no-wait to curl", Unchanged),
            ("make that decision today", "make that decision today", Unchanged),
            ("three or four days", "three or four days", Unchanged),
            ("Send the invoice to John, no wait, send it to Maria in finance.", "Send the invoice to John, no wait, send it to Maria in finance.", Ambiguous),
            ("Set the retry limit to three, no wait, make it five retries.", "Set the retry limit to three, no wait, make it five retries.", Ambiguous),
            // #75: mid-dictation it drops only the sentence before it.
            ("Meet at noon. Bring notes. Scratch that. Call me at 5.", "Meet at noon. Call me at 5.", Edited),
            ("Never mind.", "", Discarded),
            ("Bring notes, never mind.", "", Discarded),
            ("Bring notes. Scratch, that.", "", Discarded),
            ("Scratch\nthat.", "", Discarded),
            ("He said no, no one came.", "He said no, no one came.", Unchanged),
            ("I called John, sorry, Mike called me", "I called John, sorry, Mike called me", Ambiguous),
            ("I'll make that Monday", "I'll make that Monday", Unchanged),
            ("Can we, I mean, what is the plan", "Can we, I mean, what is the plan", Ambiguous),
            ("Sorry, I mean Monday.", "Sorry, I mean Monday.", Unchanged),
            ("the budget is 8,000 dollars, I mean 9,000 after the last approval", "the budget is 8,000 dollars, I mean 9,000 after the last approval", Ambiguous),
            ("buy 3 apples, I mean 4, and some milk", "buy 4 apples, and some milk", Edited),
            // Review of #107: a number after "actually" isn't a correction by itself.
            ("Version 3 is actually 4 times faster.", "Version 3 is actually 4 times faster.", Ambiguous),
            ("The value is 3, actually 3 is correct.", "The value is 3, actually 3 is correct.", Ambiguous),
            ("There were 3 people. Actually, 4 dogs live next door.", "There were 3 people. Actually, 4 dogs live next door.", Ambiguous),
            // Quoted and literal text stays.
            ("She said 'Meet at 3, actually 5.'", "She said 'Meet at 3, actually 5.'", Unchanged),
            ("She said ‘Meet at 3, actually 5.’", "She said ‘Meet at 3, actually 5.’", Unchanged),
            ("Use `3, no, 4` as a literal example.", "Use `3, no, 4` as a literal example.", Unchanged),
            ("I like the phrase scratch that.", "I like the phrase scratch that.", Unchanged),
            ("It's three o'clock, make that four", "It's four o'clock", Edited),
            // Past the repair cap the text stays verbatim for the model.
            ("3 no 4 no 5 no 6 no 7 no 8 no 9 no 10 no 11 no 12", "3 no 4 no 5 no 6 no 7 no 8 no 9 no 10 no 11 no 12", Ambiguous),
            ("3, no, 4, no, 5, no, 6, no, 7, no, 8, no, 9, no, 10, no, 11, no, 12", "3, no, 4, no, 5, no, 6, no, 7, no, 8, no, 9, no, 10, no, 11, no, 12", Ambiguous),
            ("3, no, 4, no, 5", "5", Edited),
        ] {
            assert_eq!(apply(input), (output.to_owned(), outcome), "{input}");
            assert_eq!(ambiguous(input), outcome == Ambiguous, "{input}");
        }
    }
    #[test]
    fn repairs_stay_linear() {
        let unit = "Send it to John, sorry, to Mike. I will send it no no I will mail it. Sorry for the delay, I mean it. ";
        let text = unit.repeat(2_000 / unit.len() + 1);
        let start = std::time::Instant::now();
        for _ in 0..10 {
            std::hint::black_box(apply(std::hint::black_box(&text)));
        }
        let each = start.elapsed() / 10;
        eprintln!("backtrack {} bytes: {each:?}", text.len());
        assert!(
            each.as_micros()
                < if cfg!(debug_assertions) {
                    20_000
                } else {
                    1_000
                },
            "{each:?}"
        );
    }
}
