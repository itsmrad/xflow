//! Spoken enumerations ("one …, two …, three …") become plain-text Markdown
//! lists. Local and deterministic, so it works with cleanup off.
use crate::text::{words, Word};

const CARDINALS: [&str; 20] = [
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
];
const ORDINALS: [&str; 10] = [
    "first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth", "tenth",
];
/// "the first time", "a second": ordinals after these are prose, not markers.
const DETERMINERS: [&str; 16] = [
    "the", "a", "an", "my", "our", "your", "their", "his", "her", "its", "this", "that", "at",
    "of", "every", "each",
];
const MAX_ITEM_WORDS: usize = 25;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Number,
    Ordinal,
    Bullet,
}
struct Marker {
    kind: Kind,
    value: usize,
    start: usize,
    end: usize,
    boundary: bool,
}

fn is(word: &Word, s: &str) -> bool {
    word.text.eq_ignore_ascii_case(s)
}
fn position(table: &[&str], word: &str) -> Option<usize> {
    table
        .iter()
        .position(|t| t.eq_ignore_ascii_case(word))
        .map(|i| i + 1)
}
/// Digits stand alone: "1.5", "10,000", "3:00" and "1.2.3" are not markers.
fn digit(text: &str, word: &Word) -> Option<usize> {
    if word.text.len() > 2 || !word.text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let joined = |s: &mut dyn Iterator<Item = char>| {
        matches!(s.next(), Some('.' | ',' | ':')) && s.next().is_some_and(|c| c.is_ascii_digit())
    };
    if joined(&mut text[word.end..].chars()) || joined(&mut text[..word.start].chars().rev()) {
        return None;
    }
    word.text.parse().ok().filter(|&n| n > 0)
}

fn markers(text: &str) -> Vec<Marker> {
    let w = words(text);
    // Without any punctuation (raw engines) boundaries are unknowable; the
    // sequence and item guards still apply.
    let unpunctuated = !text.contains(['.', ',', ':', ';', '?', '!', '\n']);
    let boundary = |i: usize| {
        let before = text[..w[i].start].trim_end_matches([' ', '\t']);
        unpunctuated
            || before.is_empty()
            || before.ends_with([',', '.', ':', ';', '?', '!', '\n'])
            || (i > 0 && (is(&w[i - 1], "and") || is(&w[i - 1], "then")))
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < w.len() {
        let numeral = |j: usize| {
            w.get(j)
                .and_then(|n| position(&CARDINALS, n.text).or_else(|| digit(text, n)))
        };
        let ordinal = position(&ORDINALS, w[i].text).or_else(|| {
            let t = w[i].text;
            t.len()
                .checked_sub(2)
                .filter(|&n| t.is_char_boundary(n) && t[n..].eq_ignore_ascii_case("ly"))
                .and_then(|n| position(&ORDINALS, &t[..n]))
        });
        let marker = if is(&w[i], "number") && numeral(i + 1).is_some() {
            // "number one" is explicit, wherever it falls.
            i += 1;
            Some((Kind::Number, numeral(i).unwrap_or(0), w[i - 1].start, true))
        } else if let Some(n) = numeral(i) {
            // "1." / "1)" are explicit, as Whisper writes them.
            let explicit = digit(text, &w[i]).is_some() && text[w[i].end..].starts_with(['.', ')']);
            Some((Kind::Number, n, w[i].start, explicit || boundary(i)))
        } else if let Some(n) = ordinal {
            // Whisper often drops the comma before "first"/"second"; ordinals
            // only need to not be noun modifiers.
            let modifier = i > 0 && DETERMINERS.iter().any(|d| is(&w[i - 1], d));
            Some((Kind::Ordinal, n, w[i].start, boundary(i) || !modifier))
        } else if is(&w[i], "bullet") && !w.get(i + 1).is_some_and(|n| is(n, "points")) {
            let next = i > 0 && is(&w[i - 1], "next");
            let start = w[if next { i - 1 } else { i }].start;
            if w.get(i + 1).is_some_and(|n| is(n, "point")) {
                i += 1;
            }
            Some((Kind::Bullet, 0, start, true))
        } else {
            None
        };
        if let Some((kind, value, start, boundary)) = marker {
            out.push(Marker {
                kind,
                value,
                start,
                end: w[i].end,
                boundary,
            });
        }
        i += 1;
    }
    out
}

/// The run of one kind: ≥ 3 markers, numbered 1, 2, 3 … with nothing skipped.
fn run(markers: &[Marker], kind: Kind) -> Option<Vec<&Marker>> {
    let m: Vec<_> = markers
        .iter()
        .filter(|m| m.kind == kind && m.boundary)
        .collect();
    let first = if kind == Kind::Bullet {
        0
    } else {
        m.iter().rposition(|m| m.value == 1)?
    };
    let m = m[first..].to_vec();
    (m.len() >= 3 && (kind == Kind::Bullet || m.iter().zip(1..).all(|(m, n)| m.value == n)))
        .then_some(m)
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    let first_word = s.split_whitespace().next().unwrap_or_default();
    match chars.next() {
        // "iOS" and "macOS" keep their case.
        Some(c) if c.is_lowercase() && !first_word.chars().skip(1).any(char::is_uppercase) => {
            c.to_uppercase().chain(chars).collect()
        }
        _ => s.to_owned(),
    }
}
fn item(s: &str) -> Option<String> {
    let mut s = s.trim_start_matches(|c: char| c.is_whitespace() || ",.:;)-".contains(c));
    loop {
        s = s.trim_end_matches(|c: char| c.is_whitespace() || ",.:;".contains(c));
        let Some(rest) = ["and", "then"].iter().find_map(|conj| {
            let cut = s.len().checked_sub(conj.len())?;
            (s.is_char_boundary(cut)
                && s[cut..].eq_ignore_ascii_case(conj)
                && s[..cut].ends_with([' ', ',']))
            .then(|| &s[..cut])
        }) else {
            break;
        };
        s = rest;
    }
    let n = s.split_whitespace().count();
    (n > 0 && n <= MAX_ITEM_WORDS).then(|| capitalize(s))
}

/// `None` when `text` holds no list.
pub fn format(text: &str) -> Option<String> {
    let markers = markers(text);
    let run = [Kind::Number, Kind::Ordinal, Kind::Bullet]
        .into_iter()
        .filter_map(|k| run(&markers, k))
        .min_by_key(|r| r[0].start)?;
    // The last item ends at its sentence; anything after is a new paragraph.
    let tail = &text[run[run.len() - 1].end..];
    // Skip the marker's own punctuation ("3. manager").
    let skip = tail.len()
        - tail
            .trim_start_matches(|c: char| c.is_whitespace() || ",.:;)-".contains(c))
            .len();
    let cut = tail[skip..]
        .char_indices()
        .find(|&(i, c)| {
            c == '\n' || ".?!".contains(c) && tail[skip + i + 1..].starts_with(char::is_whitespace)
        })
        .map_or(tail.len(), |(i, _)| skip + i);
    let mut items = Vec::with_capacity(run.len());
    for (k, m) in run.iter().enumerate() {
        let end = run.get(k + 1).map_or(m.end + cut, |n| n.start);
        let item = item(&text[m.end..end])?;
        // "one by one, two by two, three by three" is an idiom, not a list.
        let last = item.rsplit(' ').next().unwrap_or_default();
        if m.kind == Kind::Number
            && (position(&CARDINALS, last) == Some(m.value) || last.parse() == Ok(m.value))
        {
            return None;
        }
        items.push(item);
    }
    let mut out = String::with_capacity(text.len() + 4 * items.len());
    let lead =
        text[..run[0].start].trim_end_matches(|c: char| c.is_whitespace() || ",.:;".contains(c));
    if !lead.is_empty() {
        out.push_str(&capitalize(lead));
        if !lead.ends_with(['?', '!']) {
            out.push(':');
        }
        out.push('\n');
    }
    for (k, item) in items.iter().enumerate() {
        if k > 0 {
            out.push('\n');
        }
        if run[0].kind == Kind::Bullet {
            out.push_str("- ");
        } else {
            out.push_str(&format!("{}. ", k + 1));
        }
        out.push_str(item);
    }
    let rest = tail[cut..].trim_start_matches(|c: char| c.is_whitespace() || ".?!".contains(c));
    if !rest.is_empty() {
        out.push_str("\n\n");
        out.push_str(rest);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, text::before_cleanup};

    #[test]
    fn enumerations_become_lists() {
        for (input, expected) in [
            // The user's example, raw and as Groq whisper-large-v3-turbo wrote it.
            (
                "we need three roles one developer two designer three manager",
                "We need three roles:\n1. Developer\n2. Designer\n3. Manager",
            ),
            (
                "We need three roles, one developer, two designer, three manager.",
                "We need three roles:\n1. Developer\n2. Designer\n3. Manager",
            ),
            (
                "We need three roles: 1. developer, 2. designer, 3. manager.",
                "We need three roles:\n1. Developer\n2. Designer\n3. Manager",
            ),
            (
                "One developer, two designer, three manager.",
                "1. Developer\n2. Designer\n3. Manager",
            ),
            // Observed Groq turbo renderings (say + afconvert, see PR #42).
            ("1 Developer 2 Designer 3 Manager", "1. Developer\n2. Designer\n3. Manager"),
            (
                "We need three roles, one developer, two designers, and three managers.",
                "We need three roles:\n1. Developer\n2. Designers\n3. Managers",
            ),
            (
                "The steps are first install the dependency, second run the tests, third deploy to production.",
                "The steps are:\n1. Install the dependency\n2. Run the tests\n3. Deploy to production",
            ),
            (
                "First clone the repo, second run make third open the browser.",
                "1. Clone the repo\n2. Run make\n3. Open the browser",
            ),
            (
                "Number 1 Fix the login bug Number 2 Update the docs Number 3 Ship the release",
                "1. Fix the login bug\n2. Update the docs\n3. Ship the release",
            ),
            (
                "My shopping list bullet milk bullet eggs bullet bread.",
                "My shopping list:\n- Milk\n- Eggs\n- Bread",
            ),
            (
                "Bullet point refactor the parser next bullet, add tests next bullet update the changelog.",
                "- Refactor the parser\n- Add tests\n- Update the changelog",
            ),
            (
                "Agenda for today 1 Stand Up 2 Code Review 3 Planning 4 Retro",
                "Agenda for today:\n1. Stand Up\n2. Code Review\n3. Planning\n4. Retro",
            ),
            (
                "To deploy firstly build the image, secondly push it, thirdly restart the service.",
                "To deploy:\n1. Build the image\n2. Push it\n3. Restart the service",
            ),
            (
                "The plan is 1. Migrate the database 2. Switch traffic 3. Delete the old cluster",
                "The plan is:\n1. Migrate the database\n2. Switch traffic\n3. Delete the old cluster",
            ),
            (
                "There are three priorities, one speed, two accuracy, three price.",
                "There are three priorities:\n1. Speed\n2. Accuracy\n3. Price",
            ),
            // Text after the last item's sentence is its own paragraph.
            (
                "First, install deps. Second, run tests. Third, deploy. Then we celebrate.",
                "1. Install deps\n2. Run tests\n3. Deploy\n\nThen we celebrate.",
            ),
            // Numbers inside items that break the sequence stay prose.
            (
                "One developer, two designers with three years each, three managers.",
                "1. Developer\n2. Designers with three years each\n3. Managers",
            ),
            (
                "What do we need? One iOS dev, two designers and then three PMs",
                "What do we need?\n1. iOS dev\n2. Designers\n3. PMs",
            ),
            (
                "number one fix the build number two cut the release number three tell the team",
                "1. Fix the build\n2. Cut the release\n3. Tell the team",
            ),
        ] {
            assert_eq!(format(input).as_deref(), Some(expected), "{input}");
        }
    }

    #[test]
    fn prose_with_numbers_stays_prose() {
        let thirty = format!("one {}, two b, three c", ["word"; 29].join(" "));
        let mut corpus = vec![
            // Observed Groq turbo renderings.
            "I have one dog and two cats.",
            "One of the two options is fine.",
            "Version 1.2.3",
            "First come first served.",
            "We tried 1, 3, 5.",
            "Wait a second I think the first one is better.",
            "In 2023 we shipped version 1.",
            "I need one coffee, two bagels and three donuts.",
            // Verdict cases.
            "one of the two options",
            "I have one dog and two cats",
            "version one two three",
            "one, three, five",
            "first come first served",
            "One developer, two designers.",
            "First, install deps. Second, run tests.",
            "bullet milk, bullet eggs",
            // More prose.
            "It took one hour, two minutes and three seconds.",
            "The first time, the second time and the third time.",
            "For the first, second, and third quarters we grew.",
            "One, two, three, go!",
            "Count to three: one, two, three.",
            "one two three four five",
            "1, 2, 3",
            "Version one, two, three.",
            "He came first, second, third.",
            "We need one developer, two designers and three managers.",
            "Give me a second, I'm on the first floor.",
            "There are two sides to every story.",
            "One more thing: the second build failed three times.",
            "The meeting is at 3:00 with 2 people and 1 room.",
            "We raised 10,000 dollars in 2 days, 1 hour.",
            "Pi is 3.14, e is 2.71 and one is 1.0.",
            "Room 1 has 2 beds and 3 chairs.",
            "At first I thought the second option was best, then the third.",
            "My first job, my second job and my third job were all remote.",
            "Each first-timer gets one free coffee.",
            "The number one priority is speed.",
            "She bit the bullet and shipped it.",
            "We discussed bullet points for the deck.",
            "Two plus two is four, and three plus one is four.",
            "Steps one through three are done.",
            "Version one shipped, version two is planned, version three is a dream.",
            "One step forward, two steps back.",
            "Take one, take two, take three.",
            "First things first.",
            "On second thought, never mind.",
            "Third time's the charm.",
            "We have one, maybe two weeks left.",
            "One by one, two by two, three by three.",
            "Line one is fine, line two is broken, line three is fine.",
            "Ask the second person in line about the first ticket.",
            "Five, four, three, two, one, liftoff.",
            "Chapter one covers setup.",
            "We need two, not three.",
        ];
        corpus.push(&thirty);
        assert!(corpus.len() >= 50, "{}", corpus.len());
        for input in corpus {
            assert_eq!(format(input), None, "{input}");
        }
    }

    #[test]
    fn terminals_and_opt_out_keep_todays_output() {
        let input = "We need three roles, one developer, two designer, three manager.";
        let mut c = Config::default();
        assert_ne!(before_cleanup(input, &c, false), input);
        assert_eq!(before_cleanup(input, &c, true), input);
        c.formatting.lists = false;
        assert_eq!(before_cleanup(input, &c, false), input);
    }

    #[test]
    fn under_a_millisecond_on_two_thousand_chars() {
        let mut text = "We need one developer, two designers and three managers. ".repeat(35);
        text.push_str("One developer, two designer, three manager.");
        assert!(text.len() >= 2_000);
        let runs = 100;
        let start = std::time::Instant::now();
        for _ in 0..runs {
            std::hint::black_box(format(std::hint::black_box(&text)));
        }
        let each = start.elapsed() / runs;
        assert!(each.as_micros() < 1_000, "{each:?}");
    }
}
