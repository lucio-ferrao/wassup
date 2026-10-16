//! What the Node version's format.ts does that text.rs doesn't, or does another way: WhatsApp's markup (*bold*, _italic_,
//! ~struck~, `code` and ```blocks```, "> " quotes, links) laid out in styled lines of a given width, with where
//! each link landed on them; when something was, as the chat list says it ("ter", "ontem 21:35", "12 set 21:35");
//! and a number without its country's code when that's mine too, whatever the country. What's shown is in
//! Portuguese or English, as the `Lang` given says; the colours are text.rs's `Theme`.
//!
//! The markup follows format.ts's waMarkup rule for rule. There, regular expressions go over the text one kind of
//! marker after the other, each replacing what it found with blessed's tags, which the expressions after it then
//! read as text: a marker opens at the start of a line or after a space, "(", "[" or ">", and closes at the end of
//! one or before a space or ".,!?;:)]", with something other than a space just inside it and no line break in
//! between; a tag beside a marker keeps it from opening or closing. Here the text is a row of characters in which
//! every marker found becomes a stand-in for its tag, so the next kind of marker meets the same neighbours it met
//! there: "*(_a_)*" is bold and italic, but "*_a_*" only bold, with its underscores, as in wassup. Two things differ
//! on purpose: nothing inside `code` is taken as a marker (format.ts, which reads the code's text like any other,
//! made "`a *b* c`" bold in the middle), and struck text is crossed out and loses its tildes, which blessed, unable
//! to cross out, kept.

use std::ops::Range;

use chrono::{DateTime, Datelike, Local, TimeZone};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use unicode_segmentation::UnicodeSegmentation;

use crate::i18n::Lang;
use crate::text::{self, Theme};

const MONTHS_PT: [&str; 12] = ["jan", "fev", "mar", "abr", "mai", "jun", "jul", "ago", "set", "out", "nov", "dez"];
const MONTHS_EN: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS_PT: [&str; 7] = ["dom", "seg", "ter", "qua", "qui", "sex", "sáb"];
const WEEKDAYS_EN: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// A string in `lang`, of the two given; i18n's strings are in the user's language only.
fn say(lang: Lang, pt: &'static str, en: &'static str) -> &'static str {
    match lang {
        Lang::Pt => pt,
        Lang::En => en,
    }
}

fn month(lang: Lang, month0: u32) -> &'static str {
    say(lang, MONTHS_PT[month0 as usize], MONTHS_EN[month0 as usize])
}

fn weekday(lang: Lang, from_sunday: u32) -> &'static str {
    say(lang, WEEKDAYS_PT[from_sunday as usize], WEEKDAYS_EN[from_sunday as usize])
}

/// A message's text as `wa_markup` lays it out: its lines, and where on each its links are.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marked {
    /// The lines, none wider than the width asked unless a single character is, in pieces of one style each.
    pub lines: Vec<Vec<Span<'static>>>,
    /// For each line, the links on it, left to right.
    pub links: Vec<Vec<LinkAt>>,
}

/// A link's place on a line: where its text as shown is, and the link it stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkAt {
    /// The columns (cells, not characters) its text takes, counted from the line's first.
    pub cols: Range<usize>,
    /// The link whole, as the message has it, for a click to copy (wassup copies it cleaned of its tracking).
    pub url: String,
}

/// A message's text with WhatsApp's markup made styles and wrapped to `width` columns, as wassup's waMarkup and
/// wrapTagged: *bold*, _italic_, ~struck~ (crossed out, in `dim`), `code` and ```blocks``` in yellow, nothing in
/// a block read as markup, a line starting with ">" a quote, after a bar and in `dim`, and links underlined in
/// `link`, shown as `short` gives them (wassup cleans them of tracking and shortens them). The markers go, the
/// text's own line breaks stay, and lines break at spaces, a word wider than a line filling the one it starts on
/// and going on in the next ones. With `usize::MAX` for `width`, only the text's own line breaks.
pub fn wa_markup(text: &str, width: usize, theme: &Theme, short: impl Fn(&str) -> String) -> Marked {
    let mut units: Vec<Unit> = text.chars().map(Unit::text).collect();
    mark_blocks(&mut units);
    mark_code(&mut units);
    let mut urls = Vec::new();
    let mut units = mark_links(units, &short, &mut urls);
    for (c, mark) in [('*', BOLD), ('_', ITALIC), ('~', STRIKE)] {
        for (open, close) in pairs(&units, c) {
            units[open].kind = Kind::Tag;
            units[close].kind = Kind::Tag;
            for u in &mut units[open + 1..close] {
                u.look.marks |= mark;
            }
        }
    }
    let mut out = Marked::default();
    for line in shown(&mut units).split(|&(ch, _)| ch == '\n') {
        lay_out(line, width.max(1), theme, &urls, &mut out);
    }
    out
}

const BOLD: u8 = 1;
const ITALIC: u8 = 2;
const STRIKE: u8 = 4;
const CODE: u8 = 8;
const QUOTE: u8 = 16;

/// How a character is shown: the marks around it, and the link it's of, by its place among the message's links.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Look {
    marks: u8,
    link: Option<usize>,
}

impl Look {
    /// The style for this look. The colour of what's innermost wins, as with blessed's tags: a link's over code's,
    /// code's over a quote's or a strike's grey.
    fn style(self, theme: &Theme) -> Style {
        let mut style = Style::new();
        if self.marks & (QUOTE | STRIKE) != 0 {
            style = style.fg(theme.dim);
        }
        if self.marks & CODE != 0 {
            style = style.fg(Color::Yellow);
        }
        if self.link.is_some() {
            style = style.fg(theme.link).add_modifier(Modifier::UNDERLINED);
        }
        for (mark, modifier) in [(BOLD, Modifier::BOLD), (ITALIC, Modifier::ITALIC), (STRIKE, Modifier::CROSSED_OUT)] {
            if self.marks & mark != 0 {
                style = style.add_modifier(modifier);
            }
        }
        style
    }
}

/// What a character of the text has become on the way through the rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Itself: shown, and read by the rules, which may take it for a marker.
    Text,
    /// Shown and read, but no marker: in code.
    Kept,
    /// The stand-in for one of format.ts's tags, where a marker or a fence was: not shown, read as a tag is.
    Tag,
    /// In a block: shown, but out of the rules' sight, as format.ts keeps blocks out of the text till the end.
    Block,
    /// The rest of a block's fences, or a quote's ">": neither shown nor read.
    Gone,
}

#[derive(Clone, Copy, Debug)]
struct Unit {
    ch: char,
    kind: Kind,
    look: Look,
}

impl Unit {
    fn text(ch: char) -> Unit {
        Unit { ch, kind: Kind::Text, look: Look::default() }
    }

    /// Whether it's `c` that can be a marker.
    fn is(&self, c: char) -> bool {
        self.kind == Kind::Text && self.ch == c
    }

    /// The character the rules read here; none for a tag, which they read as something neither a space, a marker
    /// nor any of the characters a marker opens after or closes before.
    fn read(&self) -> Option<char> {
        matches!(self.kind, Kind::Text | Kind::Kept).then_some(self.ch)
    }

    fn space(&self) -> bool {
        self.read().is_some_and(js_space)
    }

    /// Whether a line ends here, for `^` and `.` in format.ts's expressions.
    fn ends_line(&self) -> bool {
        self.read().is_some_and(|c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
    }
}

/// Whether a character is a space to JavaScript's `\s`.
fn js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

/// The places of the units the rules read, in order: the text as format.ts's expressions see it.
fn seen(units: &[Unit]) -> Vec<usize> {
    (0..units.len()).filter(|&i| matches!(units[i].kind, Kind::Text | Kind::Kept | Kind::Tag)).collect()
}

/// The ```blocks```, taken as format.ts takes them out of the text before anything else (```` ```([\s\S]*?)``` ````,
/// left to right): their text out of the rules' sight, to be shown in yellow, and the first character of their
/// opening fence left to the rules as the stand-in format.ts puts in their place.
fn mark_blocks(units: &mut [Unit]) {
    let fence = |units: &[Unit], i: usize| units.get(i..i + 3).is_some_and(|f| f.iter().all(|u| u.ch == '`'));
    let mut i = 0;
    while i < units.len() {
        if !fence(units, i) {
            i += 1;
            continue;
        }
        // With no fence after this one to close it, none after it has one either.
        let Some(j) = (i + 3..units.len()).find(|&j| fence(units, j)) else { break };
        units[i].kind = Kind::Tag;
        for k in (i + 1..i + 3).chain(j..j + 3) {
            units[k].kind = Kind::Gone;
        }
        for u in &mut units[i + 3..j] {
            u.kind = Kind::Block;
            u.look.marks |= CODE;
        }
        i = j + 3;
    }
}

/// `code`, as format.ts finds it once the blocks are out (`` `([^`\n]+)` ``): yellow, its backticks tags, and,
/// unlike there, its text no markers.
fn mark_code(units: &mut [Unit]) {
    let v = seen(units);
    let mut p = 0;
    while p < v.len() {
        if units[v[p]].is('`') {
            let end = (p + 1..v.len()).find(|&q| units[v[q]].is('`') || units[v[q]].read() == Some('\n'));
            if let Some(q) = end.filter(|&q| q > p + 1 && units[v[q]].is('`')) {
                units[v[p]].kind = Kind::Tag;
                units[v[q]].kind = Kind::Tag;
                for u in &mut units[v[p] + 1..v[q]] {
                    if u.kind == Kind::Text {
                        u.kind = Kind::Kept;
                    }
                    u.look.marks |= CODE;
                }
                p = q + 1;
                continue;
            }
        }
        p += 1;
    }
}

/// The links, as format.ts finds them once the code is marked (`https?://[^\s<>"')\]]+`): each put as `short`
/// shows it, between tags as there, its text then read by the markers' rules as there; `urls` gets each whole. A
/// link in code is one too, as there, but ends with the code, where format.ts's ran on into the tag that closes it.
fn mark_links(units: Vec<Unit>, short: &impl Fn(&str) -> String, urls: &mut Vec<String>) -> Vec<Unit> {
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let Some(end) = link_end(&units, i) else {
            out.push(units[i]);
            i += 1;
            continue;
        };
        let url: String = units[i..end].iter().map(|u| u.ch).collect();
        let tag = Unit { kind: Kind::Tag, ..units[i] };
        let look = Look { link: Some(urls.len()), ..units[i].look };
        out.push(tag);
        out.extend(short(&url).chars().map(|ch| Unit { ch, kind: units[i].kind, look }));
        out.push(tag);
        urls.push(url);
        i = end;
    }
    out
}

/// Where the link that starts at `i` ends, if one starts there.
fn link_end(units: &[Unit], i: usize) -> Option<usize> {
    let read = |j: usize| units.get(j).and_then(Unit::read);
    let head: String = (i..i + 8).map_while(read).collect();
    let scheme = if head.starts_with("https://") {
        8
    } else if head.starts_with("http://") {
        7
    } else {
        return None;
    };
    let in_link = |c: char| !js_space(c) && !"<>\"')]".contains(c);
    let end = (i + scheme..units.len()).find(|&j| !read(j).is_some_and(in_link)).unwrap_or(units.len());
    (end > i + scheme).then_some(end)
}

/// Where `c` opens and closes, as format.ts's expression for it finds them, left to right, each search going on
/// where the last match ended: `(^|[\s(\[{>])c(\S(?:[^c\n]*?\S)?)c(?=$|[\s.,!?;:)\]}])`. The braces in it never
/// meet a marker there, a brace of the text being escaped into a tag, so they're left out.
fn pairs(units: &[Unit], c: char) -> Vec<(usize, usize)> {
    let v = seen(units);
    let n = v.len();
    let at = |p: usize| &units[v[p]];
    let marker = |p: usize| p < n && at(p).is(c);
    let solid = |p: usize| p < n && !at(p).space();
    let line_start = |p: usize| p == 0 || at(p - 1).ends_line();
    let opens_after = |p: usize| at(p).read().is_some_and(|ch| js_space(ch) || "([>".contains(ch));
    let closes_before = |p: usize| p == n || at(p).read().is_some_and(|ch| js_space(ch) || ".,!?;:)]".contains(ch));
    // The marker that closes the one at `k`: with the lazy run between the first and last characters inside tried
    // shortest first, then with only one character inside.
    let close = |k: usize| {
        if !solid(k + 1) {
            return None;
        }
        let mut m = k + 2;
        while m < n {
            if solid(m) && marker(m + 1) && closes_before(m + 2) {
                return Some(m + 1);
            }
            if at(m).is(c) || at(m).read() == Some('\n') {
                break;
            }
            m += 1;
        }
        (marker(k + 2) && closes_before(k + 3)).then_some(k + 2)
    };
    let mut found = Vec::new();
    let mut p = 0;
    while p < n {
        let tries = [(line_start(p), p), (opens_after(p), p + 1)];
        match tries.into_iter().filter(|&(ok, k)| ok && marker(k)).find_map(|(_, k)| Some((k, close(k)?))) {
            Some((k, e)) => {
                found.push((v[k], v[e]));
                p = e + 1;
            }
            None => p += 1,
        }
    }
    found
}

/// The characters shown, in order, with how each looks, after format.ts's last rule (`^(&gt;|>) ?(.*)$`, over the
/// lines as the blocks' stand-ins leave them): a ">" (or "&gt;") starting a line, and a space after it, made a
/// quote's bar, and the line after it dim.
fn shown(units: &mut [Unit]) -> Vec<(char, Look)> {
    let v = seen(units);
    let mut bars = vec![false; units.len()];
    let mut p = 0;
    while p < v.len() {
        let end = (p..v.len()).find(|&q| units[v[q]].ends_line()).unwrap_or(v.len());
        let reads =
            |from: usize, s: &str| s.chars().enumerate().all(|(d, c)| from + d < end && units[v[from + d]].is(c));
        let lead = if reads(p, "&gt;") {
            4
        } else if reads(p, ">") {
            1
        } else {
            0
        };
        if lead > 0 {
            let lead = lead + usize::from(reads(p + lead, " "));
            let stop = v.get(end).copied().unwrap_or(units.len());
            for u in &mut units[v[p]..stop] {
                u.look.marks |= QUOTE;
            }
            for &q in &v[p..p + lead] {
                units[q].kind = Kind::Gone;
            }
            bars[v[p]] = true;
        }
        p = end + 1;
    }
    let bar = Look { marks: QUOTE, link: None };
    let mut out = Vec::with_capacity(units.len());
    for (u, bar_here) in units.iter().zip(bars) {
        if bar_here {
            out.extend([('│', bar), (' ', bar)]);
        }
        if matches!(u.kind, Kind::Text | Kind::Kept | Kind::Block) {
            out.push((u.ch, u.look));
        }
    }
    out
}

/// A character as the wrapping sees it: a grapheme, what the user takes for one.
struct Glyph<'a> {
    text: &'a str,
    width: usize,
    space: bool,
    look: Look,
}

/// One of the text's own lines wrapped to `width`, into `out`.
fn lay_out(line: &[(char, Look)], width: usize, theme: &Theme, urls: &[String], out: &mut Marked) {
    let s: String = line.iter().map(|&(ch, _)| ch).collect();
    let mut at = 0;
    let glyphs: Vec<Glyph> = s
        .graphemes(true)
        .map(|g| {
            let look = line[at].1;
            at += g.chars().count();
            Glyph { text: g, width: text::width(g), space: g.chars().all(js_space), look }
        })
        .collect();
    for row in wrap(&glyphs, width) {
        let row = &glyphs[row];
        let mut spans: Vec<(Look, String)> = Vec::new();
        let mut links: Vec<LinkAt> = Vec::new();
        let (mut col, mut last) = (0, None);
        for g in row {
            match spans.last_mut() {
                Some((look, s)) if *look == g.look => s.push_str(g.text),
                _ => spans.push((g.look, g.text.to_string())),
            }
            if let Some(l) = g.look.link {
                match links.last_mut() {
                    Some(link) if last == Some(l) => link.cols.end += g.width,
                    _ => links.push(LinkAt { cols: col..col + g.width, url: urls[l].clone() }),
                }
            }
            last = g.look.link;
            col += g.width;
        }
        out.lines.push(spans.into_iter().map(|(look, s)| Span::styled(s, look.style(theme))).collect());
        out.links.push(links);
    }
}

/// Where a line breaks, as wassup's wrapTagged breaks it: at runs of spaces, which go where the line breaks and
/// stay where they fit; a word that doesn't fit on the line goes to the next, and one wider than a whole line fills
/// the one it's on and goes on in the next ones; the spaces at the end of each line left out. Unlike there, a break
/// never leaves an empty line before the text. The rows, as ranges of `glyphs`.
fn wrap(glyphs: &[Glyph], width: usize) -> Vec<Range<usize>> {
    let mut rows = Vec::new();
    // The row being filled, if anything went on it yet, and its width.
    let mut row: Option<Range<usize>> = None;
    let mut w = 0;
    let trimmed = |mut r: Range<usize>| {
        while r.end > r.start && glyphs[r.end - 1].space {
            r.end -= 1;
        }
        r
    };
    let mut i = 0;
    while i < glyphs.len() {
        let space = glyphs[i].space;
        let j = i + glyphs[i..].iter().take_while(|g| g.space == space).count();
        let run_w: usize = glyphs[i..j].iter().map(|g| g.width).sum();
        if w + run_w <= width {
            row = Some(row.map_or(i, |r| r.start)..j);
            w += run_w;
        } else if space {
            rows.extend(row.take().map(trimmed));
            w = 0;
        } else if run_w <= width {
            rows.extend(row.take().map(trimmed));
            row = Some(i..j);
            w = run_w;
        } else {
            for (k, g) in glyphs.iter().enumerate().take(j).skip(i) {
                if w + g.width > width && row.is_some() {
                    rows.extend(row.take().map(trimmed));
                    w = 0;
                }
                row = Some(row.map_or(k, |r| r.start)..k + 1);
                w += g.width;
            }
        }
        i = j;
    }
    if row.is_some() || rows.is_empty() {
        rows.push(trimmed(row.unwrap_or(0..0)));
    }
    rows
}

fn local(ts: i64) -> DateTime<Local> {
    Local.timestamp_opt(ts, 0).single().unwrap_or_else(Local::now)
}

/// Whole calendar days from `ts` to today, as wassup's daysAgo: 0 today, 1 yesterday, less than 0 ahead.
fn days_between(then: DateTime<Local>, now: DateTime<Local>) -> i64 {
    (now.date_naive() - then.date_naive()).num_days()
}

/// A day as the separators say it, as wassup's fmtDay: "hoje", "ontem", or the day and month, and the year if it's
/// another.
pub fn fmt_day(ts: i64, lang: Lang) -> String {
    day(local(ts), Local::now(), lang)
}

/// When something was, short, as wassup's fmtWhen: the time today, "ontem", the weekday within the week ("ter"),
/// the day and month before that, and the year if it's another.
pub fn fmt_when(ts: i64, lang: Lang) -> String {
    when(local(ts), Local::now(), lang)
}

/// When a chat's last message was, for its row in the list, as wassup's fmtWhenAt: the time today, "ontem 21:35",
/// "12 set 21:35", and "12 set 2025" in another year.
pub fn fmt_when_at(ts: i64, lang: Lang) -> String {
    when_at(local(ts), Local::now(), lang)
}

fn time(d: DateTime<Local>) -> String {
    text::fmt_time(d.timestamp())
}

/// "12 set", and the year after it if it's not this one.
fn date(d: DateTime<Local>, now: DateTime<Local>, lang: Lang) -> String {
    let day = format!("{} {}", d.day(), month(lang, d.month0()));
    if d.year() == now.year() { day } else { format!("{day} {}", d.year()) }
}

fn day(d: DateTime<Local>, now: DateTime<Local>, lang: Lang) -> String {
    match days_between(d, now) {
        0 => say(lang, "hoje", "today").into(),
        1 => say(lang, "ontem", "yesterday").into(),
        _ => date(d, now, lang),
    }
}

fn when(d: DateTime<Local>, now: DateTime<Local>, lang: Lang) -> String {
    match days_between(d, now) {
        ..=0 => time(d),
        1 => say(lang, "ontem", "yesterday").into(),
        2..7 => weekday(lang, d.weekday().num_days_from_sunday()).into(),
        _ => date(d, now, lang),
    }
}

fn when_at(d: DateTime<Local>, now: DateTime<Local>, lang: Lang) -> String {
    match days_between(d, now) {
        ..=0 => time(d),
        1 => format!("{} {}", say(lang, "ontem", "yesterday"), time(d)),
        _ if d.year() == now.year() => format!("{} {} {}", d.day(), month(lang, d.month0()), time(d)),
        _ => format!("{} {} {}", d.day(), month(lang, d.month0()), d.year()),
    }
}

/// The country calling code a phone number's digits start with. The codes are prefix-free (E.164), and only these
/// take one or two digits: any other takes three.
pub fn calling_code(digits: &str) -> &str {
    let b = digits.as_bytes();
    let len = match (b.first(), b.get(1)) {
        (Some(b'1' | b'7'), _) => 1,
        (Some(b'2'), Some(b'0' | b'7'))
        | (Some(b'3'), Some(b'0'..=b'4' | b'6' | b'9'))
        | (Some(b'4'), Some(b'0' | b'1' | b'3'..=b'9'))
        | (Some(b'5'), Some(b'1'..=b'8'))
        | (Some(b'6'), Some(b'0'..=b'6'))
        | (Some(b'8'), Some(b'1' | b'2' | b'4' | b'6'))
        | (Some(b'9'), Some(b'0'..=b'5' | b'8')) => 2,
        _ => 3,
    };
    &digits[..digits.char_indices().nth(len).map_or(digits.len(), |(i, _)| i)]
}

/// The digits of a jid ("351900000001@s.whatsapp.net", or a device's, ":2" after them), or the digits given.
fn user(jid: &str) -> &str {
    jid.split(['@', ':']).next().unwrap_or(jid)
}

/// A person's number, from their jid or its digits, as wassup's list shows it: without the country's code when it's
/// the same as mine (`me`, my jid or my number), whichever country that is; a Portuguese one grouped three by three
/// ("900 000 001", "+351 900 000 001" from elsewhere); any other whole, after "+".
pub fn phone_number(jid: &str, me: &str) -> String {
    let digits = user(jid);
    if digits.is_empty() {
        return String::new();
    }
    let code = calling_code(digits);
    let national = &digits[code.len()..];
    let pt = (code == "351" && national.len() == 9 && national.is_ascii())
        .then(|| format!("{} {} {}", &national[..3], &national[3..6], &national[6..]));
    match pt {
        _ if code == calling_code(user(me)) => pt.unwrap_or_else(|| national.to_string()),
        Some(pt) => format!("+351 {pt}"),
        None => format!("+{digits}"),
    }
}

/// What the list says of a chat under its name, as wassup's chatNumber: for a group, "grupo", and how many members
/// it has when `members` says; for a person, their number as `phone_number` gives it. A hidden number ("@lid") has
/// none to show: pass the phone number's jid it stands for, when known, instead.
pub fn chat_number(jid: &str, me: &str, members: Option<usize>, lang: Lang) -> String {
    if jid.ends_with("@g.us") {
        return match members {
            Some(n) => format!("{} · {n} {}", say(lang, "grupo", "group"), say(lang, "membros", "members")),
            None => say(lang, "grupo", "group").into(),
        };
    }
    if jid.contains('@') && !jid.ends_with("@s.whatsapp.net") {
        return String::new();
    }
    phone_number(jid, me)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The theme when the terminal doesn't say its background: dim 247, links 75.
    fn dark() -> Theme {
        Theme::new(None, None, false)
    }

    fn mark_at(text: &str, width: usize) -> Marked {
        wa_markup(text, width, &dark(), |url| url.to_string())
    }

    fn mark(text: &str) -> Marked {
        mark_at(text, 80)
    }

    fn plain(m: &Marked) -> Vec<String> {
        m.lines.iter().map(|l| l.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    /// The style of the piece whose text is `piece`.
    fn style_of(m: &Marked, piece: &str) -> Style {
        m.lines.iter().flatten().find(|s| s.content == piece).unwrap_or_else(|| panic!("no {piece:?} in {m:?}")).style
    }

    fn with(modifier: Modifier) -> Style {
        Style::new().add_modifier(modifier)
    }

    const DIM: Color = Color::Indexed(247);

    #[test]
    fn each_marker_gives_its_style_and_goes() {
        let m = mark("*negrito* _itálico_ ~riscado~ `código`");
        assert_eq!(plain(&m), ["negrito itálico riscado código"]);
        assert_eq!(style_of(&m, "negrito"), with(Modifier::BOLD));
        assert_eq!(style_of(&m, "itálico"), with(Modifier::ITALIC));
        assert_eq!(style_of(&m, "riscado"), with(Modifier::CROSSED_OUT).fg(DIM));
        assert_eq!(style_of(&m, "código"), Style::new().fg(Color::Yellow));
    }

    #[test]
    fn markers_inside_words_stay() {
        for text in ["2*3*4 e 5*6", "nome_de_ficheiro.txt", "a~b~c", "x*y* z", "*a*b"] {
            assert_eq!(plain(&mark(text)), [text]);
        }
    }

    #[test]
    fn unclosed_or_spaced_markers_stay() {
        for text in ["*sem fim", "_a b", "~ a~", "*a *", "`sem fim", "```bloco sem fim", "``"] {
            assert_eq!(plain(&mark(text)), [text]);
        }
        // Nor does a marker close on another line.
        assert_eq!(plain(&mark("*um\ndois*")), ["*um", "dois*"]);
        assert_eq!(plain(&mark("`um\ndois`")), ["`um", "dois`"]);
    }

    #[test]
    fn markers_open_after_brackets_and_close_before_punctuation() {
        let m = mark("(*sim*), [_não_]. >~talvez~!");
        assert_eq!(plain(&m), ["(sim), [não]. >talvez!"]);
        assert_eq!(style_of(&m, "sim"), with(Modifier::BOLD));
        assert_eq!(style_of(&m, "não"), with(Modifier::ITALIC));
        assert_eq!(style_of(&m, "talvez"), with(Modifier::CROSSED_OUT).fg(DIM));
        // A word with a single character, and one closing at the end of a line.
        let m = mark("o *x*\n*y*");
        assert_eq!(plain(&m), ["o x", "y"]);
        assert_eq!(style_of(&m, "y"), with(Modifier::BOLD));
    }

    #[test]
    fn a_doubled_marker_is_inside_the_single_one() {
        // The text inside may start and end with the marker itself, as in format.ts.
        let m = mark("**a**");
        assert_eq!(plain(&m), ["*a*"]);
        assert_eq!(style_of(&m, "*a*"), with(Modifier::BOLD));
    }

    #[test]
    fn nesting_as_format_ts() {
        // Bold goes first, and its tags then stand beside the underscores, which neither open nor close.
        let m = mark("*_só negrito_*");
        assert_eq!(plain(&m), ["_só negrito_"]);
        assert_eq!(style_of(&m, "_só negrito_"), with(Modifier::BOLD));
        // An underscore isn't among what a marker opens after.
        let m = mark("_*só itálico*_");
        assert_eq!(plain(&m), ["*só itálico*"]);
        assert_eq!(style_of(&m, "*só itálico*"), with(Modifier::ITALIC));
        // Brackets between them let both through.
        let m = mark("*(_ambos_)*");
        assert_eq!(plain(&m), ["(ambos)"]);
        assert_eq!(style_of(&m, "ambos"), with(Modifier::BOLD | Modifier::ITALIC));
        // Bold around code, and around a block.
        let m = mark("*`x`* e *a ```b``` c*");
        assert_eq!(style_of(&m, "x"), with(Modifier::BOLD).fg(Color::Yellow));
        assert_eq!(style_of(&m, "b"), with(Modifier::BOLD).fg(Color::Yellow));
        assert_eq!(style_of(&m, "a "), with(Modifier::BOLD));
    }

    #[test]
    fn nothing_inside_code_is_markup() {
        let m = mark("`*a* _b_ ~c~`");
        assert_eq!(plain(&m), ["*a* _b_ ~c~"]);
        assert_eq!(style_of(&m, "*a* _b_ ~c~"), Style::new().fg(Color::Yellow));
        // format.ts would make the "b" bold: its expressions read the code's text.
        let m = mark("`a *b* c`");
        assert_eq!(plain(&m), ["a *b* c"]);
        let m = mark("antes ```*a* _b_\n> c https://example.com``` depois");
        assert_eq!(plain(&m), ["antes *a* _b_", "> c https://example.com depois"]);
        assert_eq!(style_of(&m, "*a* _b_"), Style::new().fg(Color::Yellow));
        assert_eq!(style_of(&m, "> c https://example.com"), Style::new().fg(Color::Yellow));
        assert!(m.links.iter().all(Vec::is_empty));
        // An empty block, and backticks left over from one.
        assert_eq!(plain(&mark("a``````b")), ["ab"]);
        assert_eq!(plain(&mark("``x`")), ["`x"]);
    }

    #[test]
    fn quotes() {
        let m = mark("> citação com *força*\n>junto ao sinal\nnão > isto\n&gt; escapado\n`>` não");
        assert_eq!(plain(&m), ["│ citação com força", "│ junto ao sinal", "não > isto", "│ escapado", "> não"]);
        assert_eq!(style_of(&m, "│ citação com "), Style::new().fg(DIM));
        assert_eq!(style_of(&m, "força"), with(Modifier::BOLD).fg(DIM));
        // A bold right after the ">" opens.
        let m = mark(">*forte*");
        assert_eq!(plain(&m), ["│ forte"]);
        assert_eq!(style_of(&m, "forte"), with(Modifier::BOLD).fg(DIM));
    }

    #[test]
    fn links_shown_short_with_their_place() {
        let short = |url: &str| url.trim_start_matches("https://").trim_end_matches('/').to_string();
        let m = wa_markup("vê https://example.com/clube/ já", 80, &dark(), short);
        assert_eq!(plain(&m), ["vê example.com/clube já"]);
        assert_eq!(m.links, [vec![LinkAt { cols: 3..20, url: "https://example.com/clube/".into() }]]);
        assert_eq!(style_of(&m, "example.com/clube"), with(Modifier::UNDERLINED).fg(Color::Indexed(75)));
        // Ends before a bracket, a quote mark or a space; in bold, it's bold too; in code, still a link.
        let m = mark("(https://example.com/a) *ver http://example.org/b agora* `https://example.net`");
        let urls: Vec<&str> = m.links[0].iter().map(|l| l.url.as_str()).collect();
        assert_eq!(urls, ["https://example.com/a", "http://example.org/b", "https://example.net"]);
        assert_eq!(
            style_of(&m, "http://example.org/b"),
            with(Modifier::UNDERLINED | Modifier::BOLD).fg(Color::Indexed(75))
        );
        // A link takes the markers stuck to it, which then can't close beside its tags.
        let m = mark("_https://example.com/a_b_ c");
        assert_eq!(plain(&m), ["_https://example.com/a_b_ c"]);
        assert_eq!(m.links[0][0].url, "https://example.com/a_b_");
    }

    #[test]
    fn a_link_cut_by_the_wrap_is_on_each_of_its_lines() {
        let url = "https://example.com/um/caminho/bastante/comprido";
        let m = mark_at(&format!("ver {url} sim"), 20);
        let lines = plain(&m);
        assert!(lines.iter().all(|l| text::width(l) <= 20), "{lines:?}");
        let mut shown = String::new();
        for (line, links) in lines.iter().zip(&m.links) {
            for l in links {
                assert_eq!(l.url, url);
                shown.extend(line.chars().skip(l.cols.start).take(l.cols.len()));
            }
        }
        assert_eq!(shown, url);
        assert_eq!(m.links[0][0].cols, 4..20);
    }

    #[test]
    fn wraps_at_spaces_with_the_styles_kept() {
        let m = mark_at("um *dois três* quatro", 8);
        assert_eq!(plain(&m), ["um dois", "três", "quatro"]);
        assert_eq!(style_of(&m, "dois"), with(Modifier::BOLD));
        assert_eq!(style_of(&m, "três"), with(Modifier::BOLD));
        // A word wider than a line fills the one it starts on first.
        assert_eq!(plain(&mark_at("ab cdefghij", 5)), ["ab cd", "efghi", "j"]);
        // Wide characters take two columns, and a grapheme isn't cut.
        assert_eq!(plain(&mark_at("日本語 ok", 4)), ["日本", "語", "ok"]);
        assert_eq!(plain(&mark_at("👍🏽👍🏽", 2)), ["👍🏽", "👍🏽"]);
        // Spaces at a break go; at the start of the text they stay.
        assert_eq!(plain(&mark_at("aaa    bbb", 5)), ["aaa", "bbb"]);
        assert_eq!(plain(&mark_at("  recuado", 20)), ["  recuado"]);
        assert_eq!(plain(&mark_at("          x", 4)), ["x"]);
    }

    #[test]
    fn the_texts_own_lines_stay() {
        assert_eq!(plain(&mark("a\n\nb\n")), ["a", "", "b", ""]);
        assert_eq!(plain(&mark("")), [""]);
        assert_eq!(plain(&mark("   ")), [""]);
        assert_eq!(mark("a\nb").links, [vec![], vec![]]);
    }

    #[test]
    fn no_line_is_wider_than_asked() {
        let text = "Olá, *isto* é _uma_ mensagem ~longa~ com `código` e https://example.com/a/b/c pelo meio,\n> e uma \
                    citação que também tem de ser cortada";
        for width in 1..40 {
            for line in mark_at(text, width).lines {
                let w: usize = line.iter().map(|s| s.width()).sum();
                assert!(w <= width, "{w} > {width}: {line:?}");
            }
        }
    }

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap()
    }

    #[test]
    fn when_with_the_time() {
        let now = at(2026, 10, 16, 12, 0);
        assert_eq!(when_at(at(2026, 10, 16, 9, 5), now, Lang::Pt), "09:05");
        assert_eq!(when_at(at(2026, 10, 15, 21, 35), now, Lang::Pt), "ontem 21:35");
        assert_eq!(when_at(at(2026, 10, 15, 21, 35), now, Lang::En), "yesterday 21:35");
        assert_eq!(when_at(at(2026, 10, 13, 8, 0), now, Lang::Pt), "13 out 08:00");
        assert_eq!(when_at(at(2026, 9, 12, 21, 35), now, Lang::Pt), "12 set 21:35");
        assert_eq!(when_at(at(2026, 9, 12, 21, 35), now, Lang::En), "12 Sep 21:35");
        assert_eq!(when_at(at(2025, 9, 12, 21, 35), now, Lang::Pt), "12 set 2025");
        // Ahead, as a clock set wrong may give: the time.
        assert_eq!(when_at(at(2026, 10, 17, 8, 0), now, Lang::Pt), "08:00");
    }

    #[test]
    fn when_with_the_weekday_within_the_week() {
        let now = at(2026, 10, 16, 12, 0);
        assert_eq!(when(at(2026, 10, 16, 0, 1), now, Lang::Pt), "00:01");
        assert_eq!(when(at(2026, 10, 15, 23, 59), now, Lang::Pt), "ontem");
        assert_eq!(when(at(2026, 10, 13, 8, 0), now, Lang::Pt), "ter");
        assert_eq!(when(at(2026, 10, 13, 8, 0), now, Lang::En), "Tue");
        assert_eq!(when(at(2026, 10, 10, 8, 0), now, Lang::Pt), "sáb");
        assert_eq!(when(at(2026, 10, 9, 8, 0), now, Lang::Pt), "9 out");
        assert_eq!(when(at(2025, 12, 31, 8, 0), now, Lang::En), "31 Dec 2025");
    }

    #[test]
    fn days_by_the_calendar() {
        let now = at(2026, 10, 16, 12, 0);
        assert_eq!(day(at(2026, 10, 16, 1, 0), now, Lang::Pt), "hoje");
        assert_eq!(day(at(2026, 10, 15, 1, 0), now, Lang::En), "yesterday");
        assert_eq!(day(at(2026, 9, 12, 1, 0), now, Lang::Pt), "12 set");
        assert_eq!(day(at(2025, 9, 12, 1, 0), now, Lang::Pt), "12 set 2025");
        // Minutes apart across midnight are a day apart, at the turn of the year too.
        assert_eq!(days_between(at(2025, 12, 31, 23, 50), at(2026, 1, 1, 0, 10)), 1);
        assert_eq!(days_between(at(2026, 1, 1, 0, 10), at(2025, 12, 31, 23, 50)), -1);
    }

    #[test]
    fn calling_codes() {
        let cases = [
            ("12025550123", "1"),
            ("79001234567", "7"),
            ("447700900123", "44"),
            ("5511900000001", "55"),
            ("351900000001", "351"),
            ("420600000001", "420"),
            ("", ""),
        ];
        for (digits, code) in cases {
            assert_eq!(calling_code(digits), code, "{digits}");
        }
    }

    #[test]
    fn numbers_without_my_countrys_code() {
        let pt = "351900000001@s.whatsapp.net";
        assert_eq!(phone_number(pt, "351900000002@s.whatsapp.net"), "900 000 001");
        assert_eq!(phone_number(pt, "351900000002:3@s.whatsapp.net"), "900 000 001");
        assert_eq!(phone_number(pt, "12025550100"), "+351 900 000 001");
        assert_eq!(phone_number(pt, ""), "+351 900 000 001");
        // Any country, not only Portugal, loses its code when it's mine.
        assert_eq!(phone_number("5511900000001@s.whatsapp.net", "5521900000002"), "11900000001");
        assert_eq!(phone_number("5511900000001@s.whatsapp.net", "351900000002"), "+5511900000001");
        assert_eq!(phone_number("447700900123", "447700900456"), "7700900123");
        assert_eq!(phone_number("12025550123", "12025550100"), "2025550123");
    }

    #[test]
    fn what_the_list_says_under_a_name() {
        let me = "351900000002@s.whatsapp.net";
        assert_eq!(chat_number("120363000000000001@g.us", me, Some(12), Lang::Pt), "grupo · 12 membros");
        assert_eq!(chat_number("120363000000000001@g.us", me, Some(12), Lang::En), "group · 12 members");
        assert_eq!(chat_number("120363000000000001@g.us", me, None, Lang::Pt), "grupo");
        assert_eq!(chat_number("123456789012345@lid", me, None, Lang::Pt), "");
        assert_eq!(chat_number("351900000001@s.whatsapp.net", me, None, Lang::Pt), "900 000 001");
    }
}
