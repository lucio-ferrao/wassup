//! Each chat's state, as one emoji the local model picks from its latest messages (`Llm::mood`), for the screen to
//! show after the chat's name, as the Node version's mood.ts. Worked out calmly, as the model takes one request at a time and
//! the writing suggestions must find it free:
//! - only chats with messages since their state was worked out, quiet for QUIET since the last one, and open in a
//!   terminal or with messages in the last day (RECENT), never one whose last message is over a week old (OLD); from
//!   the most recent to the oldest;
//! - one request at a time, a look every PAUSE;
//! - never while I write (`MoodKeeper::writing`), and a request under way gives way when I start (the chat stays in
//!   the queue);
//! - the model, asleep, is woken by the request itself, and its requests keep it awake while the queue goes through.
//!
//! The messages go from the latest back, as many as fit in the model's context, short of a margin for the
//! instructions and the answer. The keeper works on a thread of its own, as its requests block; what it needs of the
//! rest of the program (the chats, their messages, where a mood is kept, who's told) it asks of a `MoodHost`.

use std::cmp::Reverse;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::llm::{Cancel, Error, Lang, Llm, clip, squeeze};

/// A chat's state as kept: the emoji ("" for none) and the time of the last message it was worked out for. In JSON
/// as wassup keeps it in its state table, under `mood:<jid>`: `{"emoji":"…","upTo":1700000000}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mood {
    pub emoji: String,
    pub up_to: i64,
}

/// A chat as the keeper chooses which to work out next.
#[derive(Clone, Debug, Default)]
pub struct MoodChat {
    pub jid: String,
    /// Its last message's time, in seconds since the epoch; 0 for none.
    pub last_ts: i64,
    /// Its kept mood's `up_to`; 0 for none.
    pub up_to: i64,
    /// Open in a terminal, in a tab.
    pub open: bool,
    pub archived: bool,
}

/// A message as the model reads it to work out its chat's state.
#[derive(Clone, Debug, Default)]
pub struct MoodMsg<'a> {
    /// Its kind as wassup stores it: "text", "image", "deleted", "reaction", "secretEncrypted", "pinInChat", "album",
    /// "ptv", "placeholder", "groupInvite", "interactive", "template"…
    pub kind: &'a str,
    pub from_me: bool,
    /// Who wrote it, by name: the sender in a group, the chat's own name otherwise. Not used for mine.
    pub who: &'a str,
    pub text: &'a str,
    /// For a pin, that it took one away.
    pub unpinned: bool,
}

/// What the keeper needs of the rest of the program, asked from its thread.
pub trait MoodHost: Send + Sync + 'static {
    /// Every chat, with what the keeper chooses by.
    fn chats(&self) -> Vec<MoodChat>;
    /// A chat's latest messages as `mood_line` writes them, oldest first, those it leaves out left out. Those that fit
    /// in `budget` characters (with a line break after each) are enough; more do no harm, as only the latest that fit
    /// are kept.
    fn lines(&self, jid: &str, budget: usize) -> Vec<String>;
    /// Keeps a chat's mood.
    fn save(&self, jid: &str, mood: &Mood);
    /// A chat's mood has just been worked out and kept.
    fn changed(&self, jid: &str);
}

/// Seconds a chat must have been quiet since its last message.
const QUIET: i64 = 120;
/// A chat not open in a terminal is only worked out with messages this recent, in seconds.
const RECENT: i64 = 24 * 3600;
/// Nor one whose last message is older than this, in seconds.
const OLD: i64 = 7 * 24 * 3600;
const PAUSE: Duration = Duration::from_secs(30);
/// No request while I typed this recently.
const WRITING: Duration = Duration::from_secs(30);
/// Tokens kept for the instructions and the answer, and how many characters a token stands for, short of the truth.
const MARGIN: usize = 2048;
const CHARS_PER_TOKEN: usize = 3;
/// A request taking longer than this is given up.
const TIMEOUT: Duration = Duration::from_secs(180);

/// A message as the model reads it to work out the chat's state, as wassup's moodLine: who wrote it and its text, or
/// what it is; None for a message left out (deleted, a reaction, one that couldn't be decrypted).
pub fn mood_line(m: &MoodMsg, lang: Lang) -> Option<String> {
    if matches!(m.kind, "deleted" | "secretEncrypted" | "reaction") {
        return None;
    }
    let pt = lang == Lang::Pt;
    let who = if m.from_me { if pt { "eu" } else { "me" } } else { m.who };
    let text = if m.text.is_empty() { format!("[{}]", type_label(m, pt)) } else { m.text.to_string() };
    Some(format!("{who}: {}", clip(&squeeze(&text), 500)))
}

/// The words for a message of a kind with no text to show, as wassup's typeLabel.
fn type_label(m: &MoodMsg, pt: bool) -> &'static str {
    let (p, e) = match m.kind {
        "pinInChat" if m.unpinned => ("📌 desafixou uma mensagem", "📌 unpinned a message"),
        "pinInChat" => ("📌 fixou uma mensagem", "📌 pinned a message"),
        "album" => ("álbum", "album"),
        "ptv" => ("mensagem de vídeo", "video message"),
        "placeholder" => ("à espera desta mensagem", "waiting for this message"),
        "groupInvite" => ("convite para um grupo", "group invite"),
        "interactive" | "template" => ("mensagem de empresa", "business message"),
        _ => ("mensagem não suportada", "unsupported message"),
    };
    if pt { p } else { e }
}

/// The next chat due at `now` (seconds since the epoch), if any: see the conditions above.
fn next_due(chats: &[MoodChat], now: i64) -> Option<&MoodChat> {
    chats
        .iter()
        .filter(|c| {
            let age = now - c.last_ts;
            let recent = c.open || age < RECENT;
            !c.archived && c.last_ts != 0 && (QUIET..=OLD).contains(&age) && recent && c.up_to < c.last_ts
        })
        .min_by_key(|c| Reverse(c.last_ts))
}

/// The latest of `lines` that fit in `budget` characters, counting a line break after each, as JavaScript counts.
fn fit(mut lines: Vec<String>, budget: usize) -> Vec<String> {
    let mut used = 0;
    let mut first = lines.len();
    for (i, l) in lines.iter().enumerate().rev() {
        let n = l.encode_utf16().count() + 1;
        if used + n > budget {
            break;
        }
        used += n;
        first = i;
    }
    lines.split_off(first)
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Works out the chats' states in the background (see the module's doc), on a thread started by `start`.
pub struct MoodKeeper {
    shared: Arc<Shared>,
    running: Mutex<Option<Running>>,
}

/// The thread at work: its flag to stop, and the channel whose end, dropped, wakes it to see it.
struct Running {
    stop: Arc<AtomicBool>,
    _wake: mpsc::Sender<()>,
}

struct Shared {
    llm: Option<Llm>,
    host: Box<dyn MoodHost>,
    /// When I last typed.
    writing_at: Mutex<Option<Instant>>,
    /// The request under way.
    busy: Mutex<Option<Cancel>>,
}

impl MoodKeeper {
    /// A keeper asking `llm` (None for no local model: it then does nothing), with `host` for the rest. Its requests
    /// are given up after three minutes.
    pub fn new(llm: Option<Llm>, host: impl MoodHost) -> MoodKeeper {
        let shared = Shared {
            llm: llm.map(|l| l.with_timeout(TIMEOUT)),
            host: Box::new(host),
            writing_at: Mutex::new(None),
            busy: Mutex::new(None),
        };
        MoodKeeper { shared: Arc::new(shared), running: Mutex::new(None) }
    }

    /// Starts the looks every PAUSE, the first one PAUSE from now, on a thread of their own; nothing if they're already
    /// going, or with no model.
    pub fn start(&self) {
        let mut running = self.running.lock().unwrap();
        if self.shared.llm.is_none() || running.is_some() {
            return;
        }
        let (wake, woken) = mpsc::channel::<()>();
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, stopped) = (self.shared.clone(), stop.clone());
        let spawned = thread::Builder::new().name("moods".into()).spawn(move || {
            let mut next = Instant::now() + PAUSE;
            loop {
                // Woken before its time only to stop, by `wake` dropped.
                let wait = next.saturating_duration_since(Instant::now());
                if woken.recv_timeout(wait) != Err(RecvTimeoutError::Timeout) {
                    return;
                }
                shared.tick(&stopped);
                let now = Instant::now();
                while next <= now {
                    next += PAUSE;
                }
            }
        });
        match spawned {
            Ok(_) => *running = Some(Running { stop, _wake: wake }),
            Err(e) => warn!("moods: {e}"),
        }
    }

    /// Stops the looks, and gives up the request under way. The thread ends on its own, at once unless it's asking
    /// the server for the model's context (three seconds at most).
    pub fn stop(&self) {
        if let Some(running) = self.running.lock().unwrap().take() {
            running.stop.store(true, Ordering::SeqCst);
        }
        if let Some(cancel) = &*self.shared.busy.lock().unwrap() {
            cancel.cancel();
        }
    }

    /// I'm writing in a terminal: no request for a while, and the one under way gives way.
    pub fn writing(&self) {
        *self.shared.writing_at.lock().unwrap() = Some(Instant::now());
        if let Some(cancel) = &*self.shared.busy.lock().unwrap() {
            cancel.cancel();
        }
    }
}

impl Drop for MoodKeeper {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Shared {
    fn writing_recently(&self) -> bool {
        self.writing_at.lock().unwrap().is_some_and(|at| at.elapsed() < WRITING)
    }

    /// One look: works out the next chat due, if any, unless I'm writing.
    fn tick(&self, stopped: &AtomicBool) {
        let Some(llm) = &self.llm else { return };
        if self.writing_recently() {
            return;
        }
        let chats = self.host.chats();
        let Some(chat) = next_due(&chats, unix_now()) else { return };
        let ctx = llm.model_ctx();
        if ctx == 0 || self.writing_recently() || stopped.load(Ordering::SeqCst) {
            return;
        }
        let budget = (ctx.saturating_sub(MARGIN) * CHARS_PER_TOKEN).max(1000);
        let lines = fit(self.host.lines(&chat.jid, budget), budget);
        let cancel = Cancel::new();
        *self.busy.lock().unwrap() = Some(cancel.clone());
        // Looked at again with the request already in `busy`, so that writing or stopping from now on gives it up.
        if self.writing_recently() || stopped.load(Ordering::SeqCst) {
            cancel.cancel();
        }
        let got = if lines.is_empty() { Ok(None) } else { llm.mood(&lines, &cancel) };
        let mut busy = self.busy.lock().unwrap();
        if busy.as_ref().is_some_and(|b| b.same(&cancel)) {
            *busy = None;
        }
        drop(busy);
        match got {
            Ok(emoji) => {
                let emoji = emoji.unwrap_or_default();
                self.host.save(&chat.jid, &Mood { emoji: emoji.clone(), up_to: chat.last_ts });
                info!("mood: {} from {} lines: {emoji:?}", chat.jid, lines.len());
                self.host.changed(&chat.jid);
            }
            // Given way to the writing, stopped or out of time: the chat stays in the queue.
            Err(Error::Cancelled | Error::Timeout) => {}
            // The server down: the same.
            Err(e) => warn!("mood: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg<'a>(kind: &'a str, from_me: bool, who: &'a str, text: &'a str) -> MoodMsg<'a> {
        MoodMsg { kind, from_me, who, text, unpinned: false }
    }

    #[test]
    fn lines_as_the_model_reads_them() {
        let line = |m: &MoodMsg| mood_line(m, Lang::Pt);
        assert_eq!(line(&msg("text", false, "Rita Lemos", "Jantamos\n\namanhã?")).as_deref(), Some("Rita Lemos: Jantamos amanhã?"));
        assert_eq!(line(&msg("text", true, "Rita Lemos", "Sim  ")).as_deref(), Some("eu: Sim "));
        assert_eq!(mood_line(&msg("text", true, "", "Yes"), Lang::En).as_deref(), Some("me: Yes"));
        assert_eq!(line(&msg("image", false, "Rita Lemos", "A praia")).as_deref(), Some("Rita Lemos: A praia"));
        assert_eq!(line(&msg("image", false, "Rita Lemos", "")).as_deref(), Some("Rita Lemos: [mensagem não suportada]"));
        assert_eq!(line(&msg("ptv", false, "Rita Lemos", "")).as_deref(), Some("Rita Lemos: [mensagem de vídeo]"));
        let unpin = MoodMsg { unpinned: true, ..msg("pinInChat", true, "", "") };
        assert_eq!(line(&unpin).as_deref(), Some("eu: [📌 desafixou uma mensagem]"));
        assert_eq!(mood_line(&msg("template", false, "Loja", ""), Lang::En).as_deref(), Some("Loja: [business message]"));
        for kind in ["deleted", "secretEncrypted", "reaction"] {
            assert_eq!(line(&msg(kind, false, "Rita Lemos", "👍")), None);
        }
        let long = "a".repeat(600);
        assert_eq!(line(&msg("text", false, "Rita", &long)).unwrap().len(), "Rita: ".len() + 500);
    }

    fn chat(jid: &str, last_ts: i64, up_to: i64, open: bool) -> MoodChat {
        MoodChat { jid: jid.into(), last_ts, up_to, open, archived: false }
    }

    #[test]
    fn which_chat_is_due() {
        let now = 1_000_000;
        let due = |chats: &[MoodChat]| next_due(chats, now).map(|c| c.jid.clone());
        // Too fresh, already done, archived, too old for a closed chat, too old for any, never written to.
        let none = [
            chat("fresh", now - 60, 0, true),
            chat("done", now - 600, now - 600, true),
            MoodChat { archived: true, ..chat("archived", now - 600, 0, true) },
            chat("closed", now - 2 * RECENT, 0, false),
            chat("old", now - OLD - 1, 0, true),
            chat("empty", 0, 0, true),
        ];
        assert_eq!(due(&none), None);
        // The most recent first; an open chat even after a day; a chat with news since its mood.
        let chats = [chat("open", now - 2 * RECENT, 0, true), chat("recent", now - 300, 0, false), chat("again", now - 200, now - 900, false)];
        assert_eq!(due(&chats).as_deref(), Some("again"));
        assert_eq!(due(&chats[..2]).as_deref(), Some("recent"));
        assert_eq!(due(&chats[..1]).as_deref(), Some("open"));
        // Of two as recent, the first.
        assert_eq!(due(&[chat("a", now - 300, 0, false), chat("b", now - 300, 0, false)]).as_deref(), Some("a"));
    }

    #[test]
    fn latest_lines_that_fit() {
        let lines = || vec!["Rita: olá".to_string(), "eu: olá".to_string(), "Rita: 😀".to_string()];
        assert_eq!(fit(lines(), 100), lines());
        // "Rita: 😀" is 8 units and its line break 1; "eu: olá" 7 and 1.
        assert_eq!(fit(lines(), 17), vec!["eu: olá", "Rita: 😀"]);
        assert_eq!(fit(lines(), 16), vec!["Rita: 😀"]);
        assert!(fit(lines(), 8).is_empty());
    }

    #[test]
    fn mood_kept_as_wassup_keeps_it() {
        let m = Mood { emoji: "🍕".into(), up_to: 1_700_000_000 };
        assert_eq!(serde_json::to_string(&m).unwrap(), r#"{"emoji":"🍕","upTo":1700000000}"#);
        assert_eq!(serde_json::from_str::<Mood>(r#"{"emoji":"","upTo":5}"#).unwrap(), Mood { emoji: String::new(), up_to: 5 });
    }

    struct Quiet;

    impl MoodHost for Quiet {
        fn chats(&self) -> Vec<MoodChat> {
            Vec::new()
        }
        fn lines(&self, _: &str, _: usize) -> Vec<String> {
            Vec::new()
        }
        fn save(&self, _: &str, _: &Mood) {}
        fn changed(&self, _: &str) {}
    }

    /// A host with one chat due, that notes what it's told.
    #[derive(Clone, Default)]
    struct Notes {
        saved: Arc<Mutex<Vec<(String, Mood)>>>,
        told: Arc<Mutex<Vec<String>>>,
        budget: Arc<Mutex<usize>>,
        last_ts: i64,
    }

    impl MoodHost for Notes {
        fn chats(&self) -> Vec<MoodChat> {
            vec![chat("123456789012345@lid", self.last_ts, 0, false)]
        }
        fn lines(&self, _: &str, budget: usize) -> Vec<String> {
            *self.budget.lock().unwrap() = budget;
            vec!["Rita Lemos: Jantamos às 8?".into(), "eu: Combinado!".into()]
        }
        fn save(&self, jid: &str, mood: &Mood) {
            self.saved.lock().unwrap().push((jid.into(), mood.clone()));
        }
        fn changed(&self, jid: &str) {
            self.told.lock().unwrap().push(jid.into());
        }
    }

    /// A server answering, one connection after another, the model's context (4096) and a mood of 🍽️.
    fn server() -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            for s in listener.incoming() {
                let mut s = s.unwrap();
                let (mut got, mut buf) = (Vec::new(), [0; 65536]);
                let body = loop {
                    let text = String::from_utf8_lossy(&got).into_owned();
                    if let Some(head) = text.find("\r\n\r\n") {
                        let length = text[..head].lines().find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok())?
                        });
                        if got.len() >= head + 4 + length.unwrap_or(0) {
                            break text;
                        }
                    }
                    let n = s.read(&mut buf).unwrap();
                    got.extend_from_slice(&buf[..n]);
                };
                let answer = if body.starts_with("GET /v1/models") {
                    r#"{"data":[{"id":"model-x","status":{"value":"loaded","args":["--ctx-size","4096"]}}]}"#
                } else {
                    r#"{"choices":[{"message":{"content":"{\"emoji\":\"🍽️\"}"}}]}"#
                };
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", answer.len());
                s.write_all(head.as_bytes()).unwrap();
                s.write_all(answer.as_bytes()).unwrap();
            }
        });
        url
    }

    #[test]
    fn a_look_works_out_the_chat_due() {
        let notes = Notes { last_ts: unix_now() - 600, ..Notes::default() };
        let keeper = MoodKeeper::new(Some(Llm::new(server(), "model-x", Lang::Pt)), notes.clone());
        let stopped = AtomicBool::new(false);
        // Not while I write.
        keeper.writing();
        keeper.shared.tick(&stopped);
        assert!(notes.saved.lock().unwrap().is_empty());
        *keeper.shared.writing_at.lock().unwrap() = None;
        keeper.shared.tick(&stopped);
        assert_eq!(*notes.budget.lock().unwrap(), (4096 - MARGIN) * CHARS_PER_TOKEN);
        let saved = notes.saved.lock().unwrap().clone();
        let mood = Mood { emoji: "🍽\u{fe0f}".into(), up_to: notes.last_ts };
        assert_eq!(saved, vec![("123456789012345@lid".to_string(), mood)]);
        assert_eq!(*notes.told.lock().unwrap(), vec!["123456789012345@lid".to_string()]);
        assert!(keeper.shared.busy.lock().unwrap().is_none());
    }

    #[test]
    fn start_and_stop() {
        let keeper = MoodKeeper::new(Some(Llm::new("http://127.0.0.1:9", "model-x", Lang::En)), Quiet);
        keeper.start();
        keeper.start();
        assert!(keeper.running.lock().unwrap().is_some());
        keeper.writing();
        assert!(keeper.shared.writing_recently());
        keeper.stop();
        assert!(keeper.running.lock().unwrap().is_none());
        keeper.start();
        drop(keeper);
        let idle = MoodKeeper::new(None, Quiet);
        idle.start();
        assert!(idle.running.lock().unwrap().is_none());
    }
}
