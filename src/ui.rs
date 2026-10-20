//! The frame, as wassup lays it out: the chat list or the open chat above; under it the band where you write (on my
//! bubbles' green, growing with the text; the list's filter too; a plain line under a faint rule while searching a
//! chat); under that the tabs, with the state at the right. Over them what floats: the emoji suggestions, the
//! model's correction, the notice of a message elsewhere fading in and out over its tab, the popup with a picture,
//! the message just sent flying to its place, the emoji rising, the text selected.

use std::io::Write;

use base64::Engine;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use ratatui_image::{Image, Resize};

use crate::app::{App, Focus, GhostView, Hit, SUGGEST_ROWS, notice_opacity};
use crate::compose;
use crate::i18n::{s, t};
use crate::text::{self, theme};

/// Draws a frame, then what goes to the terminal beside it: the title, the bell, the clipboard.
pub fn frame<B: ratatui::backend::Backend>(app: &mut App, terminal: &mut Terminal<B>) -> anyhow::Result<()> {
    if std::mem::take(&mut app.clear_screen) {
        terminal.clear().map_err(|e| anyhow::anyhow!("{e:?}"))?;
    }
    app.web_pics.clear();
    terminal
        .draw(|f| {
            draw(f, app);
            crate::unicode::settle(f.buffer_mut());
            // In the terminal the band's cursor is the terminal's own, shown there; without it, hidden.
            if app.web.is_none()
                && let Some(at) = app.caret
            {
                f.set_cursor_position(at);
            }
        })
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    if let Some(w) = app.web.clone() {
        // The page: its pictures, its title and icon, what to copy; the rest is the frame (webtab.rs).
        let _ = w.send(crate::app::WebMsg::Frame(std::mem::take(&mut app.web_pics), app.web_panel.take()));
        // The page's tab says what wassup's tab does: the list's name and mark, or the chat's; its icon the chat's.
        let jid = if app.picker_open { None } else { app.current().map(String::from) };
        let title = match &jid {
            None => format!("wassup{}", app.list_mark()),
            Some(j) => format!("{}{}", app.store.name_of(j), app.tab_mark(j)),
        };
        if title != app.title_shown {
            app.title_shown = title.clone();
            let _ = w.send(crate::app::WebMsg::Title(title));
        }
        let (photo, initials, color) = match &jid {
            None => (None, None, None),
            Some(j) => match app.store.avatar(j) {
                Some(p) => (Some(p), None, None),
                None => (None, Some(text::initials(&app.store.name_of(j))), Some(hex(text::color_for(j)))),
            },
        };
        let key = format!("{photo:?}{initials:?}{color:?}");
        if key != app.icon_shown {
            app.icon_shown = key;
            let _ = w.send(crate::app::WebMsg::Icon { photo, initials, color });
        }
        if let Some(text) = app.clipboard.take().filter(|t| !t.is_empty()) {
            let _ = w.send(crate::app::WebMsg::Copy(text));
            app.flash(s("copied"), 6);
        }
        return Ok(());
    }
    let mut out = std::io::stdout();
    // Kitty's pictures, placed over their cells after the frame, each sent once.
    if let Some(k) = app.kitty.as_mut() {
        k.clear(&mut out);
        for p in std::mem::take(&mut app.web_pics) {
            if let crate::app::WebPic::Image { key, png: Some(img), x, y, w, h, from, to, .. } = p {
                let rows = ((to - from) * h as f32).round().max(1.0) as u16;
                k.place(&mut out, &format!("{key}#{}x{}", img.width(), img.height()), &img, x, y, w, rows, from, to);
            }
        }
    }
    // Emoji twice their size, written by the terminal over their blank cells (worked out with the frame).
    if let Some(s) = app.big_out.take() {
        let _ = write!(out, "{s}");
    }
    if let Some(title) = app.update_title() {
        let _ = write!(out, "\x1b]2;{title}\x07");
    }
    if std::mem::take(&mut app.bell) {
        let _ = write!(out, "\x07");
    }
    if let Some(text) = app.clipboard.take().filter(|t| !t.is_empty()) {
        let _ = write!(out, "\x1b]52;c;{}\x1b\\", base64::engine::general_purpose::STANDARD.encode(text.as_bytes()));
        app.flash(s("copied"), 6);
    }
    let _ = out.flush();
    Ok(())
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.hits.clear();
    app.web_panel = None;
    if area.height < 5 || area.width < 16 {
        app.page_top = None;
        return;
    }
    let tabs = Rect { y: area.y + area.height - 1, height: 1, ..area };
    let band_h = draw_band(f, app, area);
    let above = Rect { height: area.height.saturating_sub(band_h + 1), ..area };
    if app.showing_qr && app.qr.is_some() {
        draw_qr(f, above, &app.qr.clone().unwrap_or_default());
    } else if app.picker_open {
        crate::list::draw(f, app, above);
    } else if app.current().is_some() {
        crate::chat::draw(f, app, above);
    }
    // The band again over whatever the panels drew into its rows (a picture's last cells).
    draw_band(f, app, area);
    draw_band_photo(f, app);
    draw_tabs(f, app, tabs);
    draw_notice(f, app, app.tabs_area);
    draw_suggestions(f, app);
    draw_ghost(f, app);
    draw_sent_fix(f, app);
    draw_flight(f, app);
    // With the popup open, only its picture is placed.
    if app.viewing.is_some() {
        app.web_pics.clear();
        if let Some(p) = app.web_panel.as_mut() {
            p.pics.clear();
        }
    }
    draw_viewer(f, app, area);
    big_text(f, app);
    // The rising emoji, over everything.
    let now = std::time::Instant::now();
    let bg = theme().bg;
    let buf = f.buffer_mut();
    app.hearts.retain_mut(|h| !h.draw(buf, now, bg));
    // Without 24-bit colour, every colour given in it comes down to the palette's nearest.
    if !theme().truecolor && app.web.is_none() {
        for cell in f.buffer_mut().content.iter_mut() {
            if matches!(cell.fg, Color::Rgb(..)) {
                cell.fg = nearest256(cell.fg);
            }
            if matches!(cell.bg, Color::Rgb(..)) {
                cell.bg = nearest256(cell.bg);
            }
        }
    }
    draw_text_sel(f, app);
    // For a browser, the chat's rows in view as the frame ended up with them, what floats over them included; with
    // no chat drawn, the page has none on top.
    match app.web_panel.as_mut() {
        Some(p) => {
            let (a, buf) = (p.area, f.buffer_mut());
            p.view = (0..a.height)
                .map(|k| {
                    let mut row = ratatui::buffer::Buffer::empty(Rect::new(0, 0, a.width, 1));
                    for x in 0..a.width {
                        if let Some(c) = buf.cell((a.x + x, a.y + k)) {
                            row[(x, 0)] = c.clone();
                        }
                    }
                    row
                })
                .collect();
        }
        None => app.page_top = None,
    }
}

/// The band: in a chat, as my next message on my bubbles' green, two rows at least, a row over them for what's being
/// answered, reacted to, edited or deleted; the list's filter in it too. Searching a chat, a plain line after
/// "procurar ❯" under a faint rule that says how many were found. Returns the rows it took.
fn draw_band(f: &mut Frame, app: &mut App, area: Rect) -> u16 {
    let w = area.width as usize;
    let searching = app.search.is_some() && !app.picker_open;
    let plain = !app.picker_open && (searching || app.current().is_none());
    let stamp_w = text::width(&text::my_time(0, 0)) + 1;
    // In a chat with Kitty, its photo left of the band, which starts that much further right.
    let by_band = photo_beside(app);
    let off = band_off(app) as usize;
    let lead = " ".repeat(1 + off);
    let band_w = w.saturating_sub(4 + stamp_w + off);
    let who = if searching { s("search") } else { String::new() };
    let pw = if plain { 1 + if who.is_empty() { 0 } else { text::width(&who) + 1 } + 2 } else { 2 + off };
    app.prompt_w = pw as u16;
    let target = if app.picker_open || searching { None } else { app.reply_to.clone().or(app.react_to.clone()).or(app.editing.clone()) };
    let found = app.search.as_ref().filter(|q| !q.query.is_empty()).map(|q| if q.hits.is_empty() { s("searchNone") } else { t("searchHeader", &[&(q.at + 1), &q.hits.len()]) });
    let header: Option<String> = if searching {
        Some(match found {
            Some(f) => format!("{} · {f}", s("search")),
            None => s("search"),
        })
    } else if let Some((_, everyone)) = &app.deleting {
        Some(s(if *everyone { "deleteForAll" } else { "deleteForMe" }))
    } else if let Some(m) = target.as_deref().and_then(|id| app.msg(id)).cloned() {
        let said = m.text.lines().next().filter(|l| !l.is_empty()).map(String::from).unwrap_or_else(|| format!("[{}]", m.kind));
        let said = crate::app::bare_text(&said).to_string();
        let who = if m.from_me { s("me") } else if text::is_group(&m.chat) { app.store.name_of(&m.sender) } else { app.store.name_of(&m.chat) };
        Some(if app.editing.is_some() {
            t("editHeader", &[&said])
        } else if app.reply_to.is_some() {
            let who = if text::is_group(&m.chat) && !m.from_me { format!("{who}: ") } else { String::new() };
            let (said, _) = text::with_mentions(&said, |d| app.store.mention_name(d));
            format!("↩ {who}{said}")
        } else {
            t("reactHeader", &[&who, &said])
        })
    } else {
        None
    };
    let view = if searching || app.picker_open { None } else { app.ghost_shown() };
    let width = (if plain { w.saturating_sub(pw + 1) } else { band_w }).max(4);
    let (value, cursor) = if app.picker_open {
        (app.filter.clone(), app.filter_cursor)
    } else if let Some(s) = &app.search {
        (s.query.clone(), s.cursor)
    } else {
        (app.input.clone(), app.cursor)
    };
    let gs = compose::graphemes(&value);
    let cursor = cursor.min(gs.len());
    let mut lines = compose::wrap_chars(&gs, width);
    let mut row = 0;
    let mut start = 0;
    while row + 1 < lines.len() && cursor >= start + lines[row].len() {
        start += lines[row].len();
        row += 1;
    }
    let mut col = cursor - start;
    let drawn = |l: &[String]| l.iter().filter(|g| *g != "\n").cloned().collect::<String>();
    if col >= lines[row].len() && text::width(&drawn(&lines[row])) >= width {
        lines.push(vec![]);
        row += 1;
        col = 0;
    }
    // The model's suggestion: the missing letters at the cursor, or a word floating over the one it replaces.
    let cursor_line = drawn(&lines[row]);
    let avail = width.saturating_sub(text::width(&cursor_line) + 1);
    let float_key = match &view {
        Some(GhostView::Fix(f)) => format!("{}:{}", f.start, f.to),
        Some(GhostView::Word(_, to)) | Some(GhostView::Suffix(_, _, to)) => format!("word:{to}"),
        None => String::new(),
    };
    if float_key != app.float_key {
        app.float_key = float_key.clone();
        app.float_since = std::time::Instant::now();
    }
    let mut ghost_next = String::new();
    app.ghost_float = None;
    if let Some(v) = view.as_ref().filter(|_| float_key != app.float_off) {
        let on_word = matches!(v, GhostView::Fix(_)) || col >= lines[row].len();
        if on_word {
            match v {
                GhostView::Suffix(rest, ..) if text::width(rest) <= avail + 1 => ghost_next = rest.clone(),
                _ => {
                    let (word, start_char) = match v {
                        GhostView::Fix(f) => (f.to.clone(), f.start),
                        GhostView::Suffix(_, from, to) | GhostView::Word(from, to) => (to.clone(), app.input.chars().count().saturating_sub(from.chars().count())),
                    };
                    let mut at = compose::len(&app.input.chars().take(start_char).collect::<String>());
                    let mut wl = 0;
                    while wl + 1 < lines.len() && at >= lines[wl].len() {
                        at -= lines[wl].len();
                        wl += 1;
                    }
                    let shown = text::truncate(&word, width);
                    let gc = text::width(&lines[wl][..at.min(lines[wl].len())].concat()).min(width.saturating_sub(text::width(&shown)));
                    app.ghost_float = Some((shown, wl, gc));
                }
            }
        }
    }
    let header_rows = if header.is_some() && !plain { 1 } else { 0 };
    let min = if plain { 1 } else { 2 + header_rows };
    let rows = (lines.len() + header_rows).min(area.height as usize / 2).max(min);
    let rows_avail = (rows - header_rows).max(1);
    let first = row.saturating_sub(rows_avail - 1).min(lines.len().saturating_sub(rows_avail));
    let total_h = rows as u16 + if plain { 1 } else { 0 };
    let tabs_y = area.y + area.height - 1;
    let top = tabs_y - total_h;
    let show_cursor = app.focus == Focus::Input || app.focus == Focus::Picker;
    // The cursor drawn as a reversed cell only for a browser, which has no cursor of its own; in the terminal it's the
    // terminal's (app.caret).
    let reversed = if app.web.is_some() { Modifier::REVERSED } else { Modifier::empty() };
    // The passages the model found wrong, by their place in the whole text.
    let mut marked = std::collections::HashSet::new();
    if !app.picker_open && !searching {
        for f in app.live_fixes() {
            let a = compose::len(&app.input.chars().take(f.start).collect::<String>());
            let b = compose::len(&app.input.chars().take(f.end).collect::<String>());
            marked.extend(a..b);
        }
    }
    let line_start: Vec<usize> = lines.iter().scan(0, |at, l| { let s = *at; *at += l.len(); Some(s) }).collect();
    let band_bg = if plain { Color::Reset } else { theme().mine };
    let paint = |gs: &[String], from: usize| -> Vec<Span<'static>> {
        gs.iter()
            .enumerate()
            .filter(|(_, g)| *g != "\n")
            .map(|(k, g)| {
                let mut st = Style::new().bg(band_bg);
                if marked.contains(&(from + k)) {
                    st = st.fg(Color::Yellow).add_modifier(Modifier::UNDERLINED);
                }
                Span::styled(crate::unicode::tame(g).into_owned(), st)
            })
            .collect()
    };
    let mut out: Vec<Line<'static>> = Vec::new();
    if plain {
        // The faint rule over the plain line, with the search's count.
        let h = if app.caps.utf8 { "─" } else { "-" };
        let label = header.as_deref().map(|h| format!(" {} ", text::truncate(h, w.saturating_sub(6)))).unwrap_or_default();
        let rule = Line::from(vec![
            Span::styled(h.repeat(2), Style::new().fg(theme().faint)),
            Span::styled(label.clone(), Style::new().fg(theme().dim)),
            Span::styled(h.repeat(w.saturating_sub(2 + text::width(&label))), Style::new().fg(theme().faint)),
        ]);
        f.render_widget(Paragraph::new(rule), Rect { x: area.x, y: top, width: area.width, height: 1 });
    }
    for (i, l) in lines.iter().enumerate().skip(first).take(rows_avail) {
        let from = line_start.get(i).copied().unwrap_or(0);
        let mut spans: Vec<Span<'static>> = Vec::new();
        if plain {
            if i == 0 {
                spans.push(Span::raw(" "));
                if !who.is_empty() {
                    spans.push(Span::styled(who.clone(), Style::new().fg(theme().green)));
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::raw("❯ "));
            } else {
                spans.push(Span::raw(" ".repeat(pw)));
            }
        } else {
            spans.push(Span::raw(lead.clone()));
            spans.push(Span::styled(" ", Style::new().bg(band_bg)));
        }
        if show_cursor && i == row {
            spans.extend(paint(&l[..col.min(l.len())], from));
            if !ghost_next.is_empty() {
                let mut g = compose::graphemes(&ghost_next).into_iter();
                let first_g = g.next().unwrap_or("").to_string();
                let rest: String = g.collect();
                let dim = Style::new().bg(band_bg).fg(theme().dim).add_modifier(Modifier::ITALIC);
                spans.push(Span::styled(first_g, dim.add_modifier(reversed)));
                spans.push(Span::styled(rest, dim));
            } else {
                let under = l.get(col).filter(|g| *g != "\n").cloned().unwrap_or_else(|| " ".into());
                spans.push(Span::styled(under, Style::new().bg(band_bg).add_modifier(reversed)));
                if col < l.len() {
                    spans.extend(paint(&l[(col + 1).min(l.len())..], from + col + 1));
                }
            }
        } else {
            spans.extend(paint(l, from));
        }
        if !plain {
            let used: usize = spans.iter().skip(2).map(|s| text::width(&s.content)).sum();
            spans.push(Span::styled(" ".repeat(band_w.saturating_sub(used) + 1), Style::new().bg(band_bg)));
        }
        out.push(Line::from(spans));
    }
    if !plain {
        while out.len() < rows_avail {
            out.push(Line::from(vec![Span::raw(lead.clone()), Span::styled(" ".repeat(band_w + 2), Style::new().bg(band_bg))]));
        }
        if let Some(h) = &header {
            let style = if app.deleting.is_some() { Style::new().bg(band_bg).fg(Color::Red) } else { Style::new().bg(band_bg).fg(theme().dim) };
            let said = text::truncate(h, band_w);
            let pad = band_w.saturating_sub(text::width(&said));
            out.insert(0, Line::from(vec![Span::raw(lead.clone()), Span::styled(" ", Style::new().bg(band_bg)), Span::styled(said, style), Span::styled(" ".repeat(pad + 1), Style::new().bg(band_bg))]));
        }
    }
    let text_top = top + if plain { 1 } else { 0 };
    let band = Rect { x: area.x, y: text_top, width: area.width, height: rows as u16 };
    f.render_widget(Clear, band);
    f.render_widget(Paragraph::new(out), band);
    app.band_area = Rect { x: area.x, y: top, width: area.width, height: total_h };
    app.band_text = Rect { x: area.x + pw as u16, y: text_top + header_rows as u16, width: width as u16, height: rows_avail as u16 };
    app.caret = show_cursor.then(|| {
        let before: String = lines[row][..col.min(lines[row].len())].iter().filter(|g| *g != "\n").map(|g| crate::unicode::tame(g).into_owned()).collect();
        (app.band_text.x + text::width(&before) as u16, app.band_text.y + (row - first) as u16)
    });
    app.band_lines = lines;
    app.band_first = first;
    app.band_header = header_rows > 0;
    if by_band {
        app.head_photo = Some(Rect { x: area.x + 1, y: tabs_y + 1 - BAND_PHOTO_ROWS, width: BAND_PHOTO_ROWS * 2, height: BAND_PHOTO_ROWS });
    }
    // "↓" and how many are below, beside the band, while the chat is scrolled up; a click goes to the latest.
    app.below_at = None;
    if !plain && !app.picker_open && !app.at_bottom && app.current().is_some() {
        let below = below_count(app);
        let label = if below > 0 { format!("↓ {below}") } else { "↓".into() };
        let x = area.x + (off + band_w) as u16 + 4;
        let y = text_top + header_rows as u16;
        f.render_widget(Paragraph::new(Span::styled(label.clone(), Style::new().fg(theme().dim))), Rect { x, y, width: area.width.saturating_sub(x), height: 1 });
        app.hits.push((x, y, text::width(&label) as u16, Hit::Below));
        app.below_at = Some((x, y));
    }
    total_h
}

/// How tall the chat's photo left of the band is, square, as it was beside the chat's big name: the band's last two
/// rows and the tabs' row.
const BAND_PHOTO_ROWS: u16 = 3;

/// Whether the chat's photo is left of the band and the tabs: in a chat, not searching it, where photo_by_band says.
fn photo_beside(app: &App) -> bool {
    !app.picker_open && app.search.is_none() && app.current().is_some_and(|j| crate::chat::photo_by_band(app, j))
}

/// How much further right the band and the tabs start for the photo left of them.
fn band_off(app: &App) -> u16 {
    if photo_beside(app) { BAND_PHOTO_ROWS * 2 + 1 } else { 0 }
}

/// The chat's photo left of the band, where draw_band left room for it; a click opens it large.
fn draw_band_photo(f: &mut Frame, app: &mut App) {
    let (Some(r), Some(jid)) = (app.head_photo, app.current().map(String::from)) else { return };
    if !crate::chat::photo_by_band(app, &jid) {
        return;
    }
    for k in 0..r.height {
        app.hits.push((r.x, r.y + k, r.width, Hit::HeadPhoto));
    }
    if let Some(img) = app.avatar(&jid) {
        let font = app.picker.font_size();
        crate::chat::draw_whole(f, app, &format!("avatar:{jid}"), &img, r, Size::new(font.width, font.height));
    }
}

/// How many messages are below the view, all of their rows out of it.
fn below_count(app: &App) -> usize {
    let end = app.top + app.chat_area.height as usize;
    let mut ids = std::collections::HashSet::new();
    for r in app.rows.iter().skip(end) {
        if let Some(m) = r.msg {
            ids.insert(m);
        }
    }
    ids.len()
}

/// Emoji on their own drawn twice their size by the terminal (OSC 66), as wassup's paintBigEmoji: those whose two
/// rows of cells are still blank in this frame (nothing drawn over them, no popup), written again each frame; those
/// gone or covered, their cells written back as the frame has them.
fn big_text(f: &mut Frame, app: &mut App) {
    if !app.caps.text_sizing {
        return;
    }
    let buf = f.buffer_mut();
    let blank = |buf: &ratatui::buffer::Buffer, x: u16, y: u16, w: u16| (y..y + 2).all(|r| (x..x + w).all(|c| buf.cell((c, r)).is_some_and(|cell| cell.symbol() == " ")));
    let now: Vec<(u16, u16, String)> = if app.viewing.is_some() { vec![] } else { std::mem::take(&mut app.big_text).into_iter().filter(|(x, y, _)| blank(buf, *x, *y, 4)).collect() };
    app.big_text.clear();
    let mut s = String::from("\x1b7");
    for (x, y, e) in &app.big_shown {
        if now.iter().any(|(nx, ny, ne)| nx == x && ny == y && ne == e) {
            continue;
        }
        for r in [*y, y + 1] {
            s.push_str(&format!("\x1b[{};{}H", r + 1, x + 1));
            for c in *x..x + 4 {
                let (sym, bg) = buf.cell((c, r)).map(|cell| (cell.symbol().to_string(), cell.bg)).unwrap_or((" ".into(), Color::Reset));
                s.push_str(&format!("\x1b[0;{}m{sym}", sgr_bg(bg)));
            }
        }
    }
    for (x, y, e) in &now {
        s.push_str(&format!("\x1b[{};{}H\x1b[0m\x1b]66;s=2:w=2;{e}\x1b\\", y + 1, x + 1));
    }
    s.push_str("\x1b[0m\x1b8");
    app.big_out = Some(s);
    app.big_shown = now;
}

/// A background colour as SGR parameters.
fn sgr_bg(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("48;2;{r};{g};{b}"),
        Color::Indexed(i) => format!("48;5;{i}"),
        _ => "49".into(),
    }
}

/// The tabs: with the list open its own first ("wassup", or "procurar" searching them all); then one for each open
/// chat, its name, its mark (someone typing, 🔔, its mood); the one in front on my bubbles' green in its colour and
/// bold, the others on theirs in the default colour. Under the pointer a faint ✖ in the mark's place. Names shorten
/// for them to fit, down to six. Against the right edge a passing message, or the connection's state when it needs
/// attention.
fn draw_tabs(f: &mut Frame, app: &mut App, bar: Rect) {
    let off = band_off(app).min(bar.width);
    let bar = Rect { x: bar.x + off, width: bar.width - off, ..bar };
    app.tabs_area = bar;
    let list: Vec<String> = if app.fixed() { app.current().map(|c| vec![c.to_string()]).unwrap_or_default() } else { app.tabs.clone() };
    let active_at = if app.picker_open { None } else if app.fixed() { Some(0) } else { Some(app.active) };
    let own = if app.picker_open { if app.picker_search { s("search") } else { "wassup".into() } } else { String::new() };
    let own_close = !own.is_empty() && app.tab_hover == Some(-1);
    let own_w = if own.is_empty() { 0 } else { text::width(&own) + 6 };
    let marks: Vec<(String, String, bool)> = list
        .iter()
        .enumerate()
        .map(|(i, jid)| {
            let mark = app.tab_mark(jid);
            let mark = if mark.is_empty() { "   ".to_string() } else { mark };
            let closing = app.tab_hover == Some(i as i64) && !app.fixed();
            let mark = if closing { format!("{:<w$}", " ✖", w = text::width(&mark).max(2)) } else { mark };
            (app.store.name_of(jid), mark, closing)
        })
        .collect();
    let width = bar.width as usize;
    let mut name_w = marks.iter().map(|m| text::width(&m.0)).max().unwrap_or(0);
    let fits = |n: usize| 1 + own_w + marks.iter().map(|m| text::width(&m.0).min(n) + 3 + text::width(&m.1)).sum::<usize>() <= width;
    while name_w > 6 && !fits(name_w) {
        name_w -= 1;
    }
    let mut x = bar.x + 1;
    let strong = if theme().dark { Color::White } else { Color::Black };
    if !own.is_empty() {
        let bg = theme().mine;
        let mark = if own_close { " ✖ ".to_string() } else { app.list_mark() };
        let mut spans = vec![Span::styled(" ", Style::new().bg(bg)), Span::styled(own.clone(), Style::new().bg(bg).fg(strong).add_modifier(Modifier::BOLD))];
        spans.push(Span::styled(mark, Style::new().bg(bg).fg(if own_close { theme().dim } else { Color::Reset })));
        spans.push(Span::styled(" ", Style::new().bg(bg)));
        let w = own_w as u16 - 1;
        f.render_widget(Paragraph::new(Line::from(spans)), Rect { x, width: w.min(bar.width), ..bar });
        app.hits.push((x, bar.y, w, if own_close { Hit::OwnClose } else { Hit::Own }));
        if own_close {
            app.hits.push((x + 2 + text::width(&own) as u16, bar.y, 3, Hit::OwnClose));
        }
        x += w + 1;
    }
    for (i, (name, mark, closing)) in marks.into_iter().enumerate() {
        let jid = &list[i];
        let name = text::truncate(&name, name_w);
        let w = (2 + text::width(&name) + text::width(&mark)) as u16;
        if x + w > bar.x + bar.width {
            break;
        }
        let front = active_at == Some(i);
        let bg = if front { theme().mine } else { theme().theirs };
        let mut style = Style::new().bg(bg);
        style = if front { style.fg(text::color_for(jid)).add_modifier(Modifier::BOLD) } else { style.fg(Color::Reset) };
        let mark_style = if closing { Style::new().bg(bg).fg(theme().dim) } else { Style::new().bg(bg) };
        let spans = vec![Span::styled(" ", Style::new().bg(bg)), Span::styled(name.clone(), style), Span::styled(mark.clone(), mark_style), Span::styled(" ", Style::new().bg(bg))];
        f.render_widget(Paragraph::new(Line::from(spans)), Rect { x, width: w, ..bar });
        if closing {
            app.hits.push((x + 2 + text::width(&name) as u16, bar.y, 3, Hit::Close(i)));
        }
        app.hits.push((x, bar.y, w, Hit::Tab(i)));
        x += w + 1;
    }
    // A passing message (faint), or the connection: the QR and connecting in yellow, closed in red with why.
    let avail = (bar.x + bar.width).saturating_sub(x + 2) as usize;
    let (text, style) = if let Some((msg, _)) = &app.transient {
        (text::truncate(msg, avail.max(1)), Style::new().fg(theme().dim))
    } else {
        match app.conn {
            crate::wa::Conn::Open => (String::new(), Style::new()),
            crate::wa::Conn::Qr => (format!("● {}", s("waitingQr")), Style::new().fg(Color::Yellow)),
            crate::wa::Conn::Connecting => (format!("● {}", s("connecting")), Style::new().fg(Color::Yellow)),
            crate::wa::Conn::Closed => (format!("● {}", if app.conn_detail.is_empty() { s("disconnected") } else { app.conn_detail.clone() }), Style::new().fg(Color::Red)),
        }
    };
    if avail >= 6 && !text.is_empty() {
        let text = text::truncate(&text, avail);
        let tw = text::width(&text) as u16;
        f.render_widget(Paragraph::new(Span::styled(text, style)), Rect { x: bar.x + bar.width - tw - 1, width: tw, ..bar });
    }
}

/// The notice of a message elsewhere, over its chat's tab: from the background up to near the text's colour, held,
/// and back, the colour at each moment the blend at its opacity.
fn draw_notice(f: &mut Frame, app: &mut App, bar: Rect) {
    let Some((jid, said, since)) = app.notice.clone() else { return };
    let width = bar.width as usize;
    let said = text::truncate(&said.replace('\n', " "), width.saturating_sub(2));
    let w = text::width(&said) as u16 + 2;
    let tab = app.hits.iter().find(|(_, _, _, h)| matches!(h, Hit::Tab(i) if app.tabs.get(*i) == Some(&jid))).map(|h| h.0);
    let left = tab.unwrap_or(bar.x + bar.width).min((bar.x + bar.width).saturating_sub(w));
    let k = 0.85 * notice_opacity(since);
    let (bg, fg) = (theme().bg, theme().fg);
    let mix = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * k).round() as u8;
    let c = Color::Rgb(mix(bg[0], fg[0]), mix(bg[1], fg[1]), mix(bg[2], fg[2]));
    let c = if theme().truecolor { c } else { nearest256(c) };
    let r = Rect { x: left, width: w.min(bar.width), ..bar };
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(Span::styled(format!(" {said} "), Style::new().fg(c))), r);
}

/// A colour as "#rrggbb", for the page: the palette's resolved to its RGB.
fn hex(c: Color) -> String {
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(i) if i >= 232 => {
            let v = 8 + 10 * (i - 232);
            (v, v, v)
        }
        Color::Indexed(i) if i >= 16 => {
            let l = [0u8, 95, 135, 175, 215, 255];
            let i = (i - 16) as usize;
            (l[i / 36], l[(i / 6) % 6], l[i % 6])
        }
        _ => (128, 128, 128),
    };
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// The 256-colour palette's colour nearest to an RGB one.
pub fn nearest256(c: Color) -> Color {
    let Color::Rgb(r, g, b) = c else { return c };
    let levels = [0u8, 95, 135, 175, 215, 255];
    let q = |v: u8| levels.iter().enumerate().min_by_key(|(_, l)| (**l as i32 - v as i32).abs()).map(|(i, _)| i).unwrap_or(0);
    let cube = 16 + 36 * q(r) + 6 * q(g) + q(b);
    let gray_i = ((r as u32 + g as u32 + b as u32) / 3).saturating_sub(8) / 10;
    let gray = 232 + gray_i.min(23) as usize;
    let dist = |i: usize| {
        let (cr, cg, cb) = if i >= 232 {
            let v = 8 + 10 * (i - 232) as i32;
            (v, v, v)
        } else {
            let i = i - 16;
            (levels[i / 36] as i32, levels[(i / 6) % 6] as i32, levels[i % 6] as i32)
        };
        (cr - r as i32).pow(2) + (cg - g as i32).pow(2) + (cb - b as i32).pow(2)
    };
    Color::Indexed(if dist(gray) < dist(cube) { gray as u8 } else { cube as u8 })
}

/// The emoji suggestions over the band, five at a time, the chosen one with "›".
fn draw_suggestions(f: &mut Frame, app: &mut App) {
    if app.suggestions.is_empty() {
        return;
    }
    if app.suggest_index < app.suggest_top {
        app.suggest_top = app.suggest_index;
    } else if app.suggest_index >= app.suggest_top + SUGGEST_ROWS {
        app.suggest_top = app.suggest_index + 1 - SUGGEST_ROWS;
    }
    let lines: Vec<(String, bool)> = app.suggestions.iter().enumerate().map(|(i, o)| (format!("{} {}  {}", if i == app.suggest_index { "›" } else { " " }, o.emoji, o.code), i == app.suggest_index)).collect();
    let w = lines.iter().map(|(l, _)| text::width(l)).max().unwrap_or(0) as u16 + 2;
    let shown: Vec<&(String, bool)> = lines.iter().skip(app.suggest_top).take(SUGGEST_ROWS).collect();
    let h = shown.len() as u16;
    let y = app.band_area.y.saturating_sub(h);
    for (k, (l, chosen)) in shown.into_iter().enumerate() {
        let mut st = Style::new().bg(theme().selected);
        if *chosen {
            st = st.add_modifier(Modifier::BOLD);
        }
        let r = Rect { x: 0, y: y + k as u16, width: w.min(f.area().width), height: 1 };
        f.render_widget(Paragraph::new(Span::styled(format!(" {l:<width$}", width = w as usize - 1), st)), r);
        app.hits.push((0, r.y, w, Hit::Suggestion(app.suggest_top + k)));
    }
}

/// The model's correction floating on the line above the word it replaces, from its column.
fn draw_ghost(f: &mut Frame, app: &mut App) {
    let Some((word, line, col)) = app.ghost_float.clone() else { return };
    if line < app.band_first || line >= app.band_first + app.band_text.height as usize {
        return;
    }
    let y = (app.band_text.y + (line - app.band_first) as u16).saturating_sub(1);
    let x = (app.band_text.x + col as u16).saturating_sub(1);
    let w = text::width(&word) as u16 + 2;
    let r = Rect { x, y, width: w.min(f.area().width.saturating_sub(x)), height: 1 };
    f.render_widget(Paragraph::new(Span::styled(format!(" {word} "), Style::new().bg(theme().selected).fg(theme().dim).add_modifier(Modifier::ITALIC))), r);
    app.hits.push((r.x, r.y, r.width, Hit::Ghost));
}

/// The correction over a passage of one of my messages: a click on it edits the message.
fn draw_sent_fix(f: &mut Frame, app: &mut App) {
    let Some((_, fix, x, y)) = app.sent_fix.clone() else { return };
    let top = app.chat_area.y;
    let y = if y > top { y - 1 } else { y + 1 };
    let w = text::width(&fix.to) as u16 + 2;
    let r = Rect { x: x.saturating_sub(1), y, width: w.min(f.area().width.saturating_sub(x.saturating_sub(1))), height: 1 };
    f.render_widget(Paragraph::new(Span::styled(format!(" {} ", fix.to), Style::new().bg(theme().selected).fg(theme().dim).add_modifier(Modifier::ITALIC))), r);
    app.hits.push((r.x, r.y, r.width, Hit::SentFix));
}

/// The message just sent: the band narrows to the bubble's width against the right and rises to its place.
fn draw_flight(f: &mut Frame, app: &mut App) {
    let Some(fl) = &app.flight else { return };
    let area = f.area();
    let right = area.width.saturating_sub(1 + text::width(&text::my_time(0, 0)) as u16 + 1);
    let inner = fl.lines.iter().map(|l| text::width(l)).max().unwrap_or(1).max(1) as u16;
    let n = fl.lines.len() as u16;
    let (left0, left1) = (1u16, right.saturating_sub(inner + 2).max(1));
    let landing = app.chat_area.y + app.chat_area.height.saturating_sub(n + 1);
    let p = (fl.frame.min(crate::app::FLY_FRAMES)) as f64 / crate::app::FLY_FRAMES as f64;
    let e = p * p * (3.0 - 2.0 * p);
    let left = (left0 as f64 + (left1 as f64 - left0 as f64) * e).round() as u16;
    let top = (fl.top as f64 + (landing as f64 - fl.top as f64) * e).round() as u16;
    let w = right.saturating_sub(left);
    for (k, l) in fl.lines.iter().enumerate() {
        let r = Rect { x: left, y: top + k as u16, width: w, height: 1 };
        if r.y >= area.height {
            continue;
        }
        let pad = (w as usize).saturating_sub(2 + text::width(l));
        f.render_widget(Paragraph::new(Span::styled(format!(" {l}{} ", " ".repeat(pad)), Style::new().bg(theme().mine))), r);
    }
}

/// The popup: the picture as large as fits, centred, its description under it; any key or click closes it.
fn draw_viewer(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(v) = app.viewing.clone() else { return };
    let r = Rect { x: area.x + 2, y: area.y + 1, width: area.width.saturating_sub(4), height: area.height.saturating_sub(2) };
    f.render_widget(Clear, r);
    f.render_widget(Block::default().borders(Borders::ALL), r);
    for y in r.y..r.y + r.height {
        app.hits.push((r.x, y, r.width, Hit::Viewer));
    }
    let inner = Rect { x: r.x + 1, y: r.y + 1, width: r.width.saturating_sub(2), height: r.height.saturating_sub(2) };
    if let Some(e) = app.failed.get(&format!("file:{}", v.path)).cloned() {
        f.render_widget(Paragraph::new(Span::styled(t("mediaUnreadable", &[&s("image"), &e]), Style::new().fg(theme().dim))), inner);
        return;
    }
    let Some(img) = app.whole(&v.path) else {
        f.render_widget(Paragraph::new(Span::styled(s("loading"), Style::new().fg(theme().dim))), inner);
        return;
    };
    let about = match app.described_as(&v.key, &v.path) {
        Some(Some(d)) => Some(d),
        Some(None) => Some(s("describing")),
        None => None,
    };
    let text_lines = about.as_deref().map(|a| text::wrap(a, (inner.width as usize).min(80))).unwrap_or_default();
    let below = if crate::llm::enabled() { 5 } else { 0 }.max(if text_lines.is_empty() { 0 } else { text_lines.len() as u16 + 1 });
    let font = app.picker.font_size();
    let cell = Size::new(font.width, font.height);
    let blocks = app.picker.protocol_type() == ratatui_image::picker::ProtocolType::Halfblocks;
    // As large as the box allows, cells taken as twice as tall as wide (wassup's cellSize, filling).
    let (iw, ih) = (img.width() as f64, img.height() as f64);
    let max_h = inner.height.saturating_sub(below).max(1);
    let mut cols = inner.width.max(1);
    let mut rows = ((cols as f64 * ih / iw / 2.0).round() as u16).max(1);
    if rows > max_h {
        rows = max_h;
        cols = ((rows as f64 * 2.0 * iw / ih).round() as u16).clamp(1, inner.width.max(1));
    }
    let top = inner.y + inner.height.saturating_sub(rows + below) / 2;
    let left = inner.x + (inner.width - cols) / 2;
    let pr = Rect { x: left, y: top, width: cols, height: rows };
    if app.web.is_some() || app.kitty.is_some() {
        app.web_pics.push(crate::app::WebPic::Image { key: format!("view:{}", v.path), file: Some(v.path.clone()), png: Some(img.clone()), x: pr.x, y: pr.y, w: cols, h: rows, from: 0.0, to: 1.0 });
    } else if blocks {
        let k = (format!("view:{}", v.path), cols, rows);
        let fitted = app.scaled.entry(k).or_insert_with(|| std::sync::Arc::new(img.resize_exact(cols as u32, rows as u32 * 2, image::imageops::FilterType::Triangle))).clone();
        crate::chat::half_blocks(f.buffer_mut(), pr, &fitted, 0);
    }
    let key = (format!("view:{}", v.path), cols, rows, 0, rows);
    if app.web.is_none() && app.kitty.is_none() && !blocks && !app.crops.contains_key(&key) {
        let fitted = img.resize_exact(cols as u32 * cell.width as u32, rows as u32 * cell.height as u32, image::imageops::FilterType::Triangle);
        if let Ok(p) = app.picker.new_protocol(fitted, Size::new(cols, rows), Resize::Fit(None)) {
            app.crops.insert(key.clone(), p);
        }
    }
    if let (None, None, false, Some(p)) = (&app.web, &app.kitty, blocks, app.crops.get(&key)) {
        f.render_widget(Image::new(p), pr);
    }
    let block = text_lines.iter().map(|l| text::width(l)).max().unwrap_or(0) as u16;
    for (k, l) in text_lines.iter().enumerate() {
        let y = top + rows + 1 + k as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let x = inner.x + (inner.width.saturating_sub(block)) / 2;
        f.render_widget(Paragraph::new(Span::styled(l.clone(), Style::new().fg(theme().dim).add_modifier(Modifier::ITALIC))), Rect { x, y, width: inner.width.saturating_sub(x - inner.x), height: 1 });
    }
}

/// The cells of the selection, in reading order, within its panel: in the messages only their text's columns; in
/// the band every row after the prompt. Inverted on the screen, and their text copied when asked.
fn draw_text_sel(f: &mut Frame, app: &mut App) {
    let Some(sel) = app.text_sel else { return };
    let back = sel.hy < sel.ay || (sel.hy == sel.ay && sel.hx < sel.ax);
    let (x0, y0, x1, y1) = if back { (sel.hx, sel.hy, sel.ax, sel.ay) } else { (sel.ax, sel.ay, sel.hx, sel.hy) };
    let copy = app.clipboard.as_deref() == Some("");
    let mut out: Vec<String> = Vec::new();
    for y in y0..=y1 {
        let (mut from, mut to) = (sel.area.x, sel.area.x + sel.area.width.saturating_sub(1));
        if !sel.input {
            let Some(cols) = crate::chat::row_at(app, y).and_then(|r| r.text) else { continue };
            from = app.chat_area.x + cols.0;
            to = (app.chat_area.x + cols.1).saturating_sub(1);
        }
        if y == y0 {
            from = from.max(x0);
        }
        if y == y1 {
            to = to.min(x1);
        }
        if from > to {
            continue;
        }
        let buf = f.buffer_mut();
        let mut line = String::new();
        for x in from..=to {
            if let Some(c) = buf.cell_mut((x, y)) {
                if copy {
                    line.push_str(c.symbol());
                }
                c.modifier.toggle(Modifier::REVERSED);
            }
        }
        out.push(line.trim().to_string());
    }
    if copy {
        app.clipboard = Some(out.join("\n"));
    }
}

/// The code the phone reads to link this device, in half-blocks, dark on light as cameras want it.
pub fn draw_qr(f: &mut Frame, body: Rect, code: &str) {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(format!("  {}", s("qrTitle")), Style::new().add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(format!("  {}", s("qrHint"))),
        Line::from(""),
    ];
    if let Ok(qr) = qrcode::QrCode::new(code.as_bytes()) {
        let s = qr
            .render::<qrcode::render::unicode::Dense1x2>()
            .dark_color(qrcode::render::unicode::Dense1x2::Dark)
            .light_color(qrcode::render::unicode::Dense1x2::Light)
            .quiet_zone(true)
            .build();
        let style = Style::new().fg(Color::Black).bg(Color::White);
        lines.extend(s.lines().map(|l| Line::from(vec![Span::raw("  "), Span::styled(l.to_string(), style)])));
    }
    f.render_widget(Clear, body);
    f.render_widget(Paragraph::new(lines), body);
}
