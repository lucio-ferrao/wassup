//! The chat list, as the Node version's ui.ts refreshPicker and pickerRows lay it out: over it the app's name in big letters
//! and the counts; the chats under a separator for each day, the most recent at the bottom, two rows each (the
//! photo, the name with its mood or someone typing, the unread count, 👀 while the person is online, "·" when it has
//! a tab, the time; under it the number, or the group's size). A filter takes the words in any order, accents aside,
//! in the name or the number, the best matches at the bottom; Ctrl+F searches the messages of every chat instead.

use std::sync::Arc;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use regex::Regex;

use crate::app::{App, Hit, Slot, WebPanel, WebPic};
use crate::i18n::{lang, t};
use crate::store::Chat;
use crate::text::{self, theme};

/// The app's name, in the rules' thin lines with round corners, as wassup's WORDMARK.
const WORDMARK: [&str; 3] = ["╷ ╷ ╷ ╭─╮ ╭─╴ ╭─╴ ╷ ╷ ╭─╮", "╰─┴─╯ ╰─┴ ╶─╯ ╶─╯ ╰─╯ ├─╯", "                      ╵"];

/// The list's rows anew, the selection kept on its chat (or message), the view where it was looked at.
fn refresh(app: &mut App, rows: usize) {
    let selected_hit = match app.slots.get(app.list_sel) {
        Some(Slot::Hit(i)) => app.hits_found.get(*i).map(|m| m.id.clone()),
        _ => None,
    };
    let selected_jid = match app.slots.get(app.list_sel) {
        Some(Slot::Top(i)) if selected_hit.is_none() => app.filtered.get(*i).map(|c| c.jid.clone()),
        _ => None,
    };
    let same_filter = app.filter_shown.as_deref() == Some(app.filter.as_str());
    let keep_view = same_filter && app.picker_focus.is_none() && !app.slots.is_empty();
    let at_end = app.list_top + rows >= app.slots.len();
    let anchor: Option<(String, usize)> = if keep_view {
        (app.list_top..app.slots.len()).find_map(|i| match &app.slots[i] {
            Slot::Top(c) => app.filtered.get(*c).map(|c| (c.jid.clone(), i - app.list_top)),
            _ => None,
        })
    } else {
        None
    };
    app.filter_shown = Some(app.filter.clone());
    app.chats = app.store.chats().unwrap_or_default();
    app.chats.reverse();
    let words: Vec<String> = text::fold(&app.filter).split_whitespace().map(String::from).collect();
    let chats: Vec<Chat> = app.chats.iter().filter(|c| !c.archived).cloned().collect();
    let matching: Vec<Chat> = chats
        .iter()
        .filter(|c| {
            let n = text::fold(&c.name);
            let u = c.jid.split('@').next().unwrap_or("");
            words.iter().all(|w| n.contains(w.as_str()) || u.contains(w.as_str()))
        })
        .cloned()
        .collect();
    let starts: Vec<Regex> = words.iter().filter_map(|w| Regex::new(&format!(r"(?:^|[^\p{{L}}\p{{N}}]){}", regex::escape(w))).ok()).collect();
    let rank = |c: &Chat| {
        let n = text::fold(&c.name);
        if !starts.iter().all(|re| re.is_match(&n)) {
            return 0;
        }
        if words.first().is_some_and(|w| n.starts_with(w.as_str())) { 2 } else { 1 }
    };
    app.filtered = if words.is_empty() { chats } else { (0..3).flat_map(|r| matching.iter().filter(|c| rank(c) == r).cloned().collect::<Vec<_>>()).collect() };
    app.list_words = words.clone();
    // Searching messages, from two characters on: the most recent at the bottom, each with its text's rows.
    let query = app.filter.trim().to_string();
    if app.picker_search && query.chars().count() >= 2 {
        if app.found_query.as_deref() != Some(query.as_str()) {
            app.hits_found = app.store.search(None, &words, 100).unwrap_or_default();
            app.found_query = Some(query);
        }
    } else {
        app.hits_found.clear();
        app.found_query = None;
    }
    let mut slots = Vec::new();
    for (i, m) in app.hits_found.iter().enumerate() {
        slots.push(Slot::Hit(i));
        for l in hit_text(app, m, &words) {
            slots.push(Slot::HitText(l));
        }
    }
    if !app.picker_search {
        let mut last_day = String::new();
        let mut fresh = true;
        for (i, c) in app.filtered.iter().enumerate() {
            let day = crate::format::fmt_day(c.last_ts, lang());
            if words.is_empty() && day != last_day {
                last_day = day.clone();
                slots.push(Slot::Day(day));
                fresh = true;
            }
            if !fresh {
                slots.push(Slot::Blank);
            }
            fresh = false;
            slots.push(Slot::Top(i));
            slots.push(Slot::Bottom(i));
        }
    }
    app.slots = slots;
    let focusing = app.picker_focus.is_some();
    let want: Option<String> = match app.picker_focus.take() {
        Some(f) => f,
        None if same_filter => selected_jid,
        None => None,
    };
    let keep = match (&selected_hit, same_filter) {
        (Some(id), true) => app.slots.iter().position(|s| matches!(s, Slot::Hit(i) if app.hits_found[*i].id == *id)),
        _ => want.as_ref().and_then(|w| app.slots.iter().position(|s| matches!(s, Slot::Top(i) if app.filtered[*i].jid == *w))),
    };
    if focusing && keep.is_none() {
        app.picked = false;
    }
    let last = app.slots.iter().rposition(Slot::selectable).unwrap_or(0);
    app.list_sel = keep.unwrap_or(last);
    let len = app.slots.len();
    if let (Some(row), Some(k)) = (app.picker_focus_row.take(), keep) {
        app.list_top = k.saturating_sub(row).min(len.saturating_sub(rows));
    } else if keep_view {
        let at = anchor.as_ref().and_then(|(j, _)| app.slots.iter().position(|s| matches!(s, Slot::Top(i) if app.filtered[*i].jid == *j)));
        app.list_top = match (at_end, at, &anchor) {
            (false, Some(at), Some((_, row))) => at.saturating_sub(*row).min(len.saturating_sub(rows)),
            _ => len.saturating_sub(rows),
        };
    } else {
        app.list_top = len.saturating_sub(rows);
    }
}

/// A message found, as rows of text: up to two, from a little before the first match, links short.
fn hit_text(app: &App, m: &crate::store::Msg, words: &[String]) -> Vec<String> {
    let end = (app.list_area.width as usize).saturating_sub(1 + 12 + 1);
    let room = end.saturating_sub(4).max(8);
    let (shown, _) = text::with_mentions(&m.text, |d| app.store.mention_name(d));
    let mut s = crate::links::show_links(&shown).split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((first, _)) = text::matches(&s, words).first()
        && *first > room / 3 {
            s = format!("…{}", s.chars().skip(first - room / 3).collect::<String>());
        }
    let mut lines = text::wrap(&s, room);
    if lines.len() > 2 {
        lines[1] = text::truncate(&format!("{} {}", lines[1], lines[2]), room);
    }
    lines.truncate(2);
    lines
}

/// What the list says under a chat's name: the number (without the country code when it's mine too), 💼 for a
/// business; a group's size.
fn chat_number(app: &App, c: &Chat) -> String {
    if text::is_group(&c.jid) {
        let members = app.store.profile(&c.jid).and_then(|p| p.members).map(|n| n as usize);
        return crate::format::chat_number(&c.jid, &app.me, members, lang());
    }
    let n = crate::format::chat_number(&c.jid, &app.me, None, lang());
    if !n.is_empty() && app.business.contains(&c.jid) { format!("{n} 💼") } else { n }
}

/// The name with the filter's words underlined, accents and case aside.
fn underlined(name: &str, words: &[String], style: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = name.chars().collect();
    let mut marked = vec![false; chars.len()];
    for (a, b) in text::matches(name, words) {
        for m in &mut marked[a..b] {
            *m = true;
        }
    }
    let mut spans = Vec::new();
    let mut run = String::new();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && marked[i] != marked[i - 1] {
            let st = if marked[i - 1] { style.add_modifier(Modifier::UNDERLINED) } else { style };
            spans.push(Span::styled(std::mem::take(&mut run), st));
        }
        run.push(*c);
    }
    if !run.is_empty() {
        let st = if marked.last() == Some(&true) { style.add_modifier(Modifier::UNDERLINED) } else { style };
        spans.push(Span::styled(run, st));
    }
    spans
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // Over the list the app's name in WhatsApp's green, its 👾, and the counts at the right of the second row (the
    // third where they don't fit); on one line without UTF-8.
    let green = Style::new().fg(theme().green);
    let open: Vec<&Chat> = app.chats.iter().filter(|c| !c.archived).collect();
    let unread = open.iter().filter(|c| c.unread > 0).count();
    let mut counts = t("chatsCount", &[&open.len()]);
    if unread > 0 {
        counts = format!("{counts} · {}", t("unreadCount", &[&unread]));
    }
    let w = area.width.saturating_sub(2) as usize;
    let head_h: u16 = if app.caps.utf8 { 3 } else { 1 };
    if app.caps.utf8 {
        let after = |row: usize| w as i64 - text::width(WORDMARK[row]) as i64 - text::width(&counts) as i64;
        let row = if after(1) >= 2 { 1 } else { 2 };
        let room = w.saturating_sub(text::width(WORDMARK[row]) + 2);
        let shown = if after(row) >= 2 { counts.clone() } else if room > 3 { text::truncate(&counts, room) } else { String::new() };
        for (i, l) in WORDMARK.iter().enumerate() {
            let mut spans = vec![Span::raw(" "), Span::styled(*l, green)];
            if i == 0 {
                spans.push(Span::raw("  👾"));
            }
            if i == row && !shown.is_empty() {
                spans.push(Span::raw(" ".repeat(w.saturating_sub(text::width(l) + text::width(&shown)))));
                spans.push(Span::styled(shown.clone(), Style::new().fg(theme().dim)));
            }
            f.render_widget(Paragraph::new(Line::from(spans)), Rect { y: area.y + i as u16, height: 1, ..area });
        }
    } else {
        let line = Line::from(vec![Span::raw(" "), Span::styled("wassup", green.add_modifier(Modifier::BOLD)), Span::raw("  "), Span::styled(counts, Style::new().fg(theme().dim))]);
        f.render_widget(Paragraph::new(line), Rect { height: 1, ..area });
    }
    let body = Rect { y: area.y + head_h, height: area.height.saturating_sub(head_h + 1), ..area };
    if let (Some(code), true) = (app.qr.clone(), app.showing_qr) {
        crate::ui::draw_qr(f, body, &code);
        return;
    }
    let rows = body.height as usize;
    app.list_rows = rows;
    if app.list_dirty || app.list_area.width != body.width {
        app.list_area = body;
        refresh(app, rows);
        app.list_dirty = false;
        app.list_gen += 1;
    }
    app.list_area = body;
    let len = app.slots.len();
    // The selection brought into view when it moved, its rows with it.
    if app.picker_chosen() && std::mem::take(&mut app.list_follow) {
        let sel = app.list_sel;
        let mut last = sel;
        while last + 1 < len && matches!(app.slots[last + 1], Slot::Bottom(_) | Slot::HitText(_)) {
            last += 1;
        }
        if sel < app.list_top {
            app.list_top = sel;
        } else if last >= app.list_top + rows {
            app.list_top = last + 1 - rows;
        }
    }
    app.list_top = app.list_top.min(len.saturating_sub(rows));
    let start = app.list_top;
    let end = (start + rows).min(len);
    // Flush to the bottom when shorter than the panel.
    let pad = rows - (end - start);
    let list = Rect { y: body.y + pad as u16, height: (end - start) as u16, ..body };
    app.list_area = Rect { y: list.y, height: list.height, ..body };
    app.list_top = start;
    // A browser scrolls the list itself: moved from here, it's told to follow.
    let web = app.web.is_some();
    if web && app.page_top != Some(app.list_top) {
        app.scroll_gen += 1;
        app.page_top = Some(app.list_top);
    }
    let width = (body.width as usize).saturating_sub(1);
    let time_cols = if !app.list_words.is_empty() || !app.hits_found.is_empty() { 12 } else { 5 };
    let end_col = width.saturating_sub(time_cols + 1);
    let font = app.picker.font_size();
    let cell = Size::new(font.width, font.height);
    let mut photos: Vec<(String, u16)> = Vec::new();
    for (k, i) in (start..end).enumerate() {
        let y = list.y + k as u16;
        let row = Rect { x: body.x + 1, y, width: body.width.saturating_sub(1), height: 1 };
        let mut photo = false;
        if let Slot::Top(c) | Slot::Bottom(c) = app.slots[i] {
            let jid = app.filtered[c].jid.clone();
            let top = matches!(app.slots[i], Slot::Top(_));
            // The photo, four cells by two, once both rows are in view; the initials meanwhile or without one.
            let both = if top { i + 1 < end } else { i > start };
            photo = both && !app.caps.no_images && app.avatar(&jid).is_some();
            if app.caps.no_images && app.is_open() && app.profile_asked.insert(jid.clone()) {
                app.backend.ensure_profile(&jid);
            }
            if photo {
                if top {
                    photos.push((jid.clone(), y));
                }
                app.hits.push((row.x, y, 4, Hit::Photo(jid)));
            }
        }
        f.render_widget(Paragraph::new(slot_line(app, i, photo, width, end_col)), row);
    }
    // A browser has every photo of the list from the panel (web_panel): those in view are drawn as in a terminal, for
    // what that asks for, but not passed on twice.
    let pics_before = app.web_pics.len();
    for (jid, y) in photos {
        if let Some(img) = app.avatar(&jid) {
            crate::chat::draw_whole(f, app, &format!("avatar:{jid}"), &img, Rect { x: body.x + 1, y, width: 4, height: 2 }, cell);
        }
    }
    if web {
        app.web_pics.truncate(pics_before);
        app.web_panel = Some(web_panel(app, Rect { x: body.x, width: body.width, ..app.list_area }, width, end_col));
    }
}

/// Slot `i` of the list as its line, from its first column on: a day's separator, a chat's two rows (its photo's
/// four cells left blank for it with `photo`, else its initials on its colour; the name with its mood or someone
/// typing, the unread count, 👀, "·" with a tab, the time; under it the number), a message found and its text. On the
/// band's green up to its time when it's the one selected.
fn slot_line(app: &App, i: usize, photo: bool, width: usize, end_col: usize) -> Line<'static> {
    let chosen = app.picker_chosen();
    let lit = chosen && (i == app.list_sel || matches!(app.slots[i], Slot::Bottom(_) | Slot::HitText(_)) && owner(app, i) == Some(app.list_sel));
    let bg = if lit { theme().mine } else { Color::Reset };
    let strong = if theme().dark { Color::White } else { Color::Black };
    match app.slots[i].clone() {
        Slot::Day(label) => {
            let label = format!("── {label} ──");
            let lead = width.saturating_sub(text::width(&label)) / 2;
            Line::from(Span::styled(format!("{}{label}", " ".repeat(lead)), Style::new().fg(theme().dim)))
        }
        Slot::Blank => Line::from(""),
        Slot::Top(c) | Slot::Bottom(c) => {
            let chat = &app.filtered[c];
            let top = matches!(app.slots[i], Slot::Top(_));
            let color = text::color_for(&chat.jid);
            let mut spans: Vec<Span<'static>> = Vec::new();
            if photo {
                spans.push(Span::raw("    "));
            } else {
                let cells = if top { format!(" {:<2} ", text::initials(&chat.name)) } else { "    ".into() };
                spans.push(Span::styled(cells, Style::new().bg(color).fg(Color::Reset).add_modifier(Modifier::BOLD)));
            }
            spans.push(Span::styled(" ", Style::new().bg(bg)));
            if top {
                let typing = app.typing.get(&chat.jid).is_some_and(|w| !w.is_empty());
                let mood = if typing { format!(" {}", App::typing_emoji()) } else { app.mood_of(&chat.jid, chat.last_ts) };
                let badge = if chat.unread > 0 { format!(" ({})", chat.unread) } else { String::new() };
                let eyes = if !text::is_group(&chat.jid) && app.online.contains(&chat.jid) && app.caps.utf8 { " 👀" } else { "" };
                let open = if app.tabs.contains(&chat.jid) { " ·" } else { "" };
                let room = end_col.saturating_sub(4 + 1 + 1 + text::width(&mood) + text::width(&badge) + text::width(eyes) + text::width(open)).max(4);
                let name = text::truncate(&crate::unicode::tame(&chat.name), room);
                let mut style = Style::new().bg(bg).fg(if lit { strong } else { color });
                if chat.unread > 0 || lit {
                    style = style.add_modifier(Modifier::BOLD);
                }
                spans.extend(underlined(&name, &app.list_words, style));
                spans.push(Span::styled(mood.clone(), Style::new().bg(bg)));
                spans.push(Span::styled(badge.clone(), Style::new().bg(bg).fg(Color::Red).add_modifier(Modifier::BOLD)));
                spans.push(Span::styled(eyes, Style::new().bg(bg)));
                spans.push(Span::styled(open, Style::new().bg(bg).fg(theme().dim)));
                let used: usize = spans.iter().map(|s| text::width(&s.content)).sum();
                spans.push(Span::styled(" ".repeat(end_col.saturating_sub(used).max(1)), Style::new().bg(bg)));
                let when = if app.list_words.is_empty() { text::fmt_time(chat.last_ts) } else { crate::format::fmt_when_at(chat.last_ts, lang()) };
                let used: usize = spans.iter().map(|s| text::width(&s.content)).sum();
                spans.push(Span::raw(" ".repeat(width.saturating_sub(used + text::width(&when)).max(1))));
                spans.push(Span::styled(when, Style::new().fg(theme().faint)));
            } else {
                let number = text::truncate(&chat_number(app, chat), end_col.saturating_sub(6));
                spans.push(Span::styled(number, Style::new().bg(bg).fg(theme().dim)));
                let used: usize = spans.iter().map(|s| text::width(&s.content)).sum();
                spans.push(Span::styled(" ".repeat(end_col.saturating_sub(used)), Style::new().bg(bg)));
            }
            Line::from(spans)
        }
        Slot::Hit(h) => {
            let m = &app.hits_found[h];
            let color = text::color_for(&m.chat);
            let who = if m.from_me { t("me", &[]) } else if text::is_group(&m.chat) { app.store.name_of(&m.sender) } else { String::new() };
            let name = text::truncate(&app.store.name_of(&m.chat), end_col.saturating_sub(4).max(8));
            let mut spans = vec![Span::styled(name, Style::new().bg(bg).fg(if lit { strong } else { color }).add_modifier(if lit { Modifier::BOLD } else { Modifier::empty() }))];
            if !who.is_empty() {
                spans.push(Span::styled(format!(" · {}", text::truncate(&who, 24)), Style::new().bg(bg).fg(theme().dim)));
            }
            let used: usize = spans.iter().map(|s| text::width(&s.content)).sum();
            spans.push(Span::styled(" ".repeat(end_col.saturating_sub(used).max(1)), Style::new().bg(bg)));
            let when = crate::format::fmt_when(m.ts, lang());
            let used: usize = spans.iter().map(|s| text::width(&s.content)).sum();
            spans.push(Span::raw(" ".repeat(width.saturating_sub(used + text::width(&when)).max(1))));
            spans.push(Span::styled(when, Style::new().fg(theme().faint)));
            Line::from(spans)
        }
        Slot::HitText(l) => {
            let mut spans = vec![Span::styled("    ", Style::new().bg(bg))];
            let chars: Vec<char> = l.chars().collect();
            let found = text::matches(&l, &app.list_words);
            let mut at = 0;
            for (a, b) in found {
                if a < at {
                    continue;
                }
                spans.push(Span::styled(chars[at..a].iter().collect::<String>(), Style::new().bg(bg)));
                spans.push(Span::styled(chars[a..b].iter().collect::<String>(), Style::new().bg(bg).add_modifier(Modifier::REVERSED)));
                at = b;
            }
            spans.push(Span::styled(chars[at..].iter().collect::<String>(), Style::new().bg(bg)));
            let used: usize = spans.iter().map(|s| text::width(&s.content)).sum();
            spans.push(Span::styled(" ".repeat(end_col.saturating_sub(used)), Style::new().bg(bg)));
            Line::from(spans)
        }
    }
}

/// The list for a browser (webtab.rs), which scrolls it itself, in `area` (the list's, from its first column): every
/// row drawn and every photo by its row, from the file that's here and never asked for. The rows are drawn again
/// only when what they show may have changed (the list worked out again, the selection, someone typing or online,
/// the moods, the tabs); the rows in view as the frame ends up with them are added once it's drawn (ui.rs).
fn web_panel(app: &mut App, area: Rect, width: usize, end_col: usize) -> WebPanel {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (app.list_gen, app.slots.len(), app.list_sel, app.picker_chosen(), area.width, app.caps.no_images).hash(&mut h);
    let mut typing: Vec<&String> = app.typing.iter().filter(|(_, w)| !w.is_empty()).map(|(j, _)| j).collect();
    typing.sort();
    typing.hash(&mut h);
    if !typing.is_empty() {
        App::typing_emoji().hash(&mut h);
    }
    let mut online: Vec<&String> = app.online.iter().collect();
    online.sort();
    online.hash(&mut h);
    let mut moods: Vec<(&String, &String)> = app.moods.iter().collect();
    moods.sort();
    moods.hash(&mut h);
    (&app.tabs, app.business.len()).hash(&mut h);
    let layout = h.finish();
    let photo_of = |app: &App, i: usize| match app.slots[i] {
        Slot::Top(c) | Slot::Bottom(c) if !app.caps.no_images => app.store.avatar(&app.filtered[c].jid),
        _ => None,
    };
    let rows = match app.list_panel.as_ref().filter(|(l, _)| *l == layout) {
        Some((_, rows)) => rows.clone(),
        None => {
            let r = Rect::new(0, 0, area.width, 1);
            let drawn: Vec<Buffer> = (0..app.slots.len())
                .map(|i| {
                    let mut buf = Buffer::empty(r);
                    let line = slot_line(app, i, photo_of(app, i).is_some(), width, end_col);
                    Paragraph::new(line).render(Rect { x: 1, width: area.width.saturating_sub(1), ..r }, &mut buf);
                    buf
                })
                .collect();
            let drawn = Arc::new(drawn);
            app.list_panel = Some((layout, drawn.clone()));
            drawn
        }
    };
    let pics = (0..app.slots.len())
        .filter(|i| matches!(app.slots[*i], Slot::Top(_)))
        .filter_map(|i| {
            let Slot::Top(c) = app.slots[i] else { return None };
            let file = photo_of(app, i)?;
            Some((i, WebPic::Image { key: format!("avatar:{}", app.filtered[c].jid), file: Some(file), png: None, x: 1, y: 0, w: 4, h: 2, from: 0.0, to: 1.0 }))
        })
        .collect();
    WebPanel { key: "list".into(), area, layout, rows, view: vec![], top: app.list_top, scroll: app.scroll_gen, pics }
}

/// The selectable row a continuation row belongs to.
fn owner(app: &App, i: usize) -> Option<usize> {
    (0..i).rev().find(|j| app.slots[*j].selectable())
}
