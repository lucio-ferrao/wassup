//! The screen's state and what changes it, as the Node version's ui.ts: keys, the mouse, the focus, the clock (animations,
//! notices, the typing that's told), and what the connection tells. A tab for each open chat, the chat list over
//! them with "/", ↑ selecting messages, → or typing over one to answer it, ":" to react, the "☺" a message shows under
//! the pointer for its quick reactions, Ctrl+F to search.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use image::DynamicImage;
use ratatui::Terminal;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::compose::{self, Edit};
use crate::emojicodes;
use crate::i18n::{s, t};
use crate::llm::{Cancel, Fix, Suggestion};
use crate::store::{Chat, Msg, Store};
use crate::term::Caps;
use crate::wa::{Backend, Conn, ToUi};

/// How many of a chat's latest messages are drawn at first, and how many more each time the top is reached.
pub const PAGE: usize = 300;
/// How long WhatsApp lets my messages be edited, and deleted for everyone, in seconds.
pub const EDIT_WINDOW: i64 = 15 * 60;
pub const DELETE_WINDOW: i64 = 2 * 24 * 3600;
/// WhatsApp Web's quick reactions, in its order, plus "⋯" for typing any other.
pub const QUICK: [&str; 7] = ["👍", "❤️", "😂", "😮", "😢", "🙏", "⋯"];
/// A chat's mood shows only while its last message is at most a week old.
pub const MOOD_AGE: i64 = 7 * 24 * 3600;
/// The notice for a message in another chat: time to appear, stay and go, in milliseconds.
pub const NOTICE: (u64, u64, u64) = (400, 6000, 800);
/// Lines of the emoji list in view.
pub const SUGGEST_ROWS: usize = 5;
/// Frames of the flight of a message just sent from the band to its place.
pub const FLY_FRAMES: u32 = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Picker,
    Messages,
    Input,
}

/// A search in the open chat (Ctrl+F): what's typed, its cursor and folded words, the messages found (the most
/// recent first) and which is in view.
pub struct Search {
    pub query: String,
    pub cursor: usize,
    pub words: Vec<String>,
    pub hits: Vec<Msg>,
    pub at: usize,
}

/// Everything that reaches the loop.
pub enum Input {
    Term(Event),
    Wa(ToUi),
    /// A picture decoded off the loop, by its key.
    Image(String, Result<DynamicImage, String>),
    /// The writing suggestion for a text (the model's, or the spell checker's).
    Ghost(String, Option<Suggestion>, bool),
    /// The corrections found in one of my sent messages, by its key.
    SentFixes(String, Vec<Fix>, bool),
    /// What a picture shows, by its key; None when the model said nothing.
    /// What a picture shows (None when the model said nothing), or that it couldn't be asked (true when it was the
    /// model that failed).
    Described(String, Result<Option<String>, bool>),
    /// Herdr's answer about this pane's focus at start, taken only if the terminal hasn't said yet.
    InitialFocus(bool),
    /// Herdr had no other chat's pane to go to: the list opens.
    HerdrNoNext,
    /// In a browser, the chat or the chat list scrolled by the page itself: the row it stopped at on top.
    Scrolled(usize),
}

/// What a click can land on, besides the list's rows and the messages.
#[derive(Clone, Debug)]
pub enum Hit {
    /// The chat list's own tab, its ✖, a chat's tab, its ✖.
    Own,
    OwnClose,
    Tab(usize),
    Close(usize),
    /// A message's "☺", one of its quick reactions.
    Smile(String),
    React(String, String),
    /// One of the emoji suggestions, the floating correction, the correction over one of my messages.
    Suggestion(usize),
    Ghost,
    SentFix,
    /// "↓ N" beside the band.
    Below,
    /// The open chat's photo, a list row's photo.
    HeadPhoto,
    Photo(String),
    /// The popup.
    Viewer,
}

/// A row of the chat list: a day's separator, a blank, a chat's two rows, or a message found and its text's rows.
#[derive(Clone, Debug)]
pub enum Slot {
    Day(String),
    Blank,
    Top(usize),
    Bottom(usize),
    Hit(usize),
    HitText(String),
}

impl Slot {
    /// Whether the selection may rest on it.
    pub fn selectable(&self) -> bool {
        matches!(self, Slot::Top(_) | Slot::Hit(_))
    }
}

/// The emoji suggestions: what each shows beside the emoji, and how many graphemes before the cursor it replaces.
#[derive(Clone, Debug)]
pub struct Sugg {
    pub emoji: String,
    pub code: String,
    pub length: usize,
}

/// The mouse selection of text: the cell pressed, the one the pointer is at, and the panel's columns and rows.
#[derive(Clone, Copy, Debug)]
pub struct TextSel {
    pub ax: u16,
    pub ay: u16,
    pub hx: u16,
    pub hy: u16,
    pub area: Rect,
    pub input: bool,
}

/// What the popup shows: a message's picture or a chat's photo.
#[derive(Clone, Debug)]
pub struct Viewing {
    pub key: String,
    pub path: String,
}

/// The message just sent, flying from the band to its place.
pub struct Flight {
    pub text: String,
    pub lines: Vec<String>,
    pub top: u16,
    pub started: Instant,
    pub room: usize,
    pub frame: u32,
    pub arrived: bool,
    pub next: Instant,
}

/// The rising emoji to launch once its message is in view: which message, which emoji, since when asked.
pub struct HeartWanted {
    pub msg: String,
    pub emoji: String,
    pub since: Instant,
}

pub struct App {
    pub store: Arc<Store>,
    pub backend: Arc<dyn Backend>,
    pub picker: Picker,
    pub caps: Caps,
    pub pid: u32,
    pub herdr: Option<crate::herdr::Herdr>,
    /// `wa <name>`: one chat at a time, closing it closes the program.
    pub wanted: Option<String>,
    pub focus: Focus,

    pub tabs: Vec<String>,
    pub active: usize,
    pub drafts: HashMap<String, (String, usize)>,

    pub picker_open: bool,
    pub picker_search: bool,
    pub chats: Vec<Chat>,
    pub slots: Vec<Slot>,
    pub filtered: Vec<Chat>,
    pub hits_found: Vec<Msg>,
    pub found_query: Option<String>,
    pub list_sel: usize,
    pub list_top: usize,
    pub list_rows: usize,
    pub picked: bool,
    pub filter: String,
    pub filter_cursor: usize,
    pub filter_shown: Option<String>,
    pub picker_focus: Option<Option<String>>,
    pub picker_focus_row: Option<usize>,
    pub picker_closed_at: Option<Instant>,
    pub picker_left: Option<(String, usize)>,
    pub picker_press: Option<(Instant, bool)>,
    pub moods: HashMap<String, String>,
    pub business: HashSet<String>,
    pub profile_asked: HashSet<String>,
    pub list_area: Rect,
    pub list_words: Vec<String>,
    /// The selection to be brought into view on the next frame: it moved, or the list opened; the wheel leaves the
    /// view where it took it.
    pub list_follow: bool,

    pub msgs: Vec<Msg>,
    pub reactions: HashMap<String, Vec<(String, usize)>>,
    pub shown: HashMap<String, usize>,
    pub older_pending: HashMap<String, Instant>,
    pub older_done: HashSet<String>,
    pub at_bottom: bool,
    /// The first row of the chat's rows in view.
    pub top: usize,
    /// The message at the top of the view and how far its first row is above it, kept while scrolled up, so what
    /// comes or goes above it doesn't move the view.
    pub anchor: Option<(String, i64)>,
    pub selected: Option<String>,
    pub reply_to: Option<String>,
    pub react_to: Option<String>,
    pub editing: Option<String>,
    pub deleting: Option<(String, bool)>,
    pub search: Option<Search>,
    pub keep_selected_in_view: bool,
    pub rendered_jid: Option<String>,
    pub loading_drawn: bool,
    /// In a browser, which scrolls the chat itself (webtab.rs): the rows laid out again each time they are (their
    /// generation), each drawn on its own for the page and kept while they last; the row the page has on top, and
    /// how many times the view was moved from here, which the page then follows; this frame's rows for it.
    pub layout_gen: u64,
    pub panel_rows: Option<(u64, u16, Arc<Vec<ratatui::buffer::Buffer>>)>,
    pub page_top: Option<usize>,
    pub scroll_gen: u64,
    pub web_panel: Option<WebPanel>,
    /// The thumbnails the messages came with, decoded for the panel's pictures out of view.
    pub thumbs: HashMap<String, Arc<DynamicImage>>,
    /// The chat list for a browser: how many times it was worked out again, and its rows drawn, by what they show.
    pub list_gen: u64,
    pub list_panel: Option<(u64, Arc<Vec<ratatui::buffer::Buffer>>)>,

    pub input: String,
    pub cursor: usize,
    pub accepted: Option<String>,
    pub suggestions: Vec<Sugg>,
    pub suggest_index: usize,
    pub suggest_top: usize,
    pub suggest_face: bool,
    pub suggest_kind: Option<&'static str>,

    pub ghost: Option<(String, Suggestion)>,
    pub ghost_due: Option<(Instant, String)>,
    pub ghost_cancel: Option<Cancel>,
    pub accept_on_arrival: usize,
    pub float_key: String,
    pub float_off: String,
    pub float_since: Instant,
    pub llm_down_until: Option<Instant>,
    pub sent_fixes: HashMap<String, Option<Vec<Fix>>>,
    pub sent_queue: Vec<Msg>,
    pub sent_checking: bool,
    pub sent_fix: Option<(Msg, Fix, u16, u16)>,
    pub descriptions: HashMap<String, Option<String>>,
    pub describing: Option<(String, Cancel)>,
    pub viewing: Option<Viewing>,
    pub own_words: Option<Arc<HashSet<String>>>,
    pub speller: Arc<std::sync::OnceLock<crate::spell::Speller>>,

    pub conn: Conn,
    pub conn_detail: String,
    pub me: String,
    pub qr: Option<String>,
    pub showing_qr: bool,
    pub typing: HashMap<String, Vec<String>>,
    pub online: HashSet<String>,
    pub group_online: HashMap<String, usize>,
    pub available: bool,
    pub transient: Option<(String, Instant)>,
    pub notice: Option<(String, String, Instant)>,

    pub last_active: Instant,
    pub last_active_saved: Option<Instant>,
    pub last_write: Option<Instant>,
    pub has_focus: Option<bool>,
    pub last_presence_touch: Option<Instant>,
    pub composing: Option<(String, Instant)>,
    pub composing_until: Option<Instant>,
    pub spawning: HashMap<String, Instant>,

    pub pointer: Option<(u16, u16)>,
    pub hover: Option<String>,
    pub quick_for: Option<String>,
    pub tab_hover: Option<i64>,
    pub drag: Option<(String, u16)>,
    pub press: Option<(u16, u16, Option<String>, bool)>,
    pub sel_press: Option<(u16, u16, Rect, bool)>,
    pub text_sel: Option<TextSel>,
    pub last_click: Option<(String, Instant)>,
    pub hits: Vec<(u16, u16, u16, Hit)>,

    pub hearts: Vec<crate::hearts::Animation>,
    pub hearts_wanted: Vec<HeartWanted>,
    pub flight: Option<Flight>,

    /// Pictures decoded, by key; a message's full one once downloaded, its thumbnail till then.
    pub images: HashMap<String, Arc<DynamicImage>>,
    pub full: HashSet<String>,
    pub asked: HashSet<String>,
    /// Pictures that couldn't be read, by key, and why; attachments asked to be downloaded.
    pub failed: HashMap<String, String>,
    pub fetching: HashSet<String>,
    pub scaled: HashMap<(String, u16, u16), Arc<DynamicImage>>,
    pub crops: HashMap<(String, u16, u16, u16, u16), Protocol>,
    pub rows: Vec<crate::chat::Row>,
    pub rows_width: u16,
    pub rows_dirty: bool,
    pub list_dirty: bool,

    pub head_area: Rect,
    pub head_photo: Option<Rect>,
    pub head_name: Option<Rect>,
    pub chat_area: Rect,
    pub band_area: Rect,
    pub band_text: Rect,
    pub band_lines: Vec<Vec<String>>,
    pub band_first: usize,
    pub band_header: bool,
    /// The model's word floating over the one it replaces: the word, its line, its column.
    pub ghost_float: Option<(String, usize, usize)>,
    pub prompt_w: u16,
    pub below_at: Option<(u16, u16)>,
    pub tabs_area: Rect,

    pub title_shown: String,
    pub icon_shown: String,
    pub unread_shown: u32,
    pub clear_screen: bool,
    pub bell: bool,
    pub clipboard: Option<String>,
    /// In a browser's tab: where what isn't a frame goes, and the frame's pictures.
    pub web: Option<UnboundedSender<WebMsg>>,
    pub web_pics: Vec<WebPic>,
    /// Kitty's graphics, the pictures placed by hand rather than drawn into the cells.
    pub kitty: Option<crate::kitty::Kitty>,
    /// Emoji on their own drawn twice their size by the terminal (OSC 66): this frame's, and the last one's.
    pub big_text: Vec<(u16, u16, String)>,
    pub big_shown: Vec<(u16, u16, String)>,
    pub big_out: Option<String>,
    pub quit: bool,
    pub tx: UnboundedSender<Input>,
}

impl App {
    pub fn new(store: Arc<Store>, backend: Arc<dyn Backend>, picker: Picker, caps: Caps, wanted: Option<String>, tx: UnboundedSender<Input>) -> Self {
        let me = store.state("me").unwrap_or_default();
        let lang = if crate::i18n::lang() == crate::i18n::Lang::Pt { "pt" } else { "en" };
        let speller = crate::spell::load_in_background(crate::spell::dictionary_dirs(None), lang.to_string());
        let kitty = (picker.protocol_type() == ratatui_image::picker::ProtocolType::Kitty).then(crate::kitty::Kitty::new);
        let mut app = Self {
            store,
            backend,
            picker,
            caps,
            pid: std::process::id(),
            herdr: if crate::DEMO.load(std::sync::atomic::Ordering::Relaxed) { None } else { crate::herdr::Herdr::detect() },
            wanted,
            focus: Focus::Input,
            tabs: vec![],
            active: 0,
            drafts: HashMap::new(),
            picker_open: false,
            picker_search: false,
            chats: vec![],
            slots: vec![],
            filtered: vec![],
            hits_found: vec![],
            found_query: None,
            list_sel: 0,
            list_top: 0,
            list_rows: 0,
            picked: false,
            filter: String::new(),
            filter_cursor: 0,
            filter_shown: None,
            picker_focus: None,
            picker_focus_row: None,
            picker_closed_at: None,
            picker_left: None,
            picker_press: None,
            moods: HashMap::new(),
            business: HashSet::new(),
            profile_asked: HashSet::new(),
            list_area: Rect::default(),
            list_words: vec![],
            list_follow: true,
            msgs: vec![],
            reactions: HashMap::new(),
            shown: HashMap::new(),
            older_pending: HashMap::new(),
            older_done: HashSet::new(),
            at_bottom: true,
            top: 0,
            anchor: None,
            selected: None,
            reply_to: None,
            react_to: None,
            editing: None,
            deleting: None,
            search: None,
            keep_selected_in_view: false,
            rendered_jid: None,
            loading_drawn: false,
            layout_gen: 0,
            panel_rows: None,
            page_top: None,
            scroll_gen: 0,
            web_panel: None,
            thumbs: HashMap::new(),
            list_gen: 0,
            list_panel: None,
            input: String::new(),
            cursor: 0,
            accepted: None,
            suggestions: vec![],
            suggest_index: 0,
            suggest_top: 0,
            suggest_face: false,
            suggest_kind: None,
            ghost: None,
            ghost_due: None,
            ghost_cancel: None,
            accept_on_arrival: 0,
            float_key: String::new(),
            float_off: String::new(),
            float_since: Instant::now(),
            llm_down_until: None,
            sent_fixes: HashMap::new(),
            sent_queue: vec![],
            sent_checking: false,
            sent_fix: None,
            descriptions: HashMap::new(),
            describing: None,
            viewing: None,
            own_words: None,
            speller,
            conn: Conn::Connecting,
            conn_detail: String::new(),
            me,
            qr: None,
            showing_qr: false,
            typing: HashMap::new(),
            online: HashSet::new(),
            group_online: HashMap::new(),
            available: false,
            transient: None,
            notice: None,
            last_active: Instant::now(),
            last_active_saved: None,
            last_write: None,
            has_focus: None,
            last_presence_touch: None,
            composing: None,
            composing_until: None,
            spawning: HashMap::new(),
            pointer: None,
            hover: None,
            quick_for: None,
            tab_hover: None,
            drag: None,
            press: None,
            sel_press: None,
            text_sel: None,
            last_click: None,
            hits: vec![],
            hearts: vec![],
            hearts_wanted: vec![],
            flight: None,
            images: HashMap::new(),
            full: HashSet::new(),
            asked: HashSet::new(),
            failed: HashMap::new(),
            fetching: HashSet::new(),
            scaled: HashMap::new(),
            crops: HashMap::new(),
            rows: vec![],
            rows_width: 0,
            rows_dirty: true,
            list_dirty: true,
            head_area: Rect::default(),
            head_photo: None,
            head_name: None,
            chat_area: Rect::default(),
            band_area: Rect::default(),
            band_text: Rect::default(),
            band_lines: vec![],
            band_first: 0,
            band_header: false,
            ghost_float: None,
            prompt_w: 2,
            below_at: None,
            tabs_area: Rect::default(),
            title_shown: String::new(),
            icon_shown: String::new(),
            unread_shown: 0,
            clear_screen: false,
            bell: false,
            clipboard: None,
            web: None,
            web_pics: vec![],
            kitty,
            big_text: vec![],
            big_shown: vec![],
            big_out: None,
            quit: false,
            tx,
        };
        app.load_moods();
        app.chats = app.store.chats().unwrap_or_default();
        app.chats.reverse();
        app.register_terminal();
        // `wa ana` opens that chat right away: the most recent whose name or number has the text.
        if let Some(w) = app.wanted.clone() {
            match app.find_chat(&w) {
                Some(jid) => app.open_tab(&jid, true),
                None => {
                    app.quit_reason(t("noChatWith", &[&w]));
                }
            }
        }
        app.open_list_if_any();
        // In Herdr the focus only comes with a change: the pane's state at start is asked of it.
        if let Some(h) = app.herdr.clone() {
            let tx = app.tx.clone();
            tokio::task::spawn_blocking(move || {
                if let Some(f) = h.pane_focused() {
                    let _ = tx.send(Input::InitialFocus(f));
                }
            });
        }
        app
    }

    /// Ends with a reason, written once the screen is the terminal's again.
    fn quit_reason(&mut self, reason: String) {
        eprintln!("{reason}");
        self.quit = true;
    }

    pub fn current(&self) -> Option<&str> {
        self.tabs.get(self.active).map(String::as_str)
    }

    /// One chat at a time: started for one, or in a Herdr pane, whose tabs are Herdr's own.
    pub fn fixed(&self) -> bool {
        self.wanted.is_some() || self.herdr.is_some()
    }

    pub fn is_open(&self) -> bool {
        self.conn == Conn::Open
    }

    pub fn msg(&self, id: &str) -> Option<&Msg> {
        self.msgs.iter().find(|m| m.id == id)
    }

    /// The picker's chosen state: a chat shows selected once picked, or with a filter or a search.
    pub fn picker_chosen(&self) -> bool {
        self.picked || !self.filter.is_empty() || self.picker_search
    }

    fn load_moods(&mut self) {
        self.moods = self
            .store
            .states("mood:")
            .into_iter()
            .filter_map(|(k, v)| {
                let emoji = serde_json::from_str::<serde_json::Value>(&v).ok()?.get("emoji")?.as_str()?.to_string();
                Some((k["mood:".len()..].to_string(), emoji))
            })
            .collect();
        self.business = self.store.business_chats().into_iter().collect();
    }

    /// A chat's mood, " " and its emoji, while its last message is no older than a week.
    pub fn mood_of(&self, jid: &str, last_ts: i64) -> String {
        match self.moods.get(jid).filter(|e| !e.is_empty()) {
            Some(e) if chrono::Utc::now().timestamp() - last_ts <= MOOD_AGE => format!(" {e}"),
            _ => String::new(),
        }
    }

    /// The two emoji taking turns every 500 ms while someone types.
    pub fn typing_emoji() -> &'static str {
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        if (ms / 500) % 2 == 1 { "💭" } else { "💬" }
    }

    /// What stands after a chat's name in its tab and the title: someone typing, 🔔 unread, or its mood.
    pub fn tab_mark(&self, jid: &str) -> String {
        if self.typing.get(jid).is_some_and(|w| !w.is_empty()) {
            return format!(" {}", Self::typing_emoji());
        }
        let chat = self.chats.iter().find(|c| c.jid == jid);
        if chat.is_some_and(|c| c.unread != 0) {
            return " 🔔".into();
        }
        self.mood_of(jid, chat.map(|c| c.last_ts).unwrap_or(0))
    }

    /// What stands after the list's own name: someone typing anywhere, 🔔 with unread anywhere, or 👾.
    pub fn list_mark(&self) -> String {
        if self.typing.values().any(|w| !w.is_empty()) {
            format!(" {}", Self::typing_emoji())
        } else if self.chats.iter().any(|c| c.unread != 0 && !c.archived) {
            " 🔔".into()
        } else {
            " 👾".into()
        }
    }

    // ---------- terminals ----------

    fn tabs_key(&self) -> String {
        format!("tabs:pid{}", self.pid)
    }

    /// Always starts with no tabs, and clears the records of terminals already gone.
    fn register_terminal(&mut self) {
        for (k, v) in self.store.states("tabs:") {
            let pid = serde_json::from_str::<serde_json::Value>(&v).ok().and_then(|v| v.get("pid")?.as_u64());
            if pid.is_none_or(|p| !crate::ipc::alive(p as u32)) {
                self.store.delete_state(&k);
            }
        }
        self.save_tabs();
    }

    pub fn save_tabs(&mut self) {
        self.last_active_saved = Some(self.last_active);
        let since = |i: Instant| chrono::Utc::now().timestamp_millis() - i.elapsed().as_millis() as i64;
        let v = serde_json::json!({
            "tabs": self.tabs, "active": self.active, "pid": self.pid, "lastActive": since(self.last_active),
            "herdrTab": self.herdr.as_ref().map(|h| h.tab_id().to_string()), "herdrPane": self.herdr.as_ref().map(|h| h.pane_id().to_string()),
        });
        let _ = self.store.set_state(&self.tabs_key(), &v.to_string());
    }

    /// The other live terminals' records: tabs, last activity, Herdr tab and pane.
    fn other_terminals(&self) -> Vec<serde_json::Value> {
        let mine = self.tabs_key();
        self.store
            .states("tabs:")
            .into_iter()
            .filter(|(k, _)| *k != mine)
            .filter_map(|(_, v)| serde_json::from_str::<serde_json::Value>(&v).ok())
            .filter(|v| v.get("pid").and_then(|p| p.as_u64()).is_some_and(|p| crate::ipc::alive(p as u32)))
            .collect()
    }

    fn open_elsewhere(&self, jid: &str) -> bool {
        self.other_terminals().iter().any(|t| t.get("tabs").and_then(|t| t.as_array()).is_some_and(|a| a.iter().any(|j| j.as_str() == Some(jid))))
    }

    fn is_most_recent_terminal(&self) -> bool {
        let mine = chrono::Utc::now().timestamp_millis() - self.last_active.elapsed().as_millis() as i64;
        self.other_terminals().iter().all(|t| t.get("lastActive").and_then(|v| v.as_i64()).unwrap_or(0) <= mine)
    }

    /// This terminal was used: the most recent, saved at most every two seconds.
    fn touch_activity(&mut self) {
        self.last_active = Instant::now();
        if self.last_active_saved.is_none_or(|s| s.elapsed() > Duration::from_secs(2)) {
            self.save_tabs();
        }
    }

    // ---------- tabs ----------

    /// Opens (or finds) a chat's tab; with `activate` it comes to the front and is read.
    pub fn open_tab(&mut self, jid: &str, activate: bool) {
        let i = match self.tabs.iter().position(|t| t == jid) {
            Some(i) => i,
            None => {
                self.tabs.push(jid.to_string());
                self.tabs.len() - 1
            }
        };
        if activate {
            self.activate_tab(i);
        } else {
            self.save_tabs();
        }
    }

    /// Puts a tab in front without changing the bar's order (typing does that).
    pub fn activate_tab(&mut self, i: usize) {
        let Some(jid) = self.tabs.get(i).cloned() else { return };
        let switching = self.current() != Some(jid.as_str()) || self.msgs.is_empty();
        if i != self.active || switching {
            self.search = None;
            self.stop_composing();
            let prev = self.current().map(String::from).filter(|p| *p != jid);
            if let Some(p) = &prev {
                self.shown.remove(p);
            }
            self.active = i;
            self.at_bottom = true;
            self.anchor = None;
            self.selected = None;
            self.reply_to = None;
            self.react_to = None;
            self.quick_for = None;
            // A correction isn't a draft: it goes. The rest stays saved in the chat left.
            if self.editing.take().is_some() {
                self.input.clear();
                self.cursor = 0;
            }
            self.switch_draft(prev.as_deref(), Some(&jid));
            self.crops.clear();
            self.reload_messages();
        }
        if self.notice.as_ref().is_some_and(|n| n.0 == jid) {
            self.notice = None;
        }
        self.save_tabs();
        self.backend.subscribe_presence(&jid);
        self.backend.ensure_name(&jid);
        if self.picker_open {
            self.close_picker();
        }
        self.set_focus(Focus::Input);
        self.mark_read_if_seen(&jid);
    }

    /// The tab in front moves to the first place, next to the band, when typing starts in it.
    fn promote_active(&mut self) {
        if self.active == 0 || self.active >= self.tabs.len() || self.fixed() {
            return;
        }
        let jid = self.tabs.remove(self.active);
        self.tabs.insert(0, jid);
        self.active = 0;
        self.save_tabs();
    }

    fn switch_draft(&mut self, from: Option<&str>, to: Option<&str>) {
        if let Some(f) = from {
            if self.input.is_empty() {
                self.drafts.remove(f);
            } else {
                self.drafts.insert(f.to_string(), (self.input.clone(), self.cursor));
            }
        }
        let (v, c) = to.and_then(|t| self.drafts.get(t).cloned()).unwrap_or_default();
        self.input = v;
        self.cursor = c;
        self.update_suggestions(150);
    }

    /// Closes a tab; the one in front goes to the next, or the previous, or to the list.
    pub fn close_tab(&mut self, i: usize) {
        let Some(closing) = self.tabs.get(i).cloned() else { return };
        let was_active = self.active == i;
        if was_active {
            self.search = None;
        }
        self.tabs.remove(i);
        if self.active > i {
            self.active -= 1;
        } else if was_active {
            self.active = i.min(self.tabs.len().saturating_sub(1));
            self.at_bottom = true;
        }
        self.drafts.remove(&closing);
        if was_active {
            self.editing = None;
            let cur = self.current().map(String::from);
            self.switch_draft(None, cur.as_deref());
        }
        self.selected = None;
        self.save_tabs();
        if self.tabs.is_empty() {
            self.msgs.clear();
            self.rows.clear();
            if self.wanted.is_some() {
                self.quit = true;
            } else {
                self.open_picker("", Some(closing));
            }
            return;
        }
        if was_active {
            self.reload_messages();
        }
        if let Some(jid) = self.current().map(String::from) {
            self.mark_read_if_seen(&jid);
        }
    }

    /// With no chat open (started without one, or the chats arriving for the first time after the QR), the list
    /// opens, once there's a chat to pick.
    fn open_list_if_any(&mut self) {
        if self.tabs.is_empty() && !self.picker_open && !self.showing_qr && !self.quit && self.chats.iter().any(|c| !c.archived) {
            self.open_picker("", None);
        }
    }

    /// Opens a chat as the list does: here, or in Herdr in a new pane or tab, or where it already is.
    pub fn open_chat(&mut self, jid: &str, how: Option<crate::herdr::Place>) {
        if let Some(h) = self.herdr.clone().filter(|_| Some(jid) != self.current()) {
            let other = self.other_terminals().into_iter().find(|t| {
                t.get("herdrTab").and_then(|v| v.as_str()).is_some() && t.get("tabs").and_then(|t| t.as_array()).is_some_and(|a| a.iter().any(|j| j.as_str() == Some(jid)))
            });
            if let Some(o) = other {
                self.close_picker();
                h.focus(o["herdrTab"].as_str().unwrap_or(""), o.get("herdrPane").and_then(|p| p.as_str()));
                return;
            }
            if let Some(place) = how {
                self.close_picker();
                {
                    let exe = crate::exe();
                    h.open_chat(&exe, jid, true, Some(place));
                }
                return;
            }
        }
        let prev = if self.fixed() { self.current().map(String::from) } else { None };
        self.open_tab(jid, true);
        if prev.is_some_and(|p| p != jid) {
            self.tabs = vec![jid.to_string()];
            self.active = 0;
            self.save_tabs();
        }
    }

    fn find_chat(&self, text: &str) -> Option<String> {
        let chats = self.store.chats().unwrap_or_default();
        if chats.iter().any(|c| c.jid == text) {
            return Some(text.to_string());
        }
        let f = crate::text::fold(text);
        chats.iter().find(|c| crate::text::fold(&c.name).contains(&f) || c.jid.split('@').next().unwrap_or("").contains(&f)).map(|c| c.jid.clone())
    }

    // ---------- the list ----------

    /// The chat list, with `filter` typed in it; `from`, the chat you come from, if not the one in front.
    pub fn open_picker(&mut self, filter: &str, from: Option<String>) {
        let from = from.or_else(|| self.current().map(String::from));
        self.picker_search = false;
        self.filter = filter.to_string();
        self.filter_cursor = compose::len(filter);
        self.picker_open = true;
        // Back within 30 s, the list opens on the chat you come from, on the same row of the screen when it's the one
        // it was left on; later, on the most recent, none selected till the keys move it.
        let back = self.picker_closed_at.is_some_and(|t| t.elapsed() < Duration::from_secs(30));
        self.picked = back && from.is_some();
        self.picker_focus = Some(if back { from.clone() } else { None });
        self.picker_focus_row = match (&self.picker_left, &from) {
            (Some((j, row)), Some(f)) if back && j == f => Some(*row),
            _ => None,
        };
        self.list_dirty = true;
        self.list_follow = true;
        self.stop_composing();
        self.set_focus(Focus::Picker);
    }

    pub fn close_picker(&mut self) {
        if !self.picker_open {
            return;
        }
        self.picker_open = false;
        self.picker_closed_at = Some(Instant::now());
        self.picker_left = match self.slots.get(self.list_sel) {
            Some(Slot::Top(i)) => self.filtered.get(*i).map(|c| (c.jid.clone(), self.list_sel.saturating_sub(self.list_top))),
            _ => None,
        };
        self.filter.clear();
        self.filter_cursor = 0;
        self.crops.clear();
        self.rows_dirty = true;
        self.set_focus(Focus::Input);
    }

    /// The chat or message chosen in the list: here, or in Herdr in a new pane ('pane') or tab ('tab').
    fn pick_chat(&mut self, index: usize, how: Option<crate::herdr::Place>) {
        match self.slots.get(index).cloned() {
            Some(Slot::Hit(i)) => {
                if let Some(m) = self.hits_found.get(i).cloned() {
                    self.open_chat(&m.chat, None);
                    if self.current() == Some(m.chat.as_str()) {
                        self.jump_to(&m.id, true);
                    }
                }
            }
            Some(Slot::Top(i)) => {
                if let Some(c) = self.filtered.get(i).cloned() {
                    self.open_chat(&c.jid, how);
                }
            }
            _ => {}
        }
    }

    /// Moves the list's selection `n` rows, past what can't be selected; with no chat chosen yet, the first move only
    /// shows it where it is.
    fn list_move(&mut self, n: i64) {
        self.list_follow = true;
        if !self.picker_chosen() {
            self.picked = true;
            return;
        }
        let len = self.slots.len() as i64;
        if len == 0 {
            return;
        }
        let mut at = (self.list_sel as i64 + n).clamp(0, len - 1);
        let dir = if n < 0 { -1 } else { 1 };
        while at >= 0 && at < len && !self.slots[at as usize].selectable() {
            at += dir;
        }
        if at < 0 || at >= len {
            at = self.list_sel as i64;
            // Back the other way to the nearest one.
            let mut j = (self.list_sel as i64 + n).clamp(0, len - 1);
            while j >= 0 && j < len && !self.slots[j as usize].selectable() {
                j -= dir;
            }
            if j >= 0 && j < len {
                at = j;
            }
        }
        self.list_sel = at as usize;
    }

    // ---------- the chat ----------

    /// Scrolled up, the message at the top of the view and how far the view's top is from its first row: kept
    /// before the messages or their rows change, so the view stays on it.
    pub fn capture_anchor(&mut self) {
        if self.at_bottom || self.anchor.is_some() {
            return;
        }
        let Some((k, i)) = self.rows.iter().enumerate().skip(self.top).find_map(|(k, r)| r.msg.map(|i| (k, i))) else { return };
        let Some(m) = self.msgs.get(i) else { return };
        let first = self.rows.iter().position(|r| r.msg == Some(i)).unwrap_or(k);
        self.anchor = Some((m.id.clone(), self.top as i64 - first as i64));
    }

    pub fn reload_messages(&mut self) {
        let Some(jid) = self.current().map(String::from) else { return };
        if self.rendered_jid.as_deref() == Some(jid.as_str()) {
            self.capture_anchor();
        }
        let limit = self.shown.get(&jid).copied().unwrap_or(PAGE);
        self.msgs = self.store.messages(&jid, limit).unwrap_or_default();
        self.reactions = self.store.reactions(&jid).unwrap_or_default();
        if let Some(sel) = &self.selected
            && !self.msgs.iter().any(|m| m.id == *sel) {
                self.selected = None;
            }
        self.rows_dirty = true;
    }

    /// Marks the chat read only while it's in front of you: in Herdr with its pane focused, elsewhere unless the
    /// terminal said it lost the focus.
    fn mark_read_if_seen(&self, jid: &str) {
        let seen = if self.herdr.is_some() { self.has_focus == Some(true) } else { self.has_focus != Some(false) };
        if seen {
            self.backend.mark_read(jid);
        }
    }

    pub fn set_focus(&mut self, f: Focus) {
        self.focus = f;
        if f != Focus::Messages && self.selected.take().is_some() {
            self.rows_dirty = true;
        }
        if f != Focus::Input && !self.suggestions.is_empty() {
            self.suggestions.clear();
        }
    }

    fn select(&mut self, id: Option<String>) {
        self.selected = id;
        self.rows_dirty = true;
        if self.selected.is_some() {
            if self.focus != Focus::Messages {
                self.set_focus(Focus::Messages);
            }
        } else if self.focus == Focus::Messages {
            self.set_focus(Focus::Input);
        }
    }

    /// The previous (-1) or next (+1) message selected; with none, ↑ takes the last; ↓ past the last goes back to the
    /// band; ↑ on the first brings older ones in.
    fn move_selection(&mut self, dir: i64) {
        if self.current().is_none() || self.msgs.is_empty() {
            return;
        }
        let ids: Vec<String> = self.msgs.iter().map(|m| m.id.clone()).collect();
        let i = self.selected.as_ref().and_then(|s| ids.iter().position(|x| x == s)).unwrap_or(ids.len()) as i64;
        if dir < 0 && i == 0 {
            self.load_older();
            return;
        }
        let next = i + dir;
        if next as usize >= ids.len() {
            self.select(None);
        } else {
            self.select(Some(ids[next.max(0) as usize].clone()));
            self.keep_selected_in_view = true;
        }
    }

    /// Selects a message of the open chat and brings it into view, drawing back as far as it.
    pub fn jump_to(&mut self, id: &str, focus: bool) {
        let Some(jid) = self.current().map(String::from) else { return };
        let Some(target) = self.store.message(&jid, id) else { return self.flash(s("messageNotFound"), 6) };
        let limit = self.shown.get(&jid).copied().unwrap_or(PAGE);
        let need = self.store.count_since(&jid, target.ts);
        if limit != usize::MAX && need > limit {
            self.shown.insert(jid.clone(), need);
            self.reload_messages();
        }
        if focus {
            self.select(Some(target.id));
        } else {
            self.selected = Some(target.id);
            self.rows_dirty = true;
        }
        self.keep_selected_in_view = true;
    }

    /// Past the top: more of the messages kept, then the phone's, until it has nothing older.
    pub fn load_older(&mut self) {
        let Some(jid) = self.current().map(String::from) else { return };
        let limit = self.shown.get(&jid).copied().unwrap_or(PAGE);
        if limit != usize::MAX && self.store.count(&jid) > limit {
            self.shown.insert(jid, limit + PAGE);
            self.reload_messages();
            return;
        }
        self.shown.insert(jid.clone(), usize::MAX);
        if self.older_pending.get(&jid).is_some_and(|t| t.elapsed() < Duration::from_secs(30)) || self.older_done.contains(&jid) {
            return;
        }
        self.older_pending.insert(jid.clone(), Instant::now());
        self.rows_dirty = true;
        self.backend.fetch_older(&jid);
    }

    /// Straight to the latest message, leaving any selection.
    fn go_to_latest(&mut self) {
        if self.selected.is_some() {
            self.select(None);
        }
        self.at_bottom = true;
        self.anchor = None;
        self.rows_dirty = true;
    }

    fn open_search(&mut self) {
        self.search = Some(Search { query: String::new(), cursor: 0, words: vec![], hits: vec![], at: 0 });
        self.suggestions.clear();
        if self.selected.take().is_some() {
            self.rows_dirty = true;
        }
        self.set_focus(Focus::Input);
    }

    fn close_search(&mut self, stay: bool) {
        let Some(s) = self.search.take() else { return };
        self.rows_dirty = true;
        let hit = if stay { s.hits.get(s.at).map(|m| m.id.clone()) } else { None };
        if hit.is_some() && hit == self.selected {
            self.set_focus(Focus::Messages);
        } else {
            self.selected = None;
            self.set_focus(Focus::Input);
        }
    }

    fn search_key(&mut self, k: KeyEvent) {
        let Some(s) = self.search.as_mut() else { return };
        match k.code {
            KeyCode::Esc => return self.close_search(false),
            KeyCode::Enter => return self.close_search(true),
            KeyCode::Up | KeyCode::Down => {
                let at = s.at as i64 + if k.code == KeyCode::Up { 1 } else { -1 };
                if at >= 0 && (at as usize) < s.hits.len() {
                    s.at = at as usize;
                    let id = s.hits[s.at].id.clone();
                    self.jump_to(&id, false);
                }
                return;
            }
            _ => {}
        }
        let Some(e) = edit_key(&k) else { return };
        let (value, cursor) = compose::edit(&s.query, s.cursor, e);
        s.cursor = cursor;
        if value != s.query {
            self.search_query(value);
        }
    }

    fn search_query(&mut self, value: String) {
        let Some(jid) = self.current().map(String::from) else { return };
        let Some(s) = self.search.as_mut() else { return };
        s.query = value;
        s.words = crate::text::fold(&s.query).split_whitespace().map(String::from).collect();
        let mut hits = self.store.search(Some(&jid), &s.words, 1000).unwrap_or_default();
        hits.reverse();
        s.hits = hits;
        s.at = 0;
        self.rows_dirty = true;
        if let Some(id) = s.hits.first().map(|m| m.id.clone()) {
            self.jump_to(&id, false);
        } else {
            self.selected = None;
        }
    }

    // ---------- writing ----------

    /// The band's text changed: the chat is told I'm typing, again every 5 s while I keep on, and that I stopped
    /// after 5 s idle, on sending, clearing everything or switching tabs.
    fn note_composing(&mut self) {
        let Some(jid) = self.current().map(String::from) else { return self.stop_composing() };
        if self.input.is_empty() || self.picker_open {
            return self.stop_composing();
        }
        let due = match &self.composing {
            Some((j, at)) => *j != jid || at.elapsed() > Duration::from_secs(5),
            None => true,
        };
        if due {
            if let Some((j, _)) = self.composing.as_ref().filter(|(j, _)| *j != jid) {
                self.backend.set_composing(j, false);
            }
            self.backend.set_composing(&jid, true);
            self.composing = Some((jid, Instant::now()));
        }
        self.composing_until = Some(Instant::now() + Duration::from_secs(5));
    }

    fn stop_composing(&mut self) {
        self.composing_until = None;
        if let Some((j, _)) = self.composing.take() {
            self.backend.set_composing(&j, false);
        }
    }

    /// Something was written in a chat: keeps this device online while it has the focus (every 10 s is enough), and
    /// what arrived meanwhile is read then.
    fn note_writing(&mut self) {
        self.last_write = Some(Instant::now());
        if self.has_focus != Some(false) && self.last_presence_touch.is_none_or(|t| t.elapsed() > Duration::from_secs(10)) {
            self.last_presence_touch = Some(Instant::now());
            self.backend.touch_presence(self.pid);
            if let Some(jid) = self.current().map(String::from) {
                self.mark_read_if_seen(&jid);
            }
        }
    }

    fn cursor_at_end(&self) -> bool {
        self.cursor >= compose::len(&self.input)
    }

    /// Pasted text goes in whole at the cursor: with its lines in the band, on one line in the filter.
    fn paste(&mut self, text: &str) {
        if let Some(s) = self.search.as_mut() {
            let ins = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let v = format!("{}{ins}{}", compose::before(&s.query, s.cursor), compose::after(&s.query, s.cursor));
            s.cursor += compose::len(&ins);
            return self.search_query(v);
        }
        if self.focus == Focus::Picker {
            let ins = text.replace('\n', " ");
            self.filter = format!("{}{ins}{}", compose::before(&self.filter, self.filter_cursor), compose::after(&self.filter, self.filter_cursor));
            self.filter_cursor += compose::len(&ins);
            self.list_dirty = true;
            return;
        }
        if self.focus != Focus::Input || text.is_empty() {
            return;
        }
        self.input = format!("{}{text}{}", compose::before(&self.input, self.cursor), compose::after(&self.input, self.cursor));
        self.cursor += compose::len(text);
        self.accepted = None;
        self.promote_active();
        self.note_composing();
        self.note_writing();
        self.update_suggestions(150);
    }

    /// Who can be mentioned in a group: those who wrote in it, most recent first, with a known name; each by its
    /// first name, or the whole when another shares it; to their hidden id when known.
    fn mentionables(&self, group: &str) -> Vec<(String, String, String)> {
        let people: Vec<(String, String, String)> = self
            .store
            .recent_senders(group, 500)
            .into_iter()
            .filter(|s| *s != self.me && self.store.has_name(s))
            .map(|sender| {
                let name = self.store.name_of(&sender);
                let jid = self.store.lid_of(&sender).unwrap_or(sender);
                let first = name.split_whitespace().next().unwrap_or("").to_string();
                (jid, name, first)
            })
            .collect();
        people
            .iter()
            .map(|(jid, name, first)| {
                let token = if people.iter().filter(|p| p.2 == *first).count() > 1 { name.clone() } else { first.clone() };
                (jid.clone(), name.clone(), token)
            })
            .collect()
    }

    /// In a group, each "@" and a member's name becomes "@" and their number, and they the ones mentioned.
    fn with_mention_ids(&self, chat: &str, text: &str) -> (String, Vec<String>) {
        if !crate::text::is_group(chat) || !text.contains('@') {
            return (text.to_string(), vec![]);
        }
        let mut people = self.mentionables(chat);
        people.sort_by_key(|p| std::cmp::Reverse(p.2.chars().count()));
        let mut out = text.to_string();
        let mut mentions = Vec::new();
        for (jid, _, token) in people {
            let pattern = format!(r"(^|[^\p{{L}}\p{{N}}_])@{}([^\p{{L}}\p{{N}}]|$)", regex::escape(&token));
            let Ok(re) = regex::Regex::new(&pattern) else { continue };
            let user = jid.split('@').next().unwrap_or("").to_string();
            let replaced = re.replace_all(&out, |c: &regex::Captures| format!("{}@{user}{}", &c[1], &c[2])).into_owned();
            if replaced != out && !mentions.contains(&jid) {
                mentions.push(jid);
            }
            out = replaced;
        }
        (out, mentions)
    }

    /// The emoji suggestions for what's before the cursor: a smiley, a ":code" being typed, an "@name" in a group.
    pub fn update_suggestions(&mut self, ghost_delay: u64) {
        let writing = self.focus == Focus::Input && self.search.is_none() && !self.picker_open;
        let before = compose::before(&self.input, self.cursor).to_string();
        let after = compose::after(&self.input, self.cursor).to_string();
        let face = if writing { emojicodes::emoticon_at(&before, &after) } else { None };
        static CODE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(^|[^\w:]):([^\s:]*)$").unwrap());
        static AT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(^|[^\p{L}\p{N}_@])@([\p{L}\p{M}]*)$").unwrap());
        let m = if writing { CODE.captures(&before).map(|c| c[2].to_string()) } else { None };
        let typed = m.as_ref().map(|p| compose::len(p) + 1).unwrap_or(0);
        let mention = if writing && self.current().is_some_and(crate::text::is_group) { AT.captures(&before).map(|c| c[2].to_string()) } else { None };
        let lang = if crate::i18n::lang() == crate::i18n::Lang::Pt { emojicodes::Lang::Pt } else { emojicodes::Lang::En };
        let mut options: Vec<Sugg> = Vec::new();
        if let Some(f) = &face {
            options.push(Sugg { emoji: f.emoji.to_string(), code: f.code.clone(), length: compose::len(&f.code) });
        }
        if let Some(p) = &m {
            options.extend(emojicodes::complete_emoji(p, lang).into_iter().map(|o| Sugg { emoji: o.emoji.to_string(), code: o.code, length: typed }));
        }
        if let (Some(q), Some(chat)) = (&mention, self.current()) {
            let q = crate::text::fold(q);
            for (_, name, token) in self.mentionables(chat) {
                if crate::text::fold(&name).split_whitespace().any(|w| w.starts_with(&q)) {
                    options.push(Sugg { emoji: format!("@{token}"), code: name, length: compose::len(&format!("@{}", mention.as_deref().unwrap_or(""))) });
                }
            }
        }
        let mut seen = HashSet::new();
        options.retain(|o| seen.insert(o.emoji.clone()));
        let same = options.len() == self.suggestions.len() && options.iter().zip(&self.suggestions).all(|(a, b)| a.emoji == b.emoji);
        self.suggest_face = face.is_some();
        self.suggest_kind = if face.is_some() { Some("smileys") } else if m.is_some() && typed > 1 && !options.is_empty() { Some("emojiNames") } else { None };
        self.suggestions = options;
        if !same {
            self.suggest_index = 0;
        }
        self.schedule_ghost(ghost_delay);
    }

    /// The ":prefix", the smiley or the "@name" gives way to the chosen one, and a space.
    fn accept_suggestion(&mut self) {
        if let Some(k) = self.suggest_kind {
            self.note_suggest(k, true);
        }
        let Some(o) = self.suggestions.get(self.suggest_index).cloned() else { return };
        let at = self.cursor.min(compose::len(&self.input));
        let head = format!("{}{} ", compose::before(&self.input, at.saturating_sub(o.length)), o.emoji);
        self.input = format!("{head}{}", compose::after(&self.input, at));
        self.cursor = compose::len(&head);
        self.suggestions.clear();
    }

    /// Suggestions of a kind taken or not: with the last two times alike, what a space does follows them.
    fn note_suggest(&mut self, kind: &str, taken: bool) {
        let kept: Option<serde_json::Value> = self.store.state(kind).and_then(|v| serde_json::from_str(&v).ok());
        let mut last: Vec<bool> = kept.as_ref().and_then(|k| k.get("last")?.as_array().map(|a| a.iter().filter_map(|b| b.as_bool()).collect())).unwrap_or_default();
        last.push(taken);
        if last.len() > 2 {
            last.remove(0);
        }
        let mode = if last.len() == 2 && last[0] == last[1] {
            if taken { "emoji" } else { "text" }.to_string()
        } else {
            kept.as_ref().and_then(|k| k.get("mode")?.as_str().map(String::from)).unwrap_or_else(|| "text".into())
        };
        let _ = self.store.set_state(kind, &serde_json::json!({ "mode": mode, "last": last }).to_string());
    }

    fn suggest_mode(&self, kind: &str) -> String {
        self.store.state(kind).and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok()).and_then(|v| v.get("mode")?.as_str().map(String::from)).unwrap_or_else(|| "text".into())
    }

    // ---------- the model's suggestions ----------

    /// The model's corrections that still apply to the text.
    pub fn live_fixes(&self) -> Vec<Fix> {
        let Some((text, s)) = &self.ghost else { return vec![] };
        if *text == self.input {
            return s.fixes.clone();
        }
        let now: Vec<char> = self.input.chars().collect();
        let was: Vec<char> = text.chars().collect();
        s.fixes
            .iter()
            .filter(|f| {
                let next = now.get(f.end);
                now.len() >= f.end && now[..f.end] == was[..f.end.min(was.len())] && next.is_none_or(|c| !(c.is_alphanumeric() || *c == '\'' || *c == '-'))
            })
            .cloned()
            .collect()
    }

    fn ghost_valid(&self) -> bool {
        self.ghost.as_ref().is_some_and(|(text, s)| (s.word.is_some() && *text == self.input) || !self.live_fixes().is_empty())
    }

    fn current_fix(&self) -> Option<Fix> {
        let fixes = self.live_fixes();
        let at = compose::before(&self.input, self.cursor).chars().count();
        fixes.iter().find(|f| at >= f.start && at <= f.end).cloned().or_else(|| if self.cursor_at_end() { fixes.last().cloned() } else { None })
    }

    /// The suggestion in view, the one Tab or → takes: the word half-typed at the end (its missing letters, or the
    /// right word), else the correction the cursor is on.
    pub fn ghost_shown(&self) -> Option<GhostView> {
        let (text, s) = self.ghost.as_ref()?;
        if self.picker_open || !self.suggestions.is_empty() || self.search.is_some() {
            return None;
        }
        if let Some(w) = s.word.as_ref().filter(|_| *text == self.input && self.cursor_at_end()) {
            let (from, to) = (w.from.to_lowercase(), w.to.to_lowercase());
            if to.starts_with(&from) && w.to.chars().count() > w.from.chars().count() {
                return Some(GhostView::Suffix(w.to.chars().skip(w.from.chars().count()).collect(), w.from.clone(), w.to.clone()));
            }
            return Some(GhostView::Word(w.from.clone(), w.to.clone()));
        }
        self.current_fix().map(GhostView::Fix)
    }

    /// Asks for a suggestion `delay` ms after the last key, with the cursor at the end and nothing else in the way.
    fn schedule_ghost(&mut self, delay: u64) {
        if self.ghost.is_some() && !self.ghost_valid() {
            self.clear_ghost();
        }
        self.ghost_due = None;
        if let Some(c) = self.ghost_cancel.take() {
            c.cancel();
        }
        if self.current().is_none() || self.focus != Focus::Input || self.picker_open || self.react_to.is_some() || self.search.is_some() {
            return;
        }
        if !self.suggestions.is_empty() && !self.suggest_face {
            return;
        }
        if !self.cursor_at_end() || self.input.trim().chars().count() < 3 || self.ghost.as_ref().is_some_and(|(t, _)| *t == self.input) {
            return;
        }
        self.ghost_due = Some((Instant::now() + Duration::from_millis(delay), self.input.clone()));
    }

    fn own_words(&mut self) -> Arc<HashSet<String>> {
        if self.own_words.is_none() {
            let texts = self.store.my_texts();
            let names = self.store.names();
            self.own_words = Some(Arc::new(crate::spell::own_words(texts.iter().map(String::as_str), names.iter().map(String::as_str))));
        }
        self.own_words.clone().unwrap()
    }

    fn who(&self, m: &Msg) -> String {
        if m.from_me {
            s("me")
        } else if crate::text::is_group(&m.chat) {
            self.store.name_of(&m.sender)
        } else {
            self.store.name_of(&m.chat)
        }
    }

    /// The request, off the loop: the model, or the spell checker when there's none or it didn't answer.
    fn request_ghost(&mut self, text: String) {
        let Some(jid) = self.current().map(String::from) else { return };
        let cancel = Cancel::new();
        self.ghost_cancel = Some(cancel.clone());
        let via_model = crate::llm::enabled() && self.llm_down_until.is_none_or(|t| Instant::now() >= t);
        let context: Vec<(String, String)> = if via_model {
            self.store.messages(&jid, 6).unwrap_or_default().into_iter().filter(|m| !m.text.is_empty() && m.kind != "deleted").map(|m| (self.who(&m), m.text)).collect()
        } else {
            vec![]
        };
        let (speller, own, tx) = (self.speller.clone(), self.own_words(), self.tx.clone());
        tokio::task::spawn_blocking(move || {
            let local = |text: &str| {
                let fixes: Vec<Fix> = speller.wait().fixes(text, &own).into_iter().map(Fix::from).collect();
                (!fixes.is_empty()).then_some(Suggestion { word: None, fixes })
            };
            let (s, down) = if via_model {
                match crate::llm::suggest_text(&context, &text, &cancel) {
                    Ok(s) => (s, false),
                    Err(_) if cancel.is_cancelled() => return,
                    Err(_) => (local(&text), true),
                }
            } else {
                (local(&text), false)
            };
            if !cancel.is_cancelled() {
                let _ = tx.send(Input::Ghost(text, s, down));
            }
        });
    }

    pub fn clear_ghost(&mut self) {
        self.ghost = None;
        self.float_key.clear();
        self.float_off.clear();
    }

    /// Takes the suggestion in view; the other corrections stay, moved along.
    fn accept_ghost(&mut self) {
        let Some(v) = self.ghost_shown() else { return };
        let at_end = self.cursor_at_end();
        let chars: Vec<char> = self.input.chars().collect();
        let (value, rest) = match &v {
            GhostView::Fix(f) => {
                let delta = f.to.chars().count() as i64 - (f.end - f.start) as i64;
                let value: String = chars[..f.start].iter().collect::<String>() + &f.to + &chars[f.end.min(chars.len())..].iter().collect::<String>();
                let rest: Vec<Fix> = self
                    .live_fixes()
                    .into_iter()
                    .filter(|x| x != f)
                    .map(|x| if x.start >= f.end { Fix { start: (x.start as i64 + delta) as usize, end: (x.end as i64 + delta) as usize, ..x } } else { x })
                    .collect();
                self.cursor = if at_end { compose::len(&value) } else { compose::len(&value.chars().take(f.start + f.to.chars().count()).collect::<String>()) };
                (value, rest)
            }
            GhostView::Suffix(_, from, to) | GhostView::Word(from, to) => {
                let keep = chars.len().saturating_sub(from.chars().count());
                let value = chars[..keep].iter().collect::<String>() + to;
                self.cursor = compose::len(&value);
                (value, self.live_fixes())
            }
        };
        self.input = value;
        self.accepted = self.input.chars().last().filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '-').map(|_| self.input.clone());
        self.clear_ghost();
        if !rest.is_empty() {
            self.ghost = Some((self.input.clone(), Suggestion { word: None, fixes: rest }));
        }
        self.promote_active();
        self.update_suggestions(0);
    }

    /// The corrections for one of my text messages while it can still be edited, asked once per text.
    pub fn sent_fixes_for(&mut self, m: &Msg) -> Option<Vec<Fix>> {
        if !m.from_me || m.kind != "text" || chrono::Utc::now().timestamp() - m.ts >= EDIT_WINDOW {
            return None;
        }
        let text = bare_text(&m.text);
        if text.trim().chars().count() < 3 {
            return None;
        }
        let key = format!("{} {} {text}", m.chat, m.id);
        match self.sent_fixes.get(&key) {
            Some(known) => known.clone(),
            None => {
                self.sent_fixes.insert(key, None);
                self.sent_queue.push(m.clone());
                self.check_sent();
                None
            }
        }
    }

    fn check_sent(&mut self) {
        if self.sent_checking || self.sent_queue.is_empty() {
            return;
        }
        let m = self.sent_queue.remove(0);
        self.sent_checking = true;
        let text = bare_text(&m.text).to_string();
        let key = format!("{} {} {text}", m.chat, m.id);
        let via_model = crate::llm::enabled() && self.llm_down_until.is_none_or(|t| Instant::now() >= t);
        let context: Vec<(String, String)> = self
            .store
            .messages(&m.chat, 8)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.id != m.id && r.ts <= m.ts && !r.text.is_empty() && r.kind != "deleted")
            .map(|r| (self.who(&r), r.text))
            .collect();
        let context = context[context.len().saturating_sub(6)..].to_vec();
        let (speller, own, tx) = (self.speller.clone(), self.own_words(), self.tx.clone());
        tokio::task::spawn_blocking(move || {
            let local = |text: &str| -> Vec<Fix> { speller.wait().fixes(text, &own).into_iter().map(Fix::from).collect() };
            let cancel = Cancel::new();
            let (fixes, down) = if via_model {
                match crate::llm::suggest_text(&context, &text, &cancel) {
                    Ok(Some(s)) => {
                        let n = text.chars().count();
                        let mut fixes = s.fixes.clone();
                        if let Some(w) = s.word.filter(|w| !w.to.to_lowercase().starts_with(&w.from.to_lowercase())) {
                            let start = n - w.from.chars().count();
                            if !fixes.iter().any(|f| f.end > start) {
                                fixes.push(Fix { from: w.from, to: w.to, start, end: n });
                            }
                        }
                        (fixes, false)
                    }
                    Ok(None) => (vec![], false),
                    Err(_) => (local(&text), true),
                }
            } else {
                (local(&text), false)
            };
            let _ = tx.send(Input::SentFixes(key, fixes, down));
        });
    }

    /// Edits my message with the correction shown over it.
    fn apply_sent_fix(&mut self) {
        let Some((m, fix, _, _)) = self.sent_fix.take() else { return };
        if chrono::Utc::now().timestamp() - m.ts >= EDIT_WINDOW {
            return self.flash(s("editTooLate"), 6);
        }
        let text = bare_text(&m.text);
        let chars: Vec<char> = text.chars().collect();
        if chars.get(fix.start..fix.end).map(|c| c.iter().collect::<String>()) != Some(fix.from.clone()) {
            return;
        }
        let value: String = chars[..fix.start].iter().collect::<String>() + &fix.to + &chars[fix.end..].iter().collect::<String>();
        let delta = fix.to.chars().count() as i64 - (fix.end - fix.start) as i64;
        let old = format!("{} {} {text}", m.chat, m.id);
        let rest: Vec<Fix> = self.sent_fixes.get(&old).cloned().flatten().unwrap_or_default().into_iter().filter(|f| *f != fix).map(|f| if f.start >= fix.end { Fix { start: (f.start as i64 + delta) as usize, end: (f.end as i64 + delta) as usize, ..f } } else { f }).collect();
        self.sent_fixes.insert(format!("{} {} {value}", m.chat, m.id), (!rest.is_empty()).then_some(rest));
        self.backend.edit(&m.chat, &m.id, &value);
    }

    /// What a picture shows, from the model, while selected or in the popup.
    pub fn described_as(&mut self, key: &str, path: &str) -> Option<Option<String>> {
        if !crate::llm::enabled() {
            return None;
        }
        if let Some(d) = self.descriptions.get(key) {
            return d.clone().map(Some);
        }
        if self.describing.as_ref().is_none_or(|(k, _)| k != key) && self.llm_down_until.is_none_or(|t| Instant::now() >= t) {
            let cancel = Cancel::new();
            self.describing = Some((key.to_string(), cancel.clone()));
            let (key, path, tx) = (key.to_string(), path.to_string(), self.tx.clone());
            tokio::task::spawn_blocking(move || {
                let png = crate::chat::open_image(&path).ok().and_then(|img| {
                    let img = img.thumbnail(1024, 1024);
                    let mut out = Vec::new();
                    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).ok().map(|_| out)
                });
                let r = png.map(|p| crate::llm::describe_png(&p, &cancel));
                if cancel.is_cancelled() {
                    return;
                }
                // A picture that can't be read has no description; a model that didn't answer is asked again later.
                let r = match r {
                    Some(Ok(t)) => Ok(t),
                    Some(Err(_)) => Err(true),
                    None => Ok(None),
                };
                let _ = tx.send(Input::Described(key, r));
            });
        }
        self.describing.as_ref().filter(|(k, _)| k == key).map(|_| None)
    }

    fn drop_describing(&mut self) {
        let keep = [
            self.selected.as_ref().and_then(|id| self.msg(id)).map(|m| format!("{} {}", m.chat, m.id)).unwrap_or_default(),
            self.viewing.as_ref().map(|v| v.key.clone()).unwrap_or_default(),
        ];
        if let Some((k, c)) = &self.describing
            && !keep.contains(k) {
                c.cancel();
                self.describing = None;
            }
    }

    // ---------- sending ----------

    async fn submit(&mut self, v: String, draft: Option<(Vec<String>, u16)>) {
        let text = emojicodes::emojify(v.trim());
        if let Some(target) = self.react_to.take() {
            let emoji = if text == ":" { String::new() } else { emojicodes::emoticonify(&text) };
            return self.react(&target, &emoji, false);
        }
        if let Some(id) = self.editing.clone() {
            if text.is_empty() {
                return;
            }
            self.editing = None;
            if !self.is_open() {
                return self.flash(s("noConnection"), 6);
            }
            if let Some(jid) = self.current().map(String::from) {
                self.backend.edit(&jid, &id, &text);
            }
            return;
        }
        if text.is_empty() {
            return;
        }
        if let Some(rest) = text.strip_prefix('/') {
            return self.open_picker(rest.trim(), None);
        }
        let Some(jid) = self.current().map(String::from) else { return self.flash(s("openFirst"), 6) };
        if !self.is_open() {
            return self.flash(s("noConnection"), 6);
        }
        self.at_bottom = true;
        self.anchor = None;
        // A message starting with a smiley isn't a ":" command.
        if emojicodes::emoticonify(&text).starts_with(':') {
            let first = text.split_whitespace().next().unwrap_or("");
            return self.flash(format!("{}: {first}. {}", s("unknownCommand"), s("help")), 10);
        }
        let reply = self.reply_to.take().filter(|id| self.msg(id).is_some_and(|m| m.chat == jid));
        let (out, mentions) = self.with_mention_ids(&jid, &text);
        if let Some((lines, top)) = draft.filter(|_| !emojicodes::emoji_only(&text)) {
            self.fly(out.clone(), lines, top);
        }
        self.backend.send(&jid, &out, reply.as_deref(), &mentions);
        if let Some(e) = emojicodes::reaction(&text) {
            self.heart_for_mine(&text, &e);
        }
    }

    /// Sends `emoji` as my reaction (empty takes it away); the same again from the mouse takes it away too.
    fn react(&mut self, id: &str, emoji: &str, toggle: bool) {
        let Some(m) = self.msg(id).cloned() else { return };
        if !self.is_open() {
            return self.flash(s("noConnection"), 6);
        }
        let mine = self.store.my_reaction(&m.chat, id);
        let send = if toggle && mine.as_deref() == Some(emoji) { "" } else { emoji };
        self.backend.react(&m.chat, id, send);
        if send.is_empty() {
            self.flash(s("reactionRemoved"), 6);
        }
    }

    fn edit_message(&mut self, id: &str) {
        let Some(m) = self.msg(id).cloned() else { return };
        if !m.from_me || m.kind != "text" {
            return self.flash(s("onlyOwnText"), 6);
        }
        self.editing = Some(m.id.clone());
        self.reply_to = None;
        self.react_to = None;
        self.set_focus(Focus::Input);
        self.input = bare_text(&m.text).to_string();
        self.cursor = compose::len(&self.input);
        self.update_suggestions(150);
    }

    fn delete_message(&mut self, id: &str, for_everyone: bool) {
        self.deleting = None;
        self.select(None);
        if !self.is_open() {
            return self.flash(s("noConnection"), 6);
        }
        if let Some(m) = self.msg(id) {
            self.backend.delete(&m.chat, &m.id, for_everyone);
        }
    }

    /// The text being written, as the band shows it, for the flight: its lines and the screen row of the first.
    fn draft_shown(&self) -> Option<(Vec<String>, u16)> {
        if self.current().is_none() || self.picker_open || self.search.is_some() || self.input.trim().is_empty() {
            return None;
        }
        let rows = (self.band_text.height as usize).max(1);
        let lines: Vec<String> = self.band_lines.iter().skip(self.band_first).take(rows).map(|l| l.iter().filter(|g| *g != "\n").cloned().collect::<String>().trim_end().to_string()).collect();
        Some((lines, self.band_text.y))
    }

    fn fly(&mut self, text: String, lines: Vec<String>, top: u16) {
        self.flight = Some(Flight { text, lines, top, started: Instant::now(), room: 0, frame: 0, arrived: false, next: Instant::now() + Duration::from_millis(30) });
    }

    // ---------- notices ----------

    pub fn flash(&mut self, text: String, secs: u64) {
        self.transient = Some((text, Instant::now() + Duration::from_secs(secs)));
    }

    fn notify(&mut self, jid: &str, text: String) {
        // A notice over one already in full view doesn't fade in again.
        let since = match &self.notice {
            Some((_, _, s)) if notice_opacity(*s) >= 1.0 => Instant::now() - Duration::from_millis(NOTICE.0),
            _ => Instant::now(),
        };
        self.notice = Some((jid.to_string(), text, since));
    }

    /// A rising emoji for a message once it's drawn.
    fn heart_for(&mut self, msg: &str, emoji: &str) {
        self.hearts_wanted.push(HeartWanted { msg: msg.into(), emoji: emoji.into(), since: Instant::now() });
    }

    /// For my own message: it only appears once the server echoes it, so the latest of mine with that text.
    fn heart_for_mine(&mut self, text: &str, emoji: &str) {
        self.hearts_wanted.push(HeartWanted { msg: format!("mine:{text}"), emoji: emoji.into(), since: Instant::now() });
    }

    // ---------- events ----------

    pub fn on_input(&mut self, input: Input) {
        match input {
            Input::Term(ev) => self.on_term(ev),
            Input::Wa(ev) => self.on_wa(ev),
            Input::Image(key, img) => {
                // Kept within bounds: the photos stay, the messages' pictures are decoded again when wanted.
                if self.images.len() > 120 {
                    self.images.retain(|k, _| k.starts_with("avatar:") || k.starts_with("emoji:"));
                    self.full.clear();
                    self.scaled.clear();
                    self.crops.clear();
                }
                // One that can't be read stays asked, so it isn't decoded again at every frame.
                let img = match img {
                    Ok(img) => img,
                    Err(e) => {
                        self.failed.insert(key, e);
                        self.rows_dirty = true;
                        return;
                    }
                };
                self.asked.remove(&key);
                {
                    self.images.insert(key.clone(), Arc::new(img));
                    self.full.insert(key.clone());
                    self.scaled.retain(|(k, _, _), _| *k != key);
                    self.crops.retain(|(k, ..), _| *k != key);
                    self.rows_dirty = true;
                    self.list_dirty = true;
                }
            }
            Input::Ghost(text, s, down) => {
                if down {
                    self.llm_down_until = Some(Instant::now() + Duration::from_secs(60));
                }
                self.ghost_cancel = None;
                if text != self.input {
                    return;
                }
                let Some(s) = s else {
                    self.accept_on_arrival = 0;
                    return;
                };
                self.ghost = Some((text, s));
                if self.accept_on_arrival > 0 {
                    self.accept_on_arrival -= 1;
                    return self.accept_ghost();
                }
                self.float_key.clear();
                self.float_off.clear();
            }
            Input::SentFixes(key, fixes, down) => {
                if down {
                    self.llm_down_until = Some(Instant::now() + Duration::from_secs(60));
                }
                if !fixes.is_empty() {
                    self.sent_fixes.insert(key, Some(fixes));
                    self.rows_dirty = true;
                }
                self.sent_checking = false;
                self.check_sent();
            }
            Input::Described(key, r) => {
                if self.describing.as_ref().is_some_and(|(k, _)| *k == key) {
                    self.describing = None;
                }
                match r {
                    Ok(text) => {
                        self.descriptions.insert(key, text);
                    }
                    Err(down) => {
                        if down {
                            self.llm_down_until = Some(Instant::now() + Duration::from_secs(60));
                        }
                    }
                }
                self.rows_dirty = true;
            }
            Input::InitialFocus(f) => {
                if self.has_focus.is_none() {
                    self.on_term(if f { Event::FocusGained } else { Event::FocusLost });
                }
            }
            Input::HerdrNoNext => self.open_picker("", None),
            Input::Scrolled(top) => {
                // The page shows it there already: the view follows, for what depends on it (the "↓", the pictures
                // to fetch, reading it to the end), and at the top the older messages come, as with the wheel.
                if self.picker_open {
                    self.list_top = top.min(self.slots.len().saturating_sub(self.list_rows));
                    self.page_top = Some(self.list_top);
                    self.list_follow = false;
                    return;
                }
                if self.current().is_none() {
                    return;
                }
                let max_top = self.rows.len().saturating_sub(self.chat_area.height as usize);
                self.top = top.min(max_top);
                self.page_top = Some(self.top);
                self.at_bottom = self.top >= max_top;
                self.anchor = None;
                self.sent_fix = None;
                self.text_sel = None;
                if self.top == 0 && !self.rows_dirty {
                    self.load_older();
                }
            }
        }
    }

    fn on_wa(&mut self, ev: ToUi) {
        match ev {
            ToUi::Connection(state, detail) => {
                if let Some(me) = self.store.state("me").filter(|m| !m.is_empty()) {
                    self.me = me;
                }
                // A change of connection voids who's typing and who's online.
                self.typing.clear();
                self.online.clear();
                self.conn = state;
                self.conn_detail = detail;
                match state {
                    Conn::Qr => {
                        self.showing_qr = true;
                        self.close_picker();
                    }
                    Conn::Open => {
                        self.qr = None;
                        self.showing_qr = false;
                        self.rows_dirty = true;
                        self.list_dirty = true;
                        for jid in self.tabs.clone() {
                            self.backend.subscribe_presence(&jid);
                            self.backend.ensure_name(&jid);
                        }
                        if self.has_focus != Some(false) && self.last_write.is_some_and(|t| t.elapsed() < Duration::from_secs(120)) {
                            self.last_presence_touch = Some(Instant::now());
                            self.backend.touch_presence(self.pid);
                        }
                        if let Some(f) = self.has_focus {
                            self.backend.set_focus(self.pid, f);
                        }
                        self.open_list_if_any();
                    }
                    _ => {}
                }
            }
            ToUi::Qr(code) => {
                self.qr = Some(code);
                self.showing_qr = true;
                self.close_picker();
            }
            ToUi::Chats => {
                self.chats = self.store.chats().unwrap_or_default();
                self.chats.reverse();
                self.list_dirty = true;
                self.open_list_if_any();
            }
            ToUi::Messages(jid) => {
                if jid == "*" || Some(jid.as_str()) == self.current() {
                    self.reload_messages();
                }
                self.list_dirty = true;
            }
            ToUi::Notify(jid, id) => self.on_notify(&jid, &id),
            ToUi::Status(text) => self.flash(text, 6),
            ToUi::Typing(jid, who) => {
                if who.is_empty() {
                    self.typing.remove(&jid);
                } else {
                    self.typing.insert(jid, who);
                }
                self.list_dirty = true;
            }
            ToUi::Reaction(jid, msg, _, emoji) => {
                if Some(jid.as_str()) == self.current()
                    && let Some(e) = emojicodes::reaction(&emoji) {
                        self.heart_for(&msg, &e);
                    }
            }
            ToUi::Presence(jid, on) => {
                if on {
                    self.online.insert(jid);
                } else {
                    self.online.remove(&jid);
                }
                self.list_dirty = true;
            }
            ToUi::GroupOnline(g, n) => {
                self.group_online.insert(g, n);
            }
            ToUi::Profile(jid) => {
                let biz = self.store.state(&format!("biz:{jid}")).as_deref() == Some("true");
                if biz {
                    self.business.insert(jid.clone());
                } else {
                    self.business.remove(&jid);
                }
                self.images.remove(&format!("avatar:{jid}"));
                self.asked.remove(&format!("avatar:{jid}"));
                self.crops.retain(|(k, ..), _| *k != format!("avatar:{jid}"));
                self.scaled.retain(|(k, ..), _| !k.ends_with(&format!("avatar:{jid}")));
                self.failed.remove(&format!("avatar:{jid}"));
                self.list_dirty = true;
            }
            ToUi::Mood(_) => {
                self.load_moods();
                self.list_dirty = true;
            }
            ToUi::Available(on) => self.available = on,
            ToUi::Older(chat, added) => {
                self.older_pending.remove(&chat);
                if added == Some(0) {
                    self.older_done.insert(chat.clone());
                }
                if Some(chat.as_str()) == self.current() {
                    self.reload_messages();
                }
            }
            ToUi::Error(text) => self.flash(text, 10),
        }
    }

    fn on_notify(&mut self, jid: &str, id: &str) {
        let Some(m) = self.store.message(jid, id) else { return };
        if Some(jid) == self.current() {
            // In the chat in front of you, read only while you show as online (writing here lately).
            if self.available {
                self.mark_read_if_seen(jid);
            }
            if m.kind == "text"
                && let Some(e) = emojicodes::reaction(&m.text) {
                    self.heart_for(id, &e);
                }
            return;
        }
        // An archived chat stays quiet.
        if self.store.chats().unwrap_or_default().iter().any(|c| c.jid == jid && c.archived) {
            return;
        }
        if self.fixed() {
            if let Some(h) = self.herdr.clone() {
                self.spawning.retain(|_, t| t.elapsed() < Duration::from_secs(15));
                if !self.open_elsewhere(jid) && self.is_most_recent_terminal() && !self.spawning.contains_key(jid) {
                    self.spawning.insert(jid.to_string(), Instant::now());
                    {
                    let exe = crate::exe();
                        h.open_chat(&exe, jid, false, None);
                    }
                }
            }
            return;
        }
        let said = if m.text.is_empty() { format!("[{}]", m.kind) } else { crate::text::with_mentions(&m.text, |d| self.store.mention_name(d)).0 };
        if self.tabs.iter().any(|t| t == jid) {
            self.bell = true;
            return self.notify(jid, said);
        }
        if self.open_elsewhere(jid) || !self.is_most_recent_terminal() {
            return;
        }
        self.open_tab(jid, false);
        self.bell = true;
        self.notify(jid, said);
    }

    fn on_term(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            Event::Paste(text) => {
                self.touch_activity();
                if self.viewing.is_some() {
                    return self.close_viewer();
                }
                self.paste(&text.replace("\r\n", "\n").replace('\r', "\n"));
            }
            Event::Mouse(m) => {
                self.touch_activity();
                self.on_mouse(m);
            }
            Event::FocusGained | Event::FocusLost => {
                let focused = matches!(ev, Event::FocusGained);
                self.has_focus = Some(focused);
                if !focused {
                    self.last_presence_touch = None;
                }
                self.backend.set_focus(self.pid, focused);
                if focused
                    && let Some(jid) = self.current().map(String::from) {
                        self.backend.mark_read(&jid);
                    }
            }
            Event::Resize(..) => {
                self.crops.clear();
                self.scaled.clear();
                self.rows_dirty = true;
                self.list_dirty = true;
            }
            _ => {}
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        self.touch_activity();
        self.text_sel = None;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        if k.code != KeyCode::Right {
            self.accept_on_arrival = 0;
        }
        if ctrl && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        // Ctrl+R draws the whole screen again.
        if ctrl && k.code == KeyCode::Char('r') {
            self.clear_screen = true;
            self.crops.clear();
            return;
        }
        if self.viewing.is_some() {
            return self.close_viewer();
        }
        self.sent_fix = None;
        if self.search.is_some() {
            return self.search_key(k);
        }
        if ctrl && k.code == KeyCode::Char('f') {
            if self.picker_open {
                self.picker_search = !self.picker_search;
                self.list_dirty = true;
            } else if self.current().is_some() {
                self.open_search();
            }
            return;
        }
        if k.code == KeyCode::Esc {
            return self.escape();
        }
        if matches!(k.code, KeyCode::PageUp | KeyCode::PageDown) && self.picker_open {
            let page = (self.list_rows as i64 - 1).max(1);
            return self.list_move(if k.code == KeyCode::PageUp { -page } else { page });
        }
        let inner = self.chat_area.height.saturating_sub(1).max(1) as usize;
        if k.code == KeyCode::PageUp && !ctrl {
            self.scroll_by(-(inner as i64));
            return;
        }
        if k.code == KeyCode::PageDown && !ctrl {
            self.scroll_by(inner as i64);
            return;
        }
        if ctrl && matches!(k.code, KeyCode::Down | KeyCode::PageDown) && !self.picker_open && self.current().is_some() {
            return self.go_to_latest();
        }
        let typing_input = self.focus == Focus::Input && !self.picker_open && !self.input.is_empty();
        let right_end = k.code == KeyCode::Right && self.cursor_at_end();
        if typing_input && (k.code == KeyCode::Tab || (right_end && (!self.suggestions.is_empty() || self.ghost_shown().is_some()))) {
            if !self.suggestions.is_empty() {
                return self.accept_suggestion();
            }
            if self.ghost_shown().is_some() {
                self.accept_ghost();
            }
            return;
        }
        if typing_input && right_end && (self.ghost_due.is_some() || self.ghost_cancel.is_some()) {
            self.accept_on_arrival += 1;
            return;
        }
        // In the list: Tab and → (at the end of the filter) open the chosen one, in Herdr in a new tab or pane.
        if self.picker_open && (k.code == KeyCode::Tab || (k.code == KeyCode::Right && self.filter_cursor >= compose::len(&self.filter))) {
            if !self.picker_chosen() {
                return;
            }
            let how = self.herdr.as_ref().map(|_| if k.code == KeyCode::Tab { crate::herdr::Place::Tab } else { crate::herdr::Place::Pane });
            return self.pick_chat(self.list_sel, how);
        }
        if k.code == KeyCode::Tab {
            if let (Some(h), false) = (self.herdr.clone(), self.picker_open) {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    if !h.focus_next_chat() {
                        let _ = tx.send(Input::HerdrNoNext);
                    }
                });
                return;
            }
            if !self.picker_open && self.tabs.len() < 2 {
                return self.open_picker("", None);
            }
            if self.tabs.is_empty() {
                return;
            }
            let i = if self.picker_open { self.active } else { (self.active + 1) % self.tabs.len() };
            return self.activate_tab(i);
        }
        match self.focus {
            Focus::Picker => self.picker_key(k),
            Focus::Input => self.input_key(k, ctrl, shift),
            Focus::Messages => self.messages_key(k, ctrl),
        }
    }

    /// Esc closes, in order: the quick reactions, the suggestions, the answer or reaction, the deletion, the edit,
    /// the selection, the list's search, its filter, the list, the tab, the program.
    fn escape(&mut self) {
        if self.quick_for.take().is_some() {
            return;
        }
        if !self.suggestions.is_empty() {
            if let Some(k) = self.suggest_kind {
                self.note_suggest(k, false);
            }
            self.suggestions.clear();
            return;
        }
        if self.reply_to.is_some() || self.react_to.is_some() {
            self.reply_to = None;
            self.react_to = None;
            return;
        }
        if self.deleting.take().is_some() {
            return;
        }
        if self.editing.take().is_some() {
            self.input.clear();
            self.cursor = 0;
            return self.update_suggestions(150);
        }
        if self.focus == Focus::Messages {
            return self.set_focus(Focus::Input);
        }
        if self.picker_open {
            if self.picker_search {
                self.picker_search = false;
                self.filter.clear();
                self.filter_cursor = 0;
                self.list_dirty = true;
                return;
            }
            if !self.filter.is_empty() {
                self.filter.clear();
                self.filter_cursor = 0;
                self.list_dirty = true;
                return;
            }
            if self.tabs.is_empty() {
                self.quit = true;
            } else {
                self.close_picker();
            }
            return;
        }
        if self.current().is_some() {
            return self.close_tab(self.active);
        }
        self.quit = true;
    }

    fn picker_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Up => return self.list_move(-1),
            KeyCode::Down => return self.list_move(1),
            KeyCode::Enter => {
                if self.picker_chosen() {
                    self.pick_chat(self.list_sel, None);
                }
                return;
            }
            _ => {}
        }
        let Some(e) = edit_key(&k) else { return };
        let (value, cursor) = compose::edit(&self.filter, self.filter_cursor, e);
        self.filter_cursor = cursor;
        if value != self.filter {
            self.filter = value;
            self.list_dirty = true;
        }
    }

    fn input_key(&mut self, k: KeyEvent, ctrl: bool, shift: bool) {
        // "/" with nothing written opens the chat list.
        if k.code == KeyCode::Char('/') && self.input.is_empty() && !ctrl {
            return self.open_picker("", None);
        }
        // Shift+Enter (with Kitty's keyboard protocol) or Ctrl+J: a new line.
        if (k.code == KeyCode::Enter && shift) || (ctrl && k.code == KeyCode::Char('j')) {
            return self.paste("\n");
        }
        if !self.suggestions.is_empty() {
            if matches!(k.code, KeyCode::Up | KeyCode::Down) && self.suggestions.len() > 1 {
                let n = self.suggestions.len();
                self.suggest_index = (self.suggest_index + if k.code == KeyCode::Up { n - 1 } else { 1 }) % n;
                return;
            }
            if k.code == KeyCode::Enter && !(self.suggest_face && self.suggest_index == 0) {
                return self.accept_suggestion();
            }
            if let (Some(kind), KeyCode::Char(' ')) = (self.suggest_kind, k.code) {
                if self.suggest_mode(kind) == "emoji" {
                    return self.accept_suggestion();
                }
                self.note_suggest(kind, false);
            }
            if self.suggest_face && k.code == KeyCode::Enter {
                self.note_suggest("smileys", false);
            }
        }
        if k.code == KeyCode::Enter {
            let draft = self.draft_shown();
            let v = std::mem::take(&mut self.input);
            if !v.is_empty() {
                self.note_writing();
            }
            self.cursor = 0;
            self.stop_composing();
            self.update_suggestions(150);
            let fut = self.submit(v, draft);
            // submit doesn't await anything; it runs to completion here.
            futures_lite_block_on(fut);
            return;
        }
        // Right after taking a suggestion that ended mid-word, a letter starts a new word, after a space.
        if let KeyCode::Char(c) = k.code
            && !ctrl && self.accepted.as_deref() == Some(self.input.as_str()) && self.cursor_at_end() && c.is_alphanumeric() {
                self.input.push(' ');
                self.cursor += 1;
            }
        let e = edit_key(&k);
        let edited = e.map(|e| compose::edit(&self.input, self.cursor, e));
        // Erasing an answer's or a reaction's text down to nothing, or Backspace with nothing, drops it.
        if self.reply_to.is_some() || self.react_to.is_some() {
            let drop = if self.input.is_empty() { k.code == KeyCode::Backspace } else { edited.as_ref().is_some_and(|(v, _)| v.is_empty()) };
            if drop {
                self.reply_to = None;
                self.react_to = None;
                self.input.clear();
                self.cursor = 0;
                return self.update_suggestions(150);
            }
        }
        let Some((value, cursor)) = edited else {
            if k.code == KeyCode::Up {
                self.move_selection(-1);
            }
            return;
        };
        if value != self.input {
            self.accepted = None;
            self.promote_active();
            self.note_writing();
        }
        self.input = value;
        self.cursor = cursor;
        self.note_composing();
        self.update_suggestions(150);
    }

    fn messages_key(&mut self, k: KeyEvent, ctrl: bool) {
        let delete = k.code == KeyCode::Delete || (ctrl && k.code == KeyCode::Char('d'));
        if let Some(sel) = self.selected.clone().filter(|_| delete) {
            let Some(m) = self.msg(&sel).cloned().filter(|m| m.kind != "deleted") else { return };
            if let Some((id, everyone)) = self.deleting.clone()
                && id == sel {
                    return self.delete_message(&id, everyone);
                }
            self.deleting = Some((sel, m.from_me && chrono::Utc::now().timestamp() - m.ts < DELETE_WINDOW));
            return;
        }
        self.deleting = None;
        match k.code {
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Right if self.selected.is_some() => {
                self.reply_to = self.selected.clone();
                self.react_to = None;
                self.set_focus(Focus::Input);
            }
            KeyCode::Backspace if self.selected.is_some() => {
                let id = self.selected.clone().unwrap_or_default();
                self.edit_message(&id);
            }
            // Typing over the selected message answers it; ":" reacts to it, and stays typed for the emoji's code.
            KeyCode::Char(c) if !ctrl && self.selected.is_some() => {
                let sel = self.selected.clone();
                if c == ':' {
                    self.react_to = sel;
                    self.reply_to = None;
                } else {
                    self.reply_to = sel;
                    self.react_to = None;
                }
                self.input = c.to_string();
                self.cursor = 1;
                self.promote_active();
                self.set_focus(Focus::Input);
            }
            _ => {}
        }
    }

    /// The chat's view moved `n` rows (up when negative); past the top, older messages come.
    fn scroll_by(&mut self, n: i64) {
        if self.current().is_none() || self.picker_open {
            return;
        }
        let height = self.chat_area.height as usize;
        let max_top = self.rows.len().saturating_sub(height);
        let top = (self.top as i64 + n).clamp(0, max_top as i64) as usize;
        self.top = top;
        self.at_bottom = top >= max_top;
        self.anchor = None;
        self.sent_fix = None;
        self.text_sel = None;
        // At the top, one page more, once the rows are the ones drawn: the page just loaded isn't laid out yet, so a
        // burst of keys or wheel notches before the next frame would find the top still there and load page after page.
        if top == 0 && n < 0 && !self.rows_dirty {
            self.load_older();
        }
    }

    // ---------- the mouse ----------

    fn hit_at(&self, x: u16, y: u16) -> Option<Hit> {
        self.hits.iter().rev().find(|(hx, hy, w, _)| y == *hy && x >= *hx && x < hx + w).map(|h| h.3.clone())
    }

    fn on_mouse(&mut self, m: ratatui::crossterm::event::MouseEvent) {
        let (x, y) = (m.column, m.row);
        match m.kind {
            // Ghostty by itself reports the pointer moving with no button held as 34 rather than 35, which reads as
            // dragging with the right button; that button does nothing here, so it's the pointer moving.
            MouseEventKind::Drag(MouseButton::Right) => self.on_mouse(ratatui::crossterm::event::MouseEvent { kind: MouseEventKind::Moved, ..m }),
            MouseEventKind::Moved => {
                self.pointer = Some((x, y));
                self.tab_hover = match self.hit_at(x, y) {
                    Some(Hit::Tab(i) | Hit::Close(i)) => Some(i as i64),
                    Some(Hit::Own | Hit::OwnClose) => Some(-1),
                    _ => None,
                };
                self.hover = if !self.picker_open && inside(self.chat_area, x, y) { crate::chat::row_at(self, y).and_then(|r| r.msg).and_then(|i| self.msgs.get(i)).map(|m| m.id.clone()) } else { None };
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let dir = if m.kind == MouseEventKind::ScrollUp { -1 } else { 1 };
                if self.picker_open && inside(self.list_area, x, y) {
                    self.list_follow = false;
                    let max = self.slots.len().saturating_sub(self.list_rows);
                    self.list_top = (self.list_top as i64 + dir).clamp(0, max as i64) as usize;
                } else if !self.picker_open && inside(self.chat_area, x, y) {
                    self.scroll_by(dir);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(x, y),
            MouseEventKind::Drag(MouseButton::Left) => self.mouse_drag(x, y),
            MouseEventKind::Up(MouseButton::Left) => self.mouse_up(x, y),
            _ => {}
        }
    }

    fn mouse_down(&mut self, x: u16, y: u16) {
        self.text_sel = None;
        self.press = None;
        self.sel_press = None;
        if self.viewing.is_some() || self.picker_open {
            if self.picker_open && inside(self.list_area, x, y) {
                let photo = x > self.list_area.x && x < self.list_area.x + 5;
                self.picker_press = Some((Instant::now(), photo));
            }
            if inside(self.band_text, x, y) {
                self.sel_press = Some((x, y, self.band_text, true));
            }
            return;
        }
        if inside(self.chat_area, x, y) {
            let row = crate::chat::row_at(self, y);
            let id = row.and_then(|r| r.msg).and_then(|i| self.msgs.get(i)).map(|m| m.id.clone());
            let on_text = row.and_then(|r| r.text).is_some_and(|(a, b)| x >= self.chat_area.x + a && x < self.chat_area.x + b);
            let header = row.is_some_and(|r| r.header) && !on_text;
            self.press = Some((x, y, id, header));
            if !header {
                self.sel_press = Some((x, y, self.chat_area, false));
            }
        } else if inside(self.band_text, x, y) {
            self.sel_press = Some((x, y, self.band_text, true));
        }
    }

    fn mouse_drag(&mut self, x: u16, y: u16) {
        self.pointer = Some((x, y));
        if let Some((px, py, Some(id), true)) = self.press.clone() {
            let dx = if py == y { x.saturating_sub(px).min(8) } else { 0 };
            if self.drag.as_ref() != Some(&(id.clone(), dx)) {
                self.drag = Some((id, dx));
                self.rows_dirty = true;
            }
            return;
        }
        if let Some((ax, ay, area, input)) = self.sel_press {
            let hx = x.clamp(area.x, area.x + area.width.saturating_sub(1));
            let hy = y.clamp(area.y, area.y + area.height.saturating_sub(1));
            self.text_sel = Some(TextSel { ax, ay, hx, hy, area, input });
        }
    }

    fn text_selected(&self) -> bool {
        self.text_sel.is_some_and(|s| s.ax != s.hx || s.ay != s.hy)
    }

    fn mouse_up(&mut self, x: u16, y: u16) {
        // A selection made: copied (ui.rs reads the cells), and kept lit till the next click or key.
        if self.sel_press.take().is_some() && self.text_selected() {
            self.press = None;
            self.drag = None;
            self.clipboard = Some(String::new());
            return;
        }
        self.text_sel = None;
        self.click(x, y);
    }

    fn click(&mut self, x: u16, y: u16) {
        let press = self.press.take();
        let dragged = press.as_ref().is_some_and(|(px, py, _, header)| *header && *py == y && x >= px + 4);
        if self.drag.take().is_some() {
            self.rows_dirty = true;
        }
        if self.viewing.is_some() {
            return self.close_viewer();
        }
        let hit = self.hit_at(x, y);
        match hit {
            Some(Hit::Own) => return,
            Some(Hit::OwnClose) => {
                self.tab_hover = None;
                if self.tabs.is_empty() {
                    self.quit = true;
                } else {
                    self.close_picker();
                }
                return;
            }
            Some(Hit::Tab(i)) => {
                if !self.fixed() && (self.picker_open || i != self.active) {
                    self.activate_tab(i);
                }
                return;
            }
            Some(Hit::Close(i)) => {
                self.tab_hover = None;
                return self.close_tab(i);
            }
            Some(Hit::Suggestion(i)) => {
                self.suggest_index = i;
                return self.accept_suggestion();
            }
            Some(Hit::Ghost) => return self.accept_ghost(),
            Some(Hit::SentFix) => return self.apply_sent_fix(),
            Some(Hit::Below) => return self.go_to_latest(),
            Some(Hit::HeadPhoto) => {
                if let Some(j) = self.current().map(String::from) {
                    self.open_photo(&j);
                }
                return;
            }
            Some(Hit::Photo(j)) => return self.open_photo(&j),
            Some(Hit::Viewer) => return self.close_viewer(),
            _ => {}
        }
        // The quick reactions open: one of them reacts ("⋯" goes to the keys); any other click closes them, unless
        // it's the second of a double click.
        let row = if inside(self.chat_area, x, y) && !self.picker_open { crate::chat::row_at(self, y).cloned() } else { None };
        let msg = row.as_ref().and_then(|r| r.msg).and_then(|i| self.msgs.get(i)).cloned();
        let now = Instant::now();
        let double = msg.as_ref().is_some_and(|m| self.last_click.as_ref().is_some_and(|(id, t)| *id == m.id && now - *t < Duration::from_millis(400)));
        self.last_click = msg.as_ref().map(|m| (m.id.clone(), now));
        if let Some(target) = self.quick_for.clone().filter(|_| !self.picker_open && inside(self.chat_area, x, y)) {
            if let Some(Hit::React(id, e)) = &hit {
                self.quick_for = None;
                if e == "⋯" {
                    self.react_to = Some(id.clone());
                    self.reply_to = None;
                    self.set_focus(Focus::Input);
                } else {
                    self.react(id, e, true);
                }
                return;
            }
            let _ = target;
            if !double {
                self.quick_for = None;
                return;
            }
        }
        if let (Some(m), true) = (&msg, dragged || double) {
            self.quick_for = None;
            self.reply_to = Some(m.id.clone());
            self.react_to = None;
            return self.set_focus(Focus::Input);
        }
        if let Some(Hit::Smile(id)) = hit {
            self.quick_for = Some(id);
            return;
        }
        self.sent_fix = None;
        if self.picker_open {
            return self.list_click(x, y);
        }
        if inside(self.head_area, x, y) {
            if self.head_name.is_some_and(|r| inside(r, x, y)) {
                self.open_picker("", None);
            }
            return;
        }
        if inside(self.band_area, x, y) {
            return self.band_click(x, y);
        }
        let Some(row) = row else { return };
        // A link copies; a passage of mine marked wrong shows its correction; a name or a mention opens their chat.
        if !dragged {
            let col = x.saturating_sub(self.chat_area.x);
            if let Some(url) = row.links.iter().find(|(a, b, _)| col >= *a && col < *b).map(|l| l.2.clone()) {
                match &self.web {
                    Some(w) => {
                        let _ = w.send(WebMsg::Link(url));
                    }
                    None => self.clipboard = Some(url),
                }
                return;
            }
            if let (Some(m), Some((a, b))) = (&msg, row.text)
                && col >= a && col < b
                    && let Some(fix) = row.fixes.iter().find(|(fa, fb, _)| col >= *fa && col < *fb).map(|f| f.2.clone()) {
                        let fx = self.chat_area.x + row.fixes.iter().find(|f| f.2 == fix).map(|f| f.0).unwrap_or(col);
                        self.sent_fix = Some((m.clone(), fix, fx, y));
                        return;
                    }
            if let Some(jid) = row.names.iter().find(|(a, b, _)| col >= *a && col < *b).map(|n| n.2.clone()) {
                let jid = self.store.canonical_of(&jid);
                return self.open_chat(&jid, None);
            }
        }
        let Some(m) = msg else { return self.set_focus(Focus::Input) };
        // A pin goes to the message it's about; a picture opens in the popup; another attachment opens.
        if m.kind == "pin" || m.kind == "unpin" {
            let target = m.text.clone();
            return self.jump_to(&target, true);
        }
        let on_picture = row.picture.is_some_and(|(a, b)| x >= self.chat_area.x + a && x < self.chat_area.x + b);
        if row.preview && on_picture {
            return self.open_viewer(&m);
        }
        if m.media.is_some() {
            // A picture counts only where one was drawn (none without pictures, or expired with no thumbnail).
            let idx = self.msgs.iter().position(|x| x.id == m.id);
            let has_picture = self.rows.iter().any(|r| r.msg == idx && r.picture.is_some());
            if !has_picture || on_picture {
                if has_picture && matches!(m.kind.as_str(), "image" | "sticker") && !self.caps.no_images {
                    return self.open_viewer(&m);
                }
                self.open_media(&m)
            }
        }
    }

    fn list_click(&mut self, x: u16, y: u16) {
        if !inside(self.list_area, x, y) {
            if inside(self.band_area, x, y) {
                self.band_click(x, y);
            }
            return;
        }
        let i = self.list_top + (y - self.list_area.y) as usize;
        let at = match self.slots.get(i) {
            Some(Slot::Bottom(_)) => i.saturating_sub(1),
            Some(Slot::HitText(_)) => (0..=i).rev().find(|j| matches!(self.slots[*j], Slot::Hit(_))).unwrap_or(i),
            _ => i,
        };
        let Some(slot) = self.slots.get(at).cloned() else { return };
        if !slot.selectable() {
            return;
        }
        if let Slot::Top(c) = slot {
            let photo = x > self.list_area.x && x < self.list_area.x + 5;
            if let Some(chat) = self.filtered.get(c).cloned()
                && photo && self.store.avatar(&chat.jid).is_some() {
                    return self.open_photo(&chat.jid);
                }
        }
        self.list_sel = at;
        self.picked = true;
        self.pick_chat(at, None);
    }

    /// A click in the band puts the cursor there; on the list's filter too.
    fn band_click(&mut self, x: u16, y: u16) {
        if !inside(self.band_text, x, y) {
            return;
        }
        if !self.picker_open {
            self.set_focus(Focus::Input);
        }
        let row = self.band_first + (y - self.band_text.y) as usize;
        let col = x.saturating_sub(self.band_text.x) as usize;
        let mut pos: usize = self.band_lines.iter().take(row).map(|l| l.len()).sum();
        if let Some(line) = self.band_lines.get(row) {
            let mut w = 0;
            for g in line {
                let gw = crate::text::width(g);
                if w as f64 + gw as f64 / 2.0 > col as f64 {
                    break;
                }
                w += gw;
                pos += 1;
            }
        }
        if self.picker_open {
            self.filter_cursor = pos;
        } else if let Some(s) = self.search.as_mut() {
            s.cursor = pos;
        } else {
            self.cursor = pos;
            self.update_suggestions(150);
        }
    }

    // ---------- pictures and attachments ----------

    /// A message's picture for the popup: its full file once here (the thumbnail when it expired), or a preview's.
    pub fn picture_of(&self, m: &Msg) -> Option<String> {
        if matches!(m.kind.as_str(), "image" | "sticker") {
            return m.media_path.clone();
        }
        let p = crate::wa::preview_path(&self.media_dir(), &m.chat, &m.id);
        (m.kind == "text" && p.exists()).then(|| p.to_string_lossy().into_owned())
    }

    pub fn media_dir(&self) -> std::path::PathBuf {
        crate::config_dir().join("media")
    }

    fn open_viewer(&mut self, m: &Msg) {
        let path = self.picture_of(m);
        let media = matches!(m.kind.as_str(), "image" | "sticker");
        let Some(path) = path else {
            if !media {
                return;
            }
            if m.media_err {
                return self.flash(s("attachmentExpired"), 6);
            }
            self.backend.ensure_media(&m.chat, &m.id);
            return self.flash(s("downloading"), 6);
        };
        self.viewing = Some(Viewing { key: format!("{} {}", m.chat, m.id), path });
        self.quick_for = None;
        self.crops.clear();
    }

    fn open_photo(&mut self, jid: &str) {
        let Some(path) = self.store.avatar(jid) else { return };
        self.viewing = Some(Viewing { key: format!("photo {jid}"), path });
        self.crops.clear();
    }

    fn close_viewer(&mut self) {
        self.viewing = None;
        self.crops.clear();
        self.drop_describing();
    }

    /// An attachment opened with the desktop's own opener, downloaded first when it isn't here.
    fn open_media(&mut self, m: &Msg) {
        let Some(file) = m.media_path.clone() else {
            if m.media_err {
                return self.flash(s("attachmentExpired"), 6);
            }
            self.backend.ensure_media(&m.chat, &m.id);
            return self.flash(s("downloading"), 6);
        };
        if let Some(w) = &self.web {
            let rel = std::path::Path::new(&file).strip_prefix(self.media_dir()).map(|p| p.to_string_lossy().into_owned()).unwrap_or(file.clone());
            let _ = w.send(WebMsg::Open(rel));
            return;
        }
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let r = std::process::Command::new(opener)
            .arg(&file)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Err(e) = r {
            self.flash(t("cannotOpen", &[&opener, &e]), 10);
        }
    }

    /// A chat's photo to draw, decoded off the loop; its profile asked for, at most once a day.
    pub fn avatar(&mut self, jid: &str) -> Option<Arc<DynamicImage>> {
        let key = format!("avatar:{jid}");
        if let Some(img) = self.images.get(&key) {
            return Some(img.clone());
        }
        if self.is_open() && self.profile_asked.insert(jid.to_string()) {
            self.backend.ensure_profile(jid);
        }
        let path = self.store.avatar(jid)?;
        if self.asked.insert(key.clone()) {
            let tx = self.tx.clone();
            crate::chat::decode(tx, key, path);
        }
        None
    }

    /// A picture for the popup, whole.
    pub fn whole(&mut self, path: &str) -> Option<Arc<DynamicImage>> {
        let key = format!("file:{path}");
        if let Some(img) = self.images.get(&key) {
            return Some(img.clone());
        }
        if self.asked.insert(key.clone()) {
            let (tx, path) = (self.tx.clone(), path.to_string());
            crate::chat::decode(tx, key, path);
        }
        None
    }

    /// A message's picture: the decoded one, or its thumbnail meanwhile; the full one asked for the first time.
    pub fn picture(&mut self, m: &Msg) -> Option<Arc<DynamicImage>> {
        let key = format!("{}/{}", m.chat, m.id);
        // Its file, once downloaded, decoded once (`asked` while it is); asked to be downloaded once (`fetching`),
        // which doesn't stop the file being read when it comes.
        if !self.full.contains(&key) && !self.asked.contains(&key) && matches!(m.kind.as_str(), "image" | "sticker") {
            if let Some(path) = &m.media_path {
                self.asked.insert(key.clone());
                let (path, tx, key) = (path.clone(), self.tx.clone(), key.clone());
                crate::chat::decode(tx, key, path);
            } else if m.media.is_some() && !m.media_err && self.fetching.insert(key.clone()) {
                self.backend.ensure_media(&m.chat, &m.id);
            }
        }
        if let Some(img) = self.images.get(&key) {
            return Some(img.clone());
        }
        let thumb = image::load_from_memory(m.thumb.as_deref()?).ok()?;
        let img = Arc::new(thumb);
        self.images.insert(key, img.clone());
        Some(img)
    }

    /// A link preview's picture, fetched when it comes into view.
    pub fn preview(&mut self, m: &Msg) -> Option<Arc<DynamicImage>> {
        let key = format!("link:{}/{}", m.chat, m.id);
        if let Some(img) = self.images.get(&key) {
            return Some(img.clone());
        }
        let path = crate::wa::preview_path(&self.media_dir(), &m.chat, &m.id);
        if !path.exists() {
            if self.fetching.insert(key.clone()) {
                self.backend.ensure_preview(&m.chat, &m.id);
            }
            return None;
        }
        if self.asked.insert(key.clone()) {
            let tx = self.tx.clone();
            crate::chat::decode(tx, key, path.to_string_lossy().into_owned());
        }
        None
    }

    // ---------- the clock ----------

    /// What's due now: the clock's work, and when it should look again.
    pub fn tick(&mut self) -> Option<Duration> {
        let now = Instant::now();
        let mut next: Vec<Duration> = Vec::new();
        if let Some((_, until)) = &self.transient {
            if now >= *until {
                self.transient = None;
            } else {
                next.push(*until - now);
            }
        }
        if let Some((_, _, since)) = &self.notice {
            if now - *since >= Duration::from_millis(NOTICE.0 + NOTICE.1 + NOTICE.2) {
                self.notice = None;
            } else {
                next.push(Duration::from_millis(40));
            }
        }
        if let Some(until) = self.composing_until {
            if now >= until {
                self.stop_composing();
            } else {
                next.push(until - now);
            }
        }
        if let Some((due, text)) = self.ghost_due.clone() {
            if now >= due {
                self.ghost_due = None;
                if text == self.input {
                    self.request_ghost(text);
                }
            } else {
                next.push(due - now);
            }
        }
        if self.typing.values().any(|w| !w.is_empty()) {
            next.push(Duration::from_millis(100));
        }
        if !self.hearts.is_empty() || !self.hearts_wanted.is_empty() {
            next.push(crate::hearts::FRAME);
        }
        self.hearts_wanted.retain(|h| h.since.elapsed() < Duration::from_millis(2500));
        if let Some(f) = &mut self.flight {
            if f.started.elapsed() > Duration::from_secs(2) {
                self.flight = None;
                self.rows_dirty = true;
            } else if now >= f.next {
                if f.room < f.lines.len() + 1 && !f.arrived {
                    f.room += 1;
                    f.next = now + Duration::from_millis(30);
                } else {
                    f.frame += 1;
                    f.next = now + Duration::from_millis(40);
                    if f.frame > FLY_FRAMES && f.arrived {
                        self.flight = None;
                    }
                }
                self.rows_dirty = true;
                next.push(Duration::from_millis(30));
            } else {
                next.push(f.next - now);
            }
        }
        if !self.float_key.is_empty() && self.float_key != self.float_off {
            let left = Duration::from_secs(10).saturating_sub(self.float_since.elapsed());
            if left.is_zero() {
                self.float_off = self.float_key.clone();
            } else {
                next.push(left);
            }
        }
        // My messages' marks of what's wrong go once they can no longer be edited.
        let now_s = chrono::Utc::now().timestamp();
        let expiry = self
            .msgs
            .iter()
            .filter(|m| m.from_me && m.kind == "text" && now_s - m.ts < EDIT_WINDOW + 2)
            .filter(|m| self.sent_fixes.get(&format!("{} {} {}", m.chat, m.id, bare_text(&m.text))).is_some_and(|f| f.is_some()))
            .map(|m| m.ts + EDIT_WINDOW)
            .min();
        if let Some(at) = expiry {
            if now_s >= at {
                self.rows_dirty = true;
            }
            next.push(Duration::from_secs((at - now_s).max(1) as u64));
        }
        if let Some((_, t)) = self.older_pending.iter().find(|(j, _)| Some(j.as_str()) == self.current()) {
            let left = Duration::from_secs(30).saturating_sub(t.elapsed());
            next.push(left.max(Duration::from_millis(100)));
        }
        next.into_iter().min()
    }

    /// The window's title and, in Herdr, the agent's state and the tab's name.
    pub fn update_title(&mut self) -> Option<String> {
        let unread: Vec<&Chat> = self.chats.iter().filter(|c| c.unread > 0 && if self.fixed() { Some(c.jid.as_str()) == self.current() } else { !c.archived }).collect();
        let typing: Vec<String> = self.typing.iter().filter(|(j, w)| !w.is_empty() && (!self.fixed() || Some(j.as_str()) == self.current())).map(|(j, _)| self.store.name_of(j)).collect();
        let mark = if self.herdr.is_some() {
            ""
        } else if !typing.is_empty() {
            Self::typing_emoji()
        } else if !unread.is_empty() {
            "●"
        } else {
            ""
        };
        let name = self.current().map(|j| self.store.name_of(j)).unwrap_or_else(|| "wassup".into());
        let title = if mark.is_empty() { name } else { format!("{mark} {name}") };
        let summary: String = unread.iter().map(|c| format!("{} ({})", c.name, c.unread)).collect::<Vec<_>>().join(", ");
        let total: u32 = unread.iter().map(|c| c.unread.max(0) as u32).sum();
        if let Some(h) = &self.herdr {
            let short = self.current().and_then(|j| short_name(&self.store, j));
            h.tab_name(short.as_deref());
            if !typing.is_empty() {
                h.report(crate::herdr::State::Working, Some(&t("typingWho", &[&typing.join(", ")])));
            } else if total > self.unread_shown {
                h.done(Some(&summary));
            } else {
                h.report(crate::herdr::State::Idle, if summary.is_empty() { None } else { Some(&summary) });
            }
        }
        self.unread_shown = total;
        if title == self.title_shown {
            return None;
        }
        self.title_shown = title.clone();
        if let Some(h) = &self.herdr {
            h.title(&title);
        }
        Some(title)
    }

    /// Before leaving: the record of this terminal goes, Herdr is let go.
    pub fn leave(&mut self) {
        if let Some(w) = &self.web {
            let _ = w.send(WebMsg::Logout);
        }
        self.stop_composing();
        self.store.delete_state(&self.tabs_key());
        if let Some(h) = &self.herdr {
            h.release();
        }
    }
}

/// What a browser's tab is told besides its frames (webtab.rs): a link to open in a new tab, text to copy, an
/// attachment to open on its device, the page's title and icon, the pictures of the frame, leaving.
#[derive(Clone, Debug)]
pub enum WebMsg {
    Link(String),
    Copy(String),
    Open(String),
    Title(String),
    /// The open chat's photo (a file under the media folder), or its initials on its colour; nothing for the list.
    Icon { photo: Option<String>, initials: Option<String>, color: Option<String> },
    /// A frame's pictures, which complete it, and the chat's rows for the page to scroll itself.
    Frame(Vec<WebPic>, Option<WebPanel>),
    Logout,
}

/// The open chat for a browser, which scrolls it itself: where it goes on the screen and how many rows are in view,
/// every row (`rows`, drawn plain, of generation `layout`; those in view, `view`, as the frame has them, with whatever
/// floats over them), the row on top and how many times it was moved here (`scroll`), and every picture by its first
/// row. `key` is the chat's: another one starts afresh.
#[derive(Clone, Debug)]
pub struct WebPanel {
    pub key: String,
    pub area: Rect,
    pub layout: u64,
    pub rows: Arc<Vec<ratatui::buffer::Buffer>>,
    pub view: Vec<ratatui::buffer::Buffer>,
    pub top: usize,
    pub scroll: u64,
    pub pics: Vec<(usize, WebPic)>,
}

/// A picture of a frame for the page: its file (or its pixels, by key), where it goes in cells, and the part of it
/// in view (top and bottom, 0 to 1); or an emoji on its own drawn twice its size.
#[derive(Clone, Debug)]
pub enum WebPic {
    Image { key: String, file: Option<String>, png: Option<Arc<DynamicImage>>, x: u16, y: u16, w: u16, h: u16, from: f32, to: f32 },
    Emoji { emoji: String, x: u16, y: u16 },
}

/// What the floating suggestion is: the letters missing from the word at the end, the right word, or a correction.
#[derive(Clone, Debug)]
pub enum GhostView {
    Suffix(String, String, String),
    Word(String, String),
    Fix(Fix),
}

/// A message's text without the "(editada)" line an edit leaves at its end.
pub fn bare_text(text: &str) -> &str {
    text.strip_suffix("\n(editada)").or_else(|| text.strip_suffix("\n(edited)")).unwrap_or(text)
}

/// The notice's opacity since it started: rises, holds, falls, smoothly.
pub fn notice_opacity(since: Instant) -> f64 {
    let t = since.elapsed().as_millis() as f64;
    let ease = |x: f64| x * x * (3.0 - 2.0 * x);
    let (fin, hold, fout) = (NOTICE.0 as f64, NOTICE.1 as f64, NOTICE.2 as f64);
    if t < fin {
        ease(t / fin)
    } else if t < fin + hold {
        1.0
    } else {
        ease((1.0 - (t - fin - hold) / fout).max(0.0))
    }
}

/// A chat's short name, for a tab's label: a person's first name, a group's whole name, nothing for a number.
pub fn short_name(store: &Store, jid: &str) -> Option<String> {
    if crate::text::is_group(jid) {
        return store.chat_name(jid);
    }
    let name = store.name_of(jid);
    if crate::store::looks_like_number(&name) || name.starts_with("lid:") {
        return None;
    }
    name.split_whitespace().next().map(String::from)
}

pub fn inside(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// The editing key a key event is, if any.
fn edit_key(k: &KeyEvent) -> Option<Edit> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL) || k.modifiers.contains(KeyModifiers::ALT);
    Some(match k.code {
        KeyCode::Left => Edit::Left,
        KeyCode::Right => Edit::Right,
        KeyCode::Home => Edit::Home,
        KeyCode::End => Edit::End,
        KeyCode::Backspace if k.modifiers.contains(KeyModifiers::SHIFT) => Edit::DeleteWord,
        KeyCode::Backspace => Edit::Backspace,
        KeyCode::Delete => Edit::Delete,
        KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => Edit::Clear,
        KeyCode::Char(c) if !ctrl => Edit::Insert(c.to_string()),
        _ => return None,
    })
}

/// Runs a future that never waits to its end, here and now.
fn futures_lite_block_on<F: std::future::Future<Output = ()>>(f: F) {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut f = std::pin::pin!(f);
    let _ = f.as_mut().poll(&mut cx);
}

/// The loop: input as it comes, the clock when something's due, drawn once whatever was pending is taken in. The
/// app comes back at the end, for the connection to be closed properly.
pub async fn run<B: ratatui::backend::Backend>(mut app: App, mut terminal: Terminal<B>, mut rx: UnboundedReceiver<Input>, mut wa_rx: UnboundedReceiver<ToUi>) -> (Result<()>, App) {
    let r = event_loop(&mut app, &mut terminal, &mut rx, &mut wa_rx).await;
    (r, app)
}

async fn event_loop<B: ratatui::backend::Backend>(app: &mut App, terminal: &mut Terminal<B>, rx: &mut UnboundedReceiver<Input>, wa_rx: &mut UnboundedReceiver<ToUi>) -> Result<()> {
    crate::ui::frame(app, terminal)?;
    while !app.quit {
        let wait = app.tick().unwrap_or(Duration::from_secs(3600));
        tokio::select! {
            Some(i) = rx.recv() => app.on_input(i),
            Some(e) = wa_rx.recv() => app.on_input(Input::Wa(e)),
            _ = tokio::time::sleep(wait) => {}
            else => break,
        }
        // Whatever else is already waiting, before drawing: a burst of wheel notches is one frame.
        loop {
            if let Ok(i) = rx.try_recv() {
                app.on_input(i);
            } else if let Ok(e) = wa_rx.try_recv() {
                app.on_input(Input::Wa(e));
            } else {
                break;
            }
        }
        if app.quit {
            break;
        }
        app.tick();
        crate::ui::frame(app, terminal)?;
    }
    Ok(())
}
