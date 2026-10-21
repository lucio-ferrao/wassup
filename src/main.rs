//! wassup (`wa`): a WhatsApp client for the terminal, a linked device of the phone through whatsapp-rust. Its data in
//! ~/.config/wassup (or $XDG_CONFIG_HOME/wassup, or WA_HOME), its log there too, at WA_LOG's level. `wa <name>` opens
//! that chat alone; `--demo` shows invented chats with no WhatsApp behind; `--web` serves it to a browser (web.rs).
//! The first one started keeps the connection; those started after it in other terminals share it (ipc.rs).

mod app;
mod bigtext;
mod chat;
mod compose;
mod demo;
mod emoji;
mod emojicodes;
mod format;
mod hearts;
mod herdr;
mod i18n;
mod kitty;
mod ipc;
mod links;
mod list;
mod llm;
mod mood;
mod spell;
mod store;
mod term;
mod text;
mod ui;
mod unicode;
mod wa;
mod web;
mod webtab;

use std::io::stdout;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use simplelog::{ConfigBuilder, LevelFilter, WriteLogger};
use tokio::sync::mpsc::unbounded_channel;

use crate::app::{App, Input};
use crate::store::Store;

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Where this run keeps its data: the session, the chats, the attachments.
pub fn config_dir() -> PathBuf {
    DIR.get().cloned().unwrap_or_default()
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

static EXE: OnceLock<PathBuf> = OnceLock::new();

/// Whether this is the demonstration, which no Herdr pane or real connection may come into.
pub static DEMO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// This program, as it was when it started: rebuilt meanwhile, the running file is gone ("… (deleted)") and a new
/// pane opens the new one at the same path.
pub fn exe() -> PathBuf {
    EXE.get().cloned().unwrap_or_else(|| std::env::current_exe().unwrap_or_default())
}

/// Where the data goes: WA_HOME, or wassup's folder in the user's configuration ($XDG_CONFIG_HOME, else
/// ~/.config); the demonstration's under ~/.cache, apart. Absolute, its `..` worked out, so every process (a
/// terminal, the web) finds the same socket.
pub fn data_dir(demo: Option<&str>) -> PathBuf {
    if demo.is_some() {
        DEMO.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let dir = match (std::env::var_os("WA_HOME"), demo) {
        (Some(d), _) => PathBuf::from(d),
        (None, Some(name)) => home().join(".cache/wassup").join(name),
        (None, None) => std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(".config")).join("wassup"),
    };
    let dir = if dir.is_absolute() { dir } else { std::env::current_dir().unwrap_or_default().join(dir) };
    dir.components().fold(PathBuf::new(), |mut p, c| {
        match c {
            std::path::Component::ParentDir => {
                p.pop();
            }
            std::path::Component::CurDir => {}
            c => p.push(c),
        }
        p
    })
}

/// What every process does first: files for this user only, the data folder and its media folder made private
/// (0700, also when they were there already), and the log there, added to, at WA_LOG's level (info unless told).
pub fn setup(dir: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: umask only sets the process's file mode mask.
    unsafe {
        libc::umask(0o077);
    }
    let _ = EXE.set(std::env::current_exe().map(|p| PathBuf::from(p.to_string_lossy().trim_end_matches(" (deleted)"))).unwrap_or_default());
    std::fs::create_dir_all(dir.join("media")).with_context(|| format!("{}", dir.display()))?;
    for d in [dir.to_path_buf(), dir.join("media")] {
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    }
    let _ = DIR.set(dir.to_path_buf());
    if let Ok(log) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("wassup.log")) {
        let level = match std::env::var("WA_LOG").as_deref() {
            Ok("trace") => LevelFilter::Trace,
            Ok("debug") => LevelFilter::Debug,
            Ok("warn") => LevelFilter::Warn,
            Ok("error") => LevelFilter::Error,
            Ok("silent") | Ok("off") => LevelFilter::Off,
            _ => LevelFilter::Info,
        };
        let _ = WriteLogger::init(level, ConfigBuilder::new().build(), log);
    }
    Ok(())
}

/// The mouse as wassup asks for it: clicks, the wheel and moving with a button held (1000, 1002) in SGR (1006);
/// moving with none (1003) only for a terminal that said its name, which a phone's SSH app doesn't.
fn enable_mouse(any_motion: bool) {
    let mut out = stdout();
    let _ = std::io::Write::write_all(&mut out, if any_motion { b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h" } else { b"\x1b[?1000h\x1b[?1002h\x1b[?1006h" });
    let _ = std::io::Write::flush(&mut out);
}

fn disable_mouse() {
    let mut out = stdout();
    let _ = std::io::Write::write_all(&mut out, b"\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l");
    let _ = std::io::Write::flush(&mut out);
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let demo = args.iter().any(|a| a == "--demo");
    if args.first().is_some_and(|a| a == "--web" || a == "--web-tab") {
        return web::main(&args);
    }
    // `wa ana maria`: every word not an option, together.
    let words: Vec<&str> = args.iter().filter(|a| !a.starts_with("--")).map(String::as_str).collect();
    let wanted = (!words.is_empty()).then(|| words.join(" "));
    let dir = data_dir(demo.then_some("demo"));
    // The demonstration starts afresh each time.
    if demo {
        let _ = std::fs::remove_dir_all(&dir);
    }
    setup(&dir)?;
    log::info!("start: pid {}, {}", std::process::id(), if demo { "demo" } else { "live" });
    // A panic goes to the log, with where, and the terminal is given back before it's told.
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
        ratatui::restore();
        default(info);
    }));

    // What the terminal can do, asked before anything else runs (the picker sets nothing of tmux's this way).
    let caps = term::probe();
    let picker = term::picker(&caps);
    text::set_theme(text::Theme::new(caps.bg, caps.fg, caps.truecolor));

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let result = runtime.block_on(run(dir, demo, wanted, caps, picker));
    // What's still under way on a thread of its own, a question to the model above all (up to its five minutes), is
    // left behind: the runtime dropped would wait for it, with the screen already the terminal's and the program
    // not ended.
    runtime.shutdown_background();
    result
}

async fn run(dir: PathBuf, demo: bool, wanted: Option<String>, caps: term::Caps, picker: ratatui_image::picker::Picker) -> Result<()> {
    let store = Arc::new(Store::open(&dir.join("wassup.db"))?);
    let (wa_tx, wa_rx) = unbounded_channel();
    // The connection: our own in the demonstration; otherwise the one another terminal already has, or a new one.
    let (backend, proxy): (Arc<dyn wa::Backend>, Option<Arc<ipc::Proxy>>) = if demo {
        demo::fill(&store, &dir)?;
        (Arc::new(demo::Demo::new(store.clone(), wa_tx.clone())), None)
    } else {
        let p = ipc::connect(store.clone(), &dir, wa_tx.clone()).await?;
        (p.clone(), Some(p))
    };

    let terminal = ratatui::init();
    execute!(stdout(), EnableFocusChange, EnableBracketedPaste)?;
    enable_mouse(caps.version.is_some());
    // Kitty's keyboard protocol, where the terminal said it has it: Shift+Backspace and Shift+Enter told apart.
    if caps.kitty_keyboard {
        let _ = execute!(stdout(), PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
    }
    // Keys, the mouse and the focus, read on a thread of their own once the terminal is in raw mode.
    let (tx, rx) = unbounded_channel();
    let reader = tx.clone();
    std::thread::spawn(move || term::read_keys(|ev| reader.send(Input::Term(ev)).is_ok()));
    let kitty_keyboard = caps.kitty_keyboard;
    let app = App::new(store, backend, picker, caps, wanted, tx);
    if demo {
        let _ = wa_tx.send(wa::ToUi::Connection(wa::Conn::Open, String::new()));
    }
    let (result, mut app) = app::run(app, terminal, rx, wa_rx).await;
    app.leave();
    if let Some(k) = app.kitty.as_mut() {
        k.dispose(&mut stdout());
    }
    if kitty_keyboard {
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    }
    disable_mouse();
    let _ = execute!(stdout(), DisableFocusChange, DisableBracketedPaste);
    ratatui::restore();
    if let Some(p) = proxy {
        p.stop().await;
    }
    result
}
