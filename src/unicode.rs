//! Emoji at the widths the screen counts them, as wassup has them (its unicode.ts, and the measuring in its term.ts).
//!
//! ratatui lays a line out by each grapheme's width as unicode-width counts it, and the terminal draws it at its own;
//! where the two disagree, what follows on the line lands a column off. Sequences terminals measure unpredictably are
//! reduced to something of known width (`tame`): the emoji without its skin tone, the first of a joined (ZWJ)
//! sequence, and "[PT]" for a flag, unless the terminal gives flags 2 columns. A text pictograph with U+FE0F ("❤️")
//! unicode-width counts 2, as most terminals draw it, but some draw it 1 (Apple's Terminal, Termius), and not always
//! the same for every symbol: before the screen is taken the terminal is asked (`measure`), each one written at the
//! top left and the cursor's column read back, and those it draws 1 wide lose the selector, which leaves the text
//! pictograph that both count 1.
//!
//! wassup moved blessed's cursor, after each of those emoji, to the cell blessed assumed when the terminal hadn't said
//! how wide it draws them. ratatui moves its own to the next cell it writes after a wide one, but after an emoji with
//! U+FE0F it writes the column that follows again, to blank it on terminals that draw it 1 wide; on one that drew it
//! 2 wide the cursor is already past that column, and the blank lands on the next character. `settle` marks those
//! cells 2 wide for ratatui's diff, which then leaves that column to the emoji.
//!
//! The cursor in a text being typed moves by graphemes (`edit`), as in wassup: an emoji with its tone, a flag, a
//! letter with its accent written apart, each one step.

use std::borrow::Cow;
use std::io::{self, Write};
use std::num::NonZeroU16;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ratatui::buffer::{Buffer, CellDiffOption};
use unicode_width::UnicodeWidthStr;

/// The variation selector that asks for a symbol's emoji form.
const VS16: char = '\u{FE0F}';
const ZWJ: char = '\u{200D}';
/// The flag the terminal is asked to draw.
const FLAG: &str = "\u{1F1F5}\u{1F1F9}";

/// How wide the terminal draws what terminals disagree on, as it said when asked (`measure`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Widths {
    /// Columns it gives a flag ("🇵🇹"): one per letter, two each, or two for the pair; None if it didn't say.
    pub flag: Option<u16>,
    /// The text pictographs it draws 1 column wide with U+FE0F ("❤️" in Apple's Terminal), in code point order; the
    /// others it draws with 2. None if it didn't say.
    pub narrow: Option<Vec<char>>,
}

impl Widths {
    /// Whether flags are drawn as such, the terminal giving them 2 columns, or as "[PT]".
    fn real_flags(&self) -> bool {
        self.flag == Some(2)
    }

    /// Whether the terminal draws `c` followed by U+FE0F 1 column wide.
    fn is_narrow(&self, c: char) -> bool {
        self.narrow.as_ref().is_some_and(|n| n.binary_search(&c).is_ok())
    }
}

static WIDTHS: OnceLock<Widths> = OnceLock::new();
static UNKNOWN: Widths = Widths { flag: None, narrow: None };

/// Sets the widths `tame` and `settle` go by, measured once at startup; a second call changes nothing.
pub fn set_widths(widths: Widths) {
    let _ = WIDTHS.set(widths);
}

/// The widths set with `set_widths`, or none known before.
pub fn widths() -> &'static Widths {
    WIDTHS.get().unwrap_or(&UNKNOWN)
}

/// `s` as the terminal can show it at the widths ratatui counts: skin tones dropped, a joined (ZWJ) sequence reduced
/// to its first emoji, a flag as "[PT]" unless the terminal gives flags 2 columns, and U+FE0F dropped after the text
/// pictographs the terminal draws 1 wide with it. Borrowed, with nothing allocated, when there's nothing to tame.
pub fn tame(s: &str) -> Cow<'_, str> {
    tame_with(s, widths())
}

fn tame_with<'a>(s: &'a str, widths: &Widths) -> Cow<'a, str> {
    let flags = widths.real_flags();
    let narrow = widths.narrow.as_ref().is_some_and(|n| !n.is_empty());
    let wild = |c: char| c == ZWJ || is_tone(c) || (!flags && is_regional(c)) || (narrow && c == VS16);
    if s.is_ascii() || !s.chars().any(wild) {
        return Cow::Borrowed(s);
    }
    // Tones go first, as in wassup: one between two regional indicators leaves a flag.
    let cs: Vec<char> = s.chars().filter(|&c| !is_tone(c)).collect();
    let at = |i: usize| cs.get(i).copied();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(c) = at(i) {
        i += 1;
        if is_regional(c) && at(i).is_some_and(is_regional) {
            let (a, b) = (c, cs[i]);
            i += 1;
            if flags {
                out.extend([a, b]);
            } else {
                out.extend(['[', letter(a), letter(b), ']']);
            }
            continue;
        }
        out.push(c);
        if !is_pictographic(c) {
            continue;
        }
        if at(i) == Some(VS16) {
            i += 1;
            if !widths.is_narrow(c) {
                out.push(VS16);
            }
        }
        // Terminals draw a joined sequence as one emoji, or each of its parts, or anything in between.
        while at(i) == Some(ZWJ) && at(i + 1).is_some_and(is_pictographic) {
            i += 2;
            if at(i) == Some(VS16) {
                i += 1;
            }
        }
    }
    Cow::Owned(out)
}

fn is_tone(c: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
}

fn is_regional(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// A regional indicator's letter: "P" for 🇵.
fn letter(c: char) -> char {
    char::from_u32(c as u32 - 0x1F1E6 + u32::from(b'A')).unwrap_or('?')
}

fn is_pictographic(c: char) -> bool {
    let c = c as u32;
    let i = PICTOGRAPHIC.partition_point(|&(_, hi)| hi < c);
    PICTOGRAPHIC.get(i).is_some_and(|&(lo, _)| lo <= c)
}

/// Marks each emoji with U+FE0F in a frame as 2 columns wide for ratatui's diff, unless the terminal said it draws it
/// 1 wide. Unmarked, ratatui writes the column after it again, to blank it where the terminal drew the emoji 1 wide;
/// where it drew it 2 wide the cursor is already past that column, and the blank lands on the next character. Marked,
/// that column is left alone and the cursor moved to the next cell written, as wassup moved blessed's; a terminal that
/// draws it 1 wide without having said so keeps whatever that column had. Called on the frame's buffer once it's
/// drawn, before ratatui flushes it.
pub fn settle(buf: &mut Buffer) {
    settle_with(buf, widths());
}

fn settle_with(buf: &mut Buffer, widths: &Widths) {
    const TWO: NonZeroU16 = NonZeroU16::new(2).unwrap();
    for cell in &mut buf.content {
        let s = cell.symbol();
        if s.len() < 4 || !s.contains(VS16) || s.width() != 2 {
            continue;
        }
        let mut cs = s.chars();
        let narrow = matches!((cs.next(), cs.next(), cs.next()), (Some(c), Some(VS16), None) if widths.is_narrow(c));
        if !narrow {
            cell.set_diff_option(CellDiffOption::ForcedWidth(TWO));
        }
    }
}

/// Raw access to a terminal already in raw mode, for `measure`.
pub trait Tty {
    /// Writes `bytes` to the terminal and flushes them.
    fn send(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Reads what the terminal answered into `buf`, waiting up to `wait` for it; 0 when nothing came in time.
    fn recv(&mut self, buf: &mut [u8], wait: Duration) -> io::Result<usize>;
}

/// The process's own terminal: stdout written, stdin read once poll says there's something, as term.rs reads it.
pub struct Stdio;

impl Tty for Stdio {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut out = io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()
    }

    fn recv(&mut self, buf: &mut [u8], wait: Duration) -> io::Result<usize> {
        let mut fds = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        let ms = wait.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: one pollfd for stdin, and a buffer as long as the length given.
        let ready = unsafe { libc::poll(&mut fds, 1, ms) };
        if ready < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted { Ok(0) } else { Err(e) };
        }
        if ready == 0 {
            return Ok(0);
        }
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        match n {
            0 => Err(io::ErrorKind::UnexpectedEof.into()),
            n if n < 0 => Err(io::Error::last_os_error()),
            n => Ok(n as usize),
        }
    }
}

/// How wide the terminal draws a flag and each text pictograph with U+FE0F, as the Node version's term.ts asks: each one
/// written at the top left, the cursor's column (CPR) says how many cells it took, all asked at once and DA1 last,
/// which every terminal answers and which closes the answers; `wait` is how long they're waited for. Terminals
/// disagree (a flag takes one cell per letter, two each, or two for the pair; "❤️" 1 or 2), so they're measured, not
/// assumed, and a width without an answer for it is left unknown. For before the screen is taken, the terminal in raw
/// mode: the probes are drawn on the alternate screen, left once they're answered.
pub fn measure(tty: &mut impl Tty, wait: Duration) -> Widths {
    let mut asked = String::from("\x1b[?1049h");
    for s in std::iter::once(FLAG.to_string()).chain(probed().map(|c| format!("{c}{VS16}"))) {
        asked.push_str("\x1b[H");
        asked.push_str(&s);
        asked.push_str("\x1b[6n");
    }
    asked.push_str("\x1b[c");
    if tty.send(asked.as_bytes()).is_err() {
        return Widths::default();
    }
    let answer = answers(tty, wait);
    let _ = tty.send(b"\x1b[?1049l");
    parse(&answer)
}

/// What the terminal answers until DA1's answer arrives or `wait` passes.
fn answers(tty: &mut impl Tty, wait: Duration) -> String {
    let deadline = Instant::now() + wait;
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match tty.recv(&mut buf, left) {
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
        if has_da1(&got) {
            break;
        }
    }
    String::from_utf8_lossy(&got).into_owned()
}

/// Whether DA1's answer ("ESC [ ? 62 ; 22 c") is in `got`.
fn has_da1(got: &[u8]) -> bool {
    (0..got.len()).filter(|&i| got[i..].starts_with(b"\x1b[?")).any(|i| {
        got[i + 3..].iter().find(|&&b| !b.is_ascii_digit() && b != b';') == Some(&b'c')
    })
}

/// The widths in the cursor's positions reported ("ESC [ row ; column R"), the flag's first and then each probe's.
fn parse(answer: &str) -> Widths {
    let cols: Vec<u16> = answer
        .split("\x1b[")
        .filter_map(|s| {
            let (row, col) = s.split_once('R')?.0.split_once(';')?;
            row.parse::<u16>().ok()?;
            col.parse::<u16>().ok()
        })
        .map(|col| col.saturating_sub(1))
        .collect();
    let Some((&flag, rest)) = cols.split_first() else {
        return Widths::default();
    };
    let narrow = (rest.len() == probed().count())
        .then(|| probed().zip(rest).filter(|&(_, &w)| w == 1).map(|(c, _)| c).collect());
    Widths { flag: Some(flag), narrow }
}

/// The pictographs measured, in code point order.
fn probed() -> impl Iterator<Item = char> {
    PROBED.iter().flat_map(|&(lo, hi)| lo..=hi).filter_map(char::from_u32)
}

/// The text pictographs U+FE0F turns into emoji ("❤️", "✔️", "☀️"), the ones measured: the pictographs with the
/// Emoji property but not Emoji_Presentation, below U+3300 or from U+1F000, as wassup's SELECTABLE (Unicode 17).
const PROBED: [(u32, u32); 116] = [
    (0x00A9, 0x00A9), (0x00AE, 0x00AE), (0x203C, 0x203C), (0x2049, 0x2049), (0x2122, 0x2122), (0x2139, 0x2139),
    (0x2194, 0x2199), (0x21A9, 0x21AA), (0x2328, 0x2328), (0x23CF, 0x23CF), (0x23ED, 0x23EF), (0x23F1, 0x23F2),
    (0x23F8, 0x23FA), (0x24C2, 0x24C2), (0x25AA, 0x25AB), (0x25B6, 0x25B6), (0x25C0, 0x25C0), (0x25FB, 0x25FC),
    (0x2600, 0x2604), (0x260E, 0x260E), (0x2611, 0x2611), (0x2618, 0x2618), (0x261D, 0x261D), (0x2620, 0x2620),
    (0x2622, 0x2623), (0x2626, 0x2626), (0x262A, 0x262A), (0x262E, 0x262F), (0x2638, 0x263A), (0x2640, 0x2640),
    (0x2642, 0x2642), (0x265F, 0x2660), (0x2663, 0x2663), (0x2665, 0x2666), (0x2668, 0x2668), (0x267B, 0x267B),
    (0x267E, 0x267E), (0x2692, 0x2692), (0x2694, 0x2697), (0x2699, 0x2699), (0x269B, 0x269C), (0x26A0, 0x26A0),
    (0x26A7, 0x26A7), (0x26B0, 0x26B1), (0x26C8, 0x26C8), (0x26CF, 0x26CF), (0x26D1, 0x26D1), (0x26D3, 0x26D3),
    (0x26E9, 0x26E9), (0x26F0, 0x26F1), (0x26F4, 0x26F4), (0x26F7, 0x26F9), (0x2702, 0x2702), (0x2708, 0x2709),
    (0x270C, 0x270D), (0x270F, 0x270F), (0x2712, 0x2712), (0x2714, 0x2714), (0x2716, 0x2716), (0x271D, 0x271D),
    (0x2721, 0x2721), (0x2733, 0x2734), (0x2744, 0x2744), (0x2747, 0x2747), (0x2763, 0x2764), (0x27A1, 0x27A1),
    (0x2934, 0x2935), (0x2B05, 0x2B07), (0x3030, 0x3030), (0x303D, 0x303D), (0x3297, 0x3297), (0x3299, 0x3299),
    (0x1F170, 0x1F171), (0x1F17E, 0x1F17F), (0x1F202, 0x1F202), (0x1F237, 0x1F237), (0x1F321, 0x1F321),
    (0x1F324, 0x1F32C), (0x1F336, 0x1F336), (0x1F37D, 0x1F37D), (0x1F396, 0x1F397), (0x1F399, 0x1F39B),
    (0x1F39E, 0x1F39F), (0x1F3CB, 0x1F3CE), (0x1F3D4, 0x1F3DF), (0x1F3F3, 0x1F3F3), (0x1F3F5, 0x1F3F5),
    (0x1F3F7, 0x1F3F7), (0x1F43F, 0x1F43F), (0x1F441, 0x1F441), (0x1F4FD, 0x1F4FD), (0x1F549, 0x1F54A),
    (0x1F56F, 0x1F570), (0x1F573, 0x1F579), (0x1F587, 0x1F587), (0x1F58A, 0x1F58D), (0x1F590, 0x1F590),
    (0x1F5A5, 0x1F5A5), (0x1F5A8, 0x1F5A8), (0x1F5B1, 0x1F5B2), (0x1F5BC, 0x1F5BC), (0x1F5C2, 0x1F5C4),
    (0x1F5D1, 0x1F5D3), (0x1F5DC, 0x1F5DE), (0x1F5E1, 0x1F5E1), (0x1F5E3, 0x1F5E3), (0x1F5E8, 0x1F5E8),
    (0x1F5EF, 0x1F5EF), (0x1F5F3, 0x1F5F3), (0x1F5FA, 0x1F5FA), (0x1F6CB, 0x1F6CB), (0x1F6CD, 0x1F6CF),
    (0x1F6E0, 0x1F6E5), (0x1F6E9, 0x1F6E9), (0x1F6F0, 0x1F6F0), (0x1F6F3, 0x1F6F3),
];

/// The code points with the Extended_Pictographic property (Unicode 17), the emoji a joined sequence is made of.
const PICTOGRAPHIC: [(u32, u32); 156] = [
    (0x00A9, 0x00A9), (0x00AE, 0x00AE), (0x203C, 0x203C), (0x2049, 0x2049), (0x2122, 0x2122), (0x2139, 0x2139),
    (0x2194, 0x2199), (0x21A9, 0x21AA), (0x231A, 0x231B), (0x2328, 0x2328), (0x23CF, 0x23CF), (0x23E9, 0x23F3),
    (0x23F8, 0x23FA), (0x24C2, 0x24C2), (0x25AA, 0x25AB), (0x25B6, 0x25B6), (0x25C0, 0x25C0), (0x25FB, 0x25FE),
    (0x2600, 0x2604), (0x260E, 0x260E), (0x2611, 0x2611), (0x2614, 0x2615), (0x2618, 0x2618), (0x261D, 0x261D),
    (0x2620, 0x2620), (0x2622, 0x2623), (0x2626, 0x2626), (0x262A, 0x262A), (0x262E, 0x262F), (0x2638, 0x263A),
    (0x2640, 0x2640), (0x2642, 0x2642), (0x2648, 0x2653), (0x265F, 0x2660), (0x2663, 0x2663), (0x2665, 0x2666),
    (0x2668, 0x2668), (0x267B, 0x267B), (0x267E, 0x267F), (0x2692, 0x2697), (0x2699, 0x2699), (0x269B, 0x269C),
    (0x26A0, 0x26A1), (0x26A7, 0x26A7), (0x26AA, 0x26AB), (0x26B0, 0x26B1), (0x26BD, 0x26BE), (0x26C4, 0x26C5),
    (0x26C8, 0x26C8), (0x26CE, 0x26CF), (0x26D1, 0x26D1), (0x26D3, 0x26D4), (0x26E9, 0x26EA), (0x26F0, 0x26F5),
    (0x26F7, 0x26FA), (0x26FD, 0x26FD), (0x2702, 0x2702), (0x2705, 0x2705), (0x2708, 0x270D), (0x270F, 0x270F),
    (0x2712, 0x2712), (0x2714, 0x2714), (0x2716, 0x2716), (0x271D, 0x271D), (0x2721, 0x2721), (0x2728, 0x2728),
    (0x2733, 0x2734), (0x2744, 0x2744), (0x2747, 0x2747), (0x274C, 0x274C), (0x274E, 0x274E), (0x2753, 0x2755),
    (0x2757, 0x2757), (0x2763, 0x2764), (0x2795, 0x2797), (0x27A1, 0x27A1), (0x27B0, 0x27B0), (0x27BF, 0x27BF),
    (0x2934, 0x2935), (0x2B05, 0x2B07), (0x2B1B, 0x2B1C), (0x2B50, 0x2B50), (0x2B55, 0x2B55), (0x3030, 0x3030),
    (0x303D, 0x303D), (0x3297, 0x3297), (0x3299, 0x3299), (0x1F004, 0x1F004), (0x1F02C, 0x1F02F), (0x1F094, 0x1F09F),
    (0x1F0AF, 0x1F0B0), (0x1F0C0, 0x1F0C0), (0x1F0CF, 0x1F0D0), (0x1F0F6, 0x1F0FF), (0x1F170, 0x1F171),
    (0x1F17E, 0x1F17F), (0x1F18E, 0x1F18E), (0x1F191, 0x1F19A), (0x1F1AE, 0x1F1E5), (0x1F201, 0x1F20F),
    (0x1F21A, 0x1F21A), (0x1F22F, 0x1F22F), (0x1F232, 0x1F23A), (0x1F23C, 0x1F23F), (0x1F249, 0x1F25F),
    (0x1F266, 0x1F321), (0x1F324, 0x1F393), (0x1F396, 0x1F397), (0x1F399, 0x1F39B), (0x1F39E, 0x1F3F0),
    (0x1F3F3, 0x1F3F5), (0x1F3F7, 0x1F3FA), (0x1F400, 0x1F4FD), (0x1F4FF, 0x1F53D), (0x1F549, 0x1F54E),
    (0x1F550, 0x1F567), (0x1F56F, 0x1F570), (0x1F573, 0x1F57A), (0x1F587, 0x1F587), (0x1F58A, 0x1F58D),
    (0x1F590, 0x1F590), (0x1F595, 0x1F596), (0x1F5A4, 0x1F5A5), (0x1F5A8, 0x1F5A8), (0x1F5B1, 0x1F5B2),
    (0x1F5BC, 0x1F5BC), (0x1F5C2, 0x1F5C4), (0x1F5D1, 0x1F5D3), (0x1F5DC, 0x1F5DE), (0x1F5E1, 0x1F5E1),
    (0x1F5E3, 0x1F5E3), (0x1F5E8, 0x1F5E8), (0x1F5EF, 0x1F5EF), (0x1F5F3, 0x1F5F3), (0x1F5FA, 0x1F64F),
    (0x1F680, 0x1F6C5), (0x1F6CB, 0x1F6D2), (0x1F6D5, 0x1F6E5), (0x1F6E9, 0x1F6E9), (0x1F6EB, 0x1F6F0),
    (0x1F6F3, 0x1F6FF), (0x1F7DA, 0x1F7FF), (0x1F80C, 0x1F80F), (0x1F848, 0x1F84F), (0x1F85A, 0x1F85F),
    (0x1F888, 0x1F88F), (0x1F8AE, 0x1F8AF), (0x1F8BC, 0x1F8BF), (0x1F8C2, 0x1F8CF), (0x1F8D9, 0x1F8FF),
    (0x1F90C, 0x1F93A), (0x1F93C, 0x1F945), (0x1F947, 0x1F9FF), (0x1FA58, 0x1FA5F), (0x1FA6E, 0x1FAFF),
    (0x1FC00, 0x1FFFD),
];

#[cfg(test)]
mod tests {
    use unicode_segmentation::UnicodeSegmentation;
    use super::*;

    use ratatui::backend::{Backend, CrosstermBackend};
    use ratatui::style::Style;

    fn known(flag: u16, narrow: &str) -> Widths {
        let mut narrow: Vec<char> = narrow.chars().collect();
        narrow.sort();
        Widths { flag: Some(flag), narrow: Some(narrow) }
    }

    #[test]
    fn plain_text_is_borrowed() {
        for s in ["", "Bom dia a todos!", "Ação às 9h — já?", "😀 ❤️ ✔️ 👍"] {
            assert!(matches!(tame_with(s, &UNKNOWN), Cow::Borrowed(_)), "{s}");
        }
        // Real flags and a selector the terminal draws wide leave nothing to tame.
        assert!(matches!(tame_with("Até 🇫🇷 ❤️", &known(2, "")), Cow::Borrowed(_)));
    }

    #[test]
    fn skin_tones_go() {
        assert_eq!(tame_with("Olá 👋🏽 👍🏿!", &UNKNOWN), "Olá 👋 👍!");
    }

    #[test]
    fn joined_sequences_keep_their_first_emoji() {
        let w = UNKNOWN.clone();
        assert_eq!(tame_with("👩‍💻 a programar", &w), "👩 a programar");
        assert_eq!(tame_with("👩🏽‍💻", &w), "👩");
        assert_eq!(tame_with("🧑‍🤝‍🧑 juntos", &w), "🧑 juntos");
        assert_eq!(tame_with("🏳️‍🌈", &w), "🏳️");
        assert_eq!(tame_with("👁️‍🗨️", &w), "👁️");
        // A joiner between letters isn't an emoji's.
        assert_eq!(tame_with("a\u{200D}b", &w), "a\u{200D}b");
    }

    #[test]
    fn flags_as_letters_unless_drawn_2_wide() {
        assert_eq!(tame_with("Paris 🇫🇷!", &UNKNOWN), "Paris [FR]!");
        assert_eq!(tame_with("Paris 🇫🇷!", &known(4, "")), "Paris [FR]!");
        assert_eq!(tame_with("Paris 🇫🇷!", &known(2, "")), "Paris 🇫🇷!");
        // In pairs from the left; one left over stays as it is. A tone between two letters goes first.
        assert_eq!(tame_with("🇫🇷🇧", &UNKNOWN), "[FR]🇧");
        assert_eq!(tame_with("🇫🏽🇷", &UNKNOWN), "[FR]");
    }

    #[test]
    fn selector_goes_where_the_terminal_draws_1_wide() {
        let w = known(2, "❤✔");
        assert_eq!(tame_with("❤️ e ✔️ e ☀️", &w), "❤ e ✔ e ☀️");
        assert_eq!(tame_with("❤️‍🔥", &w), "❤");
        // Not measured: kept, ratatui counts 2 and `settle` keeps the diff to that.
        assert_eq!(tame_with("❤️ e ✔️", &UNKNOWN), "❤️ e ✔️");
    }

    #[test]
    fn tamed_widths_match_the_terminal() {
        // unicode-width counts every probe with the selector 2, as a terminal that draws it wide; for one that draws it
        // 1 wide what's left is the bare pictograph, which both count the same.
        let all: String = probed().collect();
        let narrow = known(2, &all);
        for c in probed() {
            let s = format!("{c}{VS16}");
            assert_eq!(s.width(), 2, "{c:?}");
            assert_eq!(tame_with(&s, &narrow), c.to_string());
        }
    }

    #[test]
    fn probe_list_as_wassups() {
        assert_eq!(probed().count(), 207);
        assert!(probed().all(is_pictographic));
        assert!(probed().is_sorted());
        assert!(PICTOGRAPHIC.windows(2).all(|w| w[0].1 < w[1].0));
    }

    /// A terminal that answers with the columns given, one CPR per probe and then DA1, in chunks.
    struct Answering {
        sent: Vec<u8>,
        answer: Vec<u8>,
    }

    impl Tty for Answering {
        fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.sent.extend_from_slice(bytes);
            Ok(())
        }

        fn recv(&mut self, buf: &mut [u8], _: Duration) -> io::Result<usize> {
            let n = self.answer.len().min(buf.len()).min(100);
            buf[..n].copy_from_slice(&self.answer[..n]);
            self.answer.drain(..n);
            Ok(n)
        }
    }

    fn answering(flag: u16, cols: impl Iterator<Item = u16>) -> Answering {
        let mut answer = format!("\x1b[1;{}R", flag + 1);
        for c in cols {
            answer += &format!("\x1b[1;{}R", c + 1);
        }
        answer += "\x1b[?62;22c";
        Answering { sent: Vec::new(), answer: answer.into_bytes() }
    }

    #[test]
    fn measures_from_the_cursor_positions() {
        let mut tty = answering(2, probed().map(|c| if c == '❤' || c == '☀' { 1 } else { 2 }));
        assert_eq!(measure(&mut tty, Duration::from_secs(1)), known(2, "❤☀"));
        let sent = String::from_utf8(tty.sent).unwrap();
        assert!(sent.starts_with("\x1b[?1049h\x1b[H🇵🇹\x1b[6n"));
        assert!(sent.ends_with("\x1b[c\x1b[?1049l"));
        assert_eq!(sent.matches("\x1b[6n").count(), 208);
    }

    #[test]
    fn missing_answers_leave_widths_unknown() {
        // A flag measured but not every probe: the probes' widths aren't known.
        let mut tty = answering(4, std::iter::repeat_n(2, 10));
        assert_eq!(measure(&mut tty, Duration::from_secs(1)), Widths { flag: Some(4), narrow: None });
        let mut tty = Answering { sent: Vec::new(), answer: b"\x1b[?62;22c".to_vec() };
        assert_eq!(measure(&mut tty, Duration::from_secs(1)), Widths::default());
        // Nothing at all: given up when the time's up.
        let mut tty = Answering { sent: Vec::new(), answer: Vec::new() };
        assert_eq!(measure(&mut tty, Duration::from_millis(20)), Widths::default());
    }

    #[test]
    fn da1_closes_the_answers() {
        assert!(has_da1(b"\x1b[1;3R\x1b[?62;4;22c"));
        assert!(has_da1(b"\x1b[?6c"));
        assert!(!has_da1(b"\x1b[1;3R\x1b[?62;4"));
        assert!(!has_da1(b"\x1b[?1;2R"));
    }

    /// What a terminal that draws each grapheme `width(g)` cells wide shows on a line, `screen`, after `bytes`: one
    /// string per cell, "" for the second of a wide one.
    fn replay(bytes: &[u8], screen: &mut [String], width: impl Fn(&str) -> usize) {
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        let mut x = 0;
        let mut rest = s.as_str();
        while !rest.is_empty() {
            if let Some(seq) = rest.strip_prefix("\x1b[") {
                let end = seq.find(|c: char| c.is_ascii_alphabetic()).unwrap();
                if seq[end..].starts_with('H') {
                    x = seq[..end].split(';').nth(1).unwrap().parse::<usize>().unwrap() - 1;
                }
                rest = &seq[end + 1..];
                continue;
            }
            let text = &rest[..rest.find('\x1b').unwrap_or(rest.len())];
            for g in text.graphemes(true) {
                let w = width(g);
                // What's pushed past the right edge is lost.
                if x + w > screen.len() {
                    break;
                }
                // Writing over either half of a wide character blanks its other half.
                if screen[x].is_empty() {
                    screen[x - 1] = " ".into();
                }
                for i in x + 1..=(x + w).min(screen.len() - 1) {
                    if screen[i].is_empty() {
                        screen[i] = " ".into();
                    }
                }
                screen[x] = g.into();
                if w == 2 {
                    screen[x + 1] = String::new();
                }
                x += w;
            }
            rest = &rest[text.len()..];
        }
    }

    /// Whether a terminal that draws "❤️" `heart` cells wide shows the frame of `after` drawn over one of `before`:
    /// tamed and settled for `widths`, or as ratatui alone draws it without them.
    fn shows(before: &str, after: &str, heart: usize, widths: Option<&Widths>) -> bool {
        let prev = Buffer::with_lines([before]);
        let mut next = Buffer::empty(prev.area);
        match widths {
            Some(w) => {
                next.set_string(0, 0, tame_with(after, w), Style::new());
                settle_with(&mut next, w);
            }
            None => next.set_string(0, 0, after, Style::new()),
        }
        let mut screen: Vec<String> = prev.content.iter().map(|c| c.symbol().to_string()).collect();
        let mut out = Vec::new();
        CrosstermBackend::new(&mut out).draw(prev.diff(&next).into_iter()).unwrap();
        replay(&out, &mut screen, |g| if g == "❤\u{FE0F}" { heart } else { g.width() });
        // The column after a wide character shows nothing: covered by it, or blank where the terminal drew it 1 wide.
        let mut covered = 0;
        next.content.iter().zip(&screen).all(|(cell, shown)| {
            let ok = if covered > 0 { shown.is_empty() || shown == " " } else { cell.symbol() == shown };
            covered = if covered > 0 { covered - 1 } else { cell.symbol().width().saturating_sub(1) };
            ok
        })
    }

    #[test]
    fn settled_frames_show_as_drawn() {
        for after in ["❤️cdefgh", "❤️xyzfgh", "a❤️ d!"] {
            // On a terminal that draws "❤️" 2 wide ratatui alone writes the column after it again, a column too far.
            assert!(!shows("abcdefgh", after, 2, None), "{after}");
            assert!(shows("abcdefgh", after, 2, Some(&UNKNOWN)), "{after}");
            assert!(shows("abcdefgh", after, 2, Some(&known(2, ""))), "{after}");
            // On one that draws it 1 wide ratatui's blank is right; measured, the selector goes and both count 1.
            assert!(shows("abcdefgh", after, 1, None), "{after}");
            assert!(shows("abcdefgh", after, 1, Some(&known(2, "❤"))), "{after}");
            // Not measured, the column after it keeps what it had, as in wassup.
            assert!(!shows("abcdefgh", after, 1, Some(&UNKNOWN)), "{after}");
        }
    }
}
