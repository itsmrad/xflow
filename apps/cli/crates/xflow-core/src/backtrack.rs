//! Spoken self-correction ("backtrack", #75): "at 3, sorry, at 5" keeps only "at 5".
//! Deterministic and local. A cue whose retracted span isn't clear stays for the
//! model (`ambiguous`); traps such as "I mean it" or "sorry for the delay" are no cue.
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
const FUNCTION: [&str; 34] = [
    "i", "a", "an", "the", "to", "of", "in", "on", "at", "and", "or", "it", "is", "we", "you",
    "he", "she", "they", "will", "be", "for", "with", "that", "this", "so", "but", "after",
    "before", "from", "by", "per", "until", "if", "when",
];
/// "no, no one came", "no, no problem": negation, not a retraction.
const NO_TRAPS: [&str; 10] = [
    "one", "body", "thing", "where", "longer", "more", "way", "problem", "doubt", "matter",
];

/// The text with every clear self-correction applied; empty when the speaker
/// cancelled everything ("scratch that" alone).
pub fn apply(text: &str) -> String {
    scan(text).0
}
/// A self-correction cue remains that `apply` can't resolve: model cleanup should
/// run even for a short dictation.
pub fn ambiguous(text: &str) -> bool {
    scan(text).1
}
/// The raw transcript asked to cancel the dictation, for history's "discarded".
pub fn cancelled(raw: &str) -> bool {
    let raw = raw.to_lowercase();
    raw.contains("scratch that") || raw.contains("never mind")
}

fn scan(text: &str) -> (String, bool) {
    let mut text = text.to_owned();
    // Each edit removes words, so this ends; the cap bounds pathological input.
    for round in 0.. {
        match pass(&text) {
            (Some(edited), _) if round < 8 => text = edited,
            (edit, ambiguous) => return (text, ambiguous || edit.is_some()),
        }
    }
    unreachable!()
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
        let quotes = self.text[..self.t[i].start]
            .chars()
            .filter(|c| matches!(c, '"' | '“' | '”'))
            .count();
        let prior = i > 0;
        let punct = self
            .gap(i)
            .contains([',', '.', ';', ':', '!', '?', '—', '\n']);
        let (prev, next) = (if prior { self.lower(i - 1) } else { "" }, self.lower(end));
        let slot_next = end < self.t.len() && self.slot(end) > 0;
        let num = |k: usize| self.t.get(k).is_some_and(number);
        let valid = quotes % 2 == 0
            && match cue {
                ["scratch", "that"] | ["never", "mind"] => !DETERMINERS.contains(&prev),
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
    let t = tokens(text);
    let low = t.iter().map(lower).collect();
    let c = Ctx { text, t, low };
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
    let cancel = (i..end).any(|k| matches!(c.lower(k), "scratch" | "never"));
    if end == c.t.len() || c.initial(end) {
        if !cancel {
            return None;
        }
        // Bare cancel: drop the sentence it takes back, through its own full stop.
        let from = c.t[window.min(i)].start;
        let to = c.t.get(end).map_or(text.len(), |w| w.start);
        return Some(format!("{}{}", &text[..from], &text[to..]));
    }
    if window == i {
        return None;
    }
    let cased = |from: usize, keep: &str| {
        let mut keep = keep.to_owned();
        if text[from..].starts_with(char::is_uppercase) {
            if let Some(first) = keep.chars().next() {
                keep.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
            }
        }
        format!("{}{keep}", &text[..from])
    };
    // Prefix restart: the repair repeats an earlier opening ("I will ... no no I will ...").
    for j in (window..i).rev() {
        let m = (0..)
            .take_while(|&k| {
                j + k < i && end + k < c.t.len() && norm(&c.t[j + k]) == norm(&c.t[end + k])
            })
            .count();
        let content = (0..m).all(|k| !FUNCTION.contains(&c.lower(j + k)));
        if m >= 3 || m == 2 && content {
            return Some(cased(c.t[j].start, &text[c.t[end].start..]));
        }
    }
    // Same slot: a number, time or name, optionally after a repeated frame ("to Mike").
    let k = (0..=2).find(|&k| {
        end + k < c.t.len() && (1..=k).all(|f| !c.initial(end + f)) && c.slot(end + k) > 0
    })?;
    let new = end + k;
    let kind = c.slot(new);
    let old = (window.max(i.saturating_sub(6))..i).rev().find(|&o| {
        o >= window + k
            && c.slot(o) == kind
            && (0..k).all(|f| norm(&c.t[o - k + f]) == norm(&c.t[end + f]))
    })?;
    let rest = new + 1 < c.t.len() && !c.initial(new + 1);
    let from = c.t[old - k].start;
    if !rest || kind == 1 && FUNCTION.contains(&c.lower(new + 1)) {
        // "buy 3 apples, I mean 4" and "8,000 dollars, I mean 9,000 after …":
        // the new value takes the old one's place and keeps its unit.
        let kept = &text[c.t[old].end..c.t[i - 1].end];
        return Some(cased(
            from,
            &format!(
                "{}{kept}{}",
                &text[c.t[end].start..c.t[new].end],
                &text[c.t[new].end..]
            ),
        ));
    }
    // A name followed by more words may start a new clause ("John, sorry, Mike called").
    (kind == 1).then(|| cased(from, &text[c.t[end].start..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_cancels_and_traps() {
        // (input, output, ambiguous): the research table for #75 and backtrack.
        for (input, output, unclear) in [
            ("I will do the meeting at 3 pm no no I will do the meeting at 5 pm", "I will do the meeting at 5 pm", false),
            ("send it to John, sorry, to Mike", "send it to Mike", false),
            ("let's use Postgres, actually, use MySQL", "let's use MySQL", false),
            ("buy 3 apples, I mean 4", "buy 4 apples", false),
            ("buy 3 apples, I mean 4 oranges", "buy 4 oranges", false),
            ("Let's meet at 2, actually 3 PM on Friday.", "Let's meet at 3 PM on Friday.", false),
            ("Call me tomorrow at 9, sorry, at 10 in the morning.", "Call me tomorrow at 10 in the morning.", false),
            ("The budget for the project is 10,000 dollars, I mean 12,000.", "The budget for the project is 12,000 dollars.", false),
            ("We'll need 50, no, 60 seats for the launch.", "We'll need 60 seats for the launch.", false),
            ("three o'clock, make that four", "four o'clock", false),
            ("Thursday, scratch that, Friday", "Friday", false),
            ("Push it to the main branch. Actually, scratch that, push it to the staging branch.", "Push it to the staging branch.", false),
            ("The planning meeting is on Tuesday. Sorry, I mean Wednesday.", "The planning meeting is on Wednesday.", false),
            ("Meet at noon. Bring notes. Scratch that.", "Meet at noon.", false),
            ("scratch that", "", false),
            ("I mean it", "I mean it", false),
            ("actually it works", "actually it works", false),
            ("I actually think the new design is better than the old one.", "I actually think the new design is better than the old one.", false),
            ("no, no one came", "no, no one came", false),
            ("I'm sorry for the late reply, I was out sick most of last week.", "I'm sorry for the late reply, I was out sick most of last week.", false),
            ("the scratch that I made", "the scratch that I made", false),
            ("she said \"scratch that\" and left", "she said \"scratch that\" and left", false),
            ("pass --no-wait to curl", "pass --no-wait to curl", false),
            ("make that decision today", "make that decision today", false),
            ("three or four days", "three or four days", false),
            ("Send the invoice to John, no wait, send it to Maria in finance.", "Send the invoice to John, no wait, send it to Maria in finance.", true),
            ("Set the retry limit to three, no wait, make it five retries.", "Set the retry limit to three, no wait, make it five retries.", true),
            // #75: a cancel mid-dictation drops only the sentence before it.
            ("Meet at noon. Bring notes. Scratch that. Call me at 5.", "Meet at noon. Call me at 5.", false),
            ("Never mind.", "", false),
            ("Bring notes, never mind.", "", false),
            ("He said no, no one came.", "He said no, no one came.", false),
            ("I called John, sorry, Mike called me", "I called John, sorry, Mike called me", true),
            ("I'll make that Monday", "I'll make that Monday", false),
            ("Can we, I mean, what is the plan", "Can we, I mean, what is the plan", true),
            ("Sorry, I mean Monday.", "Sorry, I mean Monday.", false),
            ("the budget is 8,000 dollars, I mean 9,000 after the last approval", "the budget is 9,000 dollars after the last approval", false),
        ] {
            assert_eq!(scan(input), (output.to_owned(), unclear), "{input}");
        }
    }
    #[test]
    fn repairs_stay_linear() {
        let unit = "Let's meet at 2, actually 3 PM. I will send it no no I will mail it. Sorry for the delay, I mean it. ";
        let text = unit.repeat(2_000 / unit.len() + 1);
        let start = std::time::Instant::now();
        for _ in 0..10 {
            std::hint::black_box(scan(std::hint::black_box(&text)));
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
