//! Text in big letters drawn with box-drawing characters, as the Node version's bigtext.ts: lowercase, rounded, each letter
//! between the middle of its cells. Four rows, kept only when the text uses them: one for what rises above the small
//! letters, two for the small letters, and one for what drops below them.

use unicode_normalization::UnicodeNormalization;

type Glyph = [&'static str; 4];

fn glyph_of(c: char) -> Option<Glyph> {
    Some(match c {
        'a' => ["   ", "╭─╮", "╰─┴", "   "],
        'b' => ["╷  ", "├─╮", "╰─╯", "   "],
        'c' => ["   ", "╭─╴", "╰─╴", "   "],
        'd' => ["  ╷", "╭─┤", "╰─╯", "   "],
        'e' => ["   ", "╭─╮", "╰─╴", "   "],
        'f' => [" ╭╴", "╶┼╴", " ╵ ", "   "],
        'g' => ["   ", "╭─╮", "╰─┤", "╶─╯"],
        'h' => ["╷  ", "├─╮", "╵ ╵", "   "],
        'i' => ["·", "╷", "╵", " "],
        'j' => [" ·", " ╷", " │", "╶╯"],
        'k' => ["╷  ", "├┬╴", "╵╰╴", "   "],
        'l' => ["╷ ", "│ ", "╰╴", "  "],
        'm' => ["     ", "╭─┬─╮", "╵ ╵ ╵", "     "],
        'n' => ["   ", "╭─╮", "╵ ╵", "   "],
        'o' => ["   ", "╭─╮", "╰─╯", "   "],
        'p' => ["   ", "╭─╮", "├─╯", "╵  "],
        'q' => ["   ", "╭─╮", "╰─┤", "  ╵"],
        'r' => ["   ", "╭─╴", "╵  ", "   "],
        's' => ["   ", "╭─╴", "╶─╯", "   "],
        't' => [" ╷ ", "╶┼╴", " ╰╴", "   "],
        'u' => ["   ", "╷ ╷", "╰─╯", "   "],
        'v' => ["   ", "╷╭╯", "╰╯ ", "   "],
        'w' => ["     ", "╷ ╷ ╷", "╰─┴─╯", "     "],
        'x' => ["  ", "╮╭", "╯╰", "  "],
        'y' => ["   ", "╷ ╷", "╰─┤", "╶─╯"],
        'z' => ["   ", "╶─╮", "╰─╴", "   "],
        'ç' => ["   ", "╭─╴", "╰┬╴", " ╯ "],
        '0' => ["╭─╮", "│ │", "╰─╯", "   "],
        '1' => ["╶╮ ", " │ ", "╶┴╴", "   "],
        '2' => ["╭─╮", "╭─╯", "╰─╴", "   "],
        '3' => ["╶─╮", " ─┤", "╶─╯", "   "],
        '4' => ["╷ ╷", "╰─┤", "  ╵", "   "],
        '5' => ["╭─╴", "╰─╮", "╶─╯", "   "],
        '6' => ["╭─╴", "├─╮", "╰─╯", "   "],
        '7' => ["╶─╮", "  │", "  ╵", "   "],
        '8' => ["╭─╮", "├─┤", "╰─╯", "   "],
        '9' => ["╭─╮", "╰─┤", "╶─╯", "   "],
        _ => return None,
    })
}

/// The accents, simplified, over the middle of the letter: the acute a slash, the grave a backslash.
fn accent_of(mark: char) -> Option<char> {
    Some(match mark {
        '\u{301}' => '/',
        '\u{300}' => '\\',
        '\u{302}' => '^',
        '\u{303}' => '~',
        '\u{308}' => '¨',
        _ => return None,
    })
}

/// A letter's big glyph: its own, or its base letter's with the accent over it; none for what has no letter.
fn glyph(c: char) -> Option<[String; 4]> {
    if let Some(g) = glyph_of(c) {
        return Some(g.map(String::from));
    }
    let mut parts = std::iter::once(c).nfd();
    let base = glyph_of(parts.next()?)?;
    let Some(accent) = parts.next().and_then(accent_of) else { return Some(base.map(String::from)) };
    let len = base[1].chars().count();
    let top = format!("{}{accent}{}", " ".repeat(len / 2), " ".repeat(len - len / 2 - 1));
    Some([top, base[1].into(), base[2].into(), base[3].into()])
}

/// `text` in big letters, as many of its words as fit in `width` columns (three between words), its rows without
/// the empty ones; None when not even the first fits, or there's nothing to draw.
pub fn big_text(text: &str, width: usize) -> Option<Vec<String>> {
    let words: Vec<Vec<[String; 4]>> = text
        .nfc()
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .map(|w| w.chars().filter_map(glyph).collect::<Vec<_>>())
        .filter(|w| !w.is_empty())
        .collect();
    let word_width = |w: &Vec<[String; 4]>| w.iter().map(|g| g[0].chars().count() + 1).sum::<usize>() - 1;
    let mut kept = Vec::new();
    let mut used: isize = -3;
    for w in &words {
        if used + 3 + word_width(w) as isize > width as isize {
            break;
        }
        used += 3 + word_width(w) as isize;
        kept.push(w);
    }
    if kept.is_empty() {
        return None;
    }
    let rows: Vec<String> = (0..4)
        .map(|r| kept.iter().map(|w| w.iter().map(|g| g[r].as_str()).collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("   ").trim_end().to_string())
        .collect();
    Some(rows.into_iter().enumerate().filter(|(r, row)| !row.is_empty() || *r == 1 || *r == 2).map(|(_, row)| row).collect())
}
