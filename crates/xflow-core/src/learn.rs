//! Learn dictionary entries from the user's own corrections of a dictation.
//! Pure functions: the daemon passes text read from the field it pasted into, or
//! an edited history entry. Nothing here stores, logs or sends that text.
use crate::config::{DictionaryConfig, Replacement, MAX_DICTIONARY_ITEMS};

/// Context kept on each side of a pasted span to find it again later.
const ANCHOR_CHARS: usize = 16;
/// Wispr and VoiceInk both cap learned candidates per dictation at four.
pub const MAX_CANDIDATES: usize = 4;
const MAX_HUNK_WORDS: usize = 3;
const MAX_DIFF_WORDS: usize = 400;

/// Where a dictation landed in a text field: the pasted text plus up to 16
/// characters on each side. Never the rest of the field.
pub struct Receipt {
    before: String,
    inserted: String,
    after: String,
}
impl Receipt {
    /// Locate `inserted` in the field read just after the paste. It must occur
    /// exactly once (a trailing space the app dropped is tolerated).
    pub fn new(field: &str, inserted: &str) -> Option<Self> {
        let inserted = [inserted, inserted.trim_end()]
            .into_iter()
            .find(|s| !s.trim().is_empty() && field.matches(*s).count() == 1)?;
        let start = field.find(inserted)?;
        let end = start + inserted.len();
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
        Some(Self {
            before: before.into(),
            inserted: inserted.into(),
            after: after.into(),
        })
    }
    pub fn inserted(&self) -> &str {
        &self.inserted
    }
    /// The span between the anchors in the field as it reads now. An empty anchor
    /// means the field's start or end; any other anchor must occur exactly once.
    /// A short dictation that filled an empty field has nothing to tell it apart
    /// from the next message typed there, so it is not trusted.
    pub fn corrected<'a>(&self, field: &'a str) -> Option<&'a str> {
        if self.before.is_empty()
            && self.after.is_empty()
            && self.inserted.split_whitespace().count() < 4
        {
            return None;
        }
        let unique = |anchor: &str| {
            (field.matches(anchor).count() == 1)
                .then(|| field.find(anchor))
                .flatten()
        };
        let start = if self.before.is_empty() {
            0
        } else {
            unique(&self.before)? + self.before.len()
        };
        let end = if self.after.is_empty() {
            field.len()
        } else {
            unique(&self.after)?
        };
        field.get(start..end)
    }
}

/// Close word corrections from `original` to `corrected` (at most four), skipping
/// words the dictionary already has or the user removed. Shared by the paste
/// observer and history edits. Nothing is learned from a mostly different text.
pub fn corrections(
    original: &str,
    corrected: &str,
    dictionary: &DictionaryConfig,
) -> Vec<Replacement> {
    let a: Vec<&str> = original.split_whitespace().map(key).collect();
    let b: Vec<&str> = corrected.split_whitespace().map(key).collect();
    if a.len() > MAX_DIFF_WORDS || b.len() > MAX_DIFF_WORDS {
        return vec![];
    }
    // Case-insensitive, so recapitalizing the next word is not part of a fix.
    let lower =
        |words: &[&str]| -> Vec<String> { words.iter().map(|w| w.to_lowercase()).collect() };
    let (la, lb) = (lower(&a), lower(&b));
    // Suffix LCS lengths; edits are the unmatched runs between matched words.
    let mut lcs = vec![vec![0u16; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if la[i] == lb[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    if usize::from(lcs[0][0]) * 2 < a.len() {
        return vec![];
    }
    let known = |word: &str| {
        let word = word.to_lowercase();
        dictionary
            .words
            .iter()
            .chain(&dictionary.never_learn)
            .any(|known| known.to_lowercase() == word)
    };
    let mut found: Vec<Replacement> = vec![];
    let (mut i, mut j, mut from, mut to) = (0, 0, 0, 0);
    loop {
        let matched = i < a.len() && j < b.len() && la[i] == lb[j];
        if matched || (i == a.len() && j == b.len()) {
            if let Some(c) = hunk(&a[from..i], &b[to..j]) {
                // A removed word is not learned back in either direction.
                if !known(&c.to)
                    && !dictionary.never_learn.contains(&c.from.to_lowercase())
                    && !found
                        .iter()
                        .any(|f| f.to.to_lowercase() == c.to.to_lowercase())
                {
                    found.push(c);
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
    found.truncate(MAX_CANDIDATES);
    found
}
/// An edit hunk, or its aligned start or end when the user also typed or
/// deleted words right next to the correction.
fn hunk(from: &[&str], to: &[&str]) -> Option<Replacement> {
    let (n, m) = (from.len(), to.len());
    let k = n.min(m);
    candidate(from, to)
        .or_else(|| candidate(&from[..k], &to[..k]))
        .or_else(|| candidate(&from[n - k..], &to[m - k..]))
}
/// A word without the punctuation around it.
fn key(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric())
}
/// One edit is learnable when it replaces 1-3 words with 1-3 plain words that
/// look or sound like what was dictated. Insertions, deletions, case or
/// punctuation changes, numbers, URLs, paths, code and everyday words (so
/// homophone and grammar fixes like their/there) are not.
fn candidate(from: &[&str], to: &[&str]) -> Option<Replacement> {
    if from.is_empty() || to.is_empty() || from.len() > MAX_HUNK_WORDS || to.len() > MAX_HUNK_WORDS
    {
        return None;
    }
    let (from, to) = (from.join(" "), to.join(" "));
    let plain = |s: &str| {
        s.chars()
            .all(|c| c.is_alphabetic() || matches!(c, ' ' | '\'' | '’' | '-'))
    };
    let fold = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ')
            .flat_map(char::to_lowercase)
            .collect()
    };
    let letters = |s: &str| -> Vec<char> {
        s.chars()
            .filter(|c| c.is_alphabetic())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (x, y) = (letters(&from), letters(&to));
    // At most a third of the letters changed, or the same consonant skeleton.
    let close = distance(&x, &y) * 3 <= x.len().max(y.len())
        || (x.len().min(y.len()) >= 5 && sounds(&x).len() >= 3 && sounds(&x) == sounds(&y));
    // ponytail: lowerCamelCase over 6 chars reads as code; "iPhone"/"macOS" pass.
    let code =
        to.starts_with(char::is_lowercase) && to.chars().any(char::is_uppercase) && to.len() > 6;
    (plain(&from)
        && plain(&to)
        && y.len() >= 2
        && to.chars().count() <= 40
        && fold(&from) != fold(&to)
        && close
        && !code
        && !to.split(' ').all(common))
    .then_some(Replacement { from, to })
}
fn distance(a: &[char], b: &[char]) -> usize {
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
fn sounds(letters: &[char]) -> Vec<u8> {
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
/// Everyday words, very common names and acronyms are never learned (Wispr skips them too).
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

impl DictionaryConfig {
    /// Save learned corrections as vocabulary only: the corrected word becomes a
    /// recognition hint (first, so provider hint limits keep it). Learning never
    /// writes replacements, which would rewrite every future transcript.
    /// Returns the newly learned words.
    pub fn learn(&mut self, found: &[Replacement]) -> Vec<String> {
        let mut added = Vec::new();
        for c in found {
            if self.words.len() >= MAX_DICTIONARY_ITEMS {
                break;
            }
            if !self.words.contains(&c.to) {
                self.words.insert(0, c.to.clone());
            }
            if !self.learned.contains(&c.to) {
                self.learned.push(c.to.clone());
            }
            added.push(c.to.clone());
        }
        added
    }
    /// Remove a word. A learned word is remembered so it is never learned again.
    pub fn forget(&mut self, word: &str) {
        self.words.retain(|w| w != word);
        let lower = word.to_lowercase();
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn learn(original: &str, corrected: &str) -> Vec<(String, String)> {
        corrections(original, corrected, &DictionaryConfig::default())
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
        // Unrelated rewrites, everyday words, numbers, URLs, code.
        assert!(learn("the cat sat", "the dog sat").is_empty());
        // Homophone and grammar fixes between everyday words.
        assert!(learn("I went their", "I went there").is_empty());
        assert!(learn("check its done", "check it's done").is_empty());
        assert!(learn("buy a apple", "buy an apple").is_empty());
        // A different text is not a correction of this one.
        assert!(learn("deploy kubernetis now", "lunch at noon with Kubernetes").is_empty());
        assert!(learn("call me at five", "call me at 5").is_empty());
        assert!(learn("open example dot com", "open example.com").is_empty());
        assert!(learn("call get user name", "call getUserName").is_empty());
        assert!(learn("fix my_var", "fix my_vat").is_empty());
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
    fn learning_adds_vocabulary_only_and_removed_words_stay_removed() {
        let mut dictionary = DictionaryConfig::default();
        let found = corrections("use kubernetis", "use Kubernetes", &dictionary);
        assert_eq!(dictionary.learn(&found), ["Kubernetes"]);
        assert_eq!(dictionary.words, ["Kubernetes"]);
        assert_eq!(dictionary.learned, ["Kubernetes"]);
        assert!(dictionary.replacements.is_empty());
        assert!(corrections("use kubernetis", "use Kubernetes", &dictionary).is_empty());
        dictionary.forget("Kubernetes");
        assert!(dictionary.words.is_empty() && dictionary.learned.is_empty());
        assert!(corrections("use kubernetis", "use Kubernetes", &dictionary).is_empty());
        assert!(corrections("use Kubernetes", "use kubernetis", &dictionary).is_empty());
    }

    #[test]
    fn receipt_refinds_the_pasted_span_by_its_anchors() {
        let inserted = "We deploy on kubernetis daily. ";
        let field = format!("Intro paragraph here. {inserted}Closing words stay.");
        let receipt = Receipt::new(&field, inserted).unwrap();
        // Edits elsewhere in the field do not matter.
        let edited =
            "Changed intro paragraph here. We deploy on Kubernetes daily. Closing words stay!";
        let span = receipt.corrected(edited).unwrap();
        assert_eq!(span, "We deploy on Kubernetes daily. ");
        assert_eq!(
            corrections(receipt.inserted(), span, &DictionaryConfig::default())[0].to,
            "Kubernetes"
        );
        // Pasted at the end of the field; the app dropped the trailing space; the
        // user typed more afterwards (an insertion, not learned).
        let receipt = Receipt::new("Notes: we use postgress", "we use postgress ").unwrap();
        let span = receipt.corrected("Notes: we use Postgres today").unwrap();
        assert_eq!(
            learn(receipt.inserted(), span),
            pair("postgress", "Postgres")
        );
        // Ambiguous: the text was pasted twice, or an anchor now occurs twice.
        assert!(Receipt::new("hi there. hi there.", "hi there.").is_none());
        let receipt = Receipt::new(&field, inserted).unwrap();
        assert!(receipt
            .corrected("Intro paragraph here. Intro paragraph here. We deploy. Closing words stay.")
            .is_none());
        // Anchor gone.
        assert!(receipt.corrected("Something else entirely").is_none());
        // A short dictation that filled an empty field has no anchors to trust.
        let receipt = Receipt::new("use postgress", "use postgress").unwrap();
        assert!(receipt.corrected("lunch with Postgres").is_none());
    }
}
