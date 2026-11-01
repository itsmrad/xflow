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
/// A full stop after these never ends a sentence.
const TITLES: [&str; 4] = ["mr", "mrs", "ms", "prof"];
/// Words of a list, range or alternative: "2 or 3", "from 2 to 3".
const LISTS: [&str; 7] = ["or", "and", "through", "between", "from", "either", "vs"];
/// Negation anywhere before a cue makes what it retracts unclear: "it's not 3, no, 4".
/// Also any word ending in "n't".
const NEGATIONS: [&str; 26] = [
    "not", "never", "no", "nobody", "nothing", "nowhere", "neither", "nor", "none", "without",
    "cannot", "dont", "doesnt", "didnt", "cant", "wont", "isnt", "arent", "wasnt", "werent",
    "shouldnt", "couldnt", "wouldnt", "havent", "hasnt", "aint",
];
/// Spoken fillers that may sit inside a cue's clause.
const FILLERS: [&str; 12] = [
    "um", "uh", "er", "erm", "ah", "hmm", "like", "so", "well", "okay", "ok", "mm",
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
    // An unclear cue seen in any pass stays decisive, even before a later repair.
    let mut doubt = false;
    // Up to 8 repairs, then one pass to confirm nothing is left.
    for _ in 0..=8 {
        match pass(&edited) {
            (Some(next), ambiguous) => {
                doubt |= ambiguous;
                edited = next;
            }
            (None, ambiguous) => {
                let ambiguous = ambiguous || doubt;
                // A cue left over after a repair ("buy 3 apples, I mean 4, no, 5"):
                // the edit is only partial, so nothing is edited.
                if edited != text && (ambiguous || Ctx::new(&edited).any_cue()) {
                    return (text.to_owned(), Outcome::Ambiguous);
                }
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
/// before a letter and pairs with the next closer that can't be a possessive
/// ("hi' ", "hi.'", "yes'."; not "dogs' toys"). An opener without a closer is an
/// apostrophe ("'em"), and "'90s" or "rock 'n' roll" never open one.
fn quoted(text: &str, t: &[Word]) -> Vec<bool> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let at = |k: usize| chars.get(k).map(|&(_, c)| c);
    let punct = |c: char| ".,!?;:".contains(c);
    let (mut singles, mut open) = (Vec::new(), None);
    for (k, &(pos, c)) in chars.iter().enumerate() {
        if !matches!(c, '\'' | '’' | '‘') {
            continue;
        }
        let (prev, next) = (k.checked_sub(1).and_then(at), at(k + 1));
        let opener = c == '‘'
            || prev.is_none_or(|p| p.is_whitespace() || "([{—\"“".contains(p))
                && next.is_some_and(char::is_alphabetic)
                && !(matches!(next, Some('n' | 'N')) && matches!(at(k + 2), Some('\'' | '’')));
        let closer = c != '‘'
            && prev.is_some_and(|p| !p.is_whitespace())
            && next.is_none_or(|n| n.is_whitespace() || punct(n) || ")\"”".contains(n))
            && (prev.is_some_and(punct)
                || next.is_none_or(punct)
                || !matches!(prev, Some('s' | 'S')));
        if opener {
            open = Some(pos);
        } else if let (Some(from), true) = (open, closer) {
            singles.push((from, pos));
            open = None;
        }
    }
    let (mut double, mut tick, mut span) = (false, false, 0);
    let mut out = Vec::with_capacity(t.len());
    for &(pos, c) in &chars {
        while out.len() < t.len() && t[out.len()].start <= pos {
            let start = t[out.len()].start;
            while singles.get(span).is_some_and(|&(_, to)| to < start) {
                span += 1;
            }
            let single = singles.get(span).is_some_and(|&(from, _)| from < start);
            out.push(double || tick || single);
        }
        match c {
            '"' => double = !double,
            '“' => double = true,
            '”' => double = false,
            '`' => tick = !tick,
            _ => {}
        }
    }
    out.resize(t.len(), double || tick);
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
impl<'a> Ctx<'a> {
    fn new(text: &'a str) -> Self {
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
        c.lead = c.lead_in(0);
        c
    }
    /// A cue phrase outside quotes, even one refused as a cue here.
    fn any_cue(&self) -> bool {
        (0..self.t.len()).any(|i| !self.quoted[i] && self.phrase(i).is_some())
    }
    /// Text between token `i - 1` (or the start) and token `i` (or the end).
    fn gap(&self, i: usize) -> &str {
        let a = if i == 0 { 0 } else { self.t[i - 1].end };
        let b = self.t.get(i).map_or(self.text.len(), |w| w.start);
        &self.text[a..b]
    }
    /// A sentence starts at `i`: after "!", "?" or a line break, or after a
    /// full stop and a space that doesn't follow an abbreviation ("Mr.", "p.m.").
    fn initial(&self, i: usize) -> bool {
        i == 0
            || self.gap(i).contains(['!', '?', '\n'])
            || self.stop(i) && !TITLES.contains(&self.lower(i - 1)) && !self.abbreviation(i - 1)
    }
    /// "etc. Next", "5 p.m. Next": a full stop that may or may not end a sentence.
    fn unsure(&self, i: usize) -> bool {
        i > 0 && !self.initial(i) && self.stop(i) && self.abbreviation(i - 1)
    }
    fn stop(&self, i: usize) -> bool {
        let gap = self.gap(i);
        gap.find('.')
            .is_some_and(|d| gap[d..].contains(char::is_whitespace))
    }
    fn abbreviation(&self, k: usize) -> bool {
        let w = self.lower(k);
        w.len() == 1 && w.starts_with(char::is_alphabetic)
            // "Main St." or "King Jr." may end a sentence too.
            || ["etc", "inc", "ltd", "approx", "st", "dr", "jr", "sr", "vs"].contains(&w)
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
                // After a full stop it is always a cancel: "He called. Scratch that."
                ["scratch", "that"] | ["never", "mind"] => {
                    (!prior || punct)
                        && (self.initial(i)
                            || !DETERMINERS.contains(&prev) && !MENTIONS.contains(&prev))
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
                _ => prior && self.value_before(i) && num(end),
            };
        if !valid {
            return None;
        }
        Some(self.stacked(end))
    }
    /// A number, or a time ("5 pm", "5 p.m."), ends just before `i`.
    fn value_before(&self, i: usize) -> bool {
        let unit = |k: usize| matches!(self.lower(k), "am" | "pm" | "m");
        self.number(i - 1)
            || i >= 2
                && unit(i - 1)
                && (self.number(i - 2)
                    || i >= 3 && matches!(self.lower(i - 2), "a" | "p") && self.number(i - 3))
    }
    /// Only cue words precede `i` in its sentence.
    /// Fillers and cue words belong to the cue's clause: "He called. Um,
    /// scratch that." takes back "He called."
    fn opens(&self, i: usize) -> bool {
        self.lead_in(self.sentence(i)) >= i
    }
    /// The end of the fillers and cue words starting at `k`.
    fn lead_in(&self, mut k: usize) -> usize {
        loop {
            if let Some((end, _)) = self.phrase(k) {
                k = end;
            } else if FILLERS.contains(&self.lower(k)) {
                k += 1;
            } else {
                return k;
            }
        }
    }
    /// The end of cue phrases stacked after `end`, with fillers between them
    /// ("sorry, um, scratch that").
    fn stacked(&self, mut end: usize) -> usize {
        loop {
            let mut next = end;
            while FILLERS.contains(&self.lower(next)) {
                next += 1;
            }
            // A stacked bare "no" may be negation: "actually, no one came".
            match self
                .phrase(next)
                .filter(|(n, cue)| *cue != ["no"] || !NO_TRAPS.contains(&self.lower(*n)))
            {
                Some((after, _)) => end = after,
                None => return end,
            }
        }
    }
    /// Nothing is attached to token `k`: not "-4", "$400", "@Mike", "(4)".
    fn plain(&self, k: usize) -> bool {
        let w = &self.t[k];
        self.text[..w.start]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace)
            && self.text[w.end..]
                .chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || ",.;:!?".contains(c))
    }
}

/// The first clear repair, and whether an unclear cue was seen before it.
fn pass(text: &str) -> (Option<String>, bool) {
    if text.is_empty() {
        return (None, false);
    }
    let c = Ctx::new(text);
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
        // A cue that is a sentence of its own ("At 3. No, no. At 5.") counts too,
        // and so does a cancel whose span isn't certain.
        let cancel = (i..end).any(|k| matches!(c.lower(k), "scratch" | "never"));
        ambiguous |= cancel || i > c.lead && end < c.t.len() && (!c.initial(end) || c.opens(i));
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
    // An edit never reaches into or out of a quotation, starts inside a token
    // ("p.|m."), or crosses a full stop that might end a sentence ("etc. Bring").
    let cut = |first: usize, to: usize, keep: &str| {
        let from = c.t[first].start;
        let clean = quote_marks(text, from, to) == 0
            && (from == 0 || text[..from].ends_with(char::is_whitespace))
            && !(first + 1..i).any(|k| c.unsure(k));
        clean.then(|| {
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
        return cut(window, to, &text[to..]);
    }
    // A negation anywhere before the cue in the sentences it edits, commas or not.
    let negated = (window..i).any(|o| {
        let w = c.lower(o);
        NEGATIONS.contains(&w) || w.ends_with("n't")
    });
    if window == i || opens && start == 0 || negated {
        return None;
    }
    // Prefix restart: the repair repeats an earlier opening ("I will ... no no I will ...").
    for j in (window..i).rev() {
        let m = (0..)
            .take_while(|&k| j + k < i && end + k < c.t.len() && c.same(j + k, end + k))
            .count();
        let content = (0..m).all(|k| !FUNCTION.contains(&c.lower(j + k)));
        if m >= 3 || m == 2 && content {
            // The opening said more than once ("it is a problem, it is a problem,
            // no no, it is …"): which one is retracted isn't certain.
            let openings = (window..i)
                .filter(|&p| p + 1 < i && c.same(p, end) && c.same(p + 1, end + 1))
                .count();
            return (openings == 1)
                .then(|| cut(j, c.t[end].start, &text[c.t[end].start..]))
                .flatten();
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
    // The last value of that kind, right before the cue; a number may keep one
    // unit word ("3 apples, I mean 4"). Never an earlier one ("to John about
    // Mike, sorry, to Sara").
    let old = (start..i).rev().find(|&o| c.slot(o) == kind)?;
    let adjacent = old + 1 == i
        || kind == 1
            && k == 0
            && old + 2 == i
            && c.slot(old + 1) == 0
            && !FUNCTION.contains(&c.lower(old + 1));
    // Signs, currencies and sigils change the value: "-4", "€400", "@Mike".
    if !adjacent
        || old < start + k
        || !(0..k).all(|f| c.same(old - k + f, end + f))
        || !c.plain(old)
        || !c.plain(new)
    {
        return None;
    }
    // Same value ("thirty, no, 30"), a value we can't compare ("5pm, no, five")
    // or part of a compound ("twenty five", "a hundred", "hundred and five"): not certain.
    let joined = |k: usize| !c.gap(k + 1).contains([',', ';']);
    let compound = old > 0
        && joined(old - 1)
        && (c.number(old - 1)
            || matches!(c.lower(old - 1), "a" | "an")
            || c.lower(old - 1) == "and" && old >= 2 && c.number(old - 2));
    let unsure = kind == 1 && (c.val[old].is_none() != c.val[new].is_none() || compound);
    // Any doubt earlier in the sentence: another value of that kind ("the budget
    // is 3, the budget is 4, no, 5"), the frame again ("to Lee, send it to Lee,
    // sorry, to Ann"), or in the old value's clause a list or range ("2 or maybe
    // 3", "from 2 to 3") or a negation ("we don't need 3, no, 4").
    let clause = (start..old)
        .rev()
        .find(|&o| c.gap(o + 1).contains([',', ';', ':']))
        .map_or(start, |o| o + 1);
    let doubt = (start..old).any(|o| c.slot(o) == kind)
        || k > 0
            && (start..old - k).any(|o| (0..k).all(|f| o + f < old - k && c.same(o + f, end + f)))
        || (clause..old).any(|o| {
            let w = c.lower(o);
            LISTS.contains(&w)
        })
        || c.gap(old).contains(['-', '–'])
        || c.lower(old + 1) == "or";
    if doubt {
        return None;
    }
    if !boundary || unsure || c.same(old, new) {
        return None;
    }
    let kept = &text[c.t[old].end..c.t[i - 1].end];
    cut(
        old - k,
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
            // Third review: a full stop after an abbreviation isn't a sentence end.
            ("Meet at 5 p.m. Scratch that. Bring notes.", "Bring notes.", Edited),
            ("Call Mr. Smith, scratch that. Call me.", "Call me.", Edited),
            ("Email Dr. Adams, scratch that. Email Lee.", "Email Dr. Adams, scratch that. Email Lee.", Ambiguous),
            ("See you at 5 p.m. No, 6 p.m.", "See you at 5 p.m. No, 6 p.m.", Ambiguous),
            ("We bought pens etc. Bring notes. Scratch that. Call me.", "We bought pens etc. Bring notes. Scratch that. Call me.", Ambiguous),
            // A leading apostrophe isn't a quote and can't hide a later cancel.
            ("the '90s were great. Scratch that.", "", Discarded),
            ("Play '90s hits. Scratch that.", "", Discarded),
            ("I love rock 'n' roll. Scratch that.", "", Discarded),
            ("I love rock ’n’ roll. Scratch that.", "", Discarded),
            ("She said 'hello' to me. Scratch that.", "", Discarded),
            ("Say 'hi' at 3, actually, 4.", "Say 'hi' at 4.", Edited),
            ("I'll see 'em at 3, actually, 4.", "I'll see 'em at 4.", Edited),
            ("She said 'Meet at noon. Scratch that.'", "She said 'Meet at noon. Scratch that.'", Unchanged),
            // A number phrase is replaced whole or not at all.
            ("a hundred, no, 200", "a hundred, no, 200", Ambiguous),
            ("one hundred and five, no, 200", "one hundred and five, no, 200", Ambiguous),
            // Only the value right before the cue.
            ("send it to John about Mike, sorry, to Sara", "send it to John about Mike, sorry, to Sara", Ambiguous),
            ("Tell May, sorry, June", "Tell June", Edited),
            ("3:30, no, 4:45", "4:45", Edited),
            ("Please scratch that.", "Please scratch that.", Unchanged),
            // Fourth review: after a full stop "scratch that" is always a cancel.
            ("He called. Scratch that.", "", Discarded),
            ("He called. Never mind.", "", Discarded),
            ("The answer is no. Scratch that.", "", Discarded),
            ("Use that. Scratch that.", "", Discarded),
            ("It sounds like. Scratch that.", "", Discarded),
            ("As I said. Scratch that. Start over.", "Start over.", Edited),
            ("J. Smith called. Scratch that. Email Lee.", "J. Smith called. Scratch that. Email Lee.", Ambiguous),
            // A value inside a list, range or alternative isn't certain.
            ("We need 2 or 3, no, 4.", "We need 2 or 3, no, 4.", Ambiguous),
            ("one or two, no, three.", "one or two, no, three.", Ambiguous),
            ("Book it from 2 to 3, no, 4.", "Book it from 2 to 3, no, 4.", Ambiguous),
            ("from 3 to 5, no, 8.", "from 3 to 5, no, 8.", Ambiguous),
            ("John or Mike, sorry, Sara", "John or Mike, sorry, Sara", Ambiguous),
            ("Pages 2-3, no, 4.", "Pages 2-3, no, 4.", Ambiguous),
            // "St." may end a sentence: never retract across it.
            ("Meet on Main St. Bring notes. Scratch that. Call me.", "Meet on Main St. Bring notes. Scratch that. Call me.", Ambiguous),
            // Fifth review: a repeated opening or frame, an earlier value, a list
            // anywhere in the clause, or a negation makes it unclear.
            ("it is a problem, it is a problem, no no, it is a solution", "it is a problem, it is a problem, no no, it is a solution", Ambiguous),
            ("go to the store, go to the park, no no, go to the beach", "go to the store, go to the park, no no, go to the beach", Ambiguous),
            ("I will be late I will be late, no no I will be early", "I will be late I will be late, no no I will be early", Ambiguous),
            ("Send it to Lee, send it to Lee, sorry, to Ann", "Send it to Lee, send it to Lee, sorry, to Ann", Ambiguous),
            ("the budget is 3, the budget is 4, no, 5", "the budget is 3, the budget is 4, no, 5", Ambiguous),
            ("Meet at the cafe at 3, meet at the cafe at 4, no no, meet at the cafe at 5", "Meet at the cafe at 3, meet at the cafe at 4, no no, meet at the cafe at 5", Ambiguous),
            ("We need 2 or maybe even about 3, no, 4.", "We need 2 or maybe even about 3, no, 4.", Ambiguous),
            ("It's not 3, no, 4.", "It's not 3, no, 4.", Ambiguous),
            ("We don't need 3, no, 4.", "We don't need 3, no, 4.", Ambiguous),
            ("never 3, no, 4", "never 3, no, 4", Ambiguous),
            // …while the clear ones still apply.
            ("Use Postgres, actually, use MySQL, actually, use SQLite", "Use SQLite", Edited),
            ("Park on Main St. The cafe opens at 3, no, 4.", "Park on Main St. The cafe opens at 4.", Edited),
            ("buy 3 apples, no, 4", "buy 3 apples, no, 4", Unchanged),
            ("He said, scratch that.", "He said, scratch that.", Unchanged),
            // Sixth review: a negation anywhere before the cue in its sentence.
            ("It's not, 3, no, 4.", "It's not, 3, no, 4.", Ambiguous),
            ("never, 3, no, 4", "never, 3, no, 4", Ambiguous),
            ("We dont need 3, no, 4.", "We dont need 3, no, 4.", Ambiguous),
            ("We cannot take 3, no, 4.", "We cannot take 3, no, 4.", Ambiguous),
            ("We cant take 3, no, 4.", "We cant take 3, no, 4.", Ambiguous),
            ("Nobody needs 3, no, 4.", "Nobody needs 3, no, 4.", Ambiguous),
            ("I will not attend the review no no I will attend the review", "I will not attend the review no no I will attend the review", Ambiguous),
            // A cue left after the repairs: the original stays, whole.
            ("buy 3 apples, I mean 4, no, 5", "buy 3 apples, I mean 4, no, 5", Ambiguous),
            ("send it to John, sorry, to Mike, no, to Ann", "send it to John, sorry, to Mike, no, to Ann", Ambiguous),
            ("buy 3 apples, I mean 4, no wait, get milk", "buy 3 apples, I mean 4, no wait, get milk", Ambiguous),
            ("send it to John, sorry, to Mike, sorry, to Ann", "send it to Ann", Edited),
            ("Email Jordan today about the launch no no email Jordan tomorrow", "Email Jordan tomorrow", Edited),
            ("He called. Please scratch that.", "He called. Please scratch that.", Unchanged),
            // Seventh review: a sign, currency or sigil changes the value.
            ("Set the temperature to 3, no, -4.", "Set the temperature to 3, no, -4.", Ambiguous),
            ("Set the temperature to 3, no, −4.", "Set the temperature to 3, no, −4.", Ambiguous),
            ("The budget is 300 dollars, actually, $400.", "The budget is 300 dollars, actually, $400.", Ambiguous),
            ("Charge 300 dollars, actually, €400.", "Charge 300 dollars, actually, €400.", Ambiguous),
            ("The budget is 300, actually, €400.", "The budget is 300, actually, €400.", Ambiguous),
            ("Send it to John, sorry, @Mike.", "Send it to John, sorry, @Mike.", Ambiguous),
            ("Call John, sorry, #Mike.", "Call John, sorry, #Mike.", Ambiguous),
            ("Use port 8080, actually, :9090.", "Use port 8080, actually, :9090.", Ambiguous),
            ("Keep 3, no, (4).", "Keep 3, no, (4).", Ambiguous),
            ("Use 3, no, [4].", "Use 3, no, [4].", Ambiguous),
            ("Use version 3.1, actually, 3.2.", "Use version 3.2.", Edited),
            // Fillers before a cancel belong to it.
            ("He called. Um, scratch that. Bring notes.", "Bring notes.", Edited),
            ("He called. Uh, never mind. Bring notes.", "Bring notes.", Edited),
            ("We will meet at 3. Um, scratch that. Meet at 5.", "Meet at 5.", Edited),
            ("He called. Sorry, um, scratch that. Bring notes.", "Bring notes.", Edited),
            // An unclear cue isn't overridden by a later repair.
            ("The value is 3, actually 3 is correct, no no the value is 4.", "The value is 3, actually 3 is correct, no no the value is 4.", Ambiguous),
        ] {
            assert_eq!(apply(input), (output.to_owned(), outcome), "{input}");
            assert_eq!(ambiguous(input), outcome == Ambiguous, "{input}");
        }
    }
    #[test]
    fn cancels_after_kept_or_removed_fillers() {
        let mut config = crate::config::Config::default();
        for remove in [true, false] {
            config.formatting.remove_fillers = remove;
            for (input, output) in [
                ("He called. Um, scratch that. Bring notes.", "Bring notes."),
                ("He called. Uh, never mind. Bring notes.", "Bring notes."),
                (
                    "We will meet at 3. Um, scratch that. Meet at 5.",
                    "Meet at 5.",
                ),
                (
                    "He called. Sorry, um, scratch that. Bring notes.",
                    "Bring notes.",
                ),
            ] {
                assert_eq!(
                    crate::text::prepare(input, &config, false),
                    (output.to_owned(), Outcome::Edited),
                    "{input} (remove_fillers = {remove})"
                );
            }
        }
    }
    #[test]
    fn repairs_stay_linear() {
        let unit = "Send it to John, sorry, to Mike. I will send it no no I will mail it. Sorry for the delay, I mean it. ";
        let text = unit.repeat(2_000 / unit.len() + 1);
        // The common-word list decodes once per process (the daemon does it at start).
        crate::words::common("warm");
        // Best of 10: the cost of the pass, not the load of a shared machine.
        let each = (0..10)
            .map(|_| {
                let start = std::time::Instant::now();
                std::hint::black_box(apply(std::hint::black_box(&text)));
                start.elapsed()
            })
            .min()
            .unwrap();
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
