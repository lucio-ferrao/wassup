//! A chat on the screen, as the Node version's ui.ts renderMessages lays it out: a day's separator where the day changes and a
//! blank row after each message; in a bubble (a column of its colour either side of the text) the sender's name in a
//! group, the message it answers ("│"), and its text or what it is; its time outside it on the last row, mine with
//! their state in the time's separator and "❮" after the last one read. Emoji on their own go bare (twice their size
//! where the terminal shows pictures); pictures stand out of the bubble; a link's preview inside it, over the text.
//! Reactions underneath. The selected message on a grey of its own, without its bubble. Each row keeps what a click
//! or a selection needs: its message, whether it's the name or time line, where the text is, the links, names and
//! mentions, the passages marked wrong, the picture's columns.

use std::sync::Arc;

use image::DynamicImage;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui_image::picker::ProtocolType;
use ratatui_image::{Image, Resize};

use crate::app::{App, Hit, QUICK, WebPanel, WebPic};
use crate::bigtext::big_text;
use crate::i18n::{s, t};
use crate::llm::Fix;
use crate::store::Msg;
use crate::text::{self, theme};

#[derive(Clone, Debug)]
pub enum RowKind {
    Line(Line<'static>),
    /// One row of a picture: from column `x`, `cols` wide, `rows` tall, this its `index`th; after its last row the
    /// time; `preview` for a link's.
    Image { x: u16, cols: u16, rows: u16, index: u16, after: Option<(u16, String)>, preview: bool },
    /// One of the two rows of emoji on their own drawn twice their size, four cells by two each, from `x`.
    Emoji { x: u16, index: u16, after: Option<(u16, String)> },
}

#[derive(Clone, Debug)]
pub struct Row {
    pub kind: RowKind,
    pub msg: Option<usize>,
    /// The name or time line: a drag from it answers, the "☺" sits on the time's.
    pub header: bool,
    /// Where the "☺" goes, on the time's row.
    pub icon: Option<u16>,
    /// The columns of the message's own text on this row, the only ones a selection takes.
    pub text: Option<(u16, u16)>,
    pub links: Vec<(u16, u16, String)>,
    /// A group member's name, and the mentions: whose chat a click opens.
    pub names: Vec<(u16, u16, String)>,
    pub fixes: Vec<(u16, u16, Fix)>,
    /// The picture's columns on this row.
    pub picture: Option<(u16, u16)>,
    pub preview: bool,
    /// Text beside a picture on this row (its column and text): the selected picture's description.
    pub aside: Option<(u16, String)>,
    /// What's under a picture drawn in a bubble: the bubble's row.
    pub under: Option<Line<'static>>,
}

impl Row {
    fn blank() -> Self {
        Self::line(Line::from(""), None)
    }

    fn line(l: Line<'static>, msg: Option<usize>) -> Self {
        Self { kind: RowKind::Line(l), msg, header: false, icon: None, text: None, links: vec![], names: vec![], fixes: vec![], picture: None, preview: false, aside: None, under: None }
    }
}

/// A line of a message before it's placed: its spans, and where things are in it, counted from its first column.
#[derive(Default, Clone)]
struct Piece {
    spans: Vec<Span<'static>>,
    text: bool,
    links: Vec<(u16, u16, String)>,
    names: Vec<(u16, u16, String)>,
    fixes: Vec<(u16, u16, Fix)>,
    header: bool,
    /// A link preview's row: the picture's width, height and which of its rows.
    pic: Option<(u16, u16, u16)>,
}

impl Piece {
    fn width(&self) -> usize {
        self.spans.iter().map(|s| text::width(&s.content)).sum()
    }

    fn plain(s: impl Into<String>, style: Style) -> Self {
        let s: String = s.into();
        Self { spans: vec![Span::styled(crate::unicode::tame(&s).into_owned(), style)], ..Default::default() }
    }
}

pub fn has_picture(m: &Msg) -> bool {
    matches!(m.kind.as_str(), "image" | "sticker") || (matches!(m.kind.as_str(), "video" | "gif" | "ptv") && m.thumb.is_some())
}

/// A picture from its file, turned as its EXIF says, kept at most 1200 pixels wide.
pub fn open_image(path: &str) -> Result<DynamicImage, String> {
    let mut decoder = image::ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?.into_decoder().map_err(|e| e.to_string())?;
    let orientation = image::ImageDecoder::orientation(&mut decoder).ok();
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(if img.width() > 1200 { img.resize(1200, u32::MAX, image::imageops::FilterType::Triangle) } else { img })
}

/// A picture decoded off the loop, two at a time (a chat full of photos asked for them all at once), and sent back
/// by its key.
pub fn decode(tx: tokio::sync::mpsc::UnboundedSender<crate::app::Input>, key: String, path: String) {
    static TWO: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> = std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));
    let two = TWO.clone();
    tokio::spawn(async move {
        let Ok(_turn) = two.acquire_owned().await else { return };
        let img = tokio::task::spawn_blocking(move || open_image(&path)).await.unwrap_or_else(|e| Err(e.to_string()));
        let _ = tx.send(crate::app::Input::Image(key, img));
    });
}

/// The row of the chat under screen row `y`.
pub fn row_at(app: &App, y: u16) -> Option<&Row> {
    let a = app.chat_area;
    if y < a.y || y >= a.y + a.height {
        return None;
    }
    let total = app.rows.len();
    let height = a.height as usize;
    let _ = (total, height);
    app.rows.get(app.top + (y - a.y) as usize)
}

/// A picture's size in cells, as wassup's cellSize: cells taken as twice as tall as wide, its natural size (8
/// pixels a column) up to the limits; with `fill` (half-blocks) the whole width, shrunk only if the height doesn't fit.
fn picture_box(w: u32, h: u32, max_cols: u16, max_rows: u16, _cell: Size, fill: bool) -> (u16, u16) {
    let (w, h) = if w == 0 || h == 0 { (4.0, 3.0) } else { (w as f64, h as f64) };
    let mut cols = if fill { max_cols.max(1) } else { ((w / 8.0).round() as u16).clamp(1, max_cols.max(1)) };
    let mut rows = ((cols as f64 * h / w / 2.0).round() as u16).max(1);
    if rows > max_rows {
        rows = max_rows.max(1);
        cols = ((rows as f64 * 2.0 * w / h).round() as u16).max(1);
    }
    (cols, rows)
}

/// What a message that isn't text says in its bubble, in wassup's colours.
fn kind_piece(app: &App, m: &Msg) -> Option<Piece> {
    let yellow = Style::new().fg(Color::Yellow);
    let hint = media_hint(app, m);
    let with_hint = |mut p: Piece| {
        if let Some(h) = hint.clone() {
            p.spans.push(h);
        }
        p
    };
    Some(match m.kind.as_str() {
        "deleted" => Piece::plain(format!("⊘ {}", s("deleted")), Style::new().fg(theme().dim)),
        "video" | "gif" | "ptv" => with_hint(Piece::plain(format!("▶ {}", if m.kind == "gif" { s("gif") } else if m.kind == "ptv" { s("videoNote") } else { s("video") }), Style::new().fg(Color::Magenta))),
        "document" => with_hint(Piece::plain(format!("📎 {}", m.media_name.clone().unwrap_or_else(|| s("file"))), yellow)),
        "voice" => with_hint(Piece::plain(format!("🎤 {} {}", s("voiceMessage"), m.text).trim_end().to_string(), yellow)),
        "audio" => with_hint(Piece::plain(format!("🎵 {} {}", s("audio"), m.text).trim_end().to_string(), yellow)),
        "contact" => Piece::plain(format!("👤 {}", m.text), yellow),
        "pin" => Piece::plain(s("pinned"), Style::new().fg(theme().dim)),
        "unpin" => Piece::plain(s("unpinned"), Style::new().fg(theme().dim)),
        "album" => Piece::plain(s("album"), Style::new().fg(theme().dim)),
        "placeholder" => Piece::plain(s("waitingMessage"), Style::new().fg(theme().dim)),
        "groupInvite" => Piece::plain(s("groupInvite"), Style::new().fg(theme().dim)),
        "interactive" => Piece::plain(s("businessMessage"), Style::new().fg(theme().dim)),
        "unsupported" => Piece::plain(s("unsupported"), Style::new().fg(theme().dim)),
        _ => return None,
    })
}

/// After an attachment: "(indisponível)" when it's gone, "⤓" in green while it isn't here yet.
fn media_hint(app: &App, m: &Msg) -> Option<Span<'static>> {
    if m.media_err && m.media_path.is_none() {
        Some(Span::styled(format!(" {}", s("unavailable")), Style::new().fg(theme().dim)))
    } else if m.media.is_some() && m.media_path.is_none() {
        Some(Span::styled(if app.caps.utf8 { " ⤓" } else { " v" }, Style::new().fg(theme().green)))
    } else {
        None
    }
}

/// A line of WhatsApp's markup, wrapped: its spans, and its links' columns with the whole clean link.
struct MLine {
    spans: Vec<Span<'static>>,
    links: Vec<(usize, usize, String)>,
}

/// `text` with WhatsApp's markup, wrapped to `width`, on `base` where the markup leaves the colour alone.
fn markup(text: &str, width: usize, base: Style) -> Vec<MLine> {
    let marked = crate::format::wa_markup(text, width, theme(), |u| crate::links::short_url(&crate::links::clean_url(u)));
    marked
        .lines
        .into_iter()
        .zip(marked.links)
        .map(|(spans, links)| MLine {
            spans: spans.into_iter().map(|s| Span::styled(s.content, base.patch(s.style))).collect(),
            links: links.into_iter().map(|l| (l.cols.start, l.cols.end, crate::links::clean_url(&l.url))).collect(),
        })
        .collect()
}

/// A message's text as lines of `width`: WhatsApp's markup, mentions by first name in their colour (whose, for a
/// click), links shortened and underlined (the whole clean one, for a click), the search's words in reverse video or
/// my passages marked wrong underlined in yellow.
fn text_pieces(app: &App, m: &Msg, width: usize, base: Style, words: &[String], fixes: &[Fix]) -> Vec<Piece> {
    let (shown, mentions) = text::with_mentions(&crate::unicode::tame(&m.text), |d| app.store.mention_name(d));
    let lines = markup(&shown, width, base);
    // Where the mentions and the search's words are, by line, matched on each line's own text.
    let mut out = Vec::new();
    for l in lines {
        let plain: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        let chars: Vec<char> = plain.chars().collect();
        let mut styles: Vec<Style> = Vec::with_capacity(chars.len());
        for sp in &l.spans {
            for _ in sp.content.chars() {
                styles.push(sp.style);
            }
        }
        let mut names = Vec::new();
        let col_of = |i: usize| text::width(&chars[..i].iter().collect::<String>()) as u16;
        for (name, jid) in &mentions {
            let n: Vec<char> = name.chars().collect();
            let mut i = 0;
            while i + n.len() <= chars.len() {
                if chars[i..i + n.len()] == n[..] {
                    for st in &mut styles[i..i + n.len()] {
                        *st = st.fg(text::color_for(jid));
                    }
                    names.push((col_of(i), col_of(i + n.len()), jid.clone()));
                    i += n.len();
                } else {
                    i += 1;
                }
            }
        }
        // The search's words, but not inside a link, shortened for showing.
        for (a, b) in text::matches(&plain, words) {
            let (ca, cb) = (col_of(a) as usize, col_of(b) as usize);
            if l.links.iter().any(|(la, lb, _)| ca < *lb && cb > *la) {
                continue;
            }
            for st in &mut styles[a..b] {
                *st = st.add_modifier(Modifier::REVERSED);
            }
        }
        let mut piece_fixes = Vec::new();
        for f in fixes {
            let fc: Vec<char> = f.from.chars().collect();
            if fc.is_empty() {
                continue;
            }
            let mut i = 0;
            while i + fc.len() <= chars.len() {
                if chars[i..i + fc.len()] == fc[..] {
                    for st in &mut styles[i..i + fc.len()] {
                        *st = st.fg(Color::Yellow).add_modifier(Modifier::UNDERLINED);
                    }
                    piece_fixes.push((col_of(i), col_of(i + fc.len()), f.clone()));
                    break;
                }
                i += 1;
            }
        }
        let links = l.links.iter().map(|(a, b, u)| (*a as u16, *b as u16, u.clone())).collect();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        for (i, c) in chars.iter().enumerate() {
            if i > 0 && styles[i] != styles[i - 1] {
                spans.push(Span::styled(std::mem::take(&mut run), styles[i - 1]));
            }
            run.push(*c);
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, *styles.last().unwrap_or(&base)));
        }
        out.push(Piece { spans, text: true, links, names, fixes: piece_fixes, header: false, pic: None });
    }
    out
}

/// The chat's rows for `width` columns.
pub fn layout(app: &mut App, width: u16, cell: Size) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let Some(jid) = app.current().map(String::from) else { return rows };
    let group = text::is_group(&jid);
    let width_us = width as usize;
    let blocks = app.picker.protocol_type() == ProtocolType::Halfblocks;
    let last_read = app.msgs.iter().rev().find(|m| m.from_me && m.status >= 4).map(|m| m.id.clone());
    let last_mine = app.msgs.iter().rev().find(|m| m.from_me).map(|m| m.id.clone());
    let words: Vec<String> = app.search.as_ref().map(|s| s.words.clone()).unwrap_or_default();
    let selected = app.selected.clone();
    let dragged = app.drag.as_ref().map(|(id, _)| id.clone());
    if app.older_pending.get(&jid).is_some_and(|t| t.elapsed().as_secs() < 30) {
        let label = s("loadingOlder");
        let lead = width_us.saturating_sub(text::width(&label)) / 2;
        rows.push(Row::line(Line::from(Span::styled(format!("{}{label}", " ".repeat(lead)), Style::new().fg(theme().dim))), None));
    }
    let faint = Style::new().fg(theme().faint);
    let msgs = app.msgs.clone();
    let mut day = 0;
    for (i, m) in msgs.iter().enumerate() {
        let d = text::day_key(m.ts);
        if d != day {
            day = d;
            let label = format!("── {} ──", crate::format::fmt_day(m.ts, crate::i18n::lang()));
            let lead = width_us.saturating_sub(text::width(&label)) / 2;
            rows.push(Row::line(Line::from(Span::styled(format!("{}{label}", " ".repeat(lead)), Style::new().fg(theme().dim))), None));
            rows.push(Row::blank());
        }
        let first_row = rows.len();
        let mine = m.from_me;
        let stamp = if mine {
            format!("{}{}", text::my_time(m.ts, m.status), if last_read.as_deref() == Some(m.id.as_str()) { "❮" } else { " " })
        } else {
            text::fmt_time(m.ts)
        };
        let stamp_w = text::width(&stamp);
        let bare = m.kind == "text" && crate::emojicodes::emoji_only(&m.text);
        let indent = if mine || bare { 0 } else { 1 };
        let text_w = if mine { width_us.saturating_sub(2 + stamp_w) } else { width_us.saturating_sub(indent) };
        let wrap_at = if bare { text_w } else { text_w.saturating_sub(1).max(1) };
        let selected_here = selected.as_deref() == Some(m.id.as_str());
        let paint = !selected_here && dragged.as_deref() != Some(m.id.as_str());

        // The message's lines in order: pieces in the bubble, and rows out of it (pictures), kept apart.
        enum Part {
            In(Piece),
            Out(Row),
        }
        let mut parts: Vec<Part> = Vec::new();
        let mut stamped = false;
        if group && !mine {
            let who = app.store.name_of(&m.sender);
            let w = text::width(&who) as u16;
            parts.push(Part::In(Piece { spans: vec![Span::styled(who, Style::new().fg(text::color_for(&m.sender)))], names: vec![(0, w, m.sender.clone())], header: true, ..Default::default() }));
        }
        if let (Some(q), false) = (&m.quoted, m.kind == "deleted") {
            let (who, said) = q.split_once('\t').unwrap_or(("", q.as_str()));
            let author = if !group || who == app.me || who.is_empty() { String::new() } else { format!("{}: ", app.store.name_of(who)) };
            let (said, _) = text::with_mentions(said, |d| app.store.mention_name(d));
            let said = crate::unicode::tame(&crate::links::show_links(&said)).into_owned();
            parts.push(Part::In(Piece::plain(text::truncate(&format!("│ {author}{said}"), width_us.saturating_sub(6)), Style::new().fg(theme().dim))));
        }
        // Pictures, out of the bubble; what came before them closes a bubble of its own above.
        if has_picture(m) {
            let beside = matches!(m.kind.as_str(), "image" | "sticker") && m.text.is_empty();
            let panel = app.chat_area.height.saturating_sub(2).max(4);
            let max_rows = if m.kind == "sticker" { 12 } else if blocks { panel } else { panel.min(18) };
            let limit = if mine && beside { width_us.saturating_sub(2 + stamp_w) } else { width_us } as u16;
            let max_cols = if blocks { limit.min(40) } else { limit.min(60) };
            let unavailable = m.media_err && m.media_path.is_none() && m.thumb.is_none();
            if app.caps.no_images || unavailable {
                let label = if app.caps.no_images { format!("[{}]", m.kind) } else { t("mediaUnavailable", &[&m.kind]) };
                parts.push(Part::In(Piece::plain(label, Style::new().fg(theme().dim))));
            } else {
                // Its size from the message, or from the picture itself once decoded.
                let (pw, ph) = if m.w > 0 && m.h > 0 {
                    (m.w, m.h)
                } else {
                    app.images.get(&format!("{}/{}", m.chat, m.id)).map(|i| (i.width(), i.height())).unwrap_or((0, 0))
                };
                let (cols, prows) = picture_box(pw, ph, max_cols, max_rows, cell, blocks);
                let edge = text_w;
                let x = if mine { edge.saturating_sub(cols as usize) } else { 0 } as u16;
                let tail = beside && x as usize + cols as usize + 2 + stamp_w <= width_us;
                for index in 0..prows {
                    let last = tail && index + 1 == prows;
                    let after = last.then(|| (x + cols, format!("  {stamp}")));
                    let mut r = Row { kind: RowKind::Image { x, cols, rows: prows, index, after, preview: false }, ..Row::line(Line::from(""), Some(i)) };
                    r.picture = Some((x, x + cols));
                    if last {
                        r.header = true;
                        r.icon = Some(icon_at(mine, x as usize, x as usize + cols as usize + 2 + stamp_w));
                    }
                    parts.push(Part::Out(r));
                }
                stamped = tail;
                // The selected one's description beside it in half-blocks, which lose its detail.
                if selected_here && blocks {
                    let key = format!("{} {}", m.chat, m.id);
                    if let Some(path) = app.picture_of(m) {
                        let about = match app.described_as(&key, &path) {
                            Some(Some(d)) => Some(d),
                            Some(None) => Some(s("describing")),
                            None => None,
                        };
                        if let Some(about) = about {
                            // Beside it, to its right for theirs, to its left for mine, when there's room and rows
                            // enough; under it otherwise.
                            let room = if mine { x as usize } else { width_us.saturating_sub(x as usize + cols as usize) }.saturating_sub(2);
                            let lines = text::wrap(&about, room.max(1));
                            let fits = room >= 16 && lines.len() <= (prows as usize).saturating_sub(if stamped && !mine { 1 } else { 0 });
                            if fits {
                                let block = lines.iter().map(|l| text::width(l)).max().unwrap_or(0) as u16;
                                let n = parts.len();
                                for (k, l) in lines.into_iter().enumerate() {
                                    if let Some(Part::Out(r)) = parts.get_mut(n - prows as usize + k) {
                                        let ax = if mine { x.saturating_sub(2 + block) } else { x + cols + 2 };
                                        r.aside = Some((ax, l));
                                    }
                                }
                            } else {
                                parts.push(Part::In(Piece::plain(about, Style::new().fg(theme().dim).add_modifier(Modifier::ITALIC))));
                            }
                        }
                    }
                }
            }
        }
        if let Some(p) = kind_piece(app, m) {
            parts.push(Part::In(p));
        }
        if m.kind == "location" {
            let lines = markup(&format!("📍 {}", m.text), wrap_at, Style::new().fg(Color::Yellow));
            for l in lines {
                parts.push(Part::In(Piece { spans: l.spans, links: l.links.iter().map(|(a, b, u)| (*a as u16, *b as u16, u.clone())).collect(), ..Default::default() }));
            }
        }
        if m.kind == "poll" {
            for l in m.text.lines() {
                parts.push(Part::In(Piece::plain(l.to_string(), Style::new().fg(Color::Yellow))));
            }
        }
        // A link's preview picture, in the bubble over the text.
        if m.kind == "text" && m.preview && !app.caps.no_images {
            let none = crate::wa::preview_path(&app.media_dir(), &m.chat, &m.id).with_extension("jpg.none").exists();
            if !none {
                let img = app.preview(m);
                let (cols, prows) = match &img {
                    Some(p) => picture_box(p.width(), p.height(), (wrap_at as u16).min(40), 12, cell, blocks),
                    None => (text::width(&format!("[{}]", s("linkPreview"))) as u16, 1),
                };
                if img.is_none() {
                    parts.push(Part::In(Piece::plain(format!("[{} {}]", s("linkPreview"), s("loading")), Style::new().fg(theme().dim))));
                } else {
                    // Inside the bubble, over the text: blank cells the picture is drawn over.
                    for index in 0..prows {
                        parts.push(Part::In(Piece { spans: vec![Span::raw(" ".repeat(cols as usize))], pic: Some((cols, prows, index)), ..Default::default() }));
                    }
                }
            }
        }
        if !m.text.is_empty() && matches!(m.kind.as_str(), "text" | "image" | "video" | "gif" | "document") {
            let fixes = if words.is_empty() && mine && m.kind == "text" && (last_mine.as_deref() == Some(m.id.as_str()) || selected_here) { app.sent_fixes_for(m).unwrap_or_default() } else { vec![] };
            // Emoji on their own, one to three, twice their size where the terminal shows pictures.
            let big: Vec<String> = if bare && (app.caps.text_sizing || app.web.is_some() || (!blocks && !app.caps.no_images)) {
                unicode_segmentation::UnicodeSegmentation::graphemes(m.text.trim(), true).filter(|g| !g.trim().is_empty()).map(String::from).collect()
            } else {
                vec![]
            };
            let big_w = big.len() * 4;
            if !big.is_empty() && big.len() <= 3 && big_w + 2 + stamp_w <= width_us && (app.caps.text_sizing || app.web.is_some() || big.iter().all(|e| crate::emoji::picture(e).is_some())) {
                let x = if mine { width_us.saturating_sub(big_w + 2 + stamp_w) } else { 0 } as u16;
                let icon = icon_at(mine, x as usize, x as usize + big_w + 2 + stamp_w);
                parts.push(Part::Out(Row { kind: RowKind::Emoji { x, index: 0, after: None }, ..Row::line(Line::from(""), Some(i)) }));
                let mut r = Row { kind: RowKind::Emoji { x, index: 1, after: Some((x + big_w as u16, format!("  {stamp}"))) }, ..Row::line(Line::from(""), Some(i)) };
                r.header = true;
                r.icon = Some(icon);
                parts.push(Part::Out(r));
                stamped = true;
            } else {
                let base = Style::new();
                let pieces = text_pieces(app, m, wrap_at, base, &words, &fixes);
                let n = pieces.len();
                for (k, mut p) in pieces.into_iter().enumerate() {
                    // Bare, the time goes at the end of the last line when it fits there.
                    if bare && k + 1 == n && p.width() + 2 + stamp_w <= width_us {
                        let tw = p.width();
                        let lead = if mine { width_us - (tw + 2 + stamp_w) } else { 0 };
                        let mut spans = vec![Span::raw(" ".repeat(lead))];
                        spans.append(&mut p.spans);
                        spans.push(Span::styled(format!("  {stamp}"), faint));
                        let mut r = Row::line(Line::from(spans), Some(i));
                        r.text = Some((lead as u16, (lead + tw) as u16));
                        r.header = true;
                        r.icon = Some(icon_at(mine, lead, lead + tw + 2 + stamp_w));
                        r.names = p.names.iter().map(|(a, b, j)| (a + lead as u16, b + lead as u16, j.clone())).collect();
                        r.links = p.links.iter().map(|(a, b, u)| (a + lead as u16, b + lead as u16, u.clone())).collect();
                        parts.push(Part::Out(r));
                        stamped = true;
                    } else {
                        parts.push(Part::In(p));
                    }
                }
            }
        }
        // Into rows: the pieces in one bubble as wide as the widest of them, the pictures standing out of it.
        let inner = parts.iter().filter_map(|p| if let Part::In(piece) = p { Some(piece.width()) } else { None }).max().unwrap_or(0);
        let mut out: Vec<Row> = Vec::new();
        for p in parts {
            match p {
                Part::In(piece) => out.push(bubble_row(piece, inner, i, mine, bare, indent, text_w, paint)),
                Part::Out(r) => out.push(r),
            }
        }
        // The time outside the bubble, on its last row: right after it for theirs, against the edge for mine.
        if !stamped
            && let Some(last) = out.last_mut().filter(|r| matches!(r.kind, RowKind::Line(_)) && r.picture.is_none() && !r.preview && r.msg.is_some())
                && let RowKind::Line(l) = &mut last.kind {
                    let used = l.width();
                    let at = if mine { width_us.saturating_sub(stamp_w) } else { used + 1 };
                    if at > used && at + stamp_w <= width_us {
                        l.spans.push(Span::raw(" ".repeat(at - used)));
                        l.spans.push(Span::styled(stamp.clone(), faint));
                        last.header = true;
                        let content_start = last.text.map(|t| t.0 as usize).unwrap_or_else(|| l.spans.iter().take(2).map(|s| text::width(&s.content)).sum());
                        last.icon = Some(icon_at(mine, content_start, at + stamp_w));
                        stamped = true;
                    }
                }
        if !stamped {
            let lead = if mine { width_us.saturating_sub(stamp_w) } else { 0 };
            let mut r = Row::line(Line::from(vec![Span::raw(" ".repeat(lead)), Span::styled(stamp.clone(), faint)]), Some(i));
            r.header = true;
            r.icon = Some(icon_at(mine, lead, lead + stamp_w));
            out.push(r);
        }
        // Reactions underneath, outside the bubble.
        if let Some(rs) = app.reactions.get(&m.id) {
            let line = rs.iter().map(|(e, n)| if *n > 1 { format!("{e} {n}") } else { e.clone() }).collect::<Vec<_>>().join("  ");
            let lead = if mine { text_w.saturating_sub(text::width(&line)) } else { indent };
            out.push(Row::line(Line::from(vec![Span::raw(" ".repeat(lead)), Span::styled(line, Style::new().fg(theme().dim))]), Some(i)));
        }
        rows.extend(out);
        let _ = first_row;
        rows.push(Row::blank());
    }
    // Room for the message just sent while it flies; once here, its rows blank until it lands.
    if let Some(f) = &mut app.flight {
        f.arrived |= msgs.iter().any(|m| m.from_me && m.text == f.text && chrono::Utc::now().timestamp() - m.ts < 60);
        if !f.arrived {
            for _ in 0..f.room {
                rows.push(Row::blank());
            }
        } else if f.frame < crate::app::FLY_FRAMES {
            for r in rows.iter_mut() {
                if r.msg.is_some_and(|i| msgs[i].from_me && msgs[i].text == f.text) {
                    r.kind = RowKind::Line(Line::from(""));
                }
            }
        }
    }
    rows
}

/// Where the "☺" goes on the time's row: a cell after the time for theirs, two before the text for mine.
fn icon_at(mine: bool, content_start: usize, time_end: usize) -> u16 {
    if mine { content_start.saturating_sub(3) as u16 } else { (time_end + 1) as u16 }
}

/// A row of a bubble as wide as `inner` with a column of its colour either side, as wassup's bubble: theirs from
/// the indent, mine each row flush against `text_w` (the bubble's left side ragged in its colour); not painted on
/// the selected or dragged message, whose own grey spans the width. A link preview's row is a picture over its
/// blank cells.
#[allow(clippy::too_many_arguments)]
fn bubble_row(p: Piece, inner: usize, msg: usize, mine: bool, bare: bool, indent: usize, text_w: usize, paint: bool) -> Row {
    let bg = if mine { theme().mine } else { theme().theirs };
    let pad = if bare { 0 } else { 1 };
    let w = p.width();
    // Where the bubble starts and where this row's text does.
    let (start, content) = if mine { (text_w.saturating_sub(inner + pad), text_w.saturating_sub(w)) } else { (indent.saturating_sub(pad), indent) };
    let bgs = |s: Style| if paint && !bare { s.bg(bg) } else { s };
    let mut spans = vec![Span::raw(" ".repeat(start))];
    if content > start {
        spans.push(Span::styled(" ".repeat(content - start), bgs(Style::new())));
    }
    for sp in p.spans {
        let st = bgs(sp.style);
        spans.push(Span::styled(sp.content, st));
    }
    let end = if mine { text_w } else { indent + inner };
    if !bare {
        spans.push(Span::styled(" ".repeat(end.saturating_sub(content + w) + pad), bgs(Style::new())));
    }
    let c = content as u16;
    let mut r = Row::line(Line::from(spans), Some(msg));
    if let Some((cols, rows, index)) = p.pic {
        if let RowKind::Line(l) = &r.kind {
            r.under = Some(l.clone());
        }
        r.kind = RowKind::Image { x: c, cols, rows, index, after: None, preview: true };
        r.preview = true;
        r.picture = Some((c, c + cols));
        return r;
    }
    r.header = p.header;
    if p.text {
        r.text = Some((c, c + w as u16));
    }
    r.links = p.links.into_iter().map(|(a, b, u)| (a + c, b + c, u)).collect();
    r.names = p.names.into_iter().map(|(a, b, j)| (a + c, b + c, j)).collect();
    r.fixes = p.fixes.into_iter().map(|(a, b, f)| (a + c, b + c, f)).collect();
    r
}

/// The chat: its photo and name at the top, its rows under it, scrolled as the view is.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(jid) = app.current().map(String::from) else { return };
    // The photo at the top left, square and as tall as the name after it, big in the chat's colour (bold on one line
    // where there's no room); its cells kept while it's decoded, so the name doesn't move.
    let name = app.store.name_of(&jid);
    let color = text::color_for(&jid);
    let has_photo = app.store.avatar(&jid).is_some() && !app.caps.no_images;
    let room = area.width.saturating_sub(2 + if has_photo { 9 } else { 0 }) as usize;
    let big = if f.area().height >= 16 && app.caps.utf8 { big_text(&name, room) } else { None };
    let head: Vec<String> = big.clone().unwrap_or_else(|| vec![text::truncate(&name, room)]);
    let head_h = head.len() as u16;
    let photo_cols = if has_photo { head_h * 2 } else { 0 };
    let style = if big.is_some() { Style::new().fg(color) } else { Style::new().fg(color).add_modifier(Modifier::BOLD) };
    let x0 = area.x + 1 + if has_photo { photo_cols + 1 } else { 0 };
    let name_w = head.iter().map(|l| text::width(l)).max().unwrap_or(0) as u16;
    for (k, l) in head.iter().enumerate() {
        f.render_widget(Paragraph::new(Span::styled(l.clone(), style)), Rect { x: x0, y: area.y + k as u16, width: area.width.saturating_sub(x0 - area.x), height: 1 });
    }
    app.head_area = Rect { height: head_h, ..area };
    app.head_name = Some(Rect { x: x0, y: area.y, width: name_w, height: head_h });
    let font = app.picker.font_size();
    let cell = Size::new(font.width, font.height);
    app.head_photo = None;
    if has_photo {
        let r = Rect { x: area.x + 1, y: area.y, width: photo_cols.max(2), height: head_h };
        app.head_photo = Some(r);
        for k in 0..head_h {
            app.hits.push((r.x, r.y + k, r.width, Hit::HeadPhoto));
        }
        if let Some(img) = app.avatar(&jid) {
            draw_whole(f, app, &format!("avatar:{jid}"), &img, r, cell);
        }
    }
    let body = Rect { y: area.y + head_h, height: area.height.saturating_sub(head_h), ..area };
    let body = Rect { x: body.x + 1, width: body.width.saturating_sub(1), ..body };
    app.chat_area = body;

    // Laid out again when the messages or the width change; scrolled up, the message at the top stays where it is.
    if app.rows_dirty || app.rows_width != body.width {
        if app.rendered_jid.as_deref() == Some(jid.as_str()) {
            app.capture_anchor();
        }
        // What was under the pointer's selection or a correction floating may be elsewhere now.
        app.text_sel = None;
        app.sent_fix = None;
        let was_loading = app.loading_drawn && app.rendered_jid.as_deref() == Some(jid.as_str());
        app.rows = layout(app, body.width, cell);
        app.rows_width = body.width;
        app.rows_dirty = false;
        app.layout_gen += 1;
        let loading = app.older_pending.contains_key(&jid);
        let height = body.height as usize;
        let max_top = app.rows.len().saturating_sub(height);
        if app.at_bottom {
            app.top = max_top;
        } else if loading && !was_loading && app.top == 0 {
            app.top = 0;
        } else if let Some((id, off)) = app.anchor.take()
            && let Some(first) = app.rows.iter().position(|r| r.msg.and_then(|i| app.msgs.get(i)).is_some_and(|x| x.id == id)) {
                app.top = ((first as i64 + off).max(0) as usize).min(max_top);
            }
        app.anchor = None;
        app.top = app.top.min(max_top);
        app.rendered_jid = Some(jid.clone());
        app.loading_drawn = loading;
    }
    let total = app.rows.len();
    let height = body.height as usize;
    let selected = app.selected.as_deref().and_then(|id| app.msgs.iter().position(|m| m.id == id));
    if std::mem::take(&mut app.keep_selected_in_view)
        && let Some(sel) = selected {
            let first = app.rows.iter().position(|r| r.msg == Some(sel));
            let last = app.rows.iter().rposition(|r| r.msg == Some(sel));
            if let (Some(first), Some(last)) = (first, last) {
                if first < app.top {
                    app.top = first;
                } else if last + 1 > app.top + height {
                    app.top = (last + 1).saturating_sub(height);
                }
            }
        }
    let max_top = total.saturating_sub(height);
    app.top = app.top.min(max_top);
    app.at_bottom = app.top >= max_top;
    // A browser scrolls the chat itself: moved from here, it's told to follow.
    let web = app.web.is_some();
    if web && app.page_top != Some(app.top) {
        app.scroll_gen += 1;
        app.page_top = Some(app.top);
    }
    let start = app.top;
    let end = (start + height).min(total);
    // Few messages start at the top of the panel, as in wassup.
    let pad = 0u16;

    // Each picture in view: its message, column, first screen row, width, full height, rows in view, first row of
    // it, and whether it's a link's preview.
    type InView = (usize, u16, u16, u16, u16, u16, u16, bool);
    let mut pictures: Vec<InView> = Vec::new();
    let mut emojis: Vec<(usize, u16, u16)> = Vec::new();
    let pointed = app.hover.as_deref().and_then(|id| app.msgs.iter().position(|m| m.id == id));
    let quick = app.quick_for.as_deref().and_then(|id| app.msgs.iter().position(|m| m.id == id));
    let dragged = app.drag.as_ref().and_then(|(id, dx)| app.msgs.iter().position(|m| m.id == *id).map(|i| (i, *dx)));
    let mut icon: Option<(usize, u16, u16)> = None;
    let rows: Vec<Row> = app.rows[start..end].to_vec();
    for (k, row) in rows.iter().enumerate() {
        let y = body.y + pad + k as u16;
        let full = Rect { y, height: 1, ..body };
        let mut rect = full;
        if let (Some((m, dx)), Some(rm)) = (dragged, row.msg)
            && m == rm && dx > 0 {
                rect = Rect { x: rect.x + dx, width: rect.width.saturating_sub(dx), ..rect };
            }
        if let (Some(m), Some(x)) = (row.msg, row.icon)
            && Some(m) == quick.or(pointed) {
                let shift = dragged.filter(|(d, _)| *d == m).map(|(_, dx)| dx).unwrap_or(0);
                icon = Some((m, x + shift, y));
            }
        paint_row(f.buffer_mut(), row, rect, full, selected.is_some() && row.msg == selected);
        match &row.kind {
            RowKind::Line(_) => {}
            RowKind::Image { x, cols, rows, index, preview, .. } => {
                let msg = row.msg.unwrap_or(0);
                match pictures.last_mut() {
                    Some(p) if p.0 == msg && p.7 == *preview => p.5 += 1,
                    _ => pictures.push((msg, *x, y, *cols, *rows, 1, *index, *preview)),
                }
            }
            RowKind::Emoji { x, index, .. } => {
                if *index == 0 && k + 1 < end - start {
                    emojis.push((row.msg.unwrap_or(0), *x, y));
                }
            }
        }
    }
    // A browser has every picture of the chat from the panel (web_panel): those in view are drawn as in a terminal,
    // for what that asks for (the file, the decoding), but not passed on twice.
    let pics_before = app.web_pics.len();
    for (msg, x, y, cols, full_rows, vis, first, preview) in pictures {
        let rect = Rect { x: body.x + x, y, width: cols, height: vis };
        if !draw_picture(f, app, msg, rect, full_rows, first, cell, preview) {
            let m = &app.msgs[msg];
            let what = if preview { s("linkPreview") } else if m.kind == "sticker" { s("sticker") } else if m.kind == "image" { s("image") } else { m.kind.clone() };
            let failed = app.failed.get(&format!("{}{}/{}", if preview { "link:" } else { "" }, m.chat, m.id)).cloned();
            let label = if let Some(e) = failed {
                t("mediaUnreadable", &[&what, &e])
            } else {
                format!("[{what} {}]", if m.media_path.is_some() { s("loading") } else { s("downloading") })
            };
            f.render_widget(Paragraph::new(Span::styled(label, Style::new().fg(theme().dim))), Rect { height: 1, ..rect });
        }
    }
    for (msg, x, y) in emojis {
        let text = app.msgs[msg].text.clone();
        for (k, e) in unicode_segmentation::UnicodeSegmentation::graphemes(text.trim(), true).filter(|g| !g.trim().is_empty()).enumerate() {
            // Where Kitty draws text bigger itself, it does (OSC 66, after the frame); elsewhere a picture.
            if app.caps.text_sizing {
                app.big_text.push((body.x + x + 4 * k as u16, y, e.to_string()));
                continue;
            }
            // The page draws them as text, twice the size.
            if app.web.is_some() {
                app.web_pics.push(crate::app::WebPic::Emoji { emoji: e.to_string(), x: body.x + x + 4 * k as u16, y });
                continue;
            }
            if let Some(img) = crate::emoji::picture(e) {
                draw_whole(f, app, &format!("emoji:{e}"), &img, Rect { x: body.x + x + 4 * k as u16, y, width: 4, height: 2 }, cell);
            }
        }
    }
    if web {
        app.web_pics.truncate(pics_before);
        app.web_panel = Some(web_panel(app, &jid, body));
    }
    if let Some((m, x, y)) = icon
        && app.viewing.is_none() && !app.showing_qr {
            draw_quick(f, app, m, body.x + x, y, body);
        }
    // The rising emoji asked for, once their message is in view: born on the emoji's cell, rising to the top of the
    // screen, over the chat's name.
    let sky = Rect { y: area.y, height: body.y + body.height - area.y, ..body };
    let now = std::time::Instant::now();
    let wanted: Vec<_> = std::mem::take(&mut app.hearts_wanted);
    for h in wanted {
        let at = if let Some(text) = h.msg.strip_prefix("mine:") {
            app.msgs.iter().rposition(|m| m.from_me && m.text == text && chrono::Utc::now().timestamp() - m.ts < 30)
        } else {
            app.msgs.iter().position(|m| m.id == h.msg)
        };
        // Born where the emoji is: twice its size, over its second row; otherwise on the message's last row with it
        // (its reactions' row, for a reaction), at its column.
        let tamed = crate::unicode::tame(&h.emoji).into_owned();
        let spot = at.and_then(|i| {
            (start..end).rev().filter(|r| app.rows[*r].msg == Some(i)).find_map(|r| match &app.rows[r].kind {
                RowKind::Emoji { x, index: 1, .. } => Some((r, x + 1)),
                RowKind::Line(l) => {
                    let plain: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                    plain.find(&h.emoji).or_else(|| plain.find(&tamed)).map(|p| (r, text::width(&plain[..p]) as u16))
                }
                _ => None,
            })
        });
        match (spot, crate::emoji::picture(&h.emoji)) {
            (Some((r, col)), Some(pic)) => {
                let y = body.y + pad + (r - start) as u16;
                let x = body.x + col;
                if let Some(a) = crate::hearts::Animation::new(&pic, sky, Some(ratatui::layout::Position { x, y }), now) {
                    app.hearts.push(a);
                }
            }
            (None, Some(pic)) if h.since.elapsed().as_millis() > 500 => {
                let _ = pad;
                if let Some(a) = crate::hearts::Animation::new(&pic, sky, None, now) {
                    app.hearts.push(a);
                }
            }
            (None, Some(_)) => app.hearts_wanted.push(h),
            _ => {}
        }
    }
}

/// One of the chat's rows into `buf`: its line at `rect` (moved right while it's dragged), or what goes around a
/// picture or emoji on their own (the bubble's row under a preview, the description beside it, the time after it);
/// on the selection's grey across `full`, the row's whole width, when its message is the one selected.
fn paint_row(buf: &mut Buffer, row: &Row, rect: Rect, full: Rect, selected: bool) {
    if selected {
        Paragraph::new("").style(Style::new().bg(theme().selected)).render(full, buf);
    }
    let beside = |buf: &mut Buffer, ax: u16, t: &str, style: Style| {
        let r = Rect { x: full.x + ax, width: full.width.saturating_sub(ax), ..full };
        Paragraph::new(Span::styled(t.to_string(), style)).render(r, buf);
    };
    match &row.kind {
        RowKind::Line(line) => Paragraph::new(line.clone()).render(rect, buf),
        RowKind::Image { after, .. } => {
            if let Some(l) = &row.under {
                Paragraph::new(l.clone()).render(rect, buf);
            }
            if let Some((ax, t)) = &row.aside {
                beside(buf, *ax, t, Style::new().fg(theme().dim).add_modifier(Modifier::ITALIC));
            }
            if let Some((ax, t)) = after {
                beside(buf, *ax, t, Style::new().fg(theme().faint));
            }
        }
        RowKind::Emoji { after, .. } => {
            if let Some((ax, t)) = after {
                beside(buf, *ax, t, Style::new().fg(theme().faint));
            }
        }
    }
}

/// The chat for a browser (webtab.rs), which scrolls it itself: every row drawn plain, again only once they're laid
/// out again, and every picture by its first row, from what's here already (the file downloaded, or the thumbnail
/// the message came with) and never asked for: those in view are, by the frame, as in a terminal. The rows in view
/// as the frame ends up with them are added once it's drawn (ui.rs).
fn web_panel(app: &mut App, jid: &str, body: Rect) -> WebPanel {
    let w = body.width;
    let kept = app.panel_rows.as_ref().filter(|(g, pw, _)| *g == app.layout_gen && *pw == w).map(|(_, _, r)| r.clone());
    let rows = match kept {
        Some(r) => r,
        None => {
            let selected = app.selected.as_deref().and_then(|id| app.msgs.iter().position(|m| m.id == id));
            let r = Rect::new(0, 0, w, 1);
            let drawn: Vec<Buffer> = app
                .rows
                .iter()
                .map(|row| {
                    let mut buf = Buffer::empty(r);
                    paint_row(&mut buf, row, r, r, selected.is_some() && row.msg == selected);
                    buf
                })
                .collect();
            let drawn = Arc::new(drawn);
            app.panel_rows = Some((app.layout_gen, w, drawn.clone()));
            drawn
        }
    };
    let mut pics = vec![];
    for k in 0..app.rows.len() {
        let msg = app.rows[k].msg.unwrap_or(0);
        match app.rows[k].kind.clone() {
            RowKind::Image { x, cols, rows: tall, index: 0, preview, .. } => {
                if let Some(p) = panel_picture(app, msg, preview, x, cols, tall) {
                    pics.push((k, p));
                }
            }
            RowKind::Emoji { x, index: 0, .. } => {
                let text = app.msgs.get(msg).map(|m| m.text.clone()).unwrap_or_default();
                let each = unicode_segmentation::UnicodeSegmentation::graphemes(text.trim(), true).filter(|g| !g.trim().is_empty());
                for (i, e) in each.enumerate() {
                    pics.push((k, WebPic::Emoji { emoji: e.to_string(), x: x + 4 * i as u16, y: 0 }));
                }
            }
            _ => {}
        }
    }
    WebPanel { key: jid.to_string(), area: body, layout: app.layout_gen, rows, view: vec![], top: app.top, scroll: app.scroll_gen, pics }
}

/// A message's picture for the panel, whole, at column `x`: its file if it's here (a link's preview, an image or
/// sticker downloaded), which the page asks for itself; else the picture decoded already, or the thumbnail the
/// message came with (decoded once and kept apart). None with neither.
fn panel_picture(app: &mut App, msg: usize, preview: bool, x: u16, cols: u16, tall: u16) -> Option<WebPic> {
    let m = app.msgs.get(msg)?.clone();
    let key = format!("{}{}/{}", if preview { "link:" } else { "" }, m.chat, m.id);
    let file = if preview {
        let p = crate::wa::preview_path(&app.media_dir(), &m.chat, &m.id);
        p.exists().then(|| p.to_string_lossy().into_owned())
    } else if matches!(m.kind.as_str(), "image" | "sticker") {
        m.media_path.clone()
    } else {
        None
    };
    let png = match app.images.get(&key) {
        Some(img) => Some(img.clone()),
        None if preview => None,
        None => match app.thumbs.get(&key) {
            Some(img) => Some(img.clone()),
            None => {
                let img = Arc::new(image::load_from_memory(m.thumb.as_deref()?).ok()?);
                if app.thumbs.len() > 1000 {
                    app.thumbs.clear();
                }
                app.thumbs.insert(key.clone(), img.clone());
                Some(img)
            }
        },
    };
    if file.is_none() && png.is_none() {
        return None;
    }
    Some(WebPic::Image { key, file, png, x, y: 0, w: cols, h: tall, from: 0.0, to: 1.0 })
}

/// The "☺" a message under the pointer shows beside its time, grey, bold under the pointer; clicked, the quick
/// reactions in its place, mine's leftwards, "⋯" last for any other.
fn draw_quick(f: &mut Frame, app: &mut App, msg: usize, x: u16, y: u16, body: Rect) {
    let id = app.msgs[msg].id.clone();
    let mine = app.msgs[msg].from_me;
    let over = |app: &App, x0: u16, w: u16| app.pointer.is_some_and(|(px, py)| py == y && px + 1 >= x0 && px <= x0 + w);
    if app.quick_for.as_deref() != Some(id.as_str()) {
        let style = if over(app, x, 1) { Style::new().add_modifier(Modifier::BOLD) } else { Style::new().fg(Color::Indexed(244)) };
        f.render_widget(Paragraph::new(Span::styled("☺", style)), Rect { x, y, width: 1, height: 1 });
        app.hits.push((x.saturating_sub(1), y, 3, Hit::Smile(id)));
        return;
    }
    let widths: Vec<u16> = QUICK.iter().map(|e| if *e == "⋯" { 1 } else { 2 }).collect();
    let total = widths.iter().sum::<u16>() + (QUICK.len() as u16 - 1) * 2;
    let mut at = if mine { (x + 1).saturating_sub(total) } else { x };
    at = at.clamp(body.x, (body.x + body.width).saturating_sub(total));
    for (e, w) in QUICK.iter().zip(widths) {
        let hot = over(app, at, w);
        let style = if hot { Style::new().bg(theme().selected) } else { Style::new() };
        f.render_widget(Paragraph::new(Span::styled(*e, style)), Rect { x: at, y, width: w, height: 1 });
        app.hits.push((at.saturating_sub(1), y, w + 2, Hit::React(id.clone(), e.to_string())));
        at += w + 2;
    }
}

/// Draws a picture whole into the cells of `rect`, cut to their shape, kept ready for the terminal under `key`.
pub fn draw_whole(f: &mut Frame, app: &mut App, key: &str, img: &Arc<DynamicImage>, rect: Rect, cell: Size) {
    if app.picker.protocol_type() == ProtocolType::Halfblocks && app.web.is_none() {
        let k = (format!("blocks:{key}"), rect.width, rect.height);
        let fitted = app.scaled.entry(k).or_insert_with(|| Arc::new(img.resize_to_fill(rect.width.max(1) as u32, rect.height.max(1) as u32 * 2, image::imageops::FilterType::Triangle))).clone();
        half_blocks(f.buffer_mut(), rect, &fitted, 0);
        return;
    }
    if app.web.is_some() || app.kitty.is_some() {
        let file = key.strip_prefix("avatar:").and_then(|j| app.store.avatar(j));
        app.web_pics.push(crate::app::WebPic::Image { key: key.to_string(), file, png: Some(img.clone()), x: rect.x, y: rect.y, w: rect.width, h: rect.height, from: 0.0, to: 1.0 });
        return;
    }
    let crop_key = (key.to_string(), rect.width, rect.height, 0, rect.height);
    if !app.crops.contains_key(&crop_key) {
        let (pw, ph) = (rect.width as u32 * cell.width as u32, rect.height as u32 * cell.height as u32);
        let fitted = composite(img.resize_to_fill(pw.max(1), ph.max(1), image::imageops::FilterType::Triangle), app);
        if let Ok(proto) = app.picker.new_protocol(fitted, Size::new(rect.width, rect.height), Resize::Fit(None)) {
            app.crops.insert(crop_key.clone(), proto);
        }
    }
    if let Some(proto) = app.crops.get(&crop_key) {
        f.render_widget(Image::new(proto), rect);
    }
}

/// A picture in half-blocks, two of its pixels a cell, as wassup's: from pixel row `from` of `img` (already as
/// wide as `rect` and twice as tall), where it's almost transparent (under a quarter) the terminal's own
/// background left.
pub fn half_blocks(buf: &mut ratatui::buffer::Buffer, rect: Rect, img: &DynamicImage, from: u32) {
    let rgba = img.to_rgba8();
    let at = |x: u32, y: u32| rgba.get_pixel_checked(x, y).filter(|p| p[3] >= 64).map(|p| Color::Rgb(p[0], p[1], p[2]));
    for cy in 0..rect.height {
        for cx in 0..rect.width {
            let (x, y) = (cx as u32, from + cy as u32 * 2);
            let (top, bottom) = (at(x, y), at(x, y + 1));
            let Some(cell) = buf.cell_mut((rect.x + cx, rect.y + cy)) else { continue };
            match (top, bottom) {
                (None, None) => continue,
                (Some(t), b) => {
                    cell.set_symbol("▀").set_fg(t).set_bg(b.unwrap_or(Color::Reset));
                }
                (None, Some(b)) => {
                    cell.set_symbol("▄").set_fg(b).set_bg(Color::Reset);
                }
            }
        }
    }
}

/// With sixels, a picture's transparent parts on the terminal's background (they'd come out black).
fn composite(img: DynamicImage, app: &App) -> DynamicImage {
    if app.picker.protocol_type() != ProtocolType::Sixel || !img.color().has_alpha() {
        return img;
    }
    let bg = theme().bg;
    let mut rgba = img.to_rgba8();
    for p in rgba.pixels_mut() {
        let a = p[3] as u32;
        for c in 0..3 {
            p[c] = ((p[c] as u32 * a + bg[c] as u32 * (255 - a)) / 255) as u8;
        }
        p[3] = 255;
    }
    DynamicImage::ImageRgba8(rgba)
}

/// Draws the part in view (rows `first`.. of `full_rows`) of a message's picture into `rect`; false while it has
/// none to show yet.
#[allow(clippy::too_many_arguments)]
fn draw_picture(f: &mut Frame, app: &mut App, msg: usize, rect: Rect, full_rows: u16, first: u16, cell: Size, preview: bool) -> bool {
    let m = app.msgs[msg].clone();
    let key = format!("{}{}/{}", if preview { "link:" } else { "" }, m.chat, m.id);
    if app.web.is_some() || app.kitty.is_some() {
        let img = if preview { app.preview(&m) } else { app.picture(&m) };
        let Some(img) = img else { return false };
        let file = if preview { Some(crate::wa::preview_path(&app.media_dir(), &m.chat, &m.id).to_string_lossy().into_owned()) } else if app.full.contains(&key) { m.media_path.clone() } else { None };
        let (from, to) = (first as f32 / full_rows as f32, (first + rect.height) as f32 / full_rows as f32);
        app.web_pics.push(crate::app::WebPic::Image { key, file, png: Some(img), x: rect.x, y: rect.y, w: rect.width, h: full_rows, from, to });
        return true;
    }
    if app.picker.protocol_type() == ProtocolType::Halfblocks {
        let img = if preview { app.preview(&m) } else { app.picture(&m) };
        let Some(img) = img else { return false };
        let k = (format!("blocks:{key}"), rect.width, full_rows);
        let scaled = app.scaled.entry(k).or_insert_with(|| Arc::new(img.resize_exact(rect.width.max(1) as u32, full_rows.max(1) as u32 * 2, image::imageops::FilterType::Triangle))).clone();
        half_blocks(f.buffer_mut(), rect, &scaled, first as u32 * 2);
        return true;
    }
    let crop_key = (key.clone(), rect.width, full_rows, first, rect.height);
    if !app.crops.contains_key(&crop_key) {
        let img = if preview { app.preview(&m) } else { app.picture(&m) };
        let Some(img) = img else { return false };
        let scaled_key = (key.clone(), rect.width, full_rows);
        let scaled = match app.scaled.get(&scaled_key) {
            Some(s) => s.clone(),
            None => {
                let (pw, ph) = (rect.width as u32 * cell.width as u32, full_rows as u32 * cell.height as u32);
                let s: Arc<DynamicImage> = Arc::new(composite(img.resize_exact(pw.max(1), ph.max(1), image::imageops::FilterType::Triangle), app));
                app.scaled.insert(scaled_key, s.clone());
                s
            }
        };
        let part = scaled.crop_imm(0, first as u32 * cell.height as u32, scaled.width(), rect.height as u32 * cell.height as u32);
        let Ok(proto) = app.picker.new_protocol(part, Size::new(rect.width, rect.height), Resize::Fit(None)) else { return false };
        if app.crops.len() > 300 {
            app.crops.clear();
        }
        app.crops.insert(crop_key.clone(), proto);
    }
    if let Some(proto) = app.crops.get(&crop_key) {
        f.render_widget(Image::new(proto), rect);
    }
    true
}
