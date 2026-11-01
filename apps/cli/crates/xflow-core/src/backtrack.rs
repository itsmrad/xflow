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
/// A day or month is a time slot, never a person: "Ask John, sorry, Friday." stays.
const DATES: [&str; 19] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
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
    // Up to 8 repairs, then one pass to confirm nothing is left.
    for _ in 0..=8 {
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
    // More than 8 repairs: no real dictation has that many; leave it to the model.
    (text.to_owned(), Outcome::Ambiguous)
}
pub fn ambiguous(text: &str) -> bool {
    apply(text).1 == Outcome::Ambiguous
}

/// A quote mark, or a possessive that may be one; an apostrophe inside a word
/// ("o'clock", "don’t") isn't. An edit never spans one.
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
            !(before && after)
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
/// Per token: inside a quotation, where nothing is a cue. A single quote opens
/// after a space and closes only where it can't be a possessive ("hi.'", "hi'."),
/// so "the dogs' toys" inside a quote keeps it open and outside opens nothing.
fn quoted(text: &str, t: &[Word]) -> Vec<bool> {
    let (mut double, mut tick, mut single) = (false, false, false);
    let mut out = Vec::with_capacity(t.len());
    let mut chars = text.char_indices().peekable();
    let mut prev: Option<char> = None;
    while let Some((at, c)) = chars.next() {
        while out.len() < t.len() && t[out.len()].start <= at {
            out.push(double || tick || single);
        }
        let next = chars.peek().map(|&(_, n)| n);
        let punct = |c: char| ".,!?;:".contains(c);
        match c {
            '"' => double = !double,
            '“' => double = true,
            '”' => double = false,
            '`' => tick = !tick,
            '‘' => single = true,
            '\'' | '’' if !single => {
                single = prev.is_none_or(|p| p.is_whitespace() || "([{—".contains(p))
                    && next.is_some_and(|n| !n.is_whitespace());
            }
            '\'' | '’' => {
                let after = next.is_none_or(|n| n.is_whitespace() || punct(n) || n == ')');
                single = !(prev.is_some_and(punct) && after
                    || prev.is_some_and(char::is_alphanumeric) && next.is_none_or(punct));
            }
            _ => {}
        }
        prev = Some(c);
    }
    out.resize(t.len(), double || tick || single);
    out
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
    let l = w.text.to_lowercase();
    if l.contains('’') {
        l.replace('’', "'")
    } else {
        l
    }
}
/// A number's value: "thirty" and "30" are 30. None for "5pm", "3:30".
fn value(l: &str) -> Option<u64> {
    if l.starts_with(|c: char| c.is_ascii_digit()) {
        return l.chars().filter(|&c| c != ',').try_fold(0u64, |n, c| {
            n.checked_mul(10)?.checked_add(u64::from(c.to_digit(10)?))
        });
    }
    let i = NUMBERS.iter().position(|n| *n == l)? as u64;
    Some(match i {
        0..=20 => i,
        21..=27 => (i - 18) * 10,
        28 => 100,
        29 => 1_000,
        _ => 1_000_000,
    })
}

struct Ctx<'a> {
    text: &'a str,
    t: Vec<Word<'a>>,
    low: Vec<String>,
    val: Vec<Option<u64>>,
    /// Token `i` is inside a quotation.
    quoted: Vec<bool>,
    /// End of the cue words that open the text ("Sorry, scratch that"): nothing
    /// before them is content to retract.
    lead: usize,
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
    fn number(&self, i: usize) -> bool {
        i < self.t.len()
            && (self.t[i].text.starts_with(|c: char| c.is_ascii_digit())
                || NUMBERS.contains(&self.lower(i)))
    }
    /// For matching only: "three" and "3" are the same word.
    fn same(&self, a: usize, b: usize) -> bool {
        match (self.val[a], self.val[b]) {
            (Some(x), Some(y)) => x == y,
            (None, None) => self.low[a] == self.low[b],
            _ => false,
        }
    }
    /// Capitalized and not "I": mid-sentence, or an uncommon word ("Thursday").
    fn name(&self, i: usize) -> bool {
        let w = self.t[i].text;
        w.starts_with(char::is_uppercase)
            && w.len() > 1
            && !w.contains(['\'', '’'])
            && (!self.initial(i) || !crate::words::common(&w.to_lowercase()))
    }
    /// 1 number or time, 2 name, 3 day or month, 0 none.
    fn slot(&self, i: usize) -> u8 {
        if self.number(i) {
            1
        } else if !self.name(i) {
            0
        } else if DATES.contains(&self.lower(i)) {
            3
        } else {
            2
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
        let prior = i > self.lead;
        let punct = self
            .gap(i)
            .contains([',', '.', ';', ':', '!', '?', '—', '\n']);
        let (prev, next) = (if prior { self.lower(i - 1) } else { "" }, self.lower(end));
        let num = |k: usize| self.number(k);
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
                ["make", "that"] => {
                    prior && end < self.t.len() && self.slot(end) > 0 && (punct || num(i - 1))
                }
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
    /// Only cue words precede `i` in its sentence.
    fn opens(&self, i: usize) -> bool {
        self.stacked(self.sentence(i)) >= i
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
    let low: Vec<String> = t.iter().map(lower).collect();
    let val = low.iter().map(|l| value(l)).collect();
    let quoted = quoted(text, &t);
    let mut c = Ctx {
        text,
        t,
        low,
        val,
        quoted,
        lead: 0,
    };
    c.lead = c.stacked(0);
    let mut ambiguous = false;
    let mut i = 0;
    while i < c.t.len() {
        let Some(end) = c.cue(i) else {
            // Skip only the refused phrase: "Sorry, scratch that." still cancels.
            i = c.phrase(i).map_or(i + 1, |(end, _)| end);
            continue;
        };
        if let Some(edited) = repair(&c, i, end) {
            return (Some(tidy(&edited)), ambiguous);
        }
        // Content before and after, but no clear span: leave it to the model.
        // A cue that is a sentence of its own ("At 3. No, no. At 5.") counts too.
        ambiguous |= i > c.lead && end < c.t.len() && (!c.initial(end) || c.opens(i));
        i = end;
    }
    (None, ambiguous)
}

fn repair(c: &Ctx, i: usize, end: usize) -> Option<String> {
    let text = c.text;
    let start = c.sentence(i);
    let opens = c.opens(i);
    // A cue that opens a sentence corrects the previous one.
    let window = if opens && start > 0 {
        c.sentence(start - 1)
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
    if window == i || opens && start == 0 {
        return None;
    }
    // Prefix restart: the repair repeats an earlier opening ("I will ... no no I will ...").
    for j in (window..i).rev() {
        let m = (0..)
            .take_while(|&k| j + k < i && end + k < c.t.len() && c.same(j + k, end + k))
            .count();
        let content = (0..m).all(|k| !FUNCTION.contains(&c.lower(j + k)));
        if m >= 3 || m == 2 && content {
            return cut(c.t[j].start, c.t[end].start, &text[c.t[end].start..]);
        }
    }
    // Same slot, only when the repair is set off by commas, is nothing but a new
    // number, time or name (after an optional repeated frame, "to Mike") and
    // stays in the cue's sentence: "buy 3 apples, I mean 4" -> "buy 4 apples".
    if opens || !c.gap(i).contains([',', ';', '—']) {
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
        o >= start + k && c.slot(o) == kind && (0..k).all(|f| c.same(o - k + f, end + f))
    })?;
    // Same value ("thirty, no, 30"), a value we can't compare ("5pm, no, five")
    // or part of a compound ("twenty five, no, 25"): not certain.
    let unsure = kind == 1
        && (c.val[old].is_none() != c.val[new].is_none()
            || old > 0 && c.number(old - 1) && !c.gap(old).contains([',', ';']));
    if !boundary || unsure || c.same(old, new) {
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
            ("3, no, 4, no, 5, no, 6, no, 7, no, 8, no, 9, no, 10, no, 11", "11", Edited),
            // Second review of #107: cue words before a cancel don't hide it.
            ("Sorry, scratch that.", "", Discarded),
            ("Actually, scratch that.", "", Discarded),
            ("I mean, scratch that.", "", Discarded),
            ("Sorry, never mind.", "", Discarded),
            ("Um, sorry, scratch that.", "", Discarded),
            // #75 mid-dictation: no sentence before it, so only the cancel goes.
            ("Sorry, scratch that. Meet at 5.", "Meet at 5.", Edited),
            // Quote state survives a closer after punctuation and a possessive.
            ("Say 'hi.' At 3, actually, 4.", "Say 'hi.' At 4.", Edited),
            ("She said 'the dogs' toys are 3, actually, 4.'", "She said 'the dogs' toys are 3, actually, 4.'", Unchanged),
            ("She said ‘the dogs’ toys are 3, actually, 4.’", "She said ‘the dogs’ toys are 3, actually, 4.’", Unchanged),
            ("dogs' 3, no, 4", "dogs' 4", Edited),
            // The same value, or one we can't compare, is no correction.
            ("thirty, no, 30", "thirty, no, 30", Ambiguous),
            ("fifty, no, 50", "fifty, no, 50", Ambiguous),
            ("50, no, fifty.", "50, no, fifty.", Ambiguous),
            ("twenty, no, 20", "twenty, no, 20", Ambiguous),
            ("twenty five, no, 25", "twenty five, no, 25", Ambiguous),
            ("5pm, no, five", "5pm, no, five", Ambiguous),
            ("thirty, no, 40", "40", Edited),
            // A cue that is its own sentence goes to the model.
            ("At 3. No, no. At 5.", "At 3. No, no. At 5.", Ambiguous),
            // A day or month isn't a person.
            ("Ask John, sorry, Friday.", "Ask John, sorry, Friday.", Ambiguous),
            ("Email Sarah, sorry, Monday.", "Email Sarah, sorry, Monday.", Ambiguous),
        ] {
            assert_eq!(apply(input), (output.to_owned(), outcome), "{input}");
            assert_eq!(ambiguous(input), outcome == Ambiguous, "{input}");
        }
    }
    #[test]
    fn repairs_stay_linear() {
        let unit = "Send it to John, sorry, to Mike. I will send it no no I will mail it. Sorry for the delay, I mean it. ";
        let text = unit.repeat(2_000 / unit.len() + 1);
        // The common-word list decodes once per process (the daemon does it at start).
        crate::words::common("warm");
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
