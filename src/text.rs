//! Colours, times and line wrapping, as wassup has them (its format.ts).

use std::sync::OnceLock;

use chrono::{DateTime, Datelike, Local, TimeZone};
use ratatui::style::Color;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// The terminal's colours as wassup uses them (its format.ts setTheme and ui.ts theme): the names' palette, the
/// greys for secondary text, the bubbles (WhatsApp Web's, in 24 bits where the terminal takes them, else the 256
/// palette's greys), the selected message's grey and WhatsApp's green, all from whether its background is dark.
#[derive(Clone, Debug)]
pub struct Theme {
    pub dark: bool,
    pub truecolor: bool,
    pub palette: [u8; 11],
    pub dim: Color,
    pub faint: Color,
    pub link: Color,
    pub mine: Color,
    pub theirs: Color,
    pub selected: Color,
    pub green: Color,
    /// The terminal's own background and text colours, for blending (the notices, the rising emoji).
    pub bg: [u8; 3],
    pub fg: [u8; 3],
}

impl Theme {
    /// From the terminal's background (OSC 11) and text colour (OSC 10) when it told them, and whether it takes
    /// 24-bit colour; dark is assumed when it didn't say.
    pub fn new(bg: Option<[u8; 3]>, fg: Option<[u8; 3]>, truecolor: bool) -> Self {
        let luma = |c: [u8; 3]| 0.299 * c[0] as f64 + 0.587 * c[1] as f64 + 0.114 * c[2] as f64;
        let dark = bg.is_none_or(|c| luma(c) < 128.0);
        // The selection's grey, moved from the background's luminance towards the light side on a dark theme and the
        // dark side on a light one; medium without an answer.
        let gray = |l: f64| 232 + ((l - 8.0) / 10.0).round().clamp(0.0, 23.0) as u8;
        let selected = match bg {
            Some(c) => gray(luma(c) + if dark { 48.0 } else { -48.0 }),
            None => 240,
        };
        let off = |n: u8| if n == selected { n - 2 } else { n };
        let (mine, theirs) = if truecolor {
            // WhatsApp Web's: on a dark theme mine toned down to the luminance of theirs.
            let lum = |c: [f64; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            let theirs: [f64; 3] = if dark { [32.0, 44.0, 51.0] } else { [255.0, 255.0, 255.0] };
            let green: [f64; 3] = if dark { [0.0, 92.0, 75.0] } else { [217.0, 253.0, 211.0] };
            let k = if dark { (lum(theirs) / lum(green)).min(1.0) } else { 1.0 };
            let m = green.map(|v| (v * k).round() as u8);
            (Color::Rgb(m[0], m[1], m[2]), Color::Rgb(theirs[0] as u8, theirs[1] as u8, theirs[2] as u8))
        } else if dark {
            (Color::Indexed(off(235)), Color::Indexed(off(236)))
        } else {
            (Color::Indexed(off(255)), Color::Indexed(off(254)))
        };
        Self {
            dark,
            truecolor,
            palette: if dark { [81, 213, 221, 120, 210, 111, 179, 151, 177, 216, 73] } else { [31, 127, 130, 28, 160, 25, 94, 65, 91, 166, 30] },
            dim: Color::Indexed(if dark { 247 } else { 242 }),
            faint: Color::Indexed(if dark { 241 } else { 248 }),
            link: Color::Indexed(if dark { 75 } else { 26 }),
            mine,
            theirs,
            selected: Color::Indexed(selected),
            green: Color::Indexed(if dark { 41 } else { 29 }),
            bg: bg.unwrap_or(if dark { [0, 0, 0] } else { [255, 255, 255] }),
            fg: fg.unwrap_or(if dark { [192, 192, 192] } else { [48, 48, 48] }),
        }
    }
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// The theme, once the terminal was asked; dark until then.
pub fn theme() -> &'static Theme {
    THEME.get_or_init(|| Theme::new(None, None, false))
}

pub fn set_theme(t: Theme) {
    let _ = THEME.set(t);
}

/// A chat's or a person's colour: the same for the same key, as wassup's colorFor.
pub fn color_for(key: &str) -> Color {
    let mut h: u32 = 0;
    for ch in key.chars() {
        h = h.wrapping_mul(31).wrapping_add(ch as u32);
    }
    let p = theme().palette;
    Color::Indexed(p[(h % p.len() as u32) as usize])
}

fn local(ts: i64) -> DateTime<Local> {
    Local.timestamp_opt(ts, 0).single().unwrap_or_else(Local::now)
}

pub fn fmt_time(ts: i64) -> String {
    local(ts).format("%H:%M").to_string()
}

pub fn day_key(ts: i64) -> i32 {
    let d = local(ts);
    d.year() * 1000 + d.ordinal() as i32
}

/// Someone's number from their id, for when there's no name: "+351 912 345 678" for a phone, the id's digits
/// otherwise (a group's, or a hidden number's, "@lid").
pub fn number_of(jid: &str) -> String {
    let user = jid.split(['@', ':']).next().unwrap_or(jid);
    if jid.ends_with("@s.whatsapp.net") && user.len() > 9 {
        let (cc, rest) = user.split_at(user.len() - 9);
        let rest: Vec<String> = rest.as_bytes().chunks(3).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
        format!("+{cc} {}", rest.join(" "))
    } else {
        user.to_string()
    }
}

pub fn is_group(jid: &str) -> bool {
    jid.ends_with("@g.us")
}

/// Two initials from a name's words, "de", "da"… left out, as wassup's; "#" without any.
pub fn initials(name: &str) -> String {
    let skip = ["de", "da", "do", "das", "dos", "e"];
    let words: Vec<&str> = name
        .split_whitespace()
        .filter(|w| w.chars().next().is_some_and(|c| c.is_alphabetic()) && !skip.contains(&w.to_lowercase().as_str()))
        .collect();
    if words.is_empty() {
        return "#".into();
    }
    words.iter().take(2).filter_map(|w| w.chars().next()).flat_map(char::to_uppercase).collect()
}

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// `s` cut to `max` columns, with "…" where it was cut.
pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for g in s.graphemes(true) {
        if width(&out) + width(g) + 1 > max {
            break;
        }
        out.push_str(g);
    }
    out + "…"
}

/// Text wrapped to `max` columns at the spaces, a word longer than a line cut where it must; the text's own line
/// breaks kept.
pub fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_inclusive(' ') {
            if width(&line) + width(word.trim_end()) <= max {
                line.push_str(word);
                continue;
            }
            if !line.is_empty() {
                lines.push(line.trim_end().to_string());
                line = String::new();
            }
            for g in word.graphemes(true) {
                if width(&line) + width(g) > max && !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                line.push_str(g);
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines
}

/// My message's time with its state in the separator, as wassup's myTime: ":" delivered or read, "." at the
/// server, a space on its way.
pub fn my_time(ts: i64, status: i32) -> String {
    let sep = if status >= 3 { ":" } else if status >= 2 { "." } else { " " };
    fmt_time(ts).replace(':', sep)
}

/// Text for comparing without accents or case, as wassup's fold: "Ação" and "acao" are the same.
pub fn fold(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfd().filter(|c| !unicode_normalization::char::is_combining_mark(*c)).flat_map(char::to_lowercase).collect()
}

/// A character folded as `fold` folds it, kept to one character so places in the text still line up.
fn fold_char(c: char) -> char {
    fold(&c.to_string()).chars().next().unwrap_or(c)
}

/// Where the words searched for are in `line` (character ranges), accents and case aside.
pub fn matches(line: &str, words: &[String]) -> Vec<(usize, usize)> {
    let folded: Vec<char> = line.chars().map(fold_char).collect();
    let mut out = Vec::new();
    for w in words {
        let w: Vec<char> = w.chars().collect();
        if w.is_empty() || w.len() > folded.len() {
            continue;
        }
        for i in 0..=folded.len() - w.len() {
            if folded[i..i + w.len()] == w[..] {
                out.push((i, i + w.len()));
            }
        }
    }
    out.sort();
    out
}

/// Mentions by number ("@351900000001", or a hidden id's digits) with the person's first name in their place when
/// `who` knows them, as wassup's withMentions: "@Ana". Returns the text and each name put in, with whose it is.
pub fn with_mentions(text: &str, who: impl Fn(&str) -> Option<(String, String)>) -> (String, Vec<(String, String)>) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut names = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let free = i == 0 || !(chars[i - 1].is_alphanumeric() || "._%+-".contains(chars[i - 1]));
        if c == '@' && free {
            let digits: String = chars[i + 1..].iter().take_while(|d| d.is_ascii_digit()).collect();
            if digits.len() >= 6
                && let Some((jid, name)) = who(&digits) {
                    let first = format!("@{}", name.split_whitespace().next().unwrap_or(&name));
                    out.push_str(&first);
                    names.push((first, jid));
                    i += 1 + digits.len();
                    continue;
                }
        }
        out.push(c);
        i += 1;
    }
    (out, names)
}
