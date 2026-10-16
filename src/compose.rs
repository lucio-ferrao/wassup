//! Writing in the band, as the Node version's ui.ts edits it: the text and its cursor counted in graphemes (a flag, a family of
//! emoji, an accent written apart are one step each), the editing keys, and the lines the band wraps it into.

use unicode_segmentation::UnicodeSegmentation;

use crate::text;

/// What an editing key does to a text with a cursor.
pub enum Edit {
    Left,
    Right,
    Home,
    End,
    Backspace,
    Delete,
    /// Ctrl+U: everything.
    Clear,
    /// Shift+Backspace: the word before the cursor.
    DeleteWord,
    Insert(String),
}

pub fn graphemes(s: &str) -> Vec<&str> {
    s.graphemes(true).collect()
}

pub fn len(s: &str) -> usize {
    s.graphemes(true).count()
}

/// The text before grapheme `at`.
pub fn before(s: &str, at: usize) -> &str {
    let end = s.grapheme_indices(true).nth(at).map(|(i, _)| i).unwrap_or(s.len());
    &s[..end]
}

/// The text from grapheme `at`.
pub fn after(s: &str, at: usize) -> &str {
    &s[before(s, at).len()..]
}

/// A key applied to `value` with its cursor: the new text and cursor. A typed ":" closing a ":code:" swaps it for its
/// emoji at once.
pub fn edit(value: &str, cursor: usize, e: Edit) -> (String, usize) {
    let n = len(value);
    let at = cursor.min(n);
    let (b, a) = (before(value, at), after(value, at));
    match e {
        Edit::Left => (value.into(), at.saturating_sub(1)),
        Edit::Right => (value.into(), (at + 1).min(n)),
        Edit::Home => (value.into(), 0),
        Edit::End => (value.into(), n),
        Edit::Backspace => {
            let kept = before(value, at.saturating_sub(1));
            (format!("{kept}{a}"), len(kept))
        }
        Edit::Delete => (format!("{b}{}", after(value, at + 1)), at),
        Edit::Clear => (String::new(), 0),
        Edit::DeleteWord => {
            let trimmed = b.trim_end();
            let cut = trimmed.rfind(char::is_whitespace).map(|i| i + trimmed[i..].chars().next().map_or(1, char::len_utf8)).unwrap_or(0);
            let kept = &b[..cut];
            (format!("{kept}{a}"), len(kept))
        }
        Edit::Insert(s) => {
            let mut head = format!("{b}{s}");
            if s == ":" {
                head = crate::emojicodes::emojify(&head);
            }
            let c = len(&head);
            (format!("{head}{a}"), c)
        }
    }
}

/// Graphemes wrapped into lines of `width` columns, breaking after the last space that fits (a word longer than a
/// line is cut where it must); a line break ends its line and stays in it, so the cursor counts it.
pub fn wrap_chars(gs: &[&str], width: usize) -> Vec<Vec<String>> {
    let width = width.max(1);
    let mut lines: Vec<Vec<String>> = vec![vec![]];
    let mut w = 0;
    for g in gs {
        let line = lines.last_mut().unwrap();
        if *g == "\n" {
            line.push((*g).into());
            lines.push(vec![]);
            w = 0;
            continue;
        }
        let gw = text::width(g);
        if w + gw > width && !(g.trim().is_empty() && w <= width) {
            // Back to the last space, which stays at the end of the line.
            let cut = line.iter().rposition(|c| c.trim().is_empty()).map(|i| i + 1).filter(|&i| i < line.len());
            let moved: Vec<String> = match cut {
                Some(i) => line.drain(i..).collect(),
                None => vec![],
            };
            w = moved.iter().map(|c| text::width(c)).sum();
            lines.push(moved);
        }
        lines.last_mut().unwrap().push((*g).into());
        w += gw;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphemes_move_whole() {
        let (v, c) = edit("a🇵🇹b", 2, Edit::Backspace);
        assert_eq!((v.as_str(), c), ("ab", 1));
        let (v, c) = edit("olá mundo  ", 11, Edit::DeleteWord);
        assert_eq!((v.as_str(), c), ("olá ", 4));
    }

    #[test]
    fn wraps_at_spaces() {
        let gs = graphemes("um dois três");
        let lines: Vec<String> = wrap_chars(&gs, 8).into_iter().map(|l| l.concat()).collect();
        assert_eq!(lines, vec!["um dois ", "três"]);
    }
}
