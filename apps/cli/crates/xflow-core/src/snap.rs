//! Deterministic, nonrecursive restoration of the user's own vocabulary.
use crate::{
    config::Config,
    learn::{distance, sounds},
    words::{common, name},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

/// Closed-class words never end a joined term ("just in" is not Justin), and
/// sound-alikes never span one ("so late" is not SQLite).
const FUNCTION_WORDS: [&str; 46] = [
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "get", "got", "had", "has",
    "have", "he", "her", "his", "i", "if", "in", "into", "is", "it", "its", "no", "not", "of",
    "on", "or", "our", "she", "so", "such", "than", "that", "the", "they", "this", "to", "up",
    "us", "was", "we", "were", "with",
];
/// Exact joins may still start on these: "i os", "no sql", "on prem", "up stash".
const FOLD_PREFIXES: [&str; 4] = ["i", "no", "on", "up"];
/// Sound-alikes only target terms this long, within ⌊letters / 5⌋ edits (at
/// least one): two edits on a 6–9 letter term turned "dogger" into Docker.
const MIN_SOUNDALIKE_LETTERS: usize = 6;
const EDITS_PER_LETTER: usize = 5;

#[derive(Debug)]
struct Term {
    text: String,
    letters: Vec<char>,
    sound: Vec<u8>,
    common: bool,
}

#[derive(Debug)]
struct Index {
    dictionary: Vec<String>,
    vocabulary: Vec<String>,
    terms: BTreeMap<(char, usize), Vec<Term>>,
}

/// Config-owned derived state. Clones share it; changed terms rebuild it once.
#[derive(Clone, Debug, Default)]
pub struct Cache(Arc<Mutex<Option<Index>>>);
impl PartialEq for Cache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

fn letters(text: &str) -> Option<Vec<char>> {
    let mut result = Vec::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            result.extend(c.to_lowercase());
        } else if !c.is_whitespace() && !matches!(c, '.' | '-' | '_') {
            return None;
        }
    }
    (!result.is_empty()).then_some(result)
}

fn index(config: &Config) -> MutexGuard<'_, Option<Index>> {
    let mut cache = config.snap.0.lock().unwrap_or_else(|e| e.into_inner());
    if cache.as_ref().is_none_or(|i| {
        i.dictionary != config.dictionary.words || i.vocabulary != config.stt.vocabulary
    }) {
        let mut terms: BTreeMap<_, Vec<Term>> = BTreeMap::new();
        for text in config.dictionary.words.iter().chain(&config.stt.vocabulary) {
            let Some(letters) = letters(text) else {
                continue;
            };
            let bucket = terms.entry((letters[0], letters.len())).or_default();
            if bucket.iter().any(|term| term.text == *text) {
                continue;
            }
            bucket.push(Term {
                text: text.clone(),
                sound: sounds(&letters),
                // ".NET" and "Log-In" are still the everyday "net" and "login".
                common: common(&text.to_lowercase()) || common(&letters.iter().collect::<String>()),
                letters,
            });
        }
        *cache = Some(Index {
            dictionary: config.dictionary.words.clone(),
            vocabulary: config.stt.vocabulary.clone(),
            terms,
        });
    }
    cache
}

/// Eagerly build the index when a config is loaded, outside the dictation path.
pub fn prepare(config: &Config) {
    if config.formatting.snap_to_dictionary {
        drop(index(config));
    }
}

impl Index {
    fn find(&self, input: &str, count: usize, folded_only: bool) -> Option<&str> {
        let lower = input.to_lowercase();
        let words: Vec<_> = lower.split_whitespace().collect();
        let function = |w: &&str| FUNCTION_WORDS.contains(w);
        if count > 1
            && (function(&words[count - 1])
                || (function(&words[0]) && !FOLD_PREFIXES.contains(&words[0])))
        {
            return None;
        }
        let letters = letters(input)?;
        let n = letters.len();
        if let Some(terms) = self.terms.get(&(letters[0], n)) {
            if terms.iter().any(|t| t.letters == letters) {
                return terms
                    .iter()
                    .find(|t| t.letters == letters && !t.common)
                    .map(|t| t.text.as_str());
            }
        }
        // Everyday words and real names (Austen is not Austin) are not misspellings.
        if folded_only
            || words.iter().any(function)
            || (count == 1 && (common(&lower) || name(&lower)))
        {
            return None;
        }
        let sound = sounds(&letters);
        // Non-rhotic "super base" has a silent r before the word boundary.
        let non_rhotic = words[..count - 1]
            .iter()
            .any(|w| w.ends_with("er"))
            .then(|| {
                let letters: Vec<_> = words
                    .iter()
                    .enumerate()
                    .flat_map(|(i, w)| {
                        let w = match w.strip_suffix('r') {
                            Some(w) if i + 1 < count && w.ends_with('e') => w,
                            _ => w,
                        };
                        w.chars()
                    })
                    .collect();
                (sounds(&letters), letters)
            });
        let digits = |l: &[char]| {
            l.iter()
                .filter(|c| c.is_numeric())
                .copied()
                .collect::<Vec<_>>()
        };
        // Bucket bound only: a term of length t is reachable when |n - t| <= t / 5.
        let radius = n.div_ceil(4) + 1;
        let mut found: Option<&Term> = None;
        for (_, terms) in self
            .terms
            .range((letters[0], n.saturating_sub(radius))..=(letters[0], n + radius))
        {
            for term in terms {
                let limit = (term.letters.len() / EDITS_PER_LETTER).max(1);
                if term.letters.len() < MIN_SOUNDALIKE_LETTERS
                    || term.common
                    || n.abs_diff(term.letters.len()) > limit
                    || (term.sound != sound && non_rhotic.as_ref().map(|r| &r.0) != Some(&term.sound))
                    // python2 is never Python3.
                    || digits(&term.letters) != digits(&letters)
                {
                    continue;
                }
                // Every word must be needed: "post gres is" or "so supabase"
                // must not swallow a neighbour that the term doesn't need.
                let slack = |part: &[&str]| {
                    self::letters(&part.join(" "))
                        .is_some_and(|l| distance(&l, &term.letters) <= limit)
                };
                let edits = non_rhotic
                    .as_ref()
                    .map_or(usize::MAX, |r| distance(&r.1, &term.letters));
                if distance(&letters, &term.letters).min(edits) <= limit
                    && (count == 1 || !(slack(&words[1..]) || slack(&words[..count - 1])))
                {
                    // Ambiguous sound-alikes are left alone rather than guessing;
                    // the same spelling saved twice is not ambiguous.
                    match found {
                        Some(other) if other.letters != term.letters => return None,
                        Some(_) => {}
                        None => found = Some(term),
                    }
                }
            }
        }
        found.map(|t| t.text.as_str())
    }
}

/// Apply fold-equal windows before sound-alikes, longest first within each.
/// Never rescan inserted text.
pub fn apply(text: &str, config: &Config) -> String {
    if !config.formatting.snap_to_dictionary
        || (config.dictionary.words.is_empty() && config.stt.vocabulary.is_empty())
    {
        return text.to_owned();
    }
    let cache = index(config);
    let index = cache.as_ref().expect("prepared index");
    let tokens: Vec<_> = text
        .split_whitespace()
        .map(|chunk| {
            let word = chunk.trim_matches(|c: char| !c.is_alphanumeric());
            let start = word.as_ptr() as usize - text.as_ptr() as usize;
            // Identifiers, paths, URLs and emails are typed text, not speech.
            let protected = chunk.contains(['@', '/', '\\', '_']) || chunk.starts_with("www.");
            (start, start + word.len(), !protected && !word.is_empty())
        })
        .collect();
    let mut out = String::with_capacity(text.len());
    let (mut cursor, mut i) = (0, 0);
    while i < tokens.len() {
        let mut matched = None;
        // Exact folds beat sound-alikes starting at the same word.
        'search: for folded_only in [true, false] {
            for count in (1..=3.min(tokens.len() - i)).rev() {
                let window = &tokens[i..i + count];
                if !window.iter().all(|t| t.2)
                    || !window.windows(2).all(|w| {
                        let gap = &text[w[0].1..w[1].0];
                        !gap.is_empty()
                            && gap
                                .chars()
                                .all(|c| c.is_whitespace() && c != '\n' && c != '\r')
                    })
                {
                    continue;
                }
                let input = &text[window[0].0..window[count - 1].1];
                if let Some(target) = index.find(input, count, folded_only) {
                    matched = Some((count, target));
                    break 'search;
                }
            }
        }
        if let Some((count, target)) = matched {
            out.push_str(&text[cursor..tokens[i].0]);
            out.push_str(target);
            cursor = tokens[i + count - 1].1;
            i += count;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::before_cleanup;

    const DEV_TERMS: [&str; 50] = [
        "DevTerms",
        "Next.js",
        "useEffect",
        "kubectl",
        "Postgres",
        "Supabase",
        "xflow",
        "React",
        "Redux",
        "TypeScript",
        "JavaScript",
        "Node.js",
        "GitHub",
        "GitLab",
        "Docker",
        "Kubernetes",
        "Terraform",
        "SQLite",
        "GraphQL",
        "OpenAI",
        "ChatGPT",
        "FastAPI",
        "Django",
        "Flask",
        "PyTorch",
        "NumPy",
        "pandas",
        "Vercel",
        "Netlify",
        "Cloudflare",
        "Tailwind",
        "Svelte",
        "Vue",
        "Nuxt",
        "Prisma",
        "Drizzle",
        "Redis",
        "MongoDB",
        "DynamoDB",
        "WebSocket",
        "OAuth",
        "Homebrew",
        "Neovim",
        "VSCode",
        "pnpm",
        "npm",
        "Bun",
        "Deno",
        "Vitest",
        "Playwright",
    ];

    /// Every false snap found by the PR #82 review, with the dictionaries that
    /// produced it: DEV_TERMS, saved names, and dotted/hyphenated/versioned terms.
    const REVIEW_TERMS: [&str; 20] = [
        "Justin",
        "Dustin",
        "Austin",
        "Martin",
        "Devin",
        "Weston",
        "Hudson",
        "Newton",
        "Sherwin",
        "Claude",
        "Daniel",
        ".NET",
        "Log-In",
        "Co-op",
        "Python3",
        "ES2022",
        "Llama3",
        "Windows11",
        "Base32",
        "OAuth2",
    ];
    const REVIEW_FALSE_SNAPS: [&str; 45] = [
        "I am sorry for my calling so late, she said.",
        "Those words sometimes convey no ideas at all.",
        "He spent the morning obtaining a note as the bank asked.",
        "It was a damp, drizzly November in my soul.",
        "He was standing upon such a base as that.",
        "They longed for that of terra firma, wild and green.",
        "You unmannerly Dutch dogger!",
        "The book was published so late as that.",
        "I made it just in time.",
        "There was no jest in his voice.",
        "There was dust in the attic.",
        "She ran out of the room.",
        "I have my share in the profits.",
        "We are not on friendly terms.",
        "We sat on the bench in silence.",
        "She resolved to act in that spirit.",
        "Cut the work into as thin slices as you can.",
        "He had soon forgotten it.",
        "They were putting their heads on the table.",
        "Miss Austen always liked the country.",
        "Eckhaust and Clyde Cohen arrived.",
        "We visited the Merton College Library.",
        "Sibbald, Brisson, Marten and Bonneterre wrote about whales.",
        "Orderi di Danilo, ran the headline.",
        "Can you type a script for me?",
        "What type of script do you need?",
        "We need to get lab results back.",
        "Can you get hub access?",
        "Let's dev in a branch.",
        "Bring an umbrella just in case.",
        "Head west on the highway.",
        "The post grows each day.",
        "The net result is good.",
        "Cast a net.",
        "Please login.",
        "The coop is full.",
        "Use python2 here.",
        "Target es2021 now.",
        "Run llama2 locally.",
        "Install windows10 today.",
        "Encode base64 now.",
        "Use oauth1 here.",
        "Ask _Sophy about it.",
        "Rename use_effect in the hook.",
        "Red is my favourite colour.",
    ];

    #[test]
    fn review_false_snaps_are_left_alone() {
        let c = config(&[&DEV_TERMS[..], &REVIEW_TERMS[..]].concat());
        let prose = include_str!("snap-prose.txt");
        let snapped: Vec<_> = REVIEW_FALSE_SNAPS
            .iter()
            .map(|input| apply(input, &c))
            .filter(|output| !REVIEW_FALSE_SNAPS.contains(&output.as_str()))
            .collect();
        assert!(snapped.is_empty(), "{snapped:#?}");
        for input in REVIEW_FALSE_SNAPS {
            assert!(
                prose.lines().any(|l| l == input),
                "add to snap-prose.txt: {input}"
            );
        }
    }

    #[test]
    fn review_guards_keep_real_snaps() {
        let mut c = config(&[
            "NoSQL", "iOS", "MySQL", "Upstash", "Python3", "Supabase", "Redis",
        ]);
        c.stt.vocabulary = vec!["supabase".into()];
        for (input, expected) in [
            ("no sql", "NoSQL"),
            ("i os", "iOS"),
            ("my sql", "MySQL"),
            ("up stash", "Upstash"),
            ("python 3", "Python3"),
            ("super base is up", "Supabase is up"),
            ("redis is down", "Redis is down"),
        ] {
            assert_eq!(apply(input, &c), expected, "{input}");
        }
    }

    fn config(terms: &[&str]) -> Config {
        let mut config = Config::default();
        config.dictionary.words = terms.iter().map(|s| (*s).into()).collect();
        config
    }

    #[test]
    fn positive_dictionary_and_vocabulary_table() {
        for vocabulary in [false, true] {
            let mut c = config(&DEV_TERMS);
            c.dictionary.learned = c.dictionary.words.clone();
            if vocabulary {
                c.stt.vocabulary = std::mem::take(&mut c.dictionary.words);
            }
            for (input, expected) in [
                ("dev terms", "DevTerms"),
                ("next js", "Next.js"),
                ("Next JS", "Next.js"),
                ("use effect", "useEffect"),
                ("kube ctl", "kubectl"),
                ("post gress", "Postgres"),
                ("super base", "Supabase"),
                ("x flow", "xflow"),
                ("…use effect.", "…useEffect."),
                ("Super Base", "Supabase"),
                ("(dev terms), next js!", "(DevTerms), Next.js!"),
                ("postgress", "Postgres"),
                ("XFLOW", "xflow"),
                ("Next.js and useEffect", "Next.js and useEffect"),
            ] {
                assert_eq!(
                    apply(input, &c),
                    expected,
                    "{input}, vocabulary={vocabulary}"
                );
            }
        }
    }

    #[test]
    fn negative_table_including_pr47() {
        let mut terms = DEV_TERMS.to_vec();
        terms.extend([
            "everyday",
            "anyway",
            "login",
            "deployed",
            "forms",
            "effect",
            "lose",
            "except",
            "principle",
            "dploy",
        ]);
        let c = config(&terms);
        let inputs = [
            "the next step",
            "use the effect",
            "post it",
            "super",
            "tail",
            "every day",
            "any way",
            "react quickly",
            "we deploy",
            "submit the form",
            "will affect",
            "don't loose",
            "we accept",
            "the principal reason",
            "log in",
            "deploy now",
            "https://example.com/use-effect",
            "https://devterms.example/next.js",
            "devterms@example.com",
            "hello@xflow.dev",
            "www.devterms.com",
            "next.js@example.com",
            "dev. terms",
            "dev? terms",
            "dev! terms",
            "next. js",
            "next? js",
            "next! js",
            "use. effect",
            "use? effect",
            "use! effect",
            "post. gress",
            "post? gress",
            "post! gress",
            "super. base",
            "super? base",
            "super! base",
            "use, effect",
            "next; js",
            "dev: terms",
            "dev\nterms",
            "use\r\neffect",
            "nextness js",
            "devtermssuffix",
            "prédev terms",
            "next/js",
            "/devterms",
            "use-effect@example.org",
            "React quickly",
            "Every day is different.",
            "We should log in every day and react quickly.",
            "dash dash dry run",
            "Red is my favourite colour.",
            "the bread is red and the sky is blue",
        ];
        assert!(inputs.len() >= 40);
        for input in inputs {
            assert_eq!(apply(input, &c), input, "{input}");
        }
        let c = config(&["dploy"]);
        assert_eq!(apply("deploy kubernetis now", &c), "deploy kubernetis now");
    }

    #[test]
    fn longest_match_nonrecursive_and_cache_invalidation() {
        let mut c = config(&["Dev", "DevTerms", "DevTermsGuide", "Visual Studio Code"]);
        assert_eq!(
            apply("dev terms guide, visual studio code.", &c),
            "DevTermsGuide, Visual Studio Code."
        );
        c.dictionary.words = vec!["Next.js".into()];
        assert_eq!(apply("dev terms next js", &c), "dev terms Next.js");
        c.stt.vocabulary.push("useEffect".into());
        assert_eq!(apply("use effect", &c), "useEffect");
        let other = c.clone();
        c.dictionary.words.clear();
        assert_eq!(apply("next js", &c), "next js");
        assert_eq!(apply("next js", &other), "Next.js");
    }

    #[test]
    fn exact_windows_prevent_fuzzy_matches_from_consuming_following_words() {
        let c = config(&["Postgres", "Supabase"]);
        for (input, expected) in [
            ("post gres is ready.", "Postgres is ready."),
            ("post gress is ready.", "Postgres is ready."),
            ("post gres a day", "Postgres a day"),
            ("postgre is ready.", "postgre is ready."),
            ("so supabase is up", "so Supabase is up"),
            ("so supa base is up", "so Supabase is up"),
            ("Postgres is ready.", "Postgres is ready."),
        ] {
            assert_eq!(before_cleanup(input, &c, false), expected);
        }
    }

    #[test]
    fn ambiguity_is_safe_and_explicit_replacements_follow_snapping() {
        let c = config(&["Postgres", "Postgrus"]);
        assert_eq!(apply("postgras", &c), "postgras");
        let mut c = config(&["DevTerms"]);
        c.dictionary.replacements.push(crate::config::Replacement {
            from: "DevTerms".into(),
            to: "literal  replacement".into(),
        });
        assert_eq!(before_cleanup("dev terms", &c, false), "literal  replacement");
    }

    #[test]
    fn disabled_matches_existing_pipeline_byte_for_byte() {
        let mut c = config(&DEV_TERMS);
        c.formatting.snap_to_dictionary = false;
        let mut baseline = c.clone();
        baseline.dictionary.words.clear();
        for input in include_str!("snap-prose.txt").lines().chain([
            "um, dev terms use effect Next JS.",
            "  next js\n x flow!  ",
            "https://example.com/next.js",
        ]) {
            assert_eq!(before_cleanup(input, &c, false), before_cleanup(input, &baseline, false));
        }
        assert_eq!(apply("  next js\n x flow!  ", &c), "  next js\n x flow!  ");
    }

    #[test]
    fn prose_control_changes_at_most_one_percent_of_words() {
        let c = config(&[&DEV_TERMS[..], &REVIEW_TERMS[..]].concat());
        let (mut changed, mut total, mut sentences) = (0, 0, 0);
        for input in include_str!("snap-prose.txt")
            .lines()
            .filter(|s| !s.is_empty())
        {
            let output = apply(input, &c);
            let a: Vec<_> = input.split_whitespace().collect();
            let b: Vec<_> = output.split_whitespace().collect();
            // Word-level Levenshtein counts substitutions, joins and deletions.
            let mut row: Vec<_> = (0..=b.len()).collect();
            for (i, word) in a.iter().enumerate() {
                let mut diagonal = row[0];
                row[0] = i + 1;
                for (j, other) in b.iter().enumerate() {
                    let value = (diagonal + usize::from(word != other))
                        .min(row[j] + 1)
                        .min(row[j + 1] + 1);
                    diagonal = row[j + 1];
                    row[j + 1] = value;
                }
            }
            changed += row[b.len()];
            total += a.len();
            sentences += 1;
        }
        eprintln!("prose control: {changed}/{total} words changed across {sentences} sentences");
        assert!(sentences >= 100 && changed * 100 <= total);
    }

    /// Run against WP3's ignored results without changing its owned benchmark files.
    #[test]
    #[ignore = "requires WP3 corpus results; set XFLOW_DEVTERMS_DIR"]
    fn wp3_prompt_only_vs_prompt_and_snap() {
        let root = std::path::PathBuf::from(std::env::var("XFLOW_DEVTERMS_DIR").unwrap());
        let phrases = std::fs::read_to_string(root.join("phrases.tsv")).unwrap();
        let cases: Vec<_> = phrases
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.split_once('\t').unwrap().1.split('|').collect::<Vec<_>>())
            .collect();
        let records = std::fs::read_to_string(root.join("results/transcripts.jsonl")).unwrap();
        let mut scores = BTreeMap::new();
        let hit = |text: &str, term: &str| {
            text.match_indices(term).any(|(i, _)| {
                let boundary = |c: char| !c.is_alphanumeric() && c != '_';
                text[..i].chars().next_back().is_none_or(boundary)
                    && text[i + term.len()..].chars().next().is_none_or(boundary)
            })
        };
        for line in records.lines() {
            let record: serde_json::Value = serde_json::from_str(line).unwrap();
            if record["kind"] != "speech" {
                continue;
            }
            let i = record["index"].as_u64().unwrap() as usize;
            let terms: Vec<_> = cases[i / 15 * 15..((i / 15 + 1) * 15).min(cases.len())]
                .iter()
                .flatten()
                .copied()
                .collect();
            let c = config(&terms);
            let text = record["text"].as_str().unwrap();
            let output = apply(text, &c);
            let before = cases[i].iter().filter(|t| hit(text, t)).count();
            let after = cases[i].iter().filter(|t| hit(&output, t)).count();
            let key = (
                record["model"].as_str().unwrap().to_owned(),
                record["variant"].as_str().unwrap().to_owned(),
                record["voice"].as_str().unwrap().to_owned(),
            );
            let score = scores
                .entry(key)
                .or_insert((0usize, 0usize, 0usize, 0usize));
            score.0 += before;
            score.1 += after;
            score.2 += cases[i].len();
            score.3 += 1;
        }
        assert!(!scores.is_empty());
        let mut improved = false;
        for ((model, variant, voice), (before, after, total, clips)) in scores {
            eprintln!(
                "{model} {variant} {voice}: {before}/{total} -> {after}/{total}, {clips} clips"
            );
            assert!(
                after >= before,
                "recall decreased for {model}/{variant}/{voice}"
            );
            improved |= after > before;
        }
        assert!(improved, "snap must improve corpus recall");
    }

    #[test]
    fn benchmark_two_thousand_chars_and_terms() {
        use std::{
            hint::black_box,
            time::{Duration, Instant},
        };
        let mut c = config(&DEV_TERMS);
        for n in 0..1950 {
            c.dictionary.words.push(format!("DeveloperLibrary{n}"));
        }
        let text = "Please use next js with post gress and dev terms in this project. ".repeat(32);
        let text = &text[..2000];
        let start = Instant::now();
        prepare(&c);
        eprintln!("index build: {:?}", start.elapsed());
        let start = Instant::now();
        for _ in 0..200 {
            black_box(apply(black_box(text), black_box(&c)));
        }
        let mean = start.elapsed() / 200;
        eprintln!("snap 2000 chars x 2000 terms mean: {mean:?}");
        assert!(
            mean < if cfg!(debug_assertions) {
                Duration::from_millis(100)
            } else {
                Duration::from_millis(1)
            }
        );
    }
}
