//! Spoken enumerations ("one …, two …, three …") become plain-text Markdown
//! lists. Local and deterministic, so it works with cleanup off.
use crate::config::DictionaryConfig;
use crate::text::{words, Word};
use std::fmt::Write;

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
/// A lone "or" ("one or two things"), "to" ("one to three") or "hundred": counting, not an item.
const NOT_ITEMS: [&str; 7] = ["or", "to", "of", "by", "hundred", "thousand", "million"];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Number,
    Ordinal,
    Bullet,
}
struct Marker<'a> {
    kind: Kind,
    value: usize,
    start: usize,
    end: usize,
    boundary: bool,
    /// Follows a sentence end rather than a comma.
    sentence: bool,
    /// The word just before the marker.
    prev: &'a str,
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

fn markers(text: &str) -> Vec<Marker<'_>> {
    let w = words(text);
    // Without any punctuation (raw engines) boundaries are unknowable; the
    // sequence and item guards still apply.
    let unpunctuated = !text.contains(['.', ',', ':', ';', '?', '!', '\n']);
    // "the first time", "our number one priority": noun modifiers, not markers.
    let modifier = |i: usize| i > 0 && DETERMINERS.iter().any(|d| is(&w[i - 1], d));
    let before = |i: usize| text[..w[i].start].trim_end_matches([' ', '\t']);
    let boundary = |i: usize| {
        (unpunctuated && !modifier(i))
            || before(i).is_empty()
            || before(i).ends_with([',', '.', ':', ';', '?', '!', '\n'])
            || (i > 0 && (is(&w[i - 1], "and") || is(&w[i - 1], "then")))
    };
    let numeral = |j: usize| {
        w.get(j)
            .and_then(|n| position(&CARDINALS, n.text).or_else(|| digit(text, n)))
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < w.len() {
        let first = i;
        let ordinal = position(&ORDINALS, w[i].text).or_else(|| {
            let t = w[i].text;
            t.len()
                .checked_sub(2)
                .filter(|&n| t.is_char_boundary(n) && t[n..].eq_ignore_ascii_case("ly"))
                .and_then(|n| position(&ORDINALS, &t[..n]))
        });
        let number = if is(&w[i], "number") {
            numeral(i + 1)
        } else {
            None
        };
        let marker = if let Some(n) = number {
            // "number one" is explicit wherever it falls, unless it modifies a noun.
            i += 1;
            Some((Kind::Number, n, !modifier(first)))
        } else if let Some(n) = numeral(i) {
            // "1." / "1)" are explicit, as Whisper writes them.
            let explicit = digit(text, &w[i]).is_some() && text[w[i].end..].starts_with(['.', ')']);
            Some((Kind::Number, n, explicit || boundary(i)))
        } else if let Some(n) = ordinal {
            // Whisper often drops the comma before "first"/"second"; ordinals
            // only need to not be noun modifiers.
            let filler: &[&[&str]] = &[&["of", "all"], &["off"], &["and", "foremost"]];
            if let Some(f) = filler.iter().filter(|_| n == 1).find(|f| {
                f.iter()
                    .enumerate()
                    .all(|(k, s)| w.get(i + 1 + k).is_some_and(|x| is(x, s)))
            }) {
                i += f.len();
            }
            Some((Kind::Ordinal, n, !modifier(first)))
        } else if is(&w[i], "bullet") && !w.get(i + 1).is_some_and(|n| is(n, "points")) {
            if w.get(i + 1).is_some_and(|n| is(n, "point")) {
                i += 1;
            }
            Some((Kind::Bullet, 0, true))
        } else {
            None
        };
        if let Some((kind, value, boundary)) = marker {
            // "next bullet" starts at "next".
            let first = if kind == Kind::Bullet && first > 0 && is(&w[first - 1], "next") {
                first - 1
            } else {
                first
            };
            out.push(Marker {
                kind,
                value,
                start: w[first].start,
                end: w[i].end,
                boundary,
                sentence: before(first).ends_with(['.', '?', '!', '\n']),
                prev: first.checked_sub(1).map_or("", |p| w[p].text),
            });
        }
        i += 1;
    }
    out
}

/// From a "1" (or the first bullet), the consecutive markers 1, 2, 3 … of its
/// kind, plus where the next marker of that kind starts.
fn run<'m, 'a>(markers: &'m [Marker<'a>], first: usize) -> (Vec<&'m Marker<'a>>, Option<usize>) {
    let kind = markers[first].kind;
    let mut run: Vec<&Marker> = vec![];
    for m in markers[first..]
        .iter()
        .filter(|m| m.kind == kind && m.boundary)
    {
        let next = kind == Kind::Bullet || m.value == run.len() + 1;
        // "one a, two b, three c. Four is too many.": a sentence break ends a
        // comma-separated list.
        let ends = run.len() >= 3 && m.sentence && !run[1].sentence;
        if !next || ends {
            return (run, Some(m.start));
        }
        run.push(m);
    }
    (run, None)
}

fn capitalize(s: &str, dictionary: &DictionaryConfig) -> String {
    let mut chars = s.chars();
    let first_word = s.split_whitespace().next().unwrap_or_default();
    // Dictionary terms ("npm") keep their case.
    let term = |list: &[String]| list.iter().any(|t| t == first_word);
    match chars.next() {
        // "iOS" and "macOS" keep their case.
        Some(c)
            if c.is_lowercase()
                && !first_word.chars().skip(1).any(char::is_uppercase)
                && !term(&dictionary.words)
                && !term(&dictionary.learned) =>
        {
            c.to_uppercase().chain(chars).collect()
        }
        _ => s.to_owned(),
    }
}
fn item(s: &str) -> Option<&str> {
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
    // Only a lone stop word: "first, to ship faster" is a real item.
    (n > 0 && n <= MAX_ITEM_WORDS && !(n == 1 && position(&NOT_ITEMS, s).is_some())).then_some(s)
}

/// `None` when `text` holds no list.
pub fn format(text: &str, dictionary: &DictionaryConfig) -> Option<String> {
    let markers = markers(text);
    // Left to right; every bullet run starts at the first bullet.
    let first_bullet = markers.iter().position(|m| m.kind == Kind::Bullet);
    (0..markers.len())
        .filter(|&k| {
            markers[k].boundary
                && match markers[k].kind {
                    Kind::Bullet => Some(k) == first_bullet,
                    _ => markers[k].value == 1,
                }
        })
        .find_map(|k| list(text, &markers, k, dictionary))
}

fn list(
    text: &str,
    markers: &[Marker],
    first: usize,
    dictionary: &DictionaryConfig,
) -> Option<String> {
    let (run, breaker) = run(markers, first);
    if run.len() < 3 {
        return None;
    }
    let kind = run[0].kind;
    // The last item ends at its sentence; anything after is a new paragraph.
    let last = run[run.len() - 1];
    let tail = &text[last.end..];
    // Skip the marker's own punctuation ("3. manager").
    let skip = tail.len()
        - tail
            .trim_start_matches(|c: char| c.is_whitespace() || ",.:;)-".contains(c))
            .len();
    // Search only up to the next same-kind marker: no sentence end before it
    // means the sequence breaks inside the list ("one, two, three, five").
    // Only the last candidate has no breaker, so this stays linear.
    let limit = breaker.map_or(tail.len(), |b| (b - last.end).max(skip));
    let cut = match tail[skip..limit].char_indices().find(|&(i, c)| {
        c == '\n' || ".?!".contains(c) && tail[skip + i + 1..].starts_with(char::is_whitespace)
    }) {
        Some((i, _)) => skip + i,
        None if breaker.is_some() => return None,
        None => tail.len(),
    };
    let mut items: Vec<&str> = Vec::with_capacity(run.len());
    for (k, m) in run.iter().enumerate() {
        let end = run.get(k + 1).map_or(last.end + cut, |n| n.start);
        let item = item(&text[m.end..end])?;
        // A comma-separated item never spans a sentence ("At one, lunch. Around two, …").
        if k + 1 < run.len()
            && !run[1].sentence
            && [". ", "? ", "! "].iter().any(|e| item.contains(e))
        {
            return None;
        }
        let last_word = item.rsplit(' ').next().unwrap_or_default();
        // "one by one, two by two" is an idiom; "1 pm 2 pm 3 pm" a schedule.
        let idiom = kind == Kind::Number
            && (position(&CARDINALS, last_word) == Some(m.value)
                || last_word.parse() == Ok(m.value));
        if idiom || items.last().is_some_and(|p| p.eq_ignore_ascii_case(item)) {
            return None;
        }
        items.push(item);
    }
    // "if x equals 1 … if x equals 2 …": every number in the same frame is code
    // or counting, not a list.
    if kind != Kind::Bullet
        && !run[0].prev.is_empty()
        && run.iter().all(|m| m.prev.eq_ignore_ascii_case(run[0].prev))
    {
        return None;
    }
    let mut out = String::with_capacity(text.len() + 4 * items.len());
    let lead =
        text[..run[0].start].trim_end_matches(|c: char| c.is_whitespace() || ",.:;".contains(c));
    if !lead.is_empty() {
        out.push_str(&capitalize(lead, dictionary));
        if !lead.ends_with(['?', '!']) {
            out.push(':');
        }
        out.push('\n');
    }
    for (k, item) in items.iter().enumerate() {
        if k > 0 {
            out.push('\n');
        }
        if kind == Kind::Bullet {
            out.push_str("- ");
        } else {
            let _ = write!(out, "{}. ", k + 1);
        }
        out.push_str(&capitalize(item, dictionary));
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

    fn format(text: &str) -> Option<String> {
        super::format(text, &DictionaryConfig::default())
    }

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
    fn pr81_review_regressions() {
        for (input, expected) in [
            // B1: the filler after "first" is part of the marker (Groq u1 verbatim).
            (
                "First of all, thanks for coming. Second, the build is red. Third, we ship on Friday.",
                "1. Thanks for coming\n2. The build is red\n3. We ship on Friday",
            ),
            (
                "First off, the build is red. Second, tests are flaky. Third, docs are stale.",
                "1. The build is red\n2. Tests are flaky\n3. Docs are stale",
            ),
            (
                "First and foremost, thanks. Second, the build. Third, the docs.",
                "1. Thanks\n2. The build\n3. The docs",
            ),
            // B3: a later numbered sentence is tail text, not part of the list.
            (
                "We need three roles, one developer, two designers, three managers. Two of them start Monday.",
                "We need three roles:\n1. Developer\n2. Designers\n3. Managers\n\nTwo of them start Monday.",
            ),
            (
                "We need three roles, one developer, two designers, three managers. One of them is remote.",
                "We need three roles:\n1. Developer\n2. Designers\n3. Managers\n\nOne of them is remote.",
            ),
            (
                "We need three roles, one developer, two designers, three managers. Four is too many.",
                "We need three roles:\n1. Developer\n2. Designers\n3. Managers\n\nFour is too many.",
            ),
            // B3: the first of two lists is formatted, not swallowed by the lead-in.
            (
                "Fruits: one apple, two pears, three plums. Veg: one leek, two kale, three peas.",
                "Fruits:\n1. Apple\n2. Pears\n3. Plums\n\nVeg: one leek, two kale, three peas.",
            ),
            // Round 2, R2: items may start with "to/by/of".
            (
                "Goals: first, to ship faster. Second, to hire. Third, to grow.",
                "Goals:\n1. To ship faster\n2. To hire\n3. To grow",
            ),
            (
                "Our goals are: one, to reduce latency, two, to improve accuracy, and three, to cut costs.",
                "Our goals are:\n1. To reduce latency\n2. To improve accuracy\n3. To cut costs",
            ),
            (
                "Deadlines: one, by Friday the API, two, by Monday the UI, three, by June the launch.",
                "Deadlines:\n1. By Friday the API\n2. By Monday the UI\n3. By June the launch",
            ),
            (
                "Steps: first, of course, back up. Second, migrate. Third, verify.",
                "Steps:\n1. Of course, back up\n2. Migrate\n3. Verify",
            ),
            // A sentence-per-item list keeps going across sentence breaks.
            (
                "First, install deps. Second, run tests. Third, deploy. Fourth, tag it.",
                "1. Install deps\n2. Run tests\n3. Deploy\n4. Tag it",
            ),
        ] {
            assert_eq!(format(input).as_deref(), Some(expected), "{input}");
        }
        for input in [
            // B2: "our number one priority" modifies a noun (Groq u2 verbatim).
            "Our number one priority is speed, number two is accuracy, and number three is price.",
            "The number one bug, the number two bug and the number three bug.",
            // B3: Groq's comma splice (u4) stays prose; so does a break inside the list.
            "We need three roles, one developer, two designers, three managers, two of them start Monday.",
            "One apple, two pears, three plums, five figs.",
            // B4: unpunctuated counting and code.
            "I have one or two things to mention and three questions",
            "one or two things then three more",
            "one hundred two hundred three hundred",
            "if x equals 1 return a if x equals 2 return b if x equals 3 return c",
            "meetings at 1 pm 2 pm 3 pm",
            "Meetings: 1 p.m., 2 p.m., 3 p.m.",
            "first of all the second build failed and the third one too",
            "take one take two take three",
            "line one is fine line two is broken line three is fine",
            "count one to three then two to four and three to five",
            // Round 2: "one," is explicit, so comma-led counting must still fail.
            "Call at 1, lunch at 2, demo at 3.",
            "On October 1, 2 and 3 we are off.",
            "Twenty one, twenty two, twenty three.",
            "Page one, line two, column three.",
            "It took one, two, three tries.",
            "Option one, the fast path, option two, the cheap path, option three, the safe path.",
            // Round 3, R3: counting prose with varying words before each number.
            "We started with one, grew to two, hit three, and stopped there.",
            "I slept until one, worked until two, ate at three, and napped.",
            "Kids? I have one, my brother has two, my sister has three, and my parents have four.",
            "At one, lunch. Around two, standup. By three, demo.",
            "Lunch at one, standup around two, demo by three, then home.",
            "In 2019 we had one, in 2020 two, in 2021 three, now ten.",
        ] {
            assert_eq!(format(input), None, "{input}");
        }
        // N1: dictionary terms keep their case; plain words are capitalised.
        let dictionary = DictionaryConfig {
            words: vec!["npm".into(), "git".into()],
            learned: vec!["cargo".into()],
            ..DictionaryConfig::default()
        };
        assert_eq!(
            super::format(
                "Run these: 1. npm install 2. cargo build 3. git push 4. deploy",
                &dictionary
            )
            .as_deref(),
            Some("Run these:\n1. npm install\n2. cargo build\n3. git push\n4. Deploy")
        );
        // N5: lines made with spoken "new line" still become a list.
        let mut c = Config::default();
        c.formatting.spoken_punctuation = true;
        assert_eq!(
            before_cleanup(
                "one Postgres new line two redis new line three kafka",
                &c,
                false
            ),
            "1. Postgres\n2. Redis\n3. Kafka"
        );
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
        // Best of several samples: a loaded machine can't fail it, a regression still does.
        let best = |text: &str| {
            (0..5)
                .map(|_| {
                    let start = std::time::Instant::now();
                    std::hint::black_box(format(std::hint::black_box(text)));
                    start.elapsed()
                })
                .min()
                .unwrap_or_default()
        };
        let each = best(&text);
        // Unoptimised builds on a loaded CI runner get headroom; release must hold 1 ms.
        let bound = if cfg!(debug_assertions) { 5_000 } else { 1_000 };
        assert!(each.as_micros() < bound, "{each:?}");
        // Round 2, R1: the 32 KiB transcript cap stays linear. Quadratic
        // candidate scans took ~500 ms debug / 32 ms release here.
        for unit in [
            "one a, two b, three c, five d, ",
            "one a, two b, three c, one d, ",
            "one x, ",
            "bullet x ",
            "first, ",
            "one a two b three c ",
        ] {
            let took = best(&unit.repeat(32 * 1024 / unit.len()));
            let bound = if cfg!(debug_assertions) {
                40_000
            } else {
                2_000
            };
            assert!(took.as_micros() < bound, "{unit:?}: {took:?}");
        }
    }
}
