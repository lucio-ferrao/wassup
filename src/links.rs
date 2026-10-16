//! Links in messages, as wassup has them (its links.ts): cleaned of what only tracks who shared them and from where,
//! with ClearURLs' rules (`clearurls/data.min.json`, from github.com/ClearURLs/Rules, LGPL-3.0, see
//! `clearurls/LICENSE`, built into the binary), and shortened for showing. The whole clean link is what a click
//! copies. The rules are JavaScript regular expressions; one the regex crate can't take is left out, as wassup leaves
//! out one that doesn't compile.

use std::collections::HashMap;
use std::fmt;
#[cfg(test)]
use std::ops::Range;
use std::sync::{LazyLock, Mutex, OnceLock};

use regex::{Captures, Regex, RegexBuilder};
use serde::Deserialize;
use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};

/// A link in a text: "http://" or "https://" up to a space, "<", ">", a quote or a closing bracket. Spaces are
/// JavaScript's, which count the byte order mark.
pub static URL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s\u{FEFF}<>"')\]]+"#).unwrap());

/// One of ClearURLs' providers as its file has it, or one of `extra`'s.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RuleSet {
    url_pattern: String,
    complete_provider: bool,
    rules: Vec<String>,
    referral_marketing: Vec<String>,
    raw_rules: Vec<String>,
    exceptions: Vec<String>,
    redirections: Vec<String>,
}

/// What ClearURLs doesn't cover yet: Instagram's share token, and X under its new domain as under twitter.com.
fn extra() -> [RuleSet; 2] {
    let strings = |list: &[&str]| list.iter().map(|s| s.to_string()).collect();
    [
        RuleSet {
            url_pattern: r"^https?:\/\/(?:[a-z0-9-]+\.)*?instagram\.com".into(),
            rules: strings(&["stkn"]),
            ..Default::default()
        },
        RuleSet {
            url_pattern: r"^https?:\/\/(?:[a-z0-9-]+\.)*?x\.com".into(),
            rules: strings(&["(?:ref_?)?src", "s", "cn", "ref_url", "t"]),
            ..Default::default()
        },
    ]
}

#[derive(Deserialize)]
struct Data {
    providers: Providers,
}

/// ClearURLs' providers in the order of the file, which is the order they're applied in (a map would sort them).
struct Providers(Vec<RuleSet>);

impl<'de> Deserialize<'de> for Providers {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct InOrder;
        impl<'de> Visitor<'de> for InOrder {
            type Value = Providers;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a map of providers")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Providers, A::Error> {
                let mut sets = Vec::new();
                while let Some((_, set)) = map.next_entry::<IgnoredAny, RuleSet>()? {
                    sets.push(set);
                }
                Ok(Providers(sets))
            }
        }
        d.deserialize_map(InOrder)
    }
}

/// A rule set made into regular expressions, all of them case-insensitive.
struct Provider {
    pattern: Regex,
    exceptions: Vec<Regex>,
    redirections: Vec<Regex>,
    raw: Vec<Regex>,
    params: Vec<Regex>,
}

fn re(s: &str) -> Option<Regex> {
    RegexBuilder::new(s).case_insensitive(true).build().ok()
}

impl Provider {
    /// The rule set compiled; a pattern that doesn't compile is left out rather than failing all, and without its
    /// URL pattern there's no provider.
    fn compile(r: &RuleSet) -> Option<Provider> {
        let all = |list: &[String]| list.iter().filter_map(|s| re(s)).collect();
        Some(Provider {
            pattern: re(&r.url_pattern)?,
            exceptions: all(&r.exceptions),
            redirections: all(&r.redirections),
            raw: all(&r.raw_rules),
            // As the extension does, referral marketing (affiliate tags) goes too.
            params: r.rules.iter().chain(&r.referral_marketing).filter_map(|s| re(&format!("^{s}$"))).collect(),
        })
    }
}

/// The providers, compiled the first time a link is cleaned.
fn providers() -> &'static [Provider] {
    static PROVIDERS: OnceLock<Vec<Provider>> = OnceLock::new();
    PROVIDERS.get_or_init(|| {
        let sets = serde_json::from_str::<Data>(include_str!("../clearurls/data.min.json"))
            .map(|d| d.providers.0)
            .unwrap_or_default();
        // A "complete provider" is a whole site of tracking, which the extension blocks: for showing a link it
        // changes nothing.
        sets.iter().filter(|r| !r.complete_provider).chain(&extra()).filter_map(Provider::compile).collect()
    })
}

/// A text with its %XX escapes decoded, as JavaScript's decodeURIComponent: None when one is malformed or what they
/// make isn't UTF-8.
fn decode_uri_component(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let hex = s.get(i + 1..i + 3).filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()))?;
        out.push(u8::from_str_radix(hex, 16).ok()?);
        i += 3;
    }
    String::from_utf8(out).ok()
}

/// The query (or a fragment written as one) without the parameters whose names a rule matches; the rest as written.
fn drop_params(url: &str, rules: &[Regex]) -> String {
    let h = url.find('#');
    let q = url.find('?').filter(|&q| h.is_none_or(|h| q < h));
    let base = &url[..q.or(h).unwrap_or(url.len())];
    let query = q.map_or("", |q| &url[q + 1..h.unwrap_or(url.len())]);
    let hash = h.map_or("", |h| &url[h + 1..]);
    let keep = |part: &str| {
        let kept: Vec<&str> = part
            .split('&')
            .filter(|kv| !kv.is_empty())
            .filter(|kv| {
                let key = kv.split('=').next().unwrap_or(kv);
                let key = decode_uri_component(key).unwrap_or_else(|| key.to_string());
                !rules.iter().any(|r| r.is_match(&key))
            })
            .collect();
        kept.join("&")
    };
    let nq = keep(query);
    let nh = if hash.contains('=') { keep(hash) } else { hash.to_string() };
    let mut out = base.to_string();
    if !nq.is_empty() {
        out.push('?');
        out.push_str(&nq);
    }
    if !nh.is_empty() {
        out.push('#');
        out.push_str(&nh);
    }
    out
}

fn is_http(s: &str) -> bool {
    ["http://", "https://"].iter().any(|p| s.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p)))
}

/// A link without its tracking: for each ClearURLs provider whose pattern takes it (and no exception spares it), a
/// redirection through the site is undone to the link it leads to (itself cleaned), its raw rules are cut out of the
/// whole link, and the parameters named by its rules are dropped. Remembered, as messages are drawn over and over.
pub fn clean_url(url: &str) -> String {
    clean(url, 0)
}

/// `clean_url`, `depth` redirections in: past three, they're no longer followed.
fn clean(url: &str, depth: u32) -> String {
    static CLEANED: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cleaned = CLEANED.get_or_init(Default::default);
    if let Some(known) = cleaned.lock().unwrap().get(url) {
        return known.clone();
    }
    let mut out = url.to_string();
    for p in providers() {
        if !p.pattern.is_match(&out) || p.exceptions.iter().any(|e| e.is_match(&out)) {
            continue;
        }
        for r in p.redirections.iter().filter(|_| depth < 3) {
            let Some(to) = r.captures(&out).and_then(|c| c.get(1)).map(|m| m.as_str()).filter(|to| !to.is_empty())
            else {
                continue;
            };
            let target = decode_uri_component(to).unwrap_or_else(|| to.to_string());
            if is_http(&target) {
                out = clean(&target, depth + 1);
                break;
            }
        }
        for r in &p.raw {
            out = r.replace_all(&out, "").into_owned();
        }
        out = drop_params(&out, &p.params);
    }
    let mut cleaned = cleaned.lock().unwrap();
    if cleaned.len() > 5000 {
        cleaned.clear();
    }
    cleaned.insert(url.to_string(), out.clone());
    out
}

/// Longest a link is shown, in characters, before its path is cut in the middle (or, failing that, its end).
const SHOWN_MAX: usize = 60;

/// A link's domain (without "www."), path and what follows (query and fragment).
static PARTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^https?://(?:www\.)?([^/?#]+)([^?#]*)(.*)$").unwrap());

/// Social networks and the like, and short links, whose paths are mostly identifiers that say nothing to whoever
/// reads them (`instagram.com/p/C0aBcD3eFgH`, `youtu.be/Xy7aB3cD9eF`, `maps.app.goo.gl/AbC123dEf456GhI78`).
static NOISY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(?:^|\.)(?:facebook\.com|fb\.com|fb\.watch|instagram\.com|threads\.(?:net|com)|tiktok\.com|x\.com|",
        r"twitter\.com|youtube\.com|youtu\.be|linkedin\.com|spotify\.com|spotify\.link|pinterest\.[a-z.]+|pin\.it|",
        r"reddit\.com|redd\.it|bsky\.app|snapchat\.com|chat\.whatsapp\.com|(?:maps|photos)\.app\.goo\.gl|forms\.gle|",
        r"share\.google)$",
    ))
    .unwrap()
});

/// Of those, the short links, whose path is only ever an identifier.
static ID_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(?:^|\.)(?:youtu\.be|vm\.tiktok\.com|vt\.tiktok\.com|spotify\.link|pin\.it|redd\.it|fb\.watch|",
        r"(?:maps|photos)\.app\.goo\.gl|forms\.gle)$",
    ))
    .unwrap()
});

/// How many times a text switches between upper and lower case letters.
fn case_switches(s: &str) -> usize {
    let upper: Vec<bool> = s.chars().filter(char::is_ascii_alphabetic).map(|c| c.is_ascii_uppercase()).collect();
    upper.windows(2).filter(|w| w[0] != w[1]).count()
}

/// An opaque identifier, as a part of a path: five or more letters, digits, "_" or "-", all digits, or digits with
/// capitals, or digits in two or more places among letters (`4kq81zr7tp`), or capitals and small letters switching
/// three or more times. Names, even with a digit (`zeca_rio7`), aren't. On the short links (ID_ONLY) any such part is.
fn opaque(part: &str, id_only: bool) -> bool {
    let b = part.as_bytes();
    if b.len() < 5 || !b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-') {
        return false;
    }
    let digits = b.iter().filter(|c| c.is_ascii_digit()).count();
    let digit_runs =
        b.chunk_by(|x, y| x.is_ascii_digit() == y.is_ascii_digit()).filter(|r| r[0].is_ascii_digit()).count();
    id_only
        || digits == b.len()
        || (digits > 0 && b.iter().any(u8::is_ascii_uppercase))
        || digit_runs >= 2
        || case_switches(part) >= 3
}

/// `s` cut to SHOWN_MAX characters, with "…" where it was cut.
fn cut(s: String) -> String {
    if s.chars().count() <= SHOWN_MAX {
        return s;
    }
    s.chars().take(SHOWN_MAX - 1).chain(['…']).collect()
}

/// A clean link as shown: without "https://", "www." or a final "/"; when that's still long, the path's middle gives
/// way to "…" (the domain stays whole, and the first and last parts of the path, then the query), and past that its
/// end. The parameters that are left after cleaning (a search, an id, a video's start) stay. From the sites in NOISY
/// only the domain and the readable parts of the path are shown, each run of identifiers made one "…", without the
/// parameters: `facebook.com/share/v/…`, `instagram.com/stories/rita.lemos/…`, `youtube.com/watch`.
pub fn short_url(url: &str) -> String {
    let Some(m) = PARTS.captures(url) else {
        return url.to_string();
    };
    let (_, [host, path, tail]) = m.extract();
    let path = path.strip_suffix('/').unwrap_or(path);
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if NOISY.is_match(host) {
        let id_only = ID_ONLY.is_match(host);
        let mut shown: Vec<&str> = Vec::new();
        for &part in &parts {
            if !opaque(part, id_only) {
                shown.push(part);
            } else if shown.last() != Some(&"…") {
                shown.push("…");
            }
        }
        return cut(std::iter::once(host).chain(shown).collect::<Vec<_>>().join("/"));
    }
    let shown = format!("{host}{path}{tail}");
    if shown.chars().count() <= SHOWN_MAX {
        return shown;
    }
    match parts[..] {
        [first, _, .., last] => cut(format!("{host}/{first}/…/{last}{tail}")),
        _ => cut(shown),
    }
}

/// A link found in a text.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Where it is in the text as written, in characters.
    pub at: Range<usize>,
    /// Where it is in the text with its links shown (what `show_links` makes of it), in characters.
    pub shown_at: Range<usize>,
    /// The link as it's shown: clean and short.
    pub shown: String,
    /// The whole clean link, which a click copies.
    pub url: String,
}

/// The links in a text, in order: each with where it is (as written, and once shown), how it's shown and the whole
/// clean link. For underlining them, and for knowing which one a click fell on even when two are shown the same.
#[cfg(test)]
pub fn links_in(text: &str) -> Vec<Link> {
    let mut links = Vec::new();
    let (mut byte, mut at, mut shown_at) = (0, 0, 0);
    for m in URL_RE.find_iter(text) {
        let gap = text[byte..m.start()].chars().count();
        at += gap;
        shown_at += gap;
        let len = m.as_str().chars().count();
        let url = clean_url(m.as_str());
        let shown = short_url(&url);
        let shown_len = shown.chars().count();
        links.push(Link { at: at..at + len, shown_at: shown_at..shown_at + shown_len, shown, url });
        at += len;
        shown_at += shown_len;
        byte = m.end();
    }
    links
}

/// A text with its links as they're shown (clean and short).
pub fn show_links(text: &str) -> String {
    URL_RE.replace_all(text, |c: &Captures| short_url(&clean_url(&c[0]))).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every provider in the file is read, in order, and the regex crate takes all their URL patterns.
    #[test]
    fn rules_load() {
        let data: Data = serde_json::from_str(include_str!("../clearurls/data.min.json")).unwrap();
        let sets: Vec<&RuleSet> = data.providers.0.iter().filter(|r| !r.complete_provider).collect();
        assert!(sets.len() > 150);
        assert_eq!(sets[0].url_pattern, r"^https?:\/\/(?:[a-z0-9-]+\.)*?amazon(?:\.[a-z]{2,}){1,}");
        assert_eq!(providers().len(), sets.len() + extra().len());
    }

    /// A tracking parameter goes, wherever it is; the rest of the query and the fragment stay as written.
    #[test]
    fn tracking_removed() {
        assert_eq!(
            clean_url("https://example.com/artigo?utm_source=newsletter&id=42#topo"),
            "https://example.com/artigo?id=42#topo"
        );
        assert_eq!(clean_url("https://example.com/a?fbclid=IwAR0aBcD&utm_medium=social"), "https://example.com/a");
        assert_eq!(
            clean_url("https://www.google.com/search?q=livros&ved=0ahUKEwi"),
            "https://www.google.com/search?q=livros"
        );
        assert_eq!(clean_url("https://example.com/#utm_campaign=x&secao=2"), "https://example.com/#secao=2");
        assert_eq!(clean_url("https://example.com/b?%75tm_source=x&p=1"), "https://example.com/b?p=1");
        assert_eq!(clean_url("https://example.com/c?nada=1"), "https://example.com/c?nada=1");
    }

    /// An exception spares a link the rules of its provider.
    #[test]
    fn exception_kept() {
        let docs = "https://docs.google.com/document/d/1aBcD3eFgH/edit?ved=xyz&usp=sharing";
        assert_eq!(clean_url(docs), docs);
        let gitlab = "https://gitlab.com/grupo/projeto?utm_source=x";
        assert_eq!(clean_url(gitlab), gitlab);
    }

    /// A redirection through the site is undone to where it leads, cleaned in turn.
    #[test]
    fn redirection_followed() {
        assert_eq!(
            clean_url("https://l.facebook.com/l.php?u=https%3A%2F%2Fexample.com%2Fartigo%3Ffbclid%3DIwAR0aBc&h=AT0xYz"),
            "https://example.com/artigo"
        );
        assert_eq!(
            clean_url(
                "https://www.google.com/url?sa=t&q=https%3A%2F%2Fexample.org%2Fpagina%3Futm_medium%3Demail&usg=AOv"
            ),
            "https://example.org/pagina"
        );
    }

    /// Instagram's share token and X's parameters, which ClearURLs doesn't know.
    #[test]
    fn extra_rules() {
        assert_eq!(
            clean_url("https://www.instagram.com/p/C0aBcD3eFgH/?igsh=MWZ0aBcD&stkn=AbC123"),
            "https://www.instagram.com/p/C0aBcD3eFgH/"
        );
        assert_eq!(
            clean_url("https://x.com/exemplo/status/1234567890123456789?s=46&t=AbCdEf"),
            "https://x.com/exemplo/status/1234567890123456789"
        );
        assert_eq!(clean_url("https://x.com/exemplo?ref_src=twsrc%5Etfw&lang=pt"), "https://x.com/exemplo?lang=pt");
    }

    /// Escapes decoded as decodeURIComponent decodes them, and refused where it would throw.
    #[test]
    fn decoding() {
        assert_eq!(decode_uri_component("a%2Fb%C3%A7").as_deref(), Some("a/bç"));
        assert_eq!(decode_uri_component("100%"), None);
        assert_eq!(decode_uri_component("%+1"), None);
        assert_eq!(decode_uri_component("%C3x"), None);
    }

    /// Scheme, "www." and a final "/" go; a long path loses its middle, then its end.
    #[test]
    fn shortening() {
        assert_eq!(short_url("https://www.example.com/"), "example.com");
        assert_eq!(short_url("http://example.com/livros/ficcao?pagina=2"), "example.com/livros/ficcao?pagina=2");
        assert_eq!(
            short_url("https://example.com/arquivo/2024/03/secao/um-titulo-comprido-demais?id=7"),
            "example.com/arquivo/…/um-titulo-comprido-demais?id=7"
        );
        let long = format!("https://example.com/{}", "a".repeat(80));
        let shown = short_url(&long);
        assert_eq!(shown.chars().count(), SHOWN_MAX);
        assert!(shown.ends_with("aaa…"));
        assert_eq!(short_url("mailto:rita@example.com"), "mailto:rita@example.com");
    }

    /// On social networks and short links the identifiers become "…" and the parameters go.
    #[test]
    fn shortening_noisy() {
        assert_eq!(short_url("https://www.instagram.com/p/C0aBcD3eFgH/"), "instagram.com/p/…");
        assert_eq!(
            short_url("https://www.instagram.com/stories/rita.lemos/3141592653589793238/"),
            "instagram.com/stories/rita.lemos/…"
        );
        assert_eq!(short_url("https://www.facebook.com/share/v/1aBcDeFgHi/"), "facebook.com/share/v/…");
        assert_eq!(short_url("https://www.youtube.com/watch?v=Xy7aB3cD9eF"), "youtube.com/watch");
        assert_eq!(short_url("https://youtu.be/Xy7aB3cD9eF?t=42"), "youtu.be/…");
        assert_eq!(short_url("https://maps.app.goo.gl/AbC123dEf456GhI78"), "maps.app.goo.gl/…");
        assert_eq!(short_url("https://x.com/zeca_rio7/status/1234567890123456789"), "x.com/zeca_rio7/status/…");
        assert_eq!(
            short_url("https://www.reddit.com/r/rust/comments/1a2b3c4/um_titulo/"),
            "reddit.com/r/rust/comments/…/um_titulo"
        );
    }

    /// Where the links are, as written and as shown, and what they're shown as and copy.
    #[test]
    fn links_found() {
        let text = "vê https://www.instagram.com/p/C0aBcD3eFgH/?igsh=MWZ0 e (https://example.com/a?utm_source=x).";
        let shown = show_links(text);
        assert_eq!(shown, "vê instagram.com/p/… e (example.com/a).");
        let links = links_in(text);
        let slice = |s: &str, r: &Range<usize>| s.chars().skip(r.start).take(r.len()).collect::<String>();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].at, 3..53);
        assert_eq!(slice(text, &links[0].at), "https://www.instagram.com/p/C0aBcD3eFgH/?igsh=MWZ0");
        assert_eq!(links[0].url, "https://www.instagram.com/p/C0aBcD3eFgH/");
        assert_eq!(links[1].at, 57..91);
        assert_eq!(slice(text, &links[1].at), "https://example.com/a?utm_source=x");
        assert_eq!(links[1].url, "https://example.com/a");
        for l in &links {
            assert_eq!(slice(&shown, &l.shown_at), l.shown);
        }
        assert_eq!(links[1].shown_at, 24..37);
        assert!(links_in("sem ligações").is_empty());
    }
}
