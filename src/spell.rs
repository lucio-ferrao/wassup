//! A small local spell checker, for when there's no model to ask, as the Node version's spell.ts: Hunspell's dictionaries for
//! Portuguese of Portugal and English (GB and US, LibreOffice's, as npm's dictionary-* packages have them) read by
//! spellbook, a word being right in any of them, so a message mixing both is fine. It only sees words, one at a time:
//! a real word in the wrong place ("as 8" for "às 8") is the model's to catch.
//!
//! The dictionaries come in the program (from its `dictionaries/` folder, one folder each with its licence), so a
//! `cargo install` has them; a folder given, or Hunspell's own (`/usr/share/hunspell/pt_PT.aff`…), is taken first
//! when it has one. Reading them takes some milliseconds, so it's done on a thread of its own (`load_in_background`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Instant;

use regex::Regex;
use spellbook::Dictionary;

/// A word to change in a text: what's written, what it should be, and where it is, in characters (as the cursor
/// counts them), the end not included. The same shape as wassup's Fix, which the model's corrections have too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub from: String,
    pub to: String,
    pub start: usize,
    pub end: usize,
}

/// The dictionaries by their Hunspell names, Portuguese first, as the folders and files are called.
const DICTIONARIES: [&str; 3] = ["pt_PT", "en_GB", "en_US"];

/// Where Hunspell's dictionaries are when the system has them installed.
const SYSTEM_DIRS: [&str; 3] = ["/usr/share/hunspell", "/usr/share/myspell", "/usr/share/myspell/dicts"];

/// Links, as the Node version's links.ts finds them (its URL_RE).
const URL_RE: &str = r#"https?://[^\s<>"')\]]+"#;

static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(URL_RE).unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{M}][\p{L}\p{M}'’-]*").unwrap());
static ADDRESS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\S*@\S*").unwrap());

/// Where dictionaries are looked for before the program's own: the folder given (an option, or an environment
/// variable), then the system's.
pub fn dictionary_dirs(given: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = given.map(Path::to_path_buf).into_iter().collect();
    dirs.extend(SYSTEM_DIRS.iter().map(PathBuf::from));
    dirs
}

/// The dictionaries that come in the program, by name.
fn embedded(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "pt_PT" => (include_str!("../dictionaries/pt_PT/index.aff"), include_str!("../dictionaries/pt_PT/index.dic")),
        "en_GB" => (include_str!("../dictionaries/en_GB/index.aff"), include_str!("../dictionaries/en_GB/index.dic")),
        "en_US" => (include_str!("../dictionaries/en_US/index.aff"), include_str!("../dictionaries/en_US/index.dic")),
        _ => return None,
    })
}

/// A dictionary's two files in the first of `dirs` that has them, as a folder of its own (`pt_PT/index.aff`) or
/// side by side with the others, as the system keeps them (`pt_PT.aff`).
fn find(name: &str, dirs: &[PathBuf]) -> Option<(PathBuf, PathBuf)> {
    dirs.iter().find_map(|dir| {
        [
            (dir.join(name).join("index.aff"), dir.join(name).join("index.dic")),
            (dir.join(format!("{name}.aff")), dir.join(format!("{name}.dic"))),
        ]
        .into_iter()
        .find(|(aff, dic)| aff.is_file() && dic.is_file())
    })
}

fn read(name: &str, dirs: &[PathBuf]) -> Result<Dictionary, String> {
    let (aff, dic) = match find(name, dirs) {
        Some((aff, dic)) => (std::fs::read_to_string(aff).map_err(|e| e.to_string())?, std::fs::read_to_string(dic).map_err(|e| e.to_string())?),
        None => embedded(name).map(|(a, d)| (a.to_string(), d.to_string())).ok_or("not found")?,
    };
    // spellbook is young: a dictionary it can't make sense of is left out rather than taking the thread with it.
    std::panic::catch_unwind(|| Dictionary::new(&aff, &dic))
        .map_err(|_| "panicked".to_string())?
        .map_err(|e| e.to_string())
}

/// Text for comparing without accents or case, as wassup's fold (text.rs has the same): "Ação" and "acao" are the
/// same.
fn fold(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfd().filter(|c| !unicode_normalization::char::is_combining_mark(*c)).flat_map(char::to_lowercase).collect()
}

/// Edit distance where two neighbouring letters swapped count as one edit ("recieve" is one from "receive").
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    // Each row starts as the first row and column have it; the rest is worked out below.
    let mut d: Vec<Vec<usize>> = (0..=a.len()).map(|i| (i..=i + b.len()).collect()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]));
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// Words taken as right whatever the dictionaries say, in lower case, as wassup's ownWords: those I wrote myself
/// more than once in `my_texts` (slang, abbreviations, names: "bjs", "tou"), and the words of contacts' and chats'
/// `names`.
pub fn own_words<'a>(
    my_texts: impl IntoIterator<Item = &'a str>,
    names: impl IntoIterator<Item = &'a str>,
) -> HashSet<String> {
    let mut count: HashMap<String, usize> = HashMap::new();
    for text in my_texts {
        for w in WORD.find_iter(text) {
            *count.entry(w.as_str().to_lowercase()).or_default() += 1;
        }
    }
    let mut own: HashSet<String> = count.into_iter().filter(|(_, n)| *n > 1).map(|(w, _)| w).collect();
    for name in names {
        own.extend(WORD.find_iter(name).map(|w| w.as_str().to_lowercase()));
    }
    own
}

/// The dictionaries, read, with the user's language first: its suggestions win ties.
pub struct Speller {
    dicts: Vec<Dictionary>,
    /// Each unknown word's best suggestion, as finding them is the slow part (tens of milliseconds a word).
    best: Mutex<HashMap<String, Option<String>>>,
}

impl Speller {
    /// Reads the dictionaries found in `dirs` (see `dictionary_dirs`), here and now; `user_lang` is the user's
    /// language or locale ("pt", "pt_PT.UTF-8", "en"), Portuguese first when it starts with "pt", English first
    /// otherwise. Those missing or unreadable are left out, with a warning in the log.
    pub fn load(dirs: &[PathBuf], user_lang: &str) -> Speller {
        let t = Instant::now();
        let mut names = DICTIONARIES.to_vec();
        if !user_lang.to_lowercase().starts_with("pt") {
            names.rotate_left(1);
        }
        let dicts = names
            .into_iter()
            .filter_map(|name| read(name, dirs).inspect_err(|e| log::warn!("spell: dictionary {name}: {e}")).ok())
            .collect::<Vec<_>>();
        log::info!("spell: {} dictionaries read in {} ms", dicts.len(), t.elapsed().as_millis());
        Speller { dicts, best: Mutex::new(HashMap::new()) }
    }

    fn ok(&self, word: &str) -> bool {
        self.dicts.iter().any(|d| d.check(word))
    }

    /// The suggestion of all the dictionaries for a word they don't know, if any: the one equal but for accents
    /// first ("nao" → "não"), then the closest (distance, accents aside), then the dictionaries' own order.
    fn suggestion(&self, word: &str) -> Option<String> {
        if let Some(known) = self.best.lock().unwrap().get(word) {
            return known.clone();
        }
        let f = fold(word);
        let (mut all, mut each) = (Vec::new(), Vec::new());
        for d in &self.dicts {
            // spellbook empties the list it's given first.
            d.suggest(word, &mut each);
            all.append(&mut each);
        }
        all.retain(|s| !s.is_empty() && s != word);
        // A stable sort, so the dictionaries' order stays among equals.
        all.sort_by_cached_key(|s| {
            let g = fold(s);
            (g != f, distance(&f, &g))
        });
        let out = all.into_iter().next();
        let mut best = self.best.lock().unwrap();
        if best.len() > 5000 {
            best.clear();
        }
        best.insert(word.to_string(), out.clone());
        out
    }

    /// What a word unknown to every dictionary should be, or None to leave it be, as wassup's correction: the best
    /// suggestion (`suggestion`), but a word of mine (in `own`) is left be unless an accent is all it lacks, and so is
    /// one that differs only in capitals ("ok" for "OK"). A capital at the start stays.
    fn correction(&self, word: &str, own: &HashSet<String>) -> Option<String> {
        let out = self.suggestion(word)?;
        let lower = word.to_lowercase();
        if (fold(&out) != fold(word) && own.contains(&lower)) || out.to_lowercase() == lower {
            return None;
        }
        let mut chars = out.chars();
        match (word.chars().next(), chars.next()) {
            (Some(w), Some(o)) if w.is_uppercase() && o.is_lowercase() => Some(o.to_uppercase().chain(chars).collect()),
            _ => Some(out),
        }
    }

    /// The finished words of `text` no dictionary knows, each with its correction, in the text's order, up to eight,
    /// as wassup's spellFixes; `own` are the words taken as right (`own_words`). Left alone: the word still being
    /// typed at the end, links and addresses, @mentions, words with digits, all-capital ones (acronyms), single
    /// letters, and a hyphenated word whose every part is known ("diz-me").
    pub fn fixes(&self, text: &str, own: &HashSet<String>) -> Vec<Fix> {
        if self.dicts.is_empty() {
            return Vec::new();
        }
        let skip: Vec<(usize, usize)> =
            URL.find_iter(text).chain(ADDRESS.find_iter(text)).map(|m| (m.start(), m.end())).collect();
        let mut fixes = Vec::new();
        for m in WORD.find_iter(text) {
            if fixes.len() >= 8 {
                break;
            }
            let word = m.as_str().trim_end_matches(['\'', '’', '-']);
            let (start, end) = (m.start(), m.start() + word.len());
            if end == text.len() || word.chars().count() < 2 || word.to_uppercase() == word {
                continue;
            }
            let digit_beside = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
            if digit_beside(text[..start].chars().next_back()) || digit_beside(text[end..].chars().next()) {
                continue;
            }
            if skip.iter().any(|&(a, b)| start < b && a < end) {
                continue;
            }
            if self.ok(word) || (word.contains('-') && word.split('-').all(|p| p.is_empty() || self.ok(p))) {
                continue;
            }
            if let Some(to) = self.correction(word, own) {
                let at = text[..start].chars().count();
                fixes.push(Fix { from: word.to_string(), to, start: at, end: at + word.chars().count() });
            }
        }
        fixes
    }
}

/// Starts reading the dictionaries on a thread of their own, as `Speller::load` does, since it takes a while: what
/// it returns has nothing in it (`get`) until they're in, and `wait` waits for them.
pub fn load_in_background(dirs: Vec<PathBuf>, user_lang: String) -> Arc<OnceLock<Speller>> {
    let speller = Arc::new(OnceLock::new());
    let filled = speller.clone();
    std::thread::spawn(move || {
        filled.get_or_init(|| Speller::load(&dirs, &user_lang));
    });
    speller
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speller() -> &'static Speller {
        static SPELLER: LazyLock<Arc<OnceLock<Speller>>> =
            LazyLock::new(|| load_in_background(dictionary_dirs(None), "pt".into()));
        let s = SPELLER.wait();
        assert_eq!(s.dicts.len(), 3, "the three dictionaries are in dictionaries/");
        s
    }

    fn fixed(text: &str, own: &[&str]) -> Vec<(String, String)> {
        let own = own.iter().map(|w| w.to_string()).collect();
        speller().fixes(text, &own).into_iter().map(|f| (f.from, f.to)).collect()
    }

    #[test]
    fn misspelt_portuguese_word_is_fixed() {
        let fixes = speller().fixes("Ontem não consegiste vir à festa?", &HashSet::new());
        assert_eq!(fixes, [Fix { from: "consegiste".into(), to: "conseguiste".into(), start: 10, end: 20 }]);
        // "quizzer" is as close, but Portuguese comes first.
        assert_eq!(
            fixed("Se quizer, deicha ficar.", &[]),
            [("quizer".into(), "quiser".into()), ("deicha".into(), "deixa".into())]
        );
    }

    #[test]
    fn missing_accent_is_fixed() {
        assert_eq!(
            fixed("Nao sei onde pus o telemovel hoje.", &[]),
            [("Nao".into(), "Não".into()), ("telemovel".into(), "telemóvel".into())]
        );
    }

    #[test]
    fn missing_accent_is_fixed_even_for_own_word() {
        assert_eq!(fixed("nao vou hoje", &["nao"]), [("nao".into(), "não".into())]);
    }

    #[test]
    fn own_words_and_names_left_be() {
        let own = own_words(["bjs até logo", "bjs grandes"], ["Rita Lemos", "Clube de Leitura"]);
        assert!(own.contains("bjs") && own.contains("lemos") && !own.contains("logo"));
        let fixes = speller().fixes("Tou com a Ritinha, bjs e até amanhã.", &own);
        assert!(fixes.iter().all(|f| f.from != "bjs"), "{fixes:?}");
        assert!(fixed("Mando bjs à tarde", &[]).iter().any(|(from, _)| from == "bjs"));
    }

    #[test]
    fn english_in_portuguese_is_fine() {
        assert_eq!(fixed("Vamos ao meeting amanhã, tenho a colour palette pronta.", &[]), []);
        assert_eq!(fixed("Vi a série, the ending was beautiful e organise-se.", &[]), []);
        assert_eq!(fixed("Recebi o tomorow de ontem.", &[]), [("tomorow".into(), "tomorrow".into())]);
    }

    #[test]
    fn links_addresses_and_the_last_word_ignored() {
        assert_eq!(fixed("Olha https://exemplo.pt/pagina-qualqer?ref=abcdef isto", &[]), []);
        assert_eq!(fixed("Escreve para rita.lemos@exemplo.pt ou @123456789012345 já", &[]), []);
        assert_eq!(fixed("Vemo-nos às 8 para jantar, ok?", &[]), []);
        assert_eq!(fixed("Estou a escrever presisamos", &[]), []);
        assert_eq!(fixed("Comprei 3kgs de UVAS e um x2, diz-me já", &[]), []);
    }

    #[test]
    fn distance_counts_a_swap_as_one() {
        assert_eq!(distance("recieve", "receive"), 1);
        assert_eq!(distance("nao", "não"), 1);
        assert_eq!(distance("", "abc"), 3);
    }

    #[test]
    fn english_first_for_english_users() {
        let s = Speller::load(&dictionary_dirs(None), "en_GB.UTF-8");
        assert_eq!(s.dicts.len(), 3);
        assert!(s.ok("colour") && s.ok("não"));
        assert_eq!(s.suggestion("quizer").as_deref(), Some("quizzer"));
    }
}
