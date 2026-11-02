//! Shipped term packs: exact heard forms with local context; never part of
//! snap's fuzzy index. See vocab/NOTICE.
use crate::config::Config;
use std::{collections::HashMap, sync::OnceLock};

/// (data, focused app ids that turn the pack on). An entry matches as a
/// substring of the lowercase app id, or exactly with a leading `=`
/// (Linux desktop ids, without `.desktop`). The first pack wins a heard form
/// two active packs share.
const PACKS: [(&str, &[&str]); 4] = [
    (
        include_str!("vocab/dev.tsv"),
        &[
            "vscode",
            "vscodium",
            "=code",
            "=code-oss",
            "=codium",
            "com.todesktop.",
            "dev.zed.",
            "=zed",
            "com.apple.dt.xcode",
            "jetbrains",
            "sublime",
            "windsurf",
            "neovide",
        ],
    ),
    (include_str!("vocab/corp.tsv"), &[]),
    (
        include_str!("vocab/sheet.tsv"),
        &[
            "com.microsoft.excel",
            "com.apple.iwork.numbers",
            "libreoffice-calc",
            "libreoffice.calc",
        ],
    ),
    (include_str!("vocab/finance.tsv"), &[]),
];

type Fields = [&'static str; 8];

/// Fields: canonical, safe heard forms, cued heard forms, left cues, right cues,
/// left blockers, right blockers, continuations after a left cue. Trailing
/// fields may be omitted and `-` means none.
fn fields(line: &'static str) -> Fields {
    let mut fields = [""; 8];
    for (slot, field) in fields.iter_mut().zip(line.split('\t')) {
        *slot = if field == "-" { "" } else { field };
    }
    fields
}

/// (pack, row) for every shipped row.
fn lines() -> impl Iterator<Item = (usize, &'static str)> {
    PACKS.iter().enumerate().flat_map(|(pack, (data, _))| {
        data.lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .map(move |line| (pack, line))
    })
}

/// (heard form, needs a cue) for one row.
fn heard(f: &Fields) -> impl Iterator<Item = (&'static str, bool)> {
    let forms = |field: &'static str, cued| {
        field
            .split('|')
            .filter(|s| !s.is_empty())
            .map(move |s| (s, cued))
    };
    forms(f[1], false).chain(forms(f[2], true))
}

fn words(form: &str) -> usize {
    form.split(' ').count()
}

/// First heard word -> (heard form, row, needs a cue), most words first. Rows
/// stay unsplit lines until one of their forms matches.
type Forms = HashMap<&'static str, Vec<(&'static str, u32, bool)>>;

fn index() -> &'static (Vec<(usize, &'static str)>, Forms) {
    static INDEX: OnceLock<(Vec<(usize, &'static str)>, Forms)> = OnceLock::new();
    INDEX.get_or_init(|| {
        let rows: Vec<_> = lines().collect();
        let mut forms = Forms::new();
        for (row, &(_, line)) in rows.iter().enumerate() {
            for (form, cued) in heard(&fields(line)) {
                let first = form.split(' ').next().unwrap_or_default();
                forms
                    .entry(first)
                    .or_default()
                    .push((form, row as u32, cued));
            }
        }
        for list in forms.values_mut() {
            list.sort_by_key(|&(form, _, _)| std::cmp::Reverse(words(form)));
        }
        (rows, forms)
    })
}

fn active(config: &Config, terminal: bool, app: Option<&str>) -> [bool; 4] {
    let app = app.unwrap_or_default().to_lowercase();
    let app = app.strip_suffix(".desktop").unwrap_or(&app);
    let on = [
        config.vocab.dev || terminal,
        config.vocab.corp,
        config.vocab.sheet,
        config.vocab.finance,
    ];
    std::array::from_fn(|i| {
        on[i]
            || PACKS[i].1.iter().any(|id| match id.strip_prefix('=') {
                Some(id) => app == id,
                None => app.contains(id),
            })
    })
}

// Conservatively protect a whole user phrase when a shorter shipped form overlaps it.
fn overlaps(a: &str, b: &str) -> bool {
    a.split_whitespace()
        .any(|a| b.split_whitespace().any(|b| a.eq_ignore_ascii_case(b)))
}

struct Token<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    eligible: bool,
}

fn gap(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| matches!(c, ' ' | '\t'))
}

fn neighbour(text: &str, tokens: &[Token<'_>], i: usize, patterns: &str, left: bool) -> bool {
    if i == 0 || i == tokens.len() || !gap(&text[tokens[i - 1].end..tokens[i].start]) {
        return false;
    }
    patterns.split('|').filter(|s| !s.is_empty()).any(|p| {
        let count = p.split_whitespace().count();
        let start = if left {
            let Some(start) = i.checked_sub(count) else {
                return false;
            };
            start
        } else {
            i
        };
        let end = start + count;
        end <= tokens.len() && matches(text, &tokens[start..end], p)
    })
}

fn matches(text: &str, tokens: &[Token<'_>], phrase: &str) -> bool {
    tokens.iter().all(|t| t.eligible)
        && tokens.iter().map(|t| t.text).eq(phrase.split_whitespace())
        && tokens.windows(2).all(|w| gap(&text[w[0].end..w[1].start]))
}

/// Blocked terms and explicit user spellings always outrank shipped corrections.
pub(crate) fn apply(text: &str, config: &Config, terminal: bool, app: Option<&str>) -> String {
    if !config.formatting.snap_to_dictionary {
        return text.to_owned();
    }
    let active = active(config, terminal, app);
    let (rows, forms) = index();
    let lower = text.to_ascii_lowercase();
    let tokens: Vec<_> = lower
        .split_whitespace()
        .map(|chunk| {
            let word = chunk.trim_matches(|c: char| !c.is_alphanumeric());
            let start = word.as_ptr() as usize - lower.as_ptr() as usize;
            Token {
                text: word,
                start,
                end: start + word.len(),
                eligible: !word.is_empty()
                    && !chunk.contains(['@', '/', '\\', '_', '`', '-'])
                    && !chunk.starts_with("www.")
                    && !crate::code::literal(chunk),
            }
        })
        .collect();
    // Shouted text keeps shouting; an "IOS" inside normal text becomes "iOS".
    let shouting = !text.chars().any(char::is_lowercase);
    let (mut cursor, mut i) = (0, 0);
    let mut out = String::with_capacity(text.len());
    while i < tokens.len() {
        let list = match forms.get(tokens[i].text) {
            Some(list) if tokens[i].eligible => list,
            _ => {
                i += 1;
                continue;
            }
        };
        // The longest matching form decides: a blocked longer phrase must not
        // fall back to a partial correction, but a same-length form of another
        // term or pack may still pass its own cues.
        let mut longest = 0;
        let mut found = None;
        for &(form, row, cued) in list {
            let count = words(form);
            if count < longest {
                break;
            }
            let ((pack, line), end) = (rows[row as usize], i + count);
            if !active[pack] || end > tokens.len() || !matches(&lower, &tokens[i..end], form) {
                continue;
            }
            // Speech-to-text already chose the case of a capitalised word
            // ("SWIFT code", "Swift code"); only lowercase everyday words change.
            if cued
                && !shouting
                && text[tokens[i].start..tokens[end - 1].end]
                    .chars()
                    .any(char::is_uppercase)
            {
                // Like a blocked phrase, it must not fall back to a shorter form.
                longest = count;
                continue;
            }
            longest = count;
            let row = fields(line);
            if let Some(term) = allowed(
                text,
                &lower,
                &tokens,
                (i, end, cued),
                &row,
                config,
                terminal,
            ) {
                found = Some((end, term));
                break;
            }
        }
        if let Some((end, term)) = found {
            out.push_str(&text[cursor..tokens[i].start]);
            if shouting {
                out.push_str(&term.to_uppercase());
            } else {
                out.push_str(term);
            }
            cursor = tokens[end - 1].end;
            i = end;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

/// The spelling to write for `tokens[i..end]`, or None when cues, blockers or
/// the user's own dictionary say no. A cued form is everyday English and needs
/// a neighbouring cue; a safe form needs none.
fn allowed<'a>(
    text: &str,
    lower: &str,
    tokens: &[Token<'_>],
    (i, end, cued): (usize, usize, bool),
    row: &Fields,
    config: &'a Config,
    terminal: bool,
) -> Option<&'a str> {
    let after = lower[tokens[end - 1].end..].trim_start();
    let boundary = after.is_empty() || after.starts_with([',', '.', ':', ';', '!', '?']);
    let left = neighbour(lower, tokens, i, row[3], true)
        && (row[7].is_empty() || boundary || neighbour(lower, tokens, end, row[7], false));
    if cued && !(left || neighbour(lower, tokens, end, row[4], false))
        || neighbour(lower, tokens, i, row[5], true)
        || neighbour(lower, tokens, end, row[6], false)
    {
        return None;
    }
    let input = &text[tokens[i].start..tokens[end - 1].end];
    let words = || config.dictionary.words.iter().chain(&config.stt.vocabulary);
    let target = words()
        .find(|w| w.eq_ignore_ascii_case(row[0]))
        .map_or(row[0], String::as_str);
    if terminal && input.eq_ignore_ascii_case(target)
        || config
            .dictionary
            .never_learn
            .iter()
            .any(|w| overlaps(row[0], w) || overlaps(input, w))
        || words().any(|w| overlaps(input, w))
        || config
            .dictionary
            .replacements
            .iter()
            .any(|r| overlaps(input, &r.from))
    {
        return None;
    }
    Some(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Replacement, text::before_cleanup};
    use std::collections::HashSet;

    #[test]
    fn dev_terms_need_neighbouring_evidence() {
        let config = Config::default();
        let cases = [
            ("ask cloud to review", "ask Claude to review"),
            ("hey cloud code", "hey Claude Code"),
            ("tell cloud to explain", "tell Claude to explain"),
            ("cloud opus", "Claude opus"),
            ("cloud sonnet", "Claude sonnet"),
            ("codecs agent", "Codex agent"),
            ("cursor editor", "Cursor editor"),
            ("kube control get pods", "kubectl get pods"),
            ("kube ctl apply", "kubectl apply"),
            ("post gress database", "Postgres database"),
            ("post gress sql server", "PostgreSQL server"),
            ("next JS app", "Next.js app"),
            ("vercell deployment", "Vercel deployment"),
            ("super base auth", "Supabase auth"),
            ("n p m install", "npm install"),
            ("p n p m install", "pnpm install"),
            ("git hub pull request", "GitHub pull request"),
            ("gitlab merge request", "GitLab merge request"),
            ("oh auth token", "OAuth token"),
            ("jason payload", "JSON payload"),
            ("yammel config", "YAML config"),
            ("engine x proxy", "nginx proxy"),
            ("sql lite database", "SQLite database"),
            ("reddis cache", "Redis cache"),
            ("kafka topic", "Kafka topic"),
            ("terraform plan", "Terraform plan"),
            ("anthro pic api", "Anthropic API"),
            ("open ai sdk", "OpenAI SDK"),
            ("gemini api", "Gemini API"),
            ("llama weights", "Llama weights"),
            ("o llama serve", "Ollama serve"),
            ("hugging face hub", "Hugging Face hub"),
            ("py torch tensor", "PyTorch tensor"),
            ("type script compiler", "TypeScript compiler"),
            ("tail wind css", "Tailwind CSS"),
            ("tail wind config", "Tailwind config"),
            ("py torch café", "PyTorch café"),
            ("shad cn components", "shadcn components"),
            ("t r p c router", "tRPC router"),
            ("g r p c service", "gRPC service"),
            ("websocket connection", "WebSocket connection"),
            ("dogger container", "Docker container"),
            ("use snake case cloud code", "use cloud_code"),
        ];
        for (input, expected) in cases {
            assert_eq!(before_cleanup(input, &config, false), expected, "{input}");
        }
        // A cued form alone never becomes its term; only safe forms do.
        let all = all_packs();
        for (_, line) in lines() {
            let f = fields(line);
            for (form, cued) in heard(&f) {
                if cued {
                    let out = apply(form, &all, false, None);
                    assert!(!out.contains(f[0]), "{form} -> {out}");
                }
            }
        }
        for input in [
            "cloud storage",
            "the cloud",
            "google cloud code",
            "deploy to the cloud in Cursor",
            "cloud",
            "ask cloud storage",
            "hey google cloud code",
            "hey cloud code storage",
            "audio codecs agent",
            "video codecs",
            "mouse cursor editor",
            "cursor position",
            "franz kafka topic",
            "kafka novel",
            "gemini zodiac",
            "llama farm",
            "dogger bank",
            "the dogger sailed",
            "so late",
            "we have a super base",
            "jason bourne",
            "a tail wind helps",
            "ask cloud.service to review",
            "ask /cloud to review",
            "ask cloud@example.com to review",
            "ask `cloud` to review",
            "ask cloud_name to review",
            "ask\ncloud to review",
            "ask. cloud to review",
            "next. js app",
            "post, gress database",
            "école cloud storage",
        ] {
            assert_eq!(before_cleanup(input, &config, false), input, "{input}");
        }
    }

    #[test]
    fn review_cloud_prose_is_unchanged() {
        eprintln!("cue tests source: {}", env!("CARGO_MANIFEST_DIR"));
        let config = Config::default();
        // Other shipped terms may still be fixed; cloud never becomes Claude here.
        for (input, expected) in [
            ("try cloud code for vs code", "try cloud code for VS Code"),
            (
                "use cloud code to debug kubernetes",
                "use cloud code to debug Kubernetes",
            ),
        ] {
            assert_eq!(before_cleanup(input, &config, false), expected, "{input}");
        }
        for input in [
            "ask cloud providers for a quote",
            "ask cloud vendors",
            "ask cloud support to help",
            "tell cloud engineers to scale",
            "ask cloud architects",
            "ask cloud native folks",
            "hey cloud team, any update",
            "the bug is in cloud code not client code",
            "we deploy with cloud code generation",
            "ask cloud code reviewers",
            "cloud code generation",
            "cloud code reviewers",
            "we need an open ai model that anyone can run",
            "an anthropic model of cosmology",
            "a codecs review",
        ] {
            assert_eq!(before_cleanup(input, &config, false), input, "{input}");
        }
    }

    #[test]
    fn addressing_cues_need_a_safe_continuation() {
        let config = Config::default();
        for (input, expected) in [
            ("ask cloud to review", "ask Claude to review"),
            ("ask cloud about this", "ask Claude about this"),
            ("ask cloud for help", "ask Claude for help"),
            ("ask cloud opus", "ask Claude opus"),
            ("hey cloud", "hey Claude"),
            ("hey cloud!", "hey Claude!"),
            ("hey cloud, review this", "hey Claude, review this"),
            ("hey cloud code", "hey Claude Code"),
            ("ask cloud code to review", "ask Claude Code to review"),
            ("tell cloud code about this", "tell Claude Code about this"),
            ("ask cloud code for help", "ask Claude Code for help"),
            ("hey cloud code!", "hey Claude Code!"),
        ] {
            assert_eq!(before_cleanup(input, &config, false), expected, "{input}");
        }
    }

    #[test]
    fn all_caps_corrections_keep_the_speakers_case() {
        let config = Config::default();
        for (input, expected) in [
            ("ASK CLOUD TO REVIEW", "ASK CLAUDE TO REVIEW"),
            ("HEY CLOUD CODE", "HEY CLAUDE CODE"),
        ] {
            assert_eq!(before_cleanup(input, &config, false), expected, "{input}");
        }
    }

    #[test]
    fn dictionary_phrases_protect_overlapping_cue_words() {
        let mut config = Config::default();
        for term in ["Google Cloud", "VS Code"] {
            config.dictionary.words = vec![term.into()];
            assert_eq!(
                before_cleanup("hey cloud code", &config, false),
                "hey cloud code"
            );
        }
    }

    #[test]
    fn punctuation_tokens_skip_the_cue_search() {
        let text = "... ".repeat(10_000);
        let start = std::time::Instant::now();
        assert_eq!(apply(&text, &all_packs(), false, None), text);
        let elapsed = start.elapsed();
        eprintln!("punctuation-only 40KB: {elapsed:?}");
        assert!(elapsed.as_millis() < if cfg!(debug_assertions) { 100 } else { 20 });
    }

    #[test]
    fn controls_and_user_spellings_win() {
        let mut config = Config::default();
        config.formatting.snap_to_dictionary = false;
        assert_eq!(
            before_cleanup("ask cloud to review", &config, false),
            "ask cloud to review"
        );
        config.formatting.snap_to_dictionary = true;
        for blocked in ["Claude", "cloud"] {
            config.dictionary.never_learn = vec![blocked.into()];
            for input in ["ask cloud to review", "hey cloud code"] {
                assert_eq!(before_cleanup(input, &config, false), input);
            }
        }
        config.dictionary.never_learn = vec!["Claude Code".into()];
        assert_eq!(
            before_cleanup("hey cloud code", &config, false),
            "hey cloud code"
        );
        config.dictionary.never_learn.clear();
        for spelling in ["cloud", "cloud code"] {
            config.dictionary.words = vec![spelling.into()];
            assert_eq!(
                before_cleanup("hey cloud code", &config, false),
                "hey cloud code"
            );
        }
        config.dictionary.words = vec!["cLaUdE".into()];
        assert_eq!(
            before_cleanup("ask claude to review", &config, false),
            "ask cLaUdE to review"
        );
        assert_eq!(
            before_cleanup("ask cloud to review", &config, false),
            "ask cLaUdE to review"
        );
        config.dictionary.words.clear();
        config.stt.vocabulary = vec!["cloud".into()];
        assert_eq!(
            before_cleanup("ask cloud to review", &config, false),
            "ask cloud to review"
        );
        config.stt.vocabulary.clear();
        config.dictionary.replacements = vec![Replacement {
            from: "Claude".into(),
            to: "Assistant".into(),
        }];
        assert_eq!(
            before_cleanup("ask cloud to review", &config, false),
            "ask Assistant to review"
        );
        config.dictionary.replacements = vec![Replacement {
            from: "cloud".into(),
            to: "AWS".into(),
        }];
        assert_eq!(
            before_cleanup("ask cloud to review", &config, false),
            "ask AWS to review"
        );
    }

    #[test]
    fn terminals_skip_only_case_changes() {
        let config = Config::default();
        for input in [
            "cursor editor",
            "docker compose",
            "json payload",
            "claude code",
        ] {
            assert_eq!(before_cleanup(input, &config, true), input);
        }
        assert_eq!(
            before_cleanup("ask cloud to review", &config, true),
            "ask Claude to review"
        );
        assert_eq!(
            before_cleanup("kube control get pods", &config, true),
            "kubectl get pods"
        );
    }

    fn all_packs() -> Config {
        let mut config = Config::default();
        config.vocab.sheet = true;
        config.vocab.finance = true;
        config
    }

    /// Names and words that SCOWL and some system dictionaries miss; each was
    /// a false correction in review.
    const EXTRA_ENGLISH: &str = "claud lora yamil kagel cagle kegel jeera geera tablo \
        tableau eban kron kates rike lotti lotty pardo clavijo samel uri grr swot sec apr \
        aum wack kegger zapper luau svelte storybook redux ghosty docker debbie jango kuda \
        carass keros naven navan seby adian lerna ansible midjourney blackline cols diff dif";

    /// Cue words too common to signal technical or finance context alone.
    const GENERIC_CUES: &str = "a an the in on at to from into onto with without via over \
        under for of by as and or but not no so than then this that these those it its our \
        your my their his her we you they i me us them all any some each every one two use \
        uses used using run runs ran running write writes wrote written learn learned open \
        opened make made get got take took go went do did is are was were be been have has \
        had can could will would should app apps application applications code project \
        projects data file files account accounts page pages team teams call calls meeting \
        meetings message messages chat link links model models program programs library \
        libraries developer developers version versions system systems service services \
        server servers tool tools platform user users report reports number numbers rate \
        rates time line lines list type types test tests check checks build builds key keys \
        document documents docs doc table tables function functions formula formulas \
        statement statements thread threads post posts group groups board boards card cards \
        price prices product products customer customers business company companies room \
        rooms web site sites search results result window job jobs task tasks work process \
        field fields record records note notes value values range object objects shape new \
        real good great big little first last annual daily total net growth help support \
        state stack view views mode part parts set step steps top end start";

    /// SCOWL plus, where installed, the system word and name lists (required on
    /// macOS, so the macOS CI job checks them).
    fn english() -> HashSet<String> {
        let mut words: HashSet<String> =
            EXTRA_ENGLISH.split_whitespace().map(String::from).collect();
        let mut found = false;
        for path in [
            "/usr/share/dict/words",
            "/usr/share/dict/propernames",
            "/usr/share/dict/connectives",
        ] {
            if let Ok(list) = std::fs::read_to_string(path) {
                found = true;
                words.extend(list.lines().map(|w| w.trim().to_lowercase()));
            }
        }
        assert!(
            found || !cfg!(target_os = "macos"),
            "missing /usr/share/dict"
        );
        words
    }

    #[test]
    fn shipped_data_is_small_unique_and_gated() {
        use crate::words::{common, name};
        let english = english();
        // A word, not a spelled letter or a digit: "a w s" is not ordinary speech.
        let word = |w: &str| match w.len() {
            1 => w == "a" || w == "i",
            _ => {
                !w.bytes().all(|c| c.is_ascii_digit())
                    && (common(w) || name(w) || english.contains(w))
            }
        };
        let ordinary = |form: &str| form.split(' ').all(word);
        let generic: HashSet<_> = GENERIC_CUES.split_whitespace().collect();
        let normal = |s: &str| {
            s.to_lowercase()
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let bytes: usize = PACKS.iter().map(|(data, _)| data.len()).sum();
        eprintln!("vocab packs: {bytes} bytes");
        assert!(bytes < 400_000);
        let mut counts = [0; 4];
        let mut wrong = Vec::new();
        let mut terms = HashSet::new();
        let mut packs_forms: Vec<HashMap<&str, &str>> = vec![HashMap::new(); 4];
        // heard form -> (canonical, cued) across packs
        let mut shared: HashMap<&str, Vec<(&str, bool)>> = HashMap::new();
        for (pack, line) in lines() {
            let f = fields(line);
            let mut bad = |why: String| wrong.push(format!("{why}: {line}"));
            counts[pack] += 1;
            if line.split('\t').count() > 8 || !f[0].is_ascii() || f[0].starts_with('#') {
                bad("shape".into());
            }
            if !terms.insert((pack, f[0].to_lowercase())) {
                bad("duplicate canonical".into());
            }
            let cued = heard(&f).any(|(_, cued)| cued);
            // A cued form needs a cue to ever fire; a cue without one is dead data.
            if cued != (!f[3].is_empty() || !f[4].is_empty()) || heard(&f).next().is_none() {
                bad("cues and cued forms disagree".into());
            }
            for cue in f[3].split('|').chain(f[4].split('|')) {
                if !cue.is_empty() && cue.split(' ').all(|w| generic.contains(w)) {
                    bad(format!("generic cue {cue:?}"));
                }
            }
            let forms: Vec<_> = heard(&f).map(|(form, _)| form).collect();
            let canonical = normal(f[0]);
            for (form, cued) in heard(&f) {
                if !form.split(' ').all(|w| {
                    !w.is_empty()
                        && w.bytes()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                }) || form == f[0]
                {
                    bad(format!("form {form:?}"));
                }
                // Ordinary speech (every word English or a name) is never safe.
                if !cued && ordinary(form) {
                    bad(format!("ordinary safe form {form:?}"));
                }
                // A form must not add or drop a plural: "monorepos" -> "monorepo".
                for base in forms.iter().copied().chain([canonical.as_str()]) {
                    let plural =
                        |a: &str, b: &str| !b.ends_with('s') && a.strip_suffix('s') == Some(b);
                    if base != form && (plural(form, base) || plural(base, form)) {
                        bad(format!("plural {form:?} vs {base:?}"));
                    }
                }
                if packs_forms[pack].insert(form, f[0]).is_some() {
                    bad(format!("duplicate form {form:?}"));
                }
                shared.entry(form).or_default().push((f[0], cued));
            }
        }
        // Packs may share a form for the same spelling; different spellings
        // ("Swift" and "SWIFT") must both wait for their cues.
        for (form, owners) in shared {
            let spellings: HashSet<_> = owners.iter().map(|(t, _)| *t).collect();
            if spellings.len() > 1 && !owners.iter().all(|(_, cued)| *cued) {
                wrong.push(format!("{form}: {owners:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
        eprintln!("vocab rows dev/corp/sheet/finance: {counts:?}");
        for (count, range) in counts
            .into_iter()
            .zip([600..=1000, 150..=250, 100..=250, 200..=300])
        {
            assert!(range.contains(&count), "{count} rows");
        }
    }

    /// The 315 sentences from the PR #113 review (ordinary prose, adversarial
    /// near-misses and cue traps) in every pack combination and app context.
    /// Only the technical acronyms and "v lookup" change, and only where their pack is on.
    #[test]
    fn review_prose_is_unchanged_in_every_pack_combination() {
        let changes = [
            (
                "We spoke about the api after lunch.",
                "We spoke about the API after lunch.",
                0,
            ),
            (
                "We need to import html into the archive.",
                "We need to import HTML into the archive.",
                0,
            ),
            (
                "The client asked us to import xml today.",
                "The client asked us to import XML today.",
                0,
            ),
            (
                "Use a v lookup for the customer list.",
                "Use a VLOOKUP for the customer list.",
                2,
            ),
        ];
        let mut wrong = Vec::new();
        for mask in 0..16 {
            let mut config = Config::default();
            config.vocab.dev = mask & 1 != 0;
            config.vocab.corp = mask & 2 != 0;
            config.vocab.sheet = mask & 4 != 0;
            config.vocab.finance = mask & 8 != 0;
            for (terminal, app) in [
                (false, None),
                (true, Some("com.apple.Terminal")),
                (false, Some("com.microsoft.VSCode")),
                (false, Some("com.microsoft.Excel")),
                (false, Some("com.apple.mail")),
            ] {
                let on = active(&config, terminal, app);
                for text in include_str!("vocab/review-prose.txt").lines() {
                    let expected = changes
                        .iter()
                        .find(|(input, out, pack)| {
                            *input == text
                                && on[*pack]
                                && !(terminal && input.eq_ignore_ascii_case(out))
                        })
                        .map_or(text, |(_, out, _)| out);
                    let got = apply(text, &config, terminal, app);
                    if got != expected {
                        wrong.push(format!("mask {mask} {app:?}: {text} -> {got}"));
                    }
                }
            }
        }
        assert!(wrong.is_empty(), "{} changed: {wrong:#?}", wrong.len());
    }

    #[test]
    fn prose_control_is_unchanged() {
        let changed: Vec<_> = include_str!("snap-prose.txt")
            .lines()
            .filter(|text| apply(text, &all_packs(), false, None) != *text)
            .collect();
        assert!(changed.is_empty(), "{changed:#?}");
    }

    #[test]
    fn packs_fix_terms_only_in_context() {
        let config = all_packs();
        let fixed = [
            // dev
            (
                "deploy the kubernetes cluster with kube control",
                "deploy the Kubernetes cluster with kubectl",
            ),
            (
                "we moved from my sequel database to post gress",
                "we moved from MySQL database to Postgres",
            ),
            ("the react component renders", "the React component renders"),
            ("rewritten in rust", "rewritten in Rust"),
            ("the rust compiler is slow", "the Rust compiler is slow"),
            ("the swift compiler", "the Swift compiler"),
            ("ask chat gpt and open ai sdk", "ask ChatGPT and OpenAI SDK"),
            (
                "the github copilot extension",
                "the GitHub Copilot extension",
            ),
            ("the java script runtime", "the JavaScript runtime"),
            ("a java exception", "a Java exception"),
            ("go routines leak", "Go routines leak"),
            ("deepseek and mistral api", "DeepSeek and Mistral API"),
            ("a w s and g c p", "AWS and GCP"),
            ("the cloudfront distribution", "the CloudFront distribution"),
            ("cursor agent mode", "Cursor agent mode"),
            ("vercel and super base auth", "Vercel and Supabase auth"),
            ("python script", "Python script"),
            ("the next js app router", "the Next.js app router"),
            ("the groq api", "the Groq API"),
            // Seen from Groq Whisper on synthetic speech.
            ("rewrote it with shadkin", "rewrote it with shadcn"),
            ("use zlookup with sumifs", "use XLOOKUP with SUMIFS"),
            ("We passed SOC 2 Type 2", "We passed SOC 2 Type II"),
            // corp
            ("hit 2 million arr this year", "hit 2 million ARR this year"),
            ("the o k rs and k p is", "the OKRs and KPIs"),
            ("we passed sock 2 type 2", "we passed SOC 2 Type II"),
            ("the soc 2 audit", "the SOC 2 audit"),
            ("log it in sales force crm", "log it in Salesforce CRM"),
            ("post in the slack channel", "post in the Slack channel"),
            ("a jira ticket in hubspot", "a Jira ticket in HubSpot"),
            ("send the n d a and s o w", "send the NDA and SOW"),
            // sheet
            ("use a v lookup or x lookup", "use a VLOOKUP or XLOOKUP"),
            ("try sumifs with countifs", "try SUMIFS with COUNTIFS"),
            ("a nested if statement", "a nested IF statement"),
            ("open the power query editor", "open the Power Query editor"),
            // finance
            ("e bit da margin", "EBITDA margin"),
            ("under u s gap accounting", "under u s GAAP accounting"),
            ("non gaap earnings", "non-GAAP earnings"),
            ("k y c and a m l checks", "KYC and AML checks"),
            ("the swift bic and i b a n", "the SWIFT bic and IBAN"),
            ("the annual ten k filing", "the annual 10-K filing"),
            ("our cogs margin", "our COGS margin"),
            ("the p and l and capex", "the P&L and CapEx"),
            ("five year kagger", "five year CAGR"),
            ("the whack calculation", "the WACC calculation"),
            ("a cruel basis", "accrual basis"),
            ("compounded sofa rate", "compounded SOFR rate"),
        ];
        let wrong: Vec<_> = fixed
            .iter()
            .map(|(input, expected)| (before_cleanup(input, &config, false), *expected))
            .filter(|(got, expected)| got != expected)
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
        let changed: Vec<_> = [
            "cloud storage",
            "the swift river",
            "a swift response",
            "taylor swift tickets",
            "a pivot in strategy",
            "a pivot table",
            "react to news",
            "they react quickly",
            "cursor position",
            "go home",
            "let's go now",
            "rust on the car",
            "a rust stain",
            "java coffee",
            "a cup of java",
            "arr matey",
            "a python snake",
            "the slack in the rope",
            "cut me some slack",
            "zoom in on the map",
            "our teams meeting",
            "a notion of fairness",
            "a linear function",
            "the sum of the parts",
            "count if you can",
            "mind the gap",
            "a gap analysis",
            "I ran a ten k",
            "a cruel joke",
            "a sofa bed",
            "a whack on the head",
            "cogs in the machine",
            "excel at math",
            "students who excel in class",
            "the index of the book",
            "the text before this",
            "a quick book",
            "a black rock",
            "sales force of fifty",
            "the clerk at the desk",
            "a stripe of paint",
            "a long chain",
            "keep an open eye",
            "the model's weights and biases",
            "a raspberry pie",
            "watched a series a while ago",
            // A capitalised word keeps the case speech-to-text chose.
            "the SWIFT code for the transfer",
            "the Swift code review",
            "written in swift code",
            "send it over Slack status",
            "Rust compiler",
        ]
        .into_iter()
        .map(|input| (input, before_cleanup(input, &config, false)))
        .filter(|(input, got)| input != got)
        .collect();
        assert!(changed.is_empty(), "{changed:#?}");
    }

    #[test]
    fn packs_follow_the_focused_app_and_config() {
        let mut config = Config::default();
        let run = |config: &Config, terminal, app| {
            crate::text::prepare("v lookup and e bit da in kube control", config, terminal, app).0
        };
        // Defaults: dev and corp everywhere; sheet in spreadsheet apps; finance off.
        assert_eq!(
            run(&config, false, None),
            "v lookup and e bit da in kubectl"
        );
        for app in [
            "com.microsoft.Excel",
            "com.apple.iWork.Numbers",
            "libreoffice-calc",
        ] {
            assert_eq!(
                run(&config, false, Some(app)),
                "VLOOKUP and e bit da in kubectl",
                "{app}"
            );
        }
        // Calculator is not a spreadsheet.
        assert_eq!(
            run(&config, false, Some("com.apple.calculator")),
            "v lookup and e bit da in kubectl"
        );
        config.vocab.finance = true;
        assert_eq!(run(&config, false, None), "v lookup and EBITDA in kubectl");
        config.vocab.sheet = true;
        assert_eq!(run(&config, false, None), "VLOOKUP and EBITDA in kubectl");
        // dev off still applies in terminals and editors.
        config.vocab.dev = false;
        assert_eq!(
            run(&config, false, Some("com.tinyspeck.slackmacgap")),
            "VLOOKUP and EBITDA in kube control"
        );
        assert_eq!(
            run(&config, true, Some("com.apple.Terminal")),
            "VLOOKUP and EBITDA in kubectl"
        );
        assert_eq!(
            run(&config, false, Some("com.microsoft.VSCode")),
            "VLOOKUP and EBITDA in kubectl"
        );
        for app in [
            "com.todesktop.230313mzl4w4u92",
            "code",
            "code.desktop",
            "dev.zed.Zed",
        ] {
            assert_eq!(
                run(&config, false, Some(app)),
                "VLOOKUP and EBITDA in kubectl",
                "{app}"
            );
        }
        config.vocab.corp = false;
        assert_eq!(
            before_cleanup("our sock 2 report", &config, false),
            "our sock 2 report"
        );
        config.vocab.corp = true;
        assert_eq!(
            before_cleanup("our sock 2 report", &config, false),
            "our SOC 2 report"
        );
        // The master switch turns every pack off.
        config.formatting.snap_to_dictionary = false;
        assert_eq!(
            run(&config, true, Some("com.microsoft.Excel")),
            "v lookup and e bit da in kube control"
        );
        // A [vocab] table round-trips through TOML with partial keys.
        let parsed: Config = toml::from_str("[vocab]\nfinance = true").unwrap();
        assert!(
            parsed.vocab.finance && parsed.vocab.dev && parsed.vocab.corp && !parsed.vocab.sheet
        );
    }

    #[test]
    fn user_dictionary_outranks_packs() {
        let mut config = all_packs();
        config.dictionary.never_learn = vec!["vlookup".into()];
        assert_eq!(
            before_cleanup("use v lookup", &config, false),
            "use v lookup"
        );
        config.dictionary.never_learn.clear();
        config.dictionary.words = vec!["VLookup".into()];
        assert_eq!(
            before_cleanup("use v lookup", &config, false),
            "use VLookup"
        );
        config.dictionary.words = vec!["sock".into()];
        assert_eq!(
            before_cleanup("our sock 2 report", &config, false),
            "our sock 2 report"
        );
        config.dictionary.words.clear();
        config.dictionary.replacements = vec![Replacement {
            from: "ebitda".into(),
            to: "earnings".into(),
        }];
        assert_eq!(
            before_cleanup("e bit da margin", &config, false),
            "earnings margin"
        );
    }

    #[test]
    fn pipeline_latency() {
        use std::{
            hint::black_box,
            time::{Duration, Instant},
        };
        let config = all_packs();
        // One-time parse on the first dictation (near zero if another test ran first).
        let start = Instant::now();
        black_box(index());
        eprintln!("pack index build: {:?}", start.elapsed());
        let text =
            "ask cloud to review the pull request with next js app and post gress database. "
                .repeat(26);
        // Dense with pack terms: every sentence has ten corrections.
        let packed = "the e bit da and v lookup in sock 2 deck for cooper netties on a w s with \
                      chat gpt using x lookup and k p is from the g a a p filing. "
            .repeat(16);
        for (label, text) in [
            ("short", "ask cloud to review the pull request"),
            ("2k", text.as_str()),
            ("2k packed", packed.as_str()),
        ] {
            let mut samples = Vec::new();
            let mut packs = Vec::new();
            for _ in 0..200 {
                let start = Instant::now();
                black_box(before_cleanup(black_box(text), &config, false));
                samples.push(start.elapsed());
                let start = Instant::now();
                black_box(apply(black_box(text), &config, false, None));
                packs.push(start.elapsed());
            }
            samples.sort_unstable();
            packs.sort_unstable();
            eprintln!(
                "before_cleanup {label}: p50 {:?}, p95 {:?}; packs alone p50 {:?}",
                samples[100], samples[190], packs[100]
            );
            assert!(
                samples[100]
                    < if cfg!(debug_assertions) {
                        Duration::from_millis(100)
                    } else {
                        Duration::from_millis(2)
                    }
            );
        }
    }
}
