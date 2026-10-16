//! What the terminal can do, asked of it before the screen takes it over, as the Node version's term.ts: nothing is assumed
//! from TERM. A sequence only goes out once there's reason to believe the terminal understands it: CSI ones any
//! terminal ignores, but an APC (Kitty's graphics query) some print as text, so it's only sent to a terminal that
//! said its name (XTVERSION) and is one that speaks it. The questions are asked on the alternate screen, so whatever
//! a terminal prints of one it doesn't know goes with that screen. The answers are read with a deadline on this
//! thread, DA1 (which every terminal answers) closing each round.

use std::io::Write;
use std::time::{Duration, Instant};

use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui_image::FontSize;
use ratatui_image::picker::{Picker, ProtocolType};
use regex::Regex;

/// What the terminal said it can do.
#[derive(Clone, Debug, Default)]
pub struct Caps {
    pub kitty_graphics: bool,
    pub utf8: bool,
    pub answers: bool,
    /// Its name and version (XTVERSION), when it told them.
    pub version: Option<String>,
    pub kitty_keyboard: bool,
    pub fg: Option<[u8; 3]>,
    pub bg: Option<[u8; 3]>,
    pub truecolor: bool,
    pub colors: Option<u32>,
    pub text_sizing: bool,
    pub sixel: bool,
    pub cell: Option<(u16, u16)>,
    /// WA_IMAGES=none: no pictures at all.
    pub no_images: bool,
}

/// Reads from stdin until DA1's answer arrives or `wait` passes.
fn read_answer(wait: Duration) -> String {
    let deadline = Instant::now() + wait;
    let mut got = Vec::new();
    static DA1: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| Regex::new(r"\x1b\[\?[\d;]*c").unwrap());
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut fds = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        // SAFETY: one pollfd for stdin, and a buffer as long as the length given.
        let ready = unsafe { libc::poll(&mut fds, 1, left.as_millis().min(50) as i32) };
        if ready <= 0 {
            continue;
        }
        let mut buf = [0u8; 4096];
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
        got.extend_from_slice(&buf[..n as usize]);
        if DA1.is_match(&String::from_utf8_lossy(&got)) {
            break;
        }
    }
    String::from_utf8_lossy(&got).into_owned()
}

/// Writes `query` and a DA1, and reads what came back up to the DA1's answer.
fn ask(query: &str, wait: Duration) -> String {
    let mut out = std::io::stdout();
    let _ = write!(out, "{query}\x1b[c");
    let _ = out.flush();
    read_answer(wait)
}

/// A component of 1 to 4 hex digits reduced to a byte.
fn hex2(c: &str) -> u8 {
    let v = u32::from_str_radix(c, 16).unwrap_or(0) as f64;
    (v / (16f64.powi(c.len() as i32) - 1.0) * 255.0).round() as u8
}

fn locale_utf8() -> bool {
    let l = ["LC_ALL", "LC_CTYPE", "LANG"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty())).unwrap_or_default().to_lowercase();
    l.contains("utf-8") || l.contains("utf8")
}

/// Asks the terminal what it can do; nothing when it isn't one.
pub fn probe() -> Caps {
    let mut caps = Caps { utf8: locale_utf8(), no_images: std::env::var("WA_IMAGES").as_deref() == Ok("none"), ..Default::default() };
    // SAFETY: isatty only looks at the descriptors.
    if unsafe { libc::isatty(0) == 0 || libc::isatty(1) == 0 } || enable_raw_mode().is_err() {
        return caps;
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[?1049h");
    let _ = out.flush();
    ask_all(&mut caps, Duration::from_millis(600));
    let _ = write!(out, "\x1b[?1049l");
    let _ = out.flush();
    // The widths of flags and of the pictographs U+FE0F makes emoji, measured where the cursor goes (unicode.rs).
    if caps.utf8 && caps.answers {
        crate::unicode::set_widths(crate::unicode::measure(&mut crate::unicode::Stdio, Duration::from_millis(800)));
    }
    let _ = disable_raw_mode();
    // WA_COLORS forces 24-bit colour on or off.
    match std::env::var("WA_COLORS").as_deref() {
        Ok("truecolor") => caps.truecolor = true,
        Ok("256") => caps.truecolor = false,
        _ => {}
    }
    log::info!("terminal: {caps:?}, TERM={}", std::env::var("TERM").unwrap_or_default());
    caps
}

fn ask_all(caps: &mut Caps, wait: Duration) {
    // 1. Who it is (XTVERSION), whether it takes Kitty's keyboard protocol, and DA1 (sixels among its attributes).
    let first = ask("\x1b[>0q\x1b[?u", wait);
    caps.kitty_keyboard = Regex::new(r"\x1b\[\?\d+u").unwrap().is_match(&first);
    let da1 = Regex::new(r"\x1b\[\?([\d;]*)c").unwrap();
    caps.answers = da1.is_match(&first);
    caps.version = Regex::new(r"\x1bP>\|([^\x1b]*)\x1b\\").unwrap().captures(&first).map(|c| c[1].trim().to_string());
    caps.sixel = da1.captures(&first).is_some_and(|c| c[1].split(';').any(|a| a == "4"));
    if !caps.answers {
        return;
    }
    // 2. Its text and background colours (OSC 10 and 11).
    let colors = ask("\x1b]10;?\x1b\\\x1b]11;?\x1b\\", wait);
    for c in Regex::new(r"(?i)\x1b\](1[01]);rgba?:([0-9a-f]+)/([0-9a-f]+)/([0-9a-f]+)").unwrap().captures_iter(&colors) {
        let rgb = [hex2(&c[2]), hex2(&c[3]), hex2(&c[4])];
        if &c[1] == "10" {
            caps.fg = Some(rgb);
        } else {
            caps.bg = Some(rgb);
        }
    }
    // 3. 24-bit colour: XTGETTCAP for RGB and Tc (and the colours' count), and DECRQSS on a 24-bit SGR.
    let hex = |s: &str| s.bytes().map(|b| format!("{b:02x}")).collect::<String>();
    let rgb = ask(&format!("\x1bP+q{};{};{}\x1b\\\x1b[38;2;1;2;3m\x1bP$qm\x1b\\\x1b[0m", hex("RGB"), hex("Tc"), hex("colors")), wait);
    caps.truecolor = Regex::new(r"(?i)\x1bP1\+r(?:524742|5463)").unwrap().is_match(&rgb) || Regex::new(r"\x1bP1\$r[^\x1b]*38[:;]2[:;]+1[:;]2[:;]3").unwrap().is_match(&rgb);
    if let Some(c) = Regex::new(r"(?i)\x1bP1\+r636f6c6f7273=([0-9a-f]+)").unwrap().captures(&rgb) {
        let digits: String = c[1].as_bytes().chunks(2).filter_map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok()).map(char::from).collect();
        caps.colors = digits.parse().ok().filter(|n: &u32| *n > 0);
    }
    // 4. Text drawn bigger by Kitty itself (OSC 66): a space at twice the size takes two cells.
    let kitty_named = caps.version.as_deref().is_some_and(|v| v.to_lowercase().contains("kitty"));
    if caps.utf8 && kitty_named {
        let a = ask("\x1b[H\x1b]66;s=2; \x1b\\\x1b[6n", wait);
        caps.text_sizing = Regex::new(r"\x1b\[\d+;(\d+)R").unwrap().captures(&a).is_some_and(|c| &c[1] == "3");
    }
    // 5. A cell's size in pixels, for a terminal that takes sixels.
    if caps.sixel {
        let a = ask("\x1b[16t", wait);
        if let Some(c) = Regex::new(r"\x1b\[6;(\d+);(\d+)t").unwrap().captures(&a) {
            let (h, w): (u16, u16) = (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0));
            if w > 0 && h > 0 {
                caps.cell = Some((w, h));
            }
        }
    }
    // 6. Kitty's graphics query, only to those who named themselves as speaking it; their cells' size too.
    let kitty_terms = Regex::new(r"(?i)ghostty|kitty|wezterm|konsole|iterm").unwrap();
    if caps.version.as_deref().is_some_and(|v| kitty_terms.is_match(v)) {
        let a = ask("\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t", wait);
        caps.kitty_graphics = a.contains("\x1b_Gi=31;OK");
        if let Some(c) = Regex::new(r"\x1b\[6;(\d+);(\d+)t").unwrap().captures(&a) {
            let (h, w): (u16, u16) = (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0));
            if w > 0 && h > 0 {
                caps.cell = Some((w, h));
            }
        }
    }
}

/// How pictures are drawn: WA_IMAGES forces it; in a Herdr pane half-blocks (the multiplexer doesn't relay the
/// pictures); else Kitty's graphics, sixels where the cells' size is known, or half-blocks. Made before anything
/// else runs: ratatui-image sets an option of tmux's when it believes it's in one, which wassup never does, so it's
/// not told.
pub fn picker(caps: &Caps, in_herdr: bool) -> Picker {
    let forced = std::env::var("WA_IMAGES").ok();
    let kind = match forced.as_deref() {
        Some("kitty") => ProtocolType::Kitty,
        Some("sixel") => ProtocolType::Sixel,
        Some("blocks") | Some("none") => ProtocolType::Halfblocks,
        _ if in_herdr => ProtocolType::Halfblocks,
        _ if caps.kitty_graphics => ProtocolType::Kitty,
        _ if caps.sixel && caps.cell.is_some() => ProtocolType::Sixel,
        _ => ProtocolType::Halfblocks,
    };
    let (w, h) = caps.cell.unwrap_or((10, 20));
    let tmux = std::env::var("TERM").is_ok_and(|t| t.starts_with("tmux")) || std::env::var("TERM_PROGRAM").as_deref() == Ok("tmux");
    let saved = (std::env::var_os("TERM"), std::env::var_os("TERM_PROGRAM"));
    if tmux {
        // SAFETY: called from main before any other thread is started.
        unsafe {
            std::env::set_var("TERM", "xterm-256color");
            std::env::remove_var("TERM_PROGRAM");
        }
    }
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(FontSize::new(w, h));
    if tmux {
        // SAFETY: as above.
        unsafe {
            match saved.0 {
                Some(v) => std::env::set_var("TERM", v),
                None => std::env::remove_var("TERM"),
            }
            if let Some(v) = saved.1 {
                std::env::set_var("TERM_PROGRAM", v);
            }
        }
    }
    picker.set_protocol_type(kind);
    log::info!("pictures: {kind:?}, cells of {w}x{h} pixels");
    picker
}
