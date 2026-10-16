//! A browser's tab of `wa --web` (`wa --web-tab`, started by web.rs for each one), as wassup's
//! webtab.ts: the same App as in a terminal, drawn by ratatui to a backend of its own, whose frames go to the page
//! (web/app.js) cell by cell with the colours resolved; the page's keys, mouse, pastes and focus come back as a
//! terminal sends them and are read into crossterm's events. The pictures go as pictures, placed over their cells.
//!
//! Both ways it's one message per line: the page's from the server on stdin (the size, the keys and the mouse as a
//! terminal would send them, and the acknowledgement of each frame), the tab's to the server on descriptor 3. A frame
//! goes only once the page has drawn the one before: the next carries everything that changed meanwhile, so a page or
//! a link that lags behind skips frames instead of falling further behind.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::FromRawFd;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

use anyhow::{Result, bail};
use base64::Engine;
use image::DynamicImage;
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::{Color, Modifier};
use ratatui_image::FontSize;
use ratatui_image::picker::{Picker, ProtocolType};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Input, WebMsg, WebPic};
use crate::store::Store;

/// The page's colours, which it draws the cells that have none of their own in.
const FG: Rgb = [0xe9, 0xed, 0xef];
const BG: Rgb = [0x0b, 0x14, 0x1a];
/// A cell's size in pixels, as the page draws it (twice as tall as wide), for the pictures.
const CELL: (u16, u16) = (10, 20);
/// Style flags as the page takes them.
const BOLD: u8 = 1;
const ITALIC: u8 = 2;
const UNDERLINE: u8 = 4;
const STRUCK: u8 = 8;
/// The longest side a picture sent as pixels keeps.
const PNG_MAX: u32 = 1024;

type Rgb = [u8; 3];

/// `wa --web-tab [--demo]`: waits for the page's size, then runs the App for it until it ends or the server
/// goes.
pub fn main(args: &[String]) -> Result<()> {
    // Descriptor 3 first, before anything opens a file that could take its number.
    // SAFETY: fstat and fcntl only look at and mark descriptor 3, whatever it is.
    let piped = unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        libc::fstat(3, &mut st) == 0 && st.st_mode & libc::S_IFMT == libc::S_IFIFO && libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC) == 0
    };
    if !piped {
        bail!("--web-tab is a browser's tab of --web, which starts it with a pipe on descriptor 3");
    }
    // SAFETY: descriptor 3 is the pipe web.rs gave this process for its messages, and nothing else here takes it.
    let out = unsafe { std::fs::File::from_raw_fd(3) };
    let demo = args.iter().any(|a| a == "--demo");
    let dir = crate::web::setup(demo)?;
    log::info!("start: pid {}, web tab{}", std::process::id(), if demo { ", demo" } else { "" });
    // Nothing is drawn before the page says how big it is.
    let mut lines = BufReader::new(std::io::stdin()).lines();
    let size = loop {
        let Some(Ok(line)) = lines.next() else { return Ok(()) };
        if let Ok(FromPage { t, cols: Some(cols), rows: Some(rows), .. }) = serde_json::from_str(&line)
            && t == "size"
            && cols > 0
            && rows > 0
        {
            break Size::new(cols, rows);
        }
    };
    // As a terminal's caps would be for the page: 24-bit colour, emoji and flags two cells wide, its colours.
    crate::unicode::set_widths(crate::unicode::Widths { flag: Some(2), narrow: Some(vec![]) });
    let caps = crate::term::Caps { utf8: true, fg: Some(FG), bg: Some(BG), truecolor: true, colors: Some(256), ..Default::default() };
    crate::text::set_theme(crate::text::Theme::new(caps.bg, caps.fg, true));
    // The pictures go to the page as pictures, so the picker never draws one; but not half-blocks, which the chat is
    // laid out for differently (pictures sized for 8 by 16 cells, no emoji twice their size), unlike the page. Sixels
    // are what a terminal with real pictures and nothing else to set up would have.
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(FontSize::new(CELL.0, CELL.1));
    picker.set_protocol_type(ProtocolType::Sixel);
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(run(dir, demo, size, lines, out, caps, picker))
}

async fn run(
    dir: PathBuf,
    demo: bool,
    size: Size,
    lines: std::io::Lines<BufReader<std::io::Stdin>>,
    out: std::fs::File,
    caps: crate::term::Caps,
    picker: Picker,
) -> Result<()> {
    let store = Arc::new(Store::open(&dir.join("wassup.db"))?);
    let (wa_tx, wa_rx) = unbounded_channel();
    // The connection: the demonstration's own; otherwise the one --web or a terminal keeps, as in a terminal.
    let (backend, proxy): (Arc<dyn crate::wa::Backend>, Option<Arc<crate::ipc::Proxy>>) = if demo {
        (Arc::new(crate::demo::Demo::new(store.clone(), wa_tx.clone())), None)
    } else {
        let p = crate::ipc::connect(store.clone(), &dir, wa_tx.clone()).await?;
        (p.clone(), Some(p))
    };
    let (out_tx, out_rx) = mpsc::channel();
    let writer = std::thread::spawn(move || write_out(out, out_rx));
    let screen = Arc::new(Mutex::new(Screen::new(size, dir.join("media"), out_tx.clone())));
    let (tx, rx) = unbounded_channel();
    let (reader_screen, reader_tx) = (screen.clone(), tx.clone());
    std::thread::spawn(move || read_page(lines, reader_screen, reader_tx));
    let (web_tx, mut web_rx) = unbounded_channel::<WebMsg>();
    let (tell_screen, tell_out) = (screen.clone(), out_tx.clone());
    let forward = tokio::spawn(async move {
        let mut icon = String::new();
        while let Some(m) = web_rx.recv().await {
            tell(&tell_screen, &tell_out, &mut icon, m);
        }
    });
    let terminal = Terminal::new(WebBackend { screen })?;
    let mut app = App::new(store, backend, picker, caps, None, tx);
    app.web = Some(web_tx);
    if demo {
        let _ = wa_tx.send(crate::wa::ToUi::Connection(crate::wa::Conn::Open, String::new()));
    }
    let (result, mut app) = crate::app::run(app, terminal, rx, wa_rx).await;
    // Leaving signs the page out, but not when it ends on an error.
    if result.is_err() {
        app.web = None;
    }
    app.leave();
    // Its sender gone, the forwarding ends once it has passed on what's left.
    drop(app);
    let _ = forward.await;
    if let Some(p) = proxy {
        p.stop().await;
    }
    let _ = out_tx.send(Out::End);
    let _ = writer.join();
    result
}

/// A message from the page, as the server passed it on.
#[derive(Deserialize)]
struct FromPage {
    t: String,
    d: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
}

/// What goes to the server: a line, or the end of them.
enum Out {
    Line(String),
    End,
}

/// Writes the lines for the server to descriptor 3; the server gone, the tab goes.
fn write_out(mut out: std::fs::File, rx: mpsc::Receiver<Out>) {
    for m in rx {
        let Out::Line(mut line) = m else { break };
        line.push('\n');
        if out.write_all(line.as_bytes()).is_err() {
            std::process::exit(0);
        }
    }
}

/// The page's messages, from the server on stdin: keys and the like as the App's events, its size, its
/// acknowledgements; the server gone, the tab goes.
fn read_page(lines: std::io::Lines<BufReader<std::io::Stdin>>, screen: Arc<Mutex<Screen>>, tx: UnboundedSender<Input>) {
    for line in lines {
        let Ok(line) = line else { break };
        let Ok(m) = serde_json::from_str::<FromPage>(&line) else { continue };
        match (m.t.as_str(), m.d, m.cols, m.rows) {
            ("in", Some(d), ..) => {
                for ev in events(&d) {
                    let _ = tx.send(Input::Term(ev));
                }
            }
            ("size", _, Some(cols), Some(rows)) if cols > 0 && rows > 0 => {
                screen.lock().unwrap().size = Size::new(cols, rows);
                let _ = tx.send(Input::Term(Event::Resize(cols, rows)));
            }
            ("ack", ..) => screen.lock().unwrap().acked(),
            _ => {}
        }
    }
    std::process::exit(0);
}

/// Passes on what the App tells the page besides its cells: the pictures of the frame just drawn (which complete
/// it), a link to open, text to copy, an attachment to open, the title, the icon (when it changes), leaving.
fn tell(screen: &Mutex<Screen>, out: &mpsc::Sender<Out>, icon_shown: &mut String, m: WebMsg) {
    let line = match m {
        WebMsg::Pictures(pics) => return screen.lock().unwrap().framed(pics),
        WebMsg::Link(url) => json!({ "t": "link", "url": url }),
        WebMsg::Copy(text) => json!({ "t": "copy", "text": text }),
        WebMsg::Open(file) => json!({ "t": "open", "file": file }),
        WebMsg::Title(text) => json!({ "t": "title", "text": text }),
        WebMsg::Icon { photo, initials, color } => {
            let media = crate::config_dir().join("media");
            let mut icon = json!({ "t": "icon" });
            match photo.and_then(|p| in_media(&media, &p)) {
                Some(photo) => icon["photo"] = json!(photo),
                None => {
                    if let Some(i) = initials {
                        icon["initials"] = json!(i);
                        icon["color"] = json!(color.as_deref().and_then(color_hex).unwrap_or_default());
                    }
                }
            }
            let key = icon.to_string();
            if key == *icon_shown {
                return;
            }
            *icon_shown = key;
            icon
        }
        WebMsg::Logout => json!({ "t": "logout" }),
    };
    let _ = out.send(Out::Line(line.to_string()));
}

/// A file's path under the media folder, as the server serves it; none for a file elsewhere.
fn in_media(media: &Path, file: &str) -> Option<String> {
    let path = Path::new(file);
    let rel = if path.is_absolute() { path.strip_prefix(media).ok()? } else { path };
    if rel.as_os_str().is_empty() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(rel.to_string_lossy().into_owned())
}

/// A colour as "#rrggbb": given so, or as ratatui writes one out ("Indexed(81)", "Rgb(1, 2, 3)").
fn color_hex(s: &str) -> Option<String> {
    if s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(s.to_ascii_lowercase());
    }
    let inner = |name: &str| s.strip_prefix(name).and_then(|r| r.strip_prefix('(')).and_then(|r| r.strip_suffix(')'));
    let c = if let Some(n) = inner("Indexed") {
        Color::Indexed(n.trim().parse().ok()?)
    } else {
        let v: Vec<u8> = inner("Rgb")?.split(',').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
        let [r, g, b] = v[..] else { return None };
        Color::Rgb(r, g, b)
    };
    rgb(c).map(hex)
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// The 256 palette's colour `i`: the 16 basic ones as xterm has them, then the 6×6×6 cube and the greys.
fn rgb256(i: u8) -> Rgb {
    const BASIC: [Rgb; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match i {
        0..=15 => BASIC[i as usize],
        16..=231 => {
            let n = (i - 16) as usize;
            [CUBE[n / 36], CUBE[n / 6 % 6], CUBE[n % 6]]
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            [g, g, g]
        }
    }
}

/// A cell's colour in RGB; none for the page's own.
fn rgb(c: Color) -> Option<Rgb> {
    Some(match c {
        Color::Reset => return None,
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Indexed(i) => rgb256(i),
        Color::Black => rgb256(0),
        Color::Red => rgb256(1),
        Color::Green => rgb256(2),
        Color::Yellow => rgb256(3),
        Color::Blue => rgb256(4),
        Color::Magenta => rgb256(5),
        Color::Cyan => rgb256(6),
        Color::Gray => rgb256(7),
        Color::DarkGray => rgb256(8),
        Color::LightRed => rgb256(9),
        Color::LightGreen => rgb256(10),
        Color::LightYellow => rgb256(11),
        Color::LightBlue => rgb256(12),
        Color::LightMagenta => rgb256(13),
        Color::LightCyan => rgb256(14),
        Color::White => rgb256(15),
    })
}

/// A character the page draws in a cell of its own: past Latin, Greek and Cyrillic, where fonts fall back to others,
/// but for the box-drawing and block ones, which every monospace font has.
fn own_cell(ch: &str) -> bool {
    let cp = ch.chars().next().map_or(0, u32::from);
    cp >= 0x2000 && !(0x2500..=0x259f).contains(&cp)
}

/// The page's end of the tab: the cells as ratatui last drew them, the frames drawn and waiting for their pictures,
/// what the page was sent and whether it has drawn it yet.
struct Screen {
    /// The page's size, which ratatui asks for before each frame.
    size: Size,
    cells: Buffer,
    cursor: Position,
    /// The frames drawn, each waiting for its pictures, which the App tells after drawing it.
    drawn: VecDeque<Buffer>,
    /// The last frame complete and not sent yet.
    ready: Option<(Buffer, Vec<WebPic>)>,
    /// A frame was sent and the page hasn't said it drew it.
    waiting: bool,
    sent_size: Option<(u16, u16)>,
    sent_rows: Vec<String>,
    sent_pics: String,
    /// The styles told to the page, by id.
    styles: HashMap<(Option<Rgb>, Option<Rgb>, u8), usize>,
    /// The pictures told to the page, by id: a media file, or a picture's pixels by its key and size.
    pics: HashMap<String, usize>,
    media: PathBuf,
    out: mpsc::Sender<Out>,
}

impl Screen {
    fn new(size: Size, media: PathBuf, out: mpsc::Sender<Out>) -> Self {
        Self {
            size,
            cells: Buffer::empty(Rect::new(0, 0, size.width, size.height)),
            cursor: Position::ORIGIN,
            drawn: VecDeque::new(),
            ready: None,
            waiting: false,
            sent_size: None,
            sent_rows: vec![],
            sent_pics: String::new(),
            styles: HashMap::new(),
            pics: HashMap::new(),
            media,
            out,
        }
    }

    /// A frame's pictures: it's complete, and goes now, or once the page has drawn the one before.
    fn framed(&mut self, pics: Vec<WebPic>) {
        let cells = self.drawn.pop_front().unwrap_or_else(|| self.cells.clone());
        self.ready = Some((cells, pics));
        if !self.waiting {
            self.send_ready();
        }
    }

    /// The page drew the last frame: the next, if there's one.
    fn acked(&mut self) {
        self.waiting = false;
        self.send_ready();
    }

    /// The frame ready, as what changed since the last one sent: the size when it changed, the rows that did, the
    /// styles not told before, the pictures when they moved and those not told before. Nothing when nothing did.
    fn send_ready(&mut self) {
        let Some((buf, pics)) = self.ready.take() else { return };
        let (cols, rows) = (buf.area.width, buf.area.height);
        let mut m = Map::new();
        m.insert("t".into(), json!("frame"));
        if self.sent_size != Some((cols, rows)) {
            self.sent_size = Some((cols, rows));
            self.sent_rows = vec![String::new(); rows as usize];
            m.insert("cols".into(), json!(cols));
            m.insert("rows".into(), json!(rows));
        }
        let mut styles = Map::new();
        let mut lines = vec![];
        for y in 0..rows {
            let runs = Value::Array(self.row(&buf, y, &mut styles));
            let key = runs.to_string();
            if self.sent_rows[y as usize] == key {
                continue;
            }
            self.sent_rows[y as usize] = key;
            lines.push(json!([y, runs]));
        }
        if !styles.is_empty() {
            m.insert("styles".into(), Value::Object(styles));
        }
        if !lines.is_empty() {
            m.insert("lines".into(), Value::Array(lines));
        }
        let (shown, defs) = self.pictures(&pics);
        let key = shown.to_string();
        if key != self.sent_pics {
            self.sent_pics = key;
            m.insert("pics".into(), shown);
            if !defs.is_empty() {
                m.insert("defs".into(), Value::Object(defs));
            }
        }
        if m.len() == 1 {
            return;
        }
        self.waiting = true;
        let _ = self.out.send(Out::Line(Value::Object(m).to_string()));
    }

    /// A row as the page draws it: runs of cells in one style, as [cells, text, style] one after the other. A wide
    /// character is a run of its own two cells, and so is any character a monospace font may not have (`own_cell`),
    /// so that whatever width the browser gives it, the cells after stay in place.
    fn row(&mut self, buf: &Buffer, y: u16, styles: &mut Map<String, Value>) -> Vec<Value> {
        struct Run {
            w: u16,
            text: String,
            id: usize,
            open: bool,
        }
        let cols = buf.area.width;
        let mut runs: Vec<Run> = vec![];
        let mut x = 0;
        while x < cols {
            let cell = &buf[(buf.area.x + x, buf.area.y + y)];
            let symbol = cell.symbol();
            let ch = if symbol.is_empty() || symbol.starts_with(|c: char| c < ' ') { " " } else { symbol };
            let id = self.style_id(cell, styles);
            // The cell after a wide character is covered by it, whatever it holds.
            let wide = symbol.width() >= 2 && x + 1 < cols;
            x += if wide { 2 } else { 1 };
            let open = !wide && !own_cell(ch);
            if let Some(last) = runs.last_mut().filter(|l| open && l.open && l.id == id) {
                last.w += 1;
                last.text.push_str(ch);
                continue;
            }
            runs.push(Run { w: if wide { 2 } else { 1 }, text: ch.to_string(), id, open });
        }
        runs.into_iter().flat_map(|r| [json!(r.w), json!(r.text), json!(r.id)]).collect()
    }

    /// The id of a cell's style, told to the page in `styles` the first time: colours resolved (inverse swapped,
    /// hidden as the background), and the flags.
    fn style_id(&mut self, cell: &Cell, styles: &mut Map<String, Value>) -> usize {
        let (mut fg, mut bg) = (rgb(cell.fg), rgb(cell.bg));
        let m = cell.modifier;
        if m.contains(Modifier::REVERSED) {
            (fg, bg) = (Some(bg.unwrap_or(BG)), Some(fg.unwrap_or(FG)));
        }
        if m.contains(Modifier::HIDDEN) {
            fg = Some(bg.unwrap_or(BG));
        }
        let flags = [(Modifier::BOLD, BOLD), (Modifier::ITALIC, ITALIC), (Modifier::UNDERLINED, UNDERLINE), (Modifier::CROSSED_OUT, STRUCK)]
            .iter()
            .filter(|(m2, _)| m.contains(*m2))
            .fold(0, |f, (_, b)| f | b);
        let def = (fg, bg, flags);
        if let Some(id) = self.styles.get(&def) {
            return *id;
        }
        let id = self.styles.len();
        self.styles.insert(def, id);
        styles.insert(id.to_string(), json!([fg.map(hex), bg.map(hex), flags]));
        id
    }

    /// The pictures where they go, in cells, with the part of each in view; each told to the page once in `defs`,
    /// by its file under the media folder, which the page asks the server for, or else by its pixels.
    fn pictures(&mut self, pics: &[WebPic]) -> (Value, Map<String, Value>) {
        let mut shown = vec![];
        let mut defs = Map::new();
        for p in pics {
            match p {
                WebPic::Image { key, file, png, x, y, w, h, from, to } => {
                    let servable = file.as_deref().and_then(|f| in_media(&self.media, f)).filter(|f| is_picture(f));
                    let ident = match (&servable, png) {
                        (Some(f), _) => format!("file:{f}"),
                        (None, Some(img)) => format!("{key}#{}x{}", img.width(), img.height()),
                        (None, None) => continue,
                    };
                    let id = match self.pics.get(&ident) {
                        Some(id) => *id,
                        None => {
                            let def = match (servable, png) {
                                (Some(f), _) => json!({ "file": f }),
                                (None, Some(img)) => match png_base64(img) {
                                    Some(data) => json!({ "png": data }),
                                    None => continue,
                                },
                                (None, None) => continue,
                            };
                            let id = self.pics.len() + 1;
                            self.pics.insert(ident, id);
                            defs.insert(id.to_string(), def);
                            id
                        }
                    };
                    // The rows in view: `h` is the whole picture's.
                    let rows = ((to - from) * *h as f32).round().max(1.0) as u16;
                    shown.push(json!({ "id": id, "x": x, "y": y, "w": w, "h": rows, "from": from, "to": to }));
                }
                WebPic::Emoji { emoji, x, y } => shown.push(json!({ "emoji": emoji, "x": x, "y": y })),
            }
        }
        (Value::Array(shown), defs)
    }
}

fn is_picture(file: &str) -> bool {
    let ext = Path::new(file).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("jpg" | "jpeg" | "png" | "gif" | "webp"))
}

/// A picture as PNG in base64, no bigger than the page needs.
fn png_base64(img: &DynamicImage) -> Option<String> {
    let small = if img.width().max(img.height()) > PNG_MAX { img.thumbnail(PNG_MAX, PNG_MAX) } else { img.clone() };
    let mut png = Vec::new();
    DynamicImage::ImageRgba8(small.to_rgba8()).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(png))
}

/// ratatui's terminal for the page: the cells it draws are kept, each frame put aside when it's flushed, to go
/// with its pictures.
struct WebBackend {
    screen: Arc<Mutex<Screen>>,
}

impl Backend for WebBackend {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut s = self.screen.lock().unwrap();
        let area = s.cells.area;
        for (x, y, cell) in content {
            if x < area.width && y < area.height {
                s.cells[(x, y)] = cell.clone();
            }
        }
        Ok(())
    }

    fn hide_cursor(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn show_cursor(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn get_cursor_position(&mut self) -> std::io::Result<Position> {
        Ok(self.screen.lock().unwrap().cursor)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
        self.screen.lock().unwrap().cursor = position.into();
        Ok(())
    }

    fn clear(&mut self) -> std::io::Result<()> {
        let mut s = self.screen.lock().unwrap();
        s.cells = Buffer::empty(Rect::new(0, 0, s.size.width, s.size.height));
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> std::io::Result<()> {
        match clear_type {
            ClearType::All => self.clear(),
            _ => Ok(()),
        }
    }

    fn size(&self) -> std::io::Result<Size> {
        Ok(self.screen.lock().unwrap().size)
    }

    fn window_size(&mut self) -> std::io::Result<WindowSize> {
        let size = self.size()?;
        Ok(WindowSize { columns_rows: size, pixels: Size::new(size.width * CELL.0, size.height * CELL.1) })
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut s = self.screen.lock().unwrap();
        let frame = s.cells.clone();
        s.drawn.push_back(frame);
        // Each frame drawn is followed by its pictures; more than a few waiting means some never came.
        while s.drawn.len() > 8 {
            s.drawn.pop_front();
        }
        Ok(())
    }
}

/// What the page sent, as a terminal sends it, read into crossterm's events as crossterm reads a terminal's: keys
/// (xterm's sequences, Alt as Escape before), the mouse (SGR), pastes (bracketed) and the focus.
fn events(input: &str) -> Vec<Event> {
    let mut out = vec![];
    let mut rest = input;
    while !rest.is_empty() {
        let (ev, used) = event(rest);
        out.extend(ev);
        // A sequence cut short may end inside a character: past it.
        let mut n = used.clamp(1, rest.len());
        while !rest.is_char_boundary(n) {
            n += 1;
        }
        rest = &rest[n..];
    }
    out
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

/// The first event in `s` and how many bytes it took.
fn event(s: &str) -> (Option<Event>, usize) {
    if let Some(after) = s.strip_prefix("\x1b[200~") {
        return match after.find("\x1b[201~") {
            Some(end) => (Some(Event::Paste(after[..end].to_string())), 6 + end + 6),
            None => (Some(Event::Paste(after.to_string())), s.len()),
        };
    }
    let b = s.as_bytes();
    match b[0] {
        0x1b => match b.get(1) {
            None => (Some(key(KeyCode::Esc, KeyModifiers::NONE)), 1),
            Some(b'[') => csi(s),
            Some(b'O') if b.len() >= 3 => {
                let code = match b[2] {
                    b'A' => KeyCode::Up,
                    b'B' => KeyCode::Down,
                    b'C' => KeyCode::Right,
                    b'D' => KeyCode::Left,
                    b'H' => KeyCode::Home,
                    b'F' => KeyCode::End,
                    c @ b'P'..=b'S' => KeyCode::F(1 + c - b'P'),
                    _ => return (None, 3),
                };
                (Some(key(code, KeyModifiers::NONE)), 3)
            }
            Some(0x1b) => (Some(key(KeyCode::Esc, KeyModifiers::NONE)), 2),
            // Alt, as Escape before the key.
            Some(_) => {
                let (ev, used) = event(&s[1..]);
                let ev = ev.map(|ev| match ev {
                    Event::Key(mut k) => {
                        k.modifiers |= KeyModifiers::ALT;
                        Event::Key(k)
                    }
                    other => other,
                });
                (ev, used + 1)
            }
        },
        b'\r' => (Some(key(KeyCode::Enter, KeyModifiers::NONE)), 1),
        b'\t' => (Some(key(KeyCode::Tab, KeyModifiers::NONE)), 1),
        0x7f => (Some(key(KeyCode::Backspace, KeyModifiers::NONE)), 1),
        c @ 0x01..=0x1a => (Some(key(KeyCode::Char((c - 0x1 + b'a') as char), KeyModifiers::CONTROL)), 1),
        c @ 0x1c..=0x1f => (Some(key(KeyCode::Char((c - 0x1c + b'4') as char), KeyModifiers::CONTROL)), 1),
        0 => (Some(key(KeyCode::Char(' '), KeyModifiers::CONTROL)), 1),
        _ => {
            let c = s.chars().next().unwrap_or(' ');
            let modifiers = if c.is_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
            (Some(key(KeyCode::Char(c), modifiers)), c.len_utf8())
        }
    }
}

/// Modifiers as xterm numbers them (1 + Shift 1, Alt 2, Ctrl 4, …).
fn modifiers(mask: u16) -> KeyModifiers {
    let m = mask.saturating_sub(1);
    let mut out = KeyModifiers::NONE;
    for (bit, k) in [(1, KeyModifiers::SHIFT), (2, KeyModifiers::ALT), (4, KeyModifiers::CONTROL), (8, KeyModifiers::SUPER), (16, KeyModifiers::HYPER), (32, KeyModifiers::META)] {
        if m & bit != 0 {
            out |= k;
        }
    }
    out
}

/// A CSI sequence: its parameters up to the final byte, read as a key, the mouse or the focus.
fn csi(s: &str) -> (Option<Event>, usize) {
    let b = s.as_bytes();
    let Some(end) = b[2..].iter().position(|c| !(0x20..=0x3f).contains(c)).map(|i| i + 2) else { return (None, s.len()) };
    let used = end + 1;
    let (params, last) = (&s[2..end], b[end]);
    let fields: Vec<u16> = params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
    let mods = modifiers(fields.get(1).copied().unwrap_or(1));
    let ev = match last {
        b'M' | b'm' if params.starts_with('<') => mouse(&params[1..], last == b'm'),
        b'I' if params.is_empty() => Some(Event::FocusGained),
        b'O' if params.is_empty() => Some(Event::FocusLost),
        b'Z' => Some(key(KeyCode::BackTab, KeyModifiers::SHIFT)),
        b'A' => Some(key(KeyCode::Up, mods)),
        b'B' => Some(key(KeyCode::Down, mods)),
        b'C' => Some(key(KeyCode::Right, mods)),
        b'D' => Some(key(KeyCode::Left, mods)),
        b'H' => Some(key(KeyCode::Home, mods)),
        b'F' => Some(key(KeyCode::End, mods)),
        b'P' => Some(key(KeyCode::F(1), mods)),
        b'Q' => Some(key(KeyCode::F(2), mods)),
        b'R' => Some(key(KeyCode::F(3), mods)),
        b'S' => Some(key(KeyCode::F(4), mods)),
        b'~' => {
            let code = match fields.first().copied().unwrap_or(0) {
                1 | 7 => KeyCode::Home,
                2 => KeyCode::Insert,
                3 => KeyCode::Delete,
                4 | 8 => KeyCode::End,
                5 => KeyCode::PageUp,
                6 => KeyCode::PageDown,
                n @ 11..=15 => KeyCode::F((n - 10) as u8),
                n @ 17..=21 => KeyCode::F((n - 11) as u8),
                n @ 23..=26 => KeyCode::F((n - 12) as u8),
                _ => return (None, used),
            };
            Some(key(code, mods))
        }
        _ => None,
    };
    (ev, used)
}

/// The mouse in SGR's terms: the button and modifiers, the cell (1-based), pressed or released.
fn mouse(params: &str, released: bool) -> Option<Event> {
    let mut it = params.split(';').map(|p| p.parse::<u16>().ok());
    let (Some(Some(cb)), Some(Some(x)), Some(Some(y))) = (it.next(), it.next(), it.next()) else { return None };
    let button = (cb & 0b11) | ((cb & 0b1100_0000) >> 4);
    let kind = match (button, cb & 0b10_0000 != 0) {
        (0, false) => MouseEventKind::Down(MouseButton::Left),
        (1, false) => MouseEventKind::Down(MouseButton::Middle),
        (2, false) => MouseEventKind::Down(MouseButton::Right),
        (0, true) => MouseEventKind::Drag(MouseButton::Left),
        (1, true) => MouseEventKind::Drag(MouseButton::Middle),
        (2, true) => MouseEventKind::Drag(MouseButton::Right),
        (3, false) => MouseEventKind::Up(MouseButton::Left),
        (3..=5, true) => MouseEventKind::Moved,
        (4, false) => MouseEventKind::ScrollUp,
        (5, false) => MouseEventKind::ScrollDown,
        (6, false) => MouseEventKind::ScrollLeft,
        (7, false) => MouseEventKind::ScrollRight,
        _ => return None,
    };
    let kind = match kind {
        MouseEventKind::Down(b) if released => MouseEventKind::Up(b),
        k => k,
    };
    let mut modifiers = KeyModifiers::NONE;
    for (bit, k) in [(4, KeyModifiers::SHIFT), (8, KeyModifiers::ALT), (16, KeyModifiers::CONTROL)] {
        if cb & bit != 0 {
            modifiers |= k;
        }
    }
    Some(Event::Mouse(MouseEvent { kind, column: x.saturating_sub(1), row: y.saturating_sub(1), modifiers }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ratatui::style::Style;

    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> Event {
        key(code, m)
    }

    #[test]
    fn keys_as_crossterm_reads_them() {
        let none = KeyModifiers::NONE;
        assert_eq!(events("ola"), vec![k(KeyCode::Char('o'), none), k(KeyCode::Char('l'), none), k(KeyCode::Char('a'), none)]);
        assert_eq!(events("É"), vec![k(KeyCode::Char('É'), KeyModifiers::SHIFT)]);
        assert_eq!(events("\r\t\x7f\x1b"), vec![k(KeyCode::Enter, none), k(KeyCode::Tab, none), k(KeyCode::Backspace, none), k(KeyCode::Esc, none)]);
        assert_eq!(events("\x1b[A\x1b[1;5B\x1b[1;2C"), vec![k(KeyCode::Up, none), k(KeyCode::Down, KeyModifiers::CONTROL), k(KeyCode::Right, KeyModifiers::SHIFT)]);
        assert_eq!(events("\x1b[5~\x1b[3;3~\x1b[24~"), vec![k(KeyCode::PageUp, none), k(KeyCode::Delete, KeyModifiers::ALT), k(KeyCode::F(12), none)]);
        assert_eq!(events("\x1bOP\x1b[Z"), vec![k(KeyCode::F(1), none), k(KeyCode::BackTab, KeyModifiers::SHIFT)]);
        assert_eq!(events("\x06\x1f\0"), vec![k(KeyCode::Char('f'), KeyModifiers::CONTROL), k(KeyCode::Char('7'), KeyModifiers::CONTROL), k(KeyCode::Char(' '), KeyModifiers::CONTROL)]);
        assert_eq!(events("\x1bx\x1b\r\x1b\x7f"), vec![k(KeyCode::Char('x'), KeyModifiers::ALT), k(KeyCode::Enter, KeyModifiers::ALT), k(KeyCode::Backspace, KeyModifiers::ALT)]);
        // Nonsense, read past without a panic.
        assert_eq!(events("\x1b[éa"), vec![k(KeyCode::Char('a'), none)]);
        assert_eq!(events("\x1b["), vec![]);
    }

    #[test]
    fn mouse_paste_and_focus() {
        let at = |kind, column, row| Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
        assert_eq!(
            events("\x1b[<0;3;4M\x1b[<0;3;4m\x1b[<32;5;4M\x1b[<35;6;4M"),
            vec![at(MouseEventKind::Down(MouseButton::Left), 2, 3), at(MouseEventKind::Up(MouseButton::Left), 2, 3), at(MouseEventKind::Drag(MouseButton::Left), 4, 3), at(MouseEventKind::Moved, 5, 3)]
        );
        assert_eq!(events("\x1b[<65;1;1M\x1b[<64;1;1M"), vec![at(MouseEventKind::ScrollDown, 0, 0), at(MouseEventKind::ScrollUp, 0, 0)]);
        assert_eq!(events("\x1b[200~uma\rlinha\x1b[201~x"), vec![Event::Paste("uma\rlinha".into()), k(KeyCode::Char('x'), KeyModifiers::NONE)]);
        assert_eq!(events("\x1b[I\x1b[O"), vec![Event::FocusGained, Event::FocusLost]);
    }

    #[test]
    fn colours() {
        assert_eq!(color_hex("Indexed(81)").as_deref(), Some("#5fd7ff"));
        assert_eq!(color_hex("Rgb(1, 2, 255)").as_deref(), Some("#0102ff"));
        assert_eq!(color_hex("#ABCDEF").as_deref(), Some("#abcdef"));
        assert_eq!(color_hex("Reset"), None);
        assert_eq!(rgb256(244), [128, 128, 128]);
    }

    fn screen() -> (Screen, mpsc::Receiver<Out>) {
        let (tx, rx) = mpsc::channel();
        (Screen::new(Size::new(8, 2), PathBuf::from("/srv/wa/media"), tx), rx)
    }

    fn line(rx: &mpsc::Receiver<Out>) -> Option<Value> {
        match rx.try_recv() {
            Ok(Out::Line(l)) => Some(serde_json::from_str(&l).unwrap()),
            _ => None,
        }
    }

    #[test]
    fn frames_wait_for_the_page() {
        let (mut s, rx) = screen();
        s.cells.set_string(0, 0, "ab界c→", Style::new().fg(Color::Indexed(81)));
        s.cells.set_string(0, 1, "x", Style::new().add_modifier(Modifier::REVERSED | Modifier::CROSSED_OUT));
        s.drawn.push_back(s.cells.clone());
        s.framed(vec![]);
        let f = line(&rx).unwrap();
        assert_eq!(f["cols"], 8);
        // "ab" in one run, "界" in its two cells, "c", "→" in a cell of its own, then the blanks.
        assert_eq!(f["lines"][0], json!([0, [2, "ab", 0, 2, "界", 0, 1, "c", 0, 1, "→", 0, 2, "  ", 1]]));
        assert_eq!(f["styles"]["0"], json!(["#5fd7ff", null, 0]));
        assert_eq!(f["styles"]["2"], json!(["#0b141a", "#e9edef", 8]));
        // Not drawn yet: the next waits, and only what changed goes.
        s.cells.set_string(7, 1, "z", Style::new());
        s.drawn.push_back(s.cells.clone());
        s.framed(vec![]);
        assert!(line(&rx).is_none());
        s.acked();
        let f = line(&rx).unwrap();
        assert!(f.get("cols").is_none());
        assert_eq!(f["lines"].as_array().unwrap().len(), 1);
        assert_eq!(f["lines"][0][0], 1);
        // Nothing changed, nothing sent, and nothing to wait for.
        s.acked();
        s.drawn.push_back(s.cells.clone());
        s.framed(vec![]);
        assert!(line(&rx).is_none());
        assert!(!s.waiting);
    }

    #[test]
    fn pictures_by_file_or_pixels() {
        let (mut s, rx) = screen();
        let img = Arc::new(DynamicImage::new_rgb8(4, 2));
        let pic = |key: &str, file: Option<&str>| WebPic::Image { key: key.into(), file: file.map(String::from), png: Some(img.clone()), x: 1, y: 0, w: 4, h: 4, from: 0.25, to: 0.75 };
        s.drawn.push_back(s.cells.clone());
        s.framed(vec![pic("c/1", Some("/srv/wa/media/c/foto.jpg")), pic("c/2", None), pic("c/3", Some("/srv/wa/media/c/video.mp4"))]);
        let f = line(&rx).unwrap();
        assert_eq!(f["defs"]["1"], json!({ "file": "c/foto.jpg" }));
        assert!(f["defs"]["2"]["png"].as_str().is_some_and(|p| p.starts_with("iVBOR")));
        assert!(f["defs"]["3"]["png"].is_string());
        assert_eq!(f["pics"][0], json!({ "id": 1, "x": 1, "y": 0, "w": 4, "h": 2, "from": 0.25, "to": 0.75 }));
        // The same pictures again: already told.
        s.acked();
        s.drawn.push_back(s.cells.clone());
        s.framed(vec![pic("c/1", Some("/srv/wa/media/c/foto.jpg"))]);
        let f = line(&rx).unwrap();
        assert!(f.get("defs").is_none());
        assert_eq!(f["pics"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn media_paths() {
        let media = Path::new("/srv/wa/media");
        assert_eq!(in_media(media, "/srv/wa/media/a/b.jpg").as_deref(), Some("a/b.jpg"));
        assert_eq!(in_media(media, "/srv/wa/other/b.jpg"), None);
        assert_eq!(in_media(media, "/srv/wa/media/../wassup.db"), None);
        assert_eq!(in_media(media, "a/b.jpg").as_deref(), Some("a/b.jpg"));
    }
}
