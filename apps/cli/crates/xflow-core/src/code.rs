//! Spoken code: explicit casing commands ("camel case user id" -> userId), a
//! closed filename grammar ("index dot ts" -> index.ts) and bracket pairs
//! ("open paren" -> "("). Bare "dot", "slash" and "backtick" stay speech.
use crate::{
    config::Config,
    text::{words, Word},
    words::common,
};

#[derive(Clone, Copy, PartialEq)]
enum Case {
    Camel,
    Snake,
    Kebab,
    Pascal,
    Constant,
}
const CASES: [(&str, Case); 5] = [
    ("camel", Case::Camel),
    ("snake", Case::Snake),
    ("kebab", Case::Kebab),
    ("pascal", Case::Pascal),
    ("constant", Case::Constant),
];
const EXTENSIONS: [&str; 18] = [
    "rs", "ts", "tsx", "js", "jsx", "json", "md", "toml", "yaml", "yml", "py", "go", "sh", "env",
    "lock", "css", "html", "sql",
];
// Filenames people spell with capitals.
const NAMES: [(&str, &str, &str); 6] = [
    ("cargo", "toml", "Cargo"),
    ("cargo", "lock", "Cargo"),
    ("readme", "md", "README"),
    ("license", "md", "LICENSE"),
    ("changelog", "md", "CHANGELOG"),
    ("contributing", "md", "CONTRIBUTING"),
];
const BRACKETS: [(&str, usize); 5] = [
    ("paren", 0),
    ("parens", 0),
    ("parenthesis", 0),
    ("bracket", 1),
    ("brace", 2),
];
const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
/// Function words end an identifier: "rename camel case user id to ...".
const STOP: &[&str] = &[
    "to",
    "and",
    "or",
    "then",
    "from",
    "into",
    "is",
    "are",
    "was",
    "were",
    "be",
    "vs",
    "versus",
    "but",
    "so",
    "because",
    "when",
    "if",
    "than",
    "the",
    "a",
    "an",
    "this",
    "that",
    "these",
    "those",
    "it",
    "its",
    "please",
    "instead",
    "here",
    "there",
    "everywhere",
    "throughout",
    "now",
    "too",
    "also",
    "only",
    "before",
    "again",
    "like",
    "unless",
    "except",
    "though",
    "since",
    "while",
    "until",
    "where",
    "otherwise",
    "dot",
];
const PREPOSITIONS: &[&str] = &["in", "on", "at", "for", "as", "after", "with", "of", "by"];
/// Stop words that also start real identifiers: isValid, toString, onClick, forEach.
const PREFIXES: &[&str] = &["is", "to", "on", "for"];
/// A final judgement or convention noun means the speaker is discussing the case.
const PROSE: &[&str] = &[
    "better",
    "best",
    "worse",
    "worst",
    "fine",
    "good",
    "bad",
    "nice",
    "nicer",
    "great",
    "cleaner",
    "clearer",
    "easier",
    "harder",
    "common",
    "standard",
    "preferred",
    "popular",
    "idiomatic",
    "conventional",
    "ugly",
    "weird",
    "wrong",
    "okay",
    "ok",
    "style",
    "convention",
    "naming",
];
/// A name follows these words, so an identifier may end mid-sentence.
const NAME_CUES: &[&str] = &[
    "to",
    "as",
    "and",
    "or",
    "then",
    "called",
    "named",
    "name",
    "rename",
    "call",
    "set",
    "get",
    "variable",
    "var",
    "let",
    "const",
    "field",
    "function",
    "fn",
    "method",
    "property",
    "key",
    "column",
    "param",
    "parameter",
    "argument",
    "arg",
    "struct",
    "class",
    "enum",
    "type",
    "interface",
    "def",
    "func",
    "insert",
    "write",
    "add",
    "check",
    "export",
    "import",
    "pass",
    "return",
    "print",
    "log",
    "read",
    "update",
    "remove",
    "delete",
    "load",
    "save",
    "store",
    "fetch",
    "find",
    "map",
    "convert",
    "replace",
    "change",
    "bullet",
    "point",
    "one",
    "two",
    "three",
    "four",
    "five",
    "first",
    "second",
    "third",
];
/// The identifier must end the clause after these: "should be camel case user id."
const WEAK_CUES: &[&str] = &[
    "use", "using", "make", "create", "define", "declare", "try", "be", "is", "was", "are", "with",
    "for", "need", "needs", "uses", "returns", "maybe", "finally", "now", "it's",
];
const DETERMINERS: &[&str] = &[
    "a", "an", "the", "this", "that", "its", "their", "our", "your", "my", "his", "her",
];
const ENV_SUFFIXES: &[&str] = &["example", "local", "development", "production", "test"];
/// What Whisper hears inside a span: "user i'd", "dot jason", "appy base url".
const REPAIRS: &[(&str, &str)] = &[
    ("i'd", "id"),
    ("eye", "id"),
    ("eyed", "id"),
    ("ide", "id"),
    ("envy", "env"),
    ("jason", "json"),
    ("appy", "api"),
    ("happy", "api"),
    ("earl", "url"),
    ("erl", "url"),
    ("as", "is"),
];

/// A repaired spelling, for words where it applies: ids end a span, "api"
/// starts one, extensions follow "dot".
fn repair(word: &str, position: Position) -> Option<&'static str> {
    let lower = word.to_lowercase();
    let (_, to) = REPAIRS.iter().find(|r| r.0 == lower)?;
    let ok = match *to {
        "id" | "url" => position == Position::Later,
        "api" | "is" => position == Position::First,
        _ => position == Position::Extension,
    };
    ok.then_some(*to)
}
/// A span's words after repairs, including Whisper's fused "useride" and
/// "accountide" (a common word plus "ide" that is not itself a word).
fn span<'a>(words: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out = Vec::new();
    for (k, word) in words.enumerate() {
        let position = if k == 0 {
            Position::First
        } else {
            Position::Later
        };
        let lower = word.to_lowercase();
        let stem = lower.strip_suffix("ide").filter(|s| s.len() >= 4);
        if let Some(to) = repair(word, position) {
            out.push(to.to_owned());
        } else if let Some(stem) = stem.filter(|s| common(s) && !common(&lower)) {
            out.push(stem.to_owned());
            out.push("id".to_owned());
        } else {
            out.push(word.to_owned());
        }
    }
    out
}
#[derive(Clone, Copy, PartialEq)]
enum Position {
    First,
    Later,
    Extension,
}

fn is(word: &Word<'_>, value: &str) -> bool {
    word.text.eq_ignore_ascii_case(value)
}
fn any(word: &Word<'_>, list: &[&str]) -> bool {
    list.iter().any(|value| is(word, value))
}
fn plural(word: &Word<'_>) -> bool {
    let w = word.text.to_lowercase();
    w.len() > 3 && w.ends_with('s') && !["ss", "us", "is"].iter().any(|end| w.ends_with(end))
}

/// Same-line whitespace and nothing else between two words.
fn gap(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c == ' ' || c == '\t')
}
fn joined(text: &str, w: &[Word<'_>]) -> bool {
    w.windows(2).all(|p| gap(&text[p[0].end..p[1].start]))
}

/// A spoken word, rather than part of a typed path, URL, identifier or quotation.
fn spoken(text: &str, word: &Word<'_>) -> bool {
    (word.text.chars().all(char::is_alphanumeric) || word.text.eq_ignore_ascii_case("i'd"))
        && text[..word.start]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || ",:;!?([{-".contains(c))
        && text[word.end..]
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || ",:;!?)]}.-".contains(c))
        && !text[word.end..]
            .strip_prefix('.')
            .is_some_and(|s| s.starts_with(char::is_alphanumeric))
}

/// A casing command at `i` and its length in words. Whisper often fuses the
/// command or hears "case" as "K": "camelCase user ID", "camel-case", "Camel K.".
fn command(text: &str, w: &[Word<'_>], i: usize) -> Option<(Case, usize)> {
    if !spoken(text, &w[i]) {
        return None;
    }
    let lower = w[i].text.to_lowercase();
    if let Some(c) = CASES
        .iter()
        .find(|c| lower.strip_suffix("case") == Some(c.0))
    {
        return Some((c.1, 1));
    }
    let next = w.get(i + 1)?;
    let between = &text[w[i].end..next.start];
    // "camel cases open": Whisper's "case is".
    if !any(next, &["case", "k", "cases"]) || !(gap(between) || between == "-") {
        return None;
    }
    CASES.iter().find(|c| is(&w[i], c.0)).map(|c| (c.1, 2))
}

/// The whole command fused into one token: "camelCaseLoadConfig" -> loadConfig.
fn fused(text: &str, word: &Word<'_>) -> Option<String> {
    let lower = word.text.to_lowercase();
    let (_, case) = CASES
        .iter()
        .find(|c| lower.starts_with(&format!("{}case", c.0)))?;
    let rest = word.text.get(lower.find("case")? + 4..)?;
    if !spoken(text, word) || !rest.starts_with(char::is_uppercase) {
        return None;
    }
    let mut parts = Vec::new();
    for c in rest.chars() {
        if c.is_uppercase() || parts.is_empty() {
            parts.push(String::new());
        }
        parts.last_mut()?.push(c);
    }
    let parts = span(parts.iter().map(String::as_str));
    Some(build(*case, parts.iter().map(String::as_str)))
}

fn build<'a>(case: Case, words: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for (k, word) in words.enumerate() {
        if k > 0 {
            out.push_str(match case {
                Case::Snake | Case::Constant => "_",
                Case::Kebab => "-",
                _ => "",
            });
        }
        // Whisper's own humps survive: "NavLink", but "URL" and "ID" are words.
        let humped =
            word.chars().skip(1).any(char::is_uppercase) && word.chars().any(char::is_lowercase);
        let lower = word.to_lowercase();
        let mut chars = word.chars();
        let first = chars.next().into_iter();
        let rest = if humped {
            chars.as_str().to_owned()
        } else {
            lower.chars().skip(1).collect()
        };
        if case == Case::Constant {
            out.push_str(&word.to_uppercase());
        } else if case == Case::Pascal || case == Case::Camel && k > 0 {
            out.extend(first.flat_map(char::to_uppercase));
            out.push_str(&rest);
        } else if humped && case == Case::Camel {
            out.extend(first.flat_map(char::to_lowercase));
            out.push_str(&rest);
        } else {
            out.push_str(&lower);
        }
    }
    out
}

/// Whether word `i` ends an identifier. A preposition ends it unless it is the
/// last word (signIn, createdAt): "rename camel case user id in next js".
/// Except after a weak cue, "by" and "of" continue: getUserById, numberOfItems.
fn stops(text: &str, w: &[Word<'_>], i: usize, by_of: bool) -> bool {
    if any(&w[i], STOP) {
        return true;
    }
    if !any(&w[i], PREPOSITIONS) {
        return false;
    }
    let rest = text[w[i].end..].trim_start_matches([' ', '\t']);
    let last = rest.is_empty()
        || rest.starts_with(|c: char| ",;:!?)]}\n\r".contains(c))
        || rest.starts_with('.') && !rest[1..].starts_with(char::is_alphanumeric);
    match w.get(i + 1).filter(|x| gap(&text[w[i].end..x.start])) {
        _ if last => false,
        Some(next) => {
            any(next, DETERMINERS) || any(next, STOP) || !by_of || !any(&w[i], &["by", "of"])
        }
        // Typed text follows: "max retries in .env".
        None => true,
    }
}

/// "open paren" / "close brace": (opens, pair index).
fn pair(text: &str, w: &[Word<'_>], i: usize) -> Option<(bool, usize)> {
    let next = w.get(i + 1)?;
    let open = is(&w[i], "open");
    if !open && !is(&w[i], "close")
        // "an open bracket", "to open parenthesis around the idea"
        || i > 0 && (any(&w[i - 1], DETERMINERS) || is(&w[i - 1], "to"))
        || !(spoken(text, &w[i]) && spoken(text, next) && joined(text, &w[i..i + 2]))
    {
        return None;
    }
    BRACKETS.iter().find(|b| is(next, b.0)).map(|b| (open, b.1))
}

/// What a command needs to its left. Plain prose ("the lesson about snake case
/// ended early") never starts one.
#[derive(Clone, Copy, PartialEq)]
enum Before {
    /// Utterance or sentence start.
    Start,
    /// A naming cue ("rename", "called", "set", inserted code): the identifier
    /// may end mid-sentence.
    Name,
    /// "should be", "using", "with": the identifier must end the clause.
    Weak,
}

fn before(text: &str, w: &[Word<'_>], i: usize, last: Option<usize>) -> Option<Before> {
    let head = text[..w[i].start].trim_end_matches([' ', '\t']);
    if head.is_empty() || head.ends_with(['.', '!', '?', ':', ';', '(', '\n', '\r']) {
        return Some(Before::Start);
    }
    // Right after inserted code ("userId user_id", "(", "userId, user_id").
    if last.is_some_and(|l| l >= head.len() || head.ends_with(',') && l == head.len() - 1) {
        return Some(Before::Name);
    }
    // "Okay, snake case retry count."
    if head.ends_with(',') {
        return Some(Before::Weak);
    }
    let p = i.checked_sub(1).filter(|&p| w[p].end == head.len())?;
    // "name it", "make them"
    let cue = |list: &[&str]| {
        any(&w[p], list) || any(&w[p], &["it", "them"]) && p > 0 && any(&w[p - 1], list)
    };
    if cue(NAME_CUES) {
        Some(Before::Name)
    } else if cue(WEAK_CUES) {
        Some(Before::Weak)
    } else {
        None
    }
}

/// The identifier for a casing command at `i`, if this is a command and not
/// someone talking about the convention ("use snake case for variables").
fn identifier(
    text: &str,
    w: &[Word<'_>],
    i: usize,
    last: Option<usize>,
) -> Option<(usize, String)> {
    let (case, len) = command(text, w, i)?;
    let context = before(text, w, i, last)?;
    let named = context == Before::Name;
    let start = i + len;
    // "camel cases open" is "camel case is open".
    let lead = is(&w[start - 1], "cases");
    // Inside a span Whisper may hyphenate: "user-eyed".
    let joins = |a: usize, b: usize| {
        let s = &text[w[a].end..w[b].start];
        gap(s)
            || b > start && s == "-"
            // "Camel K. Sappy base URL": Whisper's full stop after a heard "K".
            || b == start && is(&w[a], "k") && s.strip_prefix('.').is_some_and(gap)
    };
    let mut end = start;
    while end < w.len()
        && end < start + 5
        && joins(end - 1, end)
        && spoken(text, &w[end])
        && command(text, w, end).is_none()
        && pair(text, w, end).is_none()
        && !((end > start || lead) && stops(text, w, end, context != Before::Weak))
    {
        end += 1;
    }
    let n = end - start + usize::from(lead);
    // Clean: the identifier ends the clause, or the next command follows.
    let next_command = |j: usize| {
        command(text, w, j).is_some()
            || pair(text, w, j).is_some()
            || any(&w[j], &["and", "or", "then"])
                && w.get(j + 1).is_some()
                && gap(&text[w[j].end..w[j + 1].start])
                && command(text, w, j + 1).is_some()
    };
    let clean = end == w.len() || !joins(end - 1, end) || next_command(end);
    if end == start || (!clean && (n == 5 || !named)) {
        return None;
    }
    let (first, last_word) = (&w[start], &w[end - 1]);
    // "as" for "is" only where a name is expected: "use camel case as default" is prose.
    let is_like = lead || is(first, "is") || is(first, "as") && context != Before::Weak;
    let prefix = (is_like || any(first, PREFIXES))
        && (2..=3).contains(&n)
        && !plural(last_word)
        && (clean || named)
        // "the style should be snake case for Python"
        && (context != Before::Weak || is_like || is(first, "to"));
    let words = span(
        lead.then_some("is")
            .into_iter()
            .chain(w[start..end].iter().map(|x| x.text)),
    );
    // A lone camel/snake/kebab word is only lowercased: the speaker meant prose.
    let lone =
        words.len() == 1 && (!matches!(case, Case::Pascal | Case::Constant) || plural(last_word));
    if (!lead && (any(first, STOP) || any(first, PREPOSITIONS)) && !prefix)
        || (lead && !prefix)
        || w[start..end].iter().any(|x| any(x, PROSE))
        || lone
    {
        return None;
    }
    Some((end, build(case, words.iter().map(String::as_str))))
}

/// "index dot ts", "next dot config dot js", "dot env", "a dot md file".
fn filename(text: &str, w: &[Word<'_>], i: usize) -> Option<(usize, String)> {
    let all_spoken =
        |range: &[Word<'_>]| range.iter().all(|x| spoken(text, x)) && joined(text, range);
    for (phrase, name) in [
        (&["dot", "git", "ignore"][..], ".gitignore"),
        (&["dot", "env"][..], ".env"),
    ] {
        let end = i + phrase.len();
        if end <= w.len()
            && w[i..end]
                .iter()
                .zip(phrase)
                .all(|(x, p)| is(x, p) || ext(x) == Some(*p))
            && all_spoken(&w[i..end])
        {
            let (mut end, mut name) = (end, name.to_owned());
            // ".env.local", ".env.example"
            while name == ".env"
                && end + 1 < w.len()
                && is(&w[end], "dot")
                && any(&w[end + 1], ENV_SUFFIXES)
                && all_spoken(&w[end - 1..end + 2])
            {
                name = format!("{name}.{}", w[end + 1].text.to_lowercase());
                end += 2;
            }
            return Some((end, name));
        }
    }
    // Bare extension after a determiner or at a start: "a dot md file" -> "a .md file".
    if is(&w[i], "dot")
        && w.get(i + 1)
            .and_then(ext)
            .is_some_and(|x| !["go", "lock", "env"].contains(&x))
        && all_spoken(&w[i..i + 2])
        && (i == 0 || !gap(&text[w[i - 1].end..w[i].start]) || any(&w[i - 1], STOP))
    {
        return Some((i + 2, format!(".{}", ext(&w[i + 1])?)));
    }
    if is(&w[i], "dot") || any(&w[i], STOP) {
        return None;
    }
    let mut end = i + 1;
    let mut found = None;
    while end + 1 < w.len() && is(&w[end], "dot") && all_spoken(&w[end - 1..end + 2]) {
        end += 2;
        // "source dot env" means the dotfile, never "source.env".
        if ext(&w[end - 1]).is_some_and(|x| x != "env") {
            found = Some(end);
        }
    }
    let end = found?;
    let stem = w[i].text.to_lowercase();
    let ext = ext(&w[end - 1])?;
    let mut name = NAMES
        .iter()
        .find(|(s, e, _)| end == i + 3 && *s == stem && *e == ext)
        .map_or(stem, |n| n.2.to_owned());
    for word in w[i + 2..end - 1].iter().step_by(2) {
        name.push('.');
        name.push_str(&word.text.to_lowercase());
    }
    name.push('.');
    name.push_str(ext);
    Some((end, name))
}

/// A known extension, lowercased and repaired ("TOML", "jason", "envy").
fn ext(word: &Word<'_>) -> Option<&'static str> {
    let lower = word.text.to_lowercase();
    EXTENSIONS
        .iter()
        .copied()
        .find(|e| *e == lower)
        .or_else(|| repair(word.text, Position::Extension))
}

/// Whisper writes filenames itself, with its own casing: "Cargo.TOML",
/// "ReadMe.md", "Main.rs" opening a sentence, ".envy". Canonicalize the
/// extension and known names; lowercase a stem capitalized only by the
/// sentence start unless it is a dictionary term. Anything else stays typed.
fn typed(text: &str, config: &Config) -> String {
    let text = text.replace(".git ignore", ".gitignore");
    let mut out = String::with_capacity(text.len());
    let mut sentence = true;
    for chunk in text.split_inclusive(char::is_whitespace) {
        let token = chunk.trim_end();
        let core = token.trim_end_matches(['.', ',', '!', '?', ':', ';']);
        let core = if core.is_empty() { token } else { core };
        let fixed = core.rsplit_once('.').and_then(|(stem, extension)| {
            let extension = ext(&Word { start: 0, end: 0, text: extension })?;
            let valid = |c: char| c.is_alphanumeric() || "._-".contains(c);
            if !stem.chars().all(valid) || stem.ends_with('.') || core.contains("..") {
                return None;
            }
            let lower = stem.to_lowercase();
            let stem = if let Some(n) = NAMES.iter().find(|n| n.0 == lower && n.1 == extension) {
                n.2.to_owned()
            } else if sentence
                // "Node.js" and "Next.js" are names, not files.
                && extension != "js"
                && stem.chars().next().is_some_and(char::is_uppercase)
                && stem.chars().skip(1).all(|c| !c.is_uppercase())
                && !config.dictionary.words.iter().chain(&config.stt.vocabulary).any(|t| t == stem || *t == core)
            {
                lower
            } else {
                stem.to_owned()
            };
            Some(format!("{stem}.{extension}"))
        });
        match fixed {
            Some(name) if name != core => {
                out.push_str(&name);
                out.push_str(&chunk[core.len()..]);
            }
            _ => out.push_str(chunk),
        }
        if !token.is_empty() {
            sentence = token.ends_with(['.', '!', '?']);
        }
    }
    out
}

/// "open paren" -> "(" glued to the next word, "close paren" glued to the
/// last. Only balanced pairs, or the phrase alone: "I said open paren" is prose.
fn bracket(
    text: &str,
    w: &[Word<'_>],
    i: usize,
    depth: &mut [usize; 3],
) -> Option<(usize, char, bool)> {
    let (open, kind) = pair(text, w, i)?;
    let ok = if w.len() == 2 {
        true
    } else if open {
        (i + 2..w.len()).any(|j| pair(text, w, j) == Some((false, kind)))
    } else {
        depth[kind] > 0
    };
    if !ok {
        return None;
    }
    if open {
        depth[kind] += 1;
    } else {
        depth[kind] = depth[kind].saturating_sub(1);
    }
    Some((
        i + 2,
        if open { PAIRS[kind].0 } else { PAIRS[kind].1 },
        open,
    ))
}

/// Structured list items retain the spelling produced by the code pass.
pub(crate) fn literal(word: &str) -> bool {
    let word = word.trim_end_matches([',', '.', ':', ';', '!', '?']);
    word.contains(['_', '-'])
        || word == ".gitignore"
        || word
            .rsplit_once('.')
            .is_some_and(|(_, ext)| EXTENSIONS.contains(&ext))
}

/// One scan; inserted code is never rescanned. Text between insertions goes
/// through `prose` (the snap pass), so a dictionary term never re-cases an
/// identifier the speaker spelled out. Output never gains a newline, so this
/// is as safe in terminals as anywhere.
pub fn apply(text: &str, config: &Config, mut prose: impl FnMut(&str) -> String) -> String {
    if !config.formatting.spoken_code {
        return prose(text);
    }
    let typed = typed(text, config);
    let text = typed.as_str();
    let w = words(text);
    let mut out = String::with_capacity(text.len());
    let (mut cursor, mut i, mut last, mut depth) = (0, 0, None, [0; 3]);
    while i < w.len() {
        let (end, code, glue_before, glue_after) = if let Some((end, id)) =
            identifier(text, &w, i, last)
                .or_else(|| fused(text, &w[i]).map(|id| (i + 1, id)))
                .or_else(|| filename(text, &w, i))
        {
            (end, id, false, false)
        } else if let Some((end, c, open)) = bracket(text, &w, i, &mut depth) {
            (end, c.to_string(), !open, open)
        } else {
            i += 1;
            continue;
        };
        out.push_str(&prose(&text[cursor..w[i].start]));
        if glue_before {
            out.truncate(out.trim_end_matches([' ', '\t']).len());
        }
        out.push_str(&code);
        cursor = w[end - 1].end;
        if glue_after {
            cursor += text[cursor..].len() - text[cursor..].trim_start_matches([' ', '\t']).len();
        }
        last = Some(cursor);
        i = end;
    }
    out.push_str(&prose(&text[cursor..]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Replacement, text::before_cleanup};

    fn code(text: &str, config: &Config) -> String {
        apply(text, config, str::to_owned)
    }

    #[test]
    fn commands_filenames_brackets_and_boundaries() {
        let config = Config::default();
        for (input, expected) in [
            ("camel case user id", "userId"),
            ("snake case user id", "user_id"),
            ("kebab case user id", "user-id"),
            ("pascal case user id", "UserId"),
            ("constant case user id", "USER_ID"),
            ("snake case max retries", "max_retries"),
            ("package dot json", "package.json"),
            ("main dot rs", "main.rs"),
            ("index dot ts", "index.ts"),
            ("dot env", ".env"),
            ("open paren", "("),
            ("camelCase user ID.", "userId."),
            ("Snakecase max retries", "max_retries"),
            (
                "rename camelCaseUserId to PascalCaseAccountId",
                "rename userId to AccountId",
            ),
            ("camelcase", "camelcase"),
            ("camelcase useride", "userId"),
            ("rename it to CamelCaseAccountide", "rename it to accountId"),
            (
                "camel case user-eyed snake case user-eyed",
                "userId user_id",
            ),
            ("change camel cases open to false", "change isOpen to false"),
            ("SnakeCase as admin", "is_admin"),
            ("use camel case as default", "use camel case as default"),
            ("Okay, snake case retry count.", "Okay, retry_count."),
            (
                "Set constant case max retries in .env",
                "Set MAX_RETRIES in .env",
            ),
            (
                "The component is Pascal Case NavLink.",
                "The component is NavLink.",
            ),
            ("check .git ignore", "check .gitignore"),
            ("camel case override", "camel case override"),
            ("INDEX DOT TS!", "index.ts!"),
            ("CAMEL CASE USER ID.", "userId."),
            ("snake case café numéro", "café_numéro"),
            ("pascal case über straße", "ÜberStraße"),
            ("camel case user id; index dot ts", "userId; index.ts"),
            (
                "rename camel case user id to camel case account id",
                "rename userId to accountId",
            ),
            (
                "add a column called snake case last seen at.",
                "add a column called last_seen_at.",
            ),
            (
                "set camel case user id in the struct",
                "set userId in the struct",
            ),
            ("camel case sign in", "signIn"),
            (
                "rename it to camel case get user by id",
                "rename it to getUserById",
            ),
            (
                "set camel case user id in main dot rs",
                "set userId in main.rs",
            ),
            ("source dot env dot local", "source .env.local"),
            ("print open paren name close paren", "print (name)"),
            ("open brace snake case user id close brace", "{user_id}"),
            // Not commands: no identifier, broken by punctuation or a line, too long, typed text.
            ("camel case", "camel case"),
            ("camel case, user id", "camel case, user id"),
            ("camel case\nuser id", "camel case\nuser id"),
            ("index dot\nts", "index dot\nts"),
            ("main dot exe", "main dot exe"),
            (
                "camel case one two three four five six",
                "camel case one two three four five six",
            ),
            (
                "slash backtick dash dash flag",
                "slash backtick dash dash flag",
            ),
            ("I said open paren twice", "I said open paren twice"),
            (
                "to open paren a and close paren",
                "to open paren a and close paren",
            ),
            (
                "Pascal case is like camel case",
                "Pascal case is like camel case",
            ),
            ("`camel case user id`", "`camel case user id`"),
            ("https://camel case user id", "https://camel case user id"),
            ("/index dot ts", "/index dot ts"),
            ("user_id dot ts", "user_id dot ts"),
            ("x.camel case user id", "x.camel case user id"),
            (
                "discuss camel case user id tomorrow",
                "discuss camel case user id tomorrow",
            ),
        ] {
            assert_eq!(code(input, &config), expected, "{input}");
        }
        for ext in EXTENSIONS.iter().filter(|e| **e != "env") {
            assert_eq!(
                code(&format!("main dot {ext}"), &config),
                format!("main.{ext}")
            );
        }
    }

    fn rows() -> impl Iterator<Item = (&'static str, &'static str)> {
        include_str!("../../../../../benchmarks/devterms/spoken-code.tsv")
            .lines()
            .filter(|r| !r.starts_with('#') && !r.is_empty())
            .map(|r| r.split_once('\t').unwrap())
    }

    #[test]
    fn corpus_exact_match_and_idempotence() {
        let config = Config::default();
        for (spoken, expected) in rows() {
            let formatted = code(spoken, &config);
            assert_eq!(formatted, expected, "{spoken}");
            assert_eq!(code(&formatted, &config), expected, "second pass: {spoken}");
        }
        assert_eq!(rows().count(), 240);
    }

    #[test]
    fn prose_control_is_unchanged() {
        let config = Config::default();
        let (mut sentences, mut total) = (0, 0);
        for text in include_str!("../../../../../benchmarks/devterms/spoken-code-prose.txt").lines()
        {
            assert_eq!(code(text, &config), text, "{text}");
            assert_eq!(
                before_cleanup(text, &config, false),
                text,
                "pipeline: {text}"
            );
            sentences += 1;
            total += text.split_whitespace().count();
        }
        eprintln!("spoken-code prose: 0/{total} changed words across {sentences} sentences");
        assert_eq!(sentences, 148);
    }

    #[test]
    fn whisper_written_code_is_normalized_and_keeps_its_spaces() {
        let mut config = Config::default();
        for (input, expected) in [
            // Whisper's own filenames: the space before a dotfile survives tidy.
            (
                "Put the key in .env, not in the code.",
                "Put the key in .env, not in the code.",
            ),
            ("Add it to .envy", "Add it to .env"),
            (
                "Add node modules to .gitignore",
                "Add node modules to .gitignore",
            ),
            ("Write a .md file", "Write a .md file"),
            ("Convert the .yaml config", "Convert the .yaml config"),
            ("Check the .json output", "Check the .json output"),
            (
                "Open Cargo.TOML and bump the version.",
                "Open Cargo.toml and bump the version.",
            ),
            ("Update ReadMe.md", "Update README.md"),
            (
                "Main.rs is too long. Install.sh too.",
                "main.rs is too long. install.sh too.",
            ),
            ("Export it from Button.tsx", "Export it from Button.tsx"),
            ("Node.js handles it.", "Node.js handles it."),
            ("Camel K. Appy base URL", "apiBaseUrl"),
            ("Camel-case user i'd", "userId"),
            ("snake case user eye", "user_id"),
            ("package dot jason", "package.json"),
        ] {
            assert_eq!(before_cleanup(input, &config, false), expected, "{input}");
        }
        config.dictionary.words = vec!["Main.rs".into()];
        assert_eq!(
            before_cleanup("Main.rs is long.", &config, false),
            "Main.rs is long."
        );
    }

    #[test]
    fn pipeline_order_terminal_and_kill_switch() {
        let mut config = Config::default();
        config.dictionary.words = vec!["Next.js".into(), "UserId".into()];
        // Spelled-out identifiers skip snap: "UserId" in the dictionary must not
        // re-case userId or user-id. Prose around them still snaps.
        for terminal in [false, true] {
            assert_eq!(
                before_cleanup(
                    "rename camel case user id in next js and kebab case user id",
                    &config,
                    terminal
                ),
                "rename userId in Next.js and user-id"
            );
        }
        assert_eq!(before_cleanup("user id", &config, false), "UserId");
        // Literal replacements still follow.
        config.dictionary.replacements = vec![Replacement {
            from: "userId".into(),
            to: "accountId".into(),
        }];
        assert_eq!(
            before_cleanup("camel case user id", &config, false),
            "accountId"
        );
        config.dictionary.replacements.clear();
        // Lists keep the generated spelling; terminals get no list newlines.
        let input = "one camel case user id, two next js, three main dot rs, four dot env";
        assert_eq!(
            before_cleanup(input, &config, false),
            "1. userId\n2. Next.js\n3. main.rs\n4. .env"
        );
        assert_eq!(
            before_cleanup(input, &config, true),
            "one userId, two Next.js, three main.rs, four .env"
        );
        config.formatting.spoken_code = false;
        assert_eq!(
            before_cleanup("camel case user id; dot env", &config, true),
            "camel case UserId; dot env"
        );
        let decoded: Config = toml::from_str("[formatting]\nspoken_code = false").unwrap();
        assert!(!decoded.formatting.spoken_code);
        assert!(toml::from_str::<Config>("").unwrap().formatting.spoken_code);
    }

    #[test]
    fn latency_on_two_thousand_characters() {
        let config = Config::default();
        let text =
            "rename camel case user id to snake case max retries in index dot ts. ".repeat(30);
        assert!(text.len() >= 2_000);
        let mut samples = Vec::new();
        for _ in 0..200 {
            let start = std::time::Instant::now();
            std::hint::black_box(code(std::hint::black_box(&text), &config));
            samples.push(start.elapsed());
        }
        samples.sort_unstable();
        eprintln!(
            "spoken-code {} bytes: p50 {:?}, p95 {:?}",
            text.len(),
            samples[100],
            samples[190]
        );
        // The whole pre-cleanup pipeline with the pass off (before) and on (after).
        let mut off = config.clone();
        off.formatting.spoken_code = false;
        for (label, config) in [("off", &off), ("on", &config)] {
            let mut pipeline: Vec<_> = (0..200)
                .map(|_| {
                    let start = std::time::Instant::now();
                    std::hint::black_box(before_cleanup(
                        std::hint::black_box(&text),
                        config,
                        false,
                    ));
                    start.elapsed()
                })
                .collect();
            pipeline.sort_unstable();
            eprintln!(
                "before_cleanup spoken_code={label}: p50 {:?}, p95 {:?}",
                pipeline[100], pipeline[190]
            );
        }
        assert!(
            samples[100].as_micros()
                < if cfg!(debug_assertions) {
                    10_000
                } else {
                    1_000
                }
        );
    }
}
