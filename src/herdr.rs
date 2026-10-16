//! Herdr, the terminal multiplexer for agents, as the Node version's herdr.ts drives it. In a Herdr pane wassup presents
//! itself as an agent called "wassup" in the pane it runs in, so the sidebar shows its state: someone typing →
//! working, a new unread message → done (in blue, until the pane is looked at), nothing → idle. The agent's name
//! follows the window title, the active chat's name; alone in its tab, the tab's label takes the chat's first name,
//! and gets its own back on exit. Another chat opens in a new pane or tab running this same program, and Tab goes
//! from one pane running wassup to the next.
//!
//! The connection is the official hooks': one JSON line over Herdr's Unix socket, one line back. The requests that
//! answer nothing go out on a thread of their own, one after the other, so the screen never waits for Herdr; those
//! whose answer is wanted block, half a second at most for each step, and belong off the loop (`spawn_blocking`).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

/// The name wassup goes by in Herdr, as the origin of its requests and as the agent: the TypeScript one's too, so
/// Tab goes through the panes of both.
const AGENT: &str = "wassup";

/// How long a request waits for Herdr to take it or to answer before giving up.
const WAIT: Duration = Duration::from_millis(500);

/// How long the exit waits for the last requests to go out.
const RELEASE_WAIT: Duration = Duration::from_secs(2);

/// A pane at least this many columns wide splits to the right, room for two chats side by side; a narrower one
/// splits down.
const WIDE: i64 = 100;

/// The agent's state, as Herdr's sidebar shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Working,
}

impl State {
    /// The name Herdr's API knows the state by.
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Working => "working",
        }
    }
}

/// Where another chat opens: a new pane beside this one, or a new tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Pane,
    Tab,
}

/// Herdr, when this process runs in one of its panes: what reports to its sidebar and opens chats in its panes and
/// tabs. Clones share the same connection thread and the same memory of what was last reported.
#[derive(Clone)]
pub struct Herdr {
    inner: Arc<Inner>,
}

struct Inner {
    ids: Ids,
    jobs: Sender<Job>,
    /// How many titles were asked for: a title still waiting when a later one comes isn't sent.
    titles: Arc<AtomicU64>,
    last: Mutex<Last>,
}

/// What was last asked for, so the same thing isn't sent again.
#[derive(Default)]
struct Last {
    /// The state and its message, on two lines.
    state: String,
    title: String,
    /// The tab's name: None before the first, Some(None) for the tab's own label.
    tab: Option<Option<String>>,
}

/// Where Herdr is and where this process is in it, from the HERDR_* variables Herdr gives its panes' shells.
#[derive(Clone, Debug)]
struct Ids {
    socket: PathBuf,
    pane: String,
    tab: String,
    workspace: Option<String>,
}

impl Ids {
    /// The variables, when they say this is a Herdr pane: HERDR_ENV=1 and the socket, the pane and the tab.
    fn from_env() -> Option<Ids> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        if var("HERDR_ENV").as_deref() != Some("1") {
            return None;
        }
        Some(Ids {
            socket: var("HERDR_SOCKET_PATH")?.into(),
            pane: var("HERDR_PANE_ID")?,
            tab: var("HERDR_TAB_ID")?,
            workspace: var("HERDR_WORKSPACE_ID"),
        })
    }
}

/// What the connection thread is given to send.
enum Job {
    Report(State, Option<String>),
    /// A title, and its count among the titles asked for.
    Title(String, u64),
    TabName(Option<String>),
    Open { program: PathBuf, chat: String, focus: bool, place: Option<Place> },
    Focus(String, Option<String>),
    /// Leaving the sidebar, and clearing the agent's name if one was given.
    Release { clear: bool },
    /// Everything before it is sent.
    Done(Sender<()>),
}

impl Herdr {
    /// Herdr, if this process runs in a Herdr pane itself: the variables say so, and the walk up its ancestors
    /// agrees (see `in_herdr_pane`). None otherwise, and everything Herdr would do is left undone.
    pub fn detect() -> Option<Herdr> {
        let ids = Ids::from_env()?;
        if !in_herdr_pane(std::process::id(), ps) {
            log::info!("herdr: variables inherited, not in a pane of its own");
            return None;
        }
        Herdr::start(ids)
    }

    fn start(ids: Ids) -> Option<Herdr> {
        let (jobs, rx) = channel();
        let titles = Arc::new(AtomicU64::new(0));
        let worker = Worker { ids: ids.clone(), seq: millis(), tab_label: TabLabel::Unasked, titles: titles.clone() };
        if let Err(e) = std::thread::Builder::new().name("herdr".into()).spawn(move || worker.run(rx)) {
            log::warn!("herdr: no thread for its requests: {e}");
            return None;
        }
        Some(Herdr { inner: Arc::new(Inner { ids, jobs, titles, last: Mutex::new(Last::default()) }) })
    }

    /// This pane's id in Herdr (HERDR_PANE_ID), for other terminals to find the chats open in it.
    pub fn pane_id(&self) -> &str {
        &self.inner.ids.pane
    }

    /// This pane's tab's id in Herdr (HERDR_TAB_ID).
    pub fn tab_id(&self) -> &str {
        &self.inner.ids.tab
    }

    /// Reports the agent's state, with a line to show under it; the same state and line again aren't sent.
    pub fn report(&self, state: State, message: Option<&str>) {
        let key = format!("{}\n{}", state.as_str(), message.unwrap_or(""));
        let mut last = self.last();
        if last.state == key {
            return;
        }
        last.state = key;
        self.queue(Job::Report(state, message.map(String::from)));
    }

    /// Marks the agent "done" (blue in the sidebar), which the API doesn't take as a state to report: Herdr gives
    /// it to an agent that goes from working to idle out of view, and keeps it until its pane is seen. So: working,
    /// then idle at once, in that order by their sequence numbers.
    pub fn done(&self, message: Option<&str>) {
        let mut last = self.last();
        self.queue(Job::Report(State::Working, message.map(String::from)));
        self.queue(Job::Report(State::Idle, message.map(String::from)));
        last.state = format!("idle\n{}", message.unwrap_or(""));
    }

    /// Gives the agent the window's title as its name (`display_agent`, what the sidebar shows for the pane in
    /// place of "wassup"), whether the pane is alone in its tab or not. Titles come fast while the typing spinner
    /// turns: one still waiting when a later one comes isn't sent.
    pub fn title(&self, title: &str) {
        let mut last = self.last();
        if last.title == title {
            return;
        }
        last.title = title.to_string();
        let n = self.inner.titles.fetch_add(1, Ordering::SeqCst) + 1;
        self.queue(Job::Title(title.to_string(), n));
    }

    /// Names the tab after the chat (its first name, no state; None for none, the tab's own label back), when this
    /// pane is alone in its tab; the label that was there is kept for the exit. A split tab's label, and the panes'
    /// labels, are left alone.
    pub fn tab_name(&self, name: Option<&str>) {
        let name = name.map(String::from);
        let mut last = self.last();
        if last.tab.as_ref() == Some(&name) {
            return;
        }
        last.tab = Some(name.clone());
        self.queue(Job::TabName(name));
    }

    /// Opens a chat in a new Herdr pane or tab, focused when it was chosen, in the background when it's an incoming
    /// message. `place` says which; left out, it follows the layout: a pane when this tab is already split, a tab
    /// when this pane is alone in it. A pane goes beside this one, to the right if it's wide enough for two chats,
    /// below otherwise. The new shell is given `exec program chat`, so the pane or tab closes with the chat:
    /// `program` is this one (`std::env::current_exe()`), `chat` what names the chat on its command line.
    pub fn open_chat(&self, program: &Path, chat: &str, focus: bool, place: Option<Place>) {
        self.queue(Job::Open { program: program.to_path_buf(), chat: chat.to_string(), focus, place });
    }

    /// Whether this pane has Herdr's focus, which the terminal's focus events only tell once it changes; None when
    /// Herdr doesn't say. Blocks.
    pub fn pane_focused(&self) -> Option<bool> {
        let ids = &self.inner.ids;
        let answer = call(&ids.socket, "pane.get", json!({ "pane_id": ids.pane }))?;
        answer.get("pane")?.get("focused")?.as_bool()
    }

    /// Moves the focus to the next pane running wassup in this workspace, in the order Herdr shows them (tabs by
    /// number, then each tab's panes in its own order), from the last back to the first; false when there's no
    /// other. Panes are told apart by the agent wassup reports in them. Blocks.
    pub fn focus_next_chat(&self) -> bool {
        let ids = &self.inner.ids;
        let workspace = json!({ "workspace_id": ids.workspace });
        let tabs = call(&ids.socket, "tab.list", workspace.clone());
        let panes = call(&ids.socket, "pane.list", workspace);
        let Some((tab, pane)) = next_chat(tabs.as_ref(), panes.as_ref(), &ids.pane) else {
            return false;
        };
        call(&ids.socket, "tab.focus", json!({ "tab_id": tab }));
        call(&ids.socket, "pane.focus", json!({ "pane_id": pane }));
        true
    }

    /// Switches to the Herdr tab, and the pane in it, where a chat is already open.
    pub fn focus(&self, tab_id: &str, pane_id: Option<&str>) {
        self.queue(Job::Focus(tab_id.to_string(), pane_id.map(String::from)));
    }

    /// On exit: leaves the sidebar, clears the agent's name and gives the tab its label back; returns once that's
    /// sent (or after two seconds), so the process doesn't end before.
    pub fn release(&self) {
        let named = matches!(&self.last().tab, Some(Some(name)) if !name.is_empty());
        if named {
            self.tab_name(None);
        }
        let clear = !self.last().title.is_empty();
        self.queue(Job::Release { clear });
        self.wait(RELEASE_WAIT);
    }

    /// Waits until everything queued so far is sent, for `within` at most; whether it was.
    fn wait(&self, within: Duration) -> bool {
        let (tx, rx) = channel();
        self.queue(Job::Done(tx));
        rx.recv_timeout(within).is_ok()
    }

    fn queue(&self, job: Job) {
        if self.inner.jobs.send(job).is_err() {
            log::warn!("herdr: its thread is gone");
        }
    }

    fn last(&self) -> MutexGuard<'_, Last> {
        self.inner.last.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The tab's own label, as far as wassup knows it.
#[derive(Debug, PartialEq, Eq)]
enum TabLabel {
    /// Not asked yet.
    Unasked,
    /// Shared with other panes, or Herdr didn't say: the label isn't wassup's to change.
    Kept,
    /// This pane alone in it: the label to put back.
    Was(String),
}

/// The thread that sends the requests answering nothing, one after the other, and what only it needs.
struct Worker {
    ids: Ids,
    /// Herdr orders the requests from the same origin by this; from the time in milliseconds, so a later run in the
    /// same pane can't tie with an earlier one.
    seq: u64,
    tab_label: TabLabel,
    titles: Arc<AtomicU64>,
}

impl Worker {
    fn run(mut self, jobs: Receiver<Job>) {
        for job in jobs {
            match job {
                Job::Report(state, message) => {
                    self.pane("pane.report_agent", json!({ "state": state.as_str(), "message": message }));
                }
                Job::Title(title, n) => {
                    if n == self.titles.load(Ordering::SeqCst) {
                        self.pane("pane.report_metadata", json!({ "display_agent": title }));
                    }
                }
                Job::TabName(name) => self.tab_name(name),
                Job::Open { program, chat, focus, place } => open(&self.ids, &program, &chat, focus, place),
                Job::Focus(tab, pane) => {
                    call(&self.ids.socket, "tab.focus", json!({ "tab_id": tab }));
                    if let Some(pane) = pane {
                        call(&self.ids.socket, "pane.focus", json!({ "pane_id": pane }));
                    }
                }
                Job::Release { clear } => {
                    self.pane("pane.release_agent", json!({}));
                    if clear {
                        self.pane("pane.report_metadata", json!({ "clear_display_agent": true }));
                    }
                }
                Job::Done(tx) => {
                    let _ = tx.send(());
                }
            }
        }
    }

    /// A request about the agent's pane, with the origin and the next sequence number.
    fn pane(&mut self, method: &str, extra: Value) -> Option<Value> {
        self.seq += 1;
        call(&self.ids.socket, method, pane_params(&self.ids.pane, self.seq, extra))
    }

    /// The tab's label: the chat's name, or the tab's own back; the first time, Herdr is asked whether the pane is
    /// alone in its tab, and the label it had.
    fn tab_name(&mut self, name: Option<String>) {
        if self.tab_label == TabLabel::Unasked {
            let answer = call(&self.ids.socket, "tab.get", json!({ "tab_id": self.ids.tab }));
            self.tab_label = label_if_alone(answer.as_ref()).map_or(TabLabel::Kept, TabLabel::Was);
        }
        if let TabLabel::Was(original) = &self.tab_label {
            let label = name.as_deref().unwrap_or(original);
            call(&self.ids.socket, "tab.rename", json!({ "tab_id": self.ids.tab, "label": label }));
        }
    }
}

/// Opens a chat in a new pane or tab (see `Herdr::open_chat`).
fn open(ids: &Ids, program: &Path, chat: &str, focus: bool, place: Option<Place>) {
    let place = place.unwrap_or_else(|| place_for(call(&ids.socket, "tab.get", json!({ "tab_id": ids.tab })).as_ref()));
    let cwd = std::env::current_dir().ok().map(|d| d.to_string_lossy().into_owned());
    let pane = match place {
        Place::Pane => {
            let layout = call(&ids.socket, "pane.layout", json!({ "pane_id": ids.pane }));
            let direction = split_direction(layout.as_ref(), &ids.pane);
            let params = json!({ "target_pane_id": ids.pane, "direction": direction, "focus": focus, "cwd": cwd });
            call(&ids.socket, "pane.split", params).and_then(|a| Some(a.get("pane")?.get("pane_id")?.as_str()?.to_string()))
        }
        Place::Tab => {
            let params = json!({ "workspace_id": ids.workspace, "cwd": cwd, "focus": focus });
            call(&ids.socket, "tab.create", params)
                .and_then(|a| Some(a.get("root_pane")?.get("pane_id")?.as_str()?.to_string()))
        }
    };
    let Some(pane) = pane else {
        log::warn!("herdr: no {place:?} came to open {chat} in");
        return;
    };
    let text = exec_line(program, chat);
    call(&ids.socket, "pane.send_input", json!({ "pane_id": pane, "text": text, "keys": ["enter"] }));
}

/// Milliseconds since the epoch.
fn millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Request ids only need to be unique: the time and a count.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Sends a request and returns its answer's `result`; None if Herdr can't be reached, answers an error, or says
/// nothing within half a second.
fn call(socket: &Path, method: &str, params: Value) -> Option<Value> {
    let id = format!("wa:{}:{}", millis(), NEXT_ID.fetch_add(1, Ordering::Relaxed));
    log::info!("herdr: {method} {params}");
    match exchange(socket, &request_line(&id, method, &params)) {
        Ok(answer) if answer.trim().is_empty() => {
            log::warn!("herdr: {method}: no answer");
            None
        }
        Ok(answer) => {
            let result = result_of(&answer);
            if result.is_none() {
                log::warn!("herdr: {method}: {}", answer.trim_end());
            }
            result
        }
        Err(e) => {
            log::warn!("herdr: {method}: {e}");
            None
        }
    }
}

/// Sends one line and reads one back, each step given up after half a second.
fn exchange(socket: &Path, line: &str) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(WAIT))?;
    stream.set_write_timeout(Some(WAIT))?;
    stream.write_all(line.as_bytes())?;
    // As the hooks do: the request is all there is to send.
    stream.shutdown(Shutdown::Write)?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    Ok(answer)
}

/// A request as Herdr reads it: one line of JSON.
fn request_line(id: &str, method: &str, params: &Value) -> String {
    let mut line = json!({ "id": id, "method": method, "params": params }).to_string();
    line.push('\n');
    line
}

/// The `result` in Herdr's answer; None for an error, or for something that isn't JSON.
fn result_of(answer: &str) -> Option<Value> {
    let mut answer: Value = serde_json::from_str(answer.trim()).ok()?;
    answer.get_mut("result").map(Value::take)
}

/// The parameters of a request about the agent's pane: the pane, the origin, the agent and the sequence number,
/// then what's particular to the request.
fn pane_params(pane: &str, seq: u64, extra: Value) -> Value {
    let mut params = Map::new();
    params.insert("pane_id".into(), pane.into());
    params.insert("source".into(), AGENT.into());
    params.insert("agent".into(), AGENT.into());
    params.insert("seq".into(), seq.into());
    if let Value::Object(extra) = extra {
        params.extend(extra);
    }
    Value::Object(params)
}

/// From `tab.get`'s result, the tab's label when this pane is alone in it ("" for none); None when the tab is
/// split, or Herdr didn't say.
fn label_if_alone(answer: Option<&Value>) -> Option<String> {
    let tab = answer?.get("tab")?;
    let panes = tab.get("pane_count").and_then(Value::as_i64).unwrap_or(1);
    (panes <= 1).then(|| tab.get("label").and_then(Value::as_str).unwrap_or("").to_string())
}

/// From `tab.get`'s result, where a chat opens when the layout decides: a pane in a split tab, a tab otherwise.
fn place_for(answer: Option<&Value>) -> Place {
    let panes = answer.and_then(|a| a.get("tab")?.get("pane_count")?.as_i64()).unwrap_or(1);
    if panes > 1 { Place::Pane } else { Place::Tab }
}

/// From `pane.layout`'s result, which way `pane` splits: to the right when it's wide enough for two chats, down
/// otherwise (and when Herdr didn't say).
fn split_direction(answer: Option<&Value>, pane: &str) -> &'static str {
    let width = answer
        .and_then(|a| {
            let panes = a.get("layout")?.get("panes")?.as_array()?;
            let own = panes.iter().find(|p| p.get("pane_id").and_then(Value::as_str) == Some(pane))?;
            own.get("rect")?.get("width")?.as_i64()
        })
        .unwrap_or(0);
    if width >= WIDE { "right" } else { "down" }
}

/// From `tab.list`'s and `pane.list`'s results, the tab and the pane after `me` among those running wassup, in
/// Herdr's order (tabs by number, then each tab's panes as listed), the first after the last; None when `me`
/// isn't among them or is alone.
fn next_chat(tabs: Option<&Value>, panes: Option<&Value>, me: &str) -> Option<(String, String)> {
    let number: HashMap<&str, i64> = tabs
        .and_then(|t| t.get("tabs")?.as_array())
        .into_iter()
        .flatten()
        .filter_map(|t| Some((t.get("tab_id")?.as_str()?, t.get("number")?.as_i64()?)))
        .collect();
    let mut ring: Vec<(&str, &str)> = panes?
        .get("panes")?
        .as_array()?
        .iter()
        .filter(|p| p.get("agent").and_then(Value::as_str) == Some(AGENT))
        .filter_map(|p| Some((p.get("tab_id")?.as_str()?, p.get("pane_id")?.as_str()?)))
        .collect();
    // A stable sort: each tab's panes stay in Herdr's order.
    ring.sort_by_key(|(tab, _)| number.get(tab).copied().unwrap_or(0));
    let at = ring.iter().position(|&(_, pane)| pane == me)?;
    if ring.len() < 2 {
        return None;
    }
    let (tab, pane) = ring[(at + 1) % ring.len()];
    Some((tab.to_string(), pane.to_string()))
}

/// What the new pane's shell is given: the program on the chat, by `exec`, so the pane goes when the chat closes.
fn exec_line(program: &Path, chat: &str) -> String {
    format!("exec {} {}", quote(&program.to_string_lossy()), quote(chat))
}

/// `s` in single quotes for the shell, each of its own closing the quotes, escaped and opening them again.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// What `ps` says of a process: its parent, its terminal ("?" for none) and its command's name.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Proc {
    ppid: u32,
    tty: String,
    comm: String,
}

/// A process as `ps` sees it; None when there's no `ps`, or no such process.
fn ps(pid: u32) -> Option<Proc> {
    let out = Command::new("ps").args(["-o", "ppid=,tty=,comm=", "-p", &pid.to_string()]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_ps(&String::from_utf8_lossy(&out.stdout))
}

/// A line of `ps -o ppid=,tty=,comm=`; the command's name may have spaces.
fn parse_ps(line: &str) -> Option<Proc> {
    let mut words = line.split_whitespace();
    let ppid = words.next()?.parse().ok()?;
    let tty = words.next()?.to_string();
    let comm = words.collect::<Vec<_>>().join(" ");
    Some(Proc { ppid, tty, comm })
}

/// Whether the process `pid` runs in a Herdr pane itself, not in a terminal started from one: foot opened from a
/// shell in Herdr inherits its HERDR_* variables, and took the pictures for Herdr's half-blocks. The first ancestor
/// on another terminal is the program that made this one, Herdr in a pane and foot in foot. `info` is `ps`, or
/// what stands in for it; without it the variables decide, and they said yes.
fn in_herdr_pane(pid: u32, mut info: impl FnMut(u32) -> Option<Proc>) -> bool {
    let Some(mut p) = info(pid) else { return true };
    let own = p.tty.clone();
    for _ in 0..20 {
        if p.ppid <= 1 {
            break;
        }
        let Some(parent) = info(p.ppid) else { return true };
        if parent.tty != own {
            return parent.comm.to_lowercase().contains("herdr");
        }
        p = parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn proc(ppid: u32, tty: &str, comm: &str) -> Proc {
        Proc { ppid, tty: tty.into(), comm: comm.into() }
    }

    /// `ps` over a made-up process tree.
    fn tree(procs: &[(u32, Proc)]) -> impl FnMut(u32) -> Option<Proc> + '_ {
        move |pid| procs.iter().find(|(p, _)| *p == pid).map(|(_, p)| p.clone())
    }

    #[test]
    fn ps_lines() {
        assert_eq!(parse_ps("  4242 pts/3    herdr\n"), Some(proc(4242, "pts/3", "herdr")));
        assert_eq!(parse_ps("1 ? tmux: server"), Some(proc(1, "?", "tmux: server")));
        assert_eq!(parse_ps("77 pts/1"), Some(proc(77, "pts/1", "")));
        assert_eq!(parse_ps(""), None);
        assert_eq!(parse_ps("x pts/1 sh"), None);
    }

    /// wassup, its shell on the same terminal, then Herdr's server on none.
    #[test]
    fn in_a_pane() {
        let procs = [
            (300, proc(200, "pts/4", "wa")),
            (200, proc(100, "pts/4", "zsh")),
            (100, proc(1, "?", "herdr")),
        ];
        assert!(in_herdr_pane(300, tree(&procs)));
    }

    /// foot started from a shell in a Herdr pane: same variables, but the first ancestor on another terminal is foot.
    #[test]
    fn in_a_terminal_started_from_a_pane() {
        let procs = [
            (500, proc(400, "pts/9", "wa")),
            (400, proc(350, "pts/9", "bash")),
            (350, proc(200, "pts/4", "foot")),
            (200, proc(100, "pts/4", "zsh")),
            (100, proc(1, "?", "herdr")),
        ];
        assert!(!in_herdr_pane(500, tree(&procs)));
    }

    /// Up to init on the same terminal, nothing made it: not a pane.
    #[test]
    fn up_to_init() {
        let procs = [(30, proc(20, "pts/2", "wa")), (20, proc(1, "pts/2", "login"))];
        assert!(!in_herdr_pane(30, tree(&procs)));
    }

    /// With no answer from `ps`, the variables decide.
    #[test]
    fn without_ps() {
        assert!(in_herdr_pane(30, |_| None));
        let procs = [(30, proc(20, "pts/2", "wa"))];
        assert!(in_herdr_pane(30, tree(&procs)));
    }

    /// A chain longer than twenty on the same terminal is given up on.
    #[test]
    fn long_chains_stop() {
        let mut asked = 0;
        let found = in_herdr_pane(1000, |pid| {
            asked += 1;
            Some(proc(pid - 1, "pts/1", "sh"))
        });
        assert!(!found);
        assert_eq!(asked, 21);
    }

    #[test]
    fn request_is_one_line() {
        let line = request_line("wa:1:0", "tab.get", &json!({ "tab_id": "w1:t2" }));
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v, json!({ "id": "wa:1:0", "method": "tab.get", "params": { "tab_id": "w1:t2" } }));
    }

    #[test]
    fn results() {
        assert_eq!(result_of("{\"id\":\"x\",\"result\":{\"type\":\"ok\"}}\n"), Some(json!({ "type": "ok" })));
        assert_eq!(result_of("{\"id\":\"x\",\"error\":{\"code\":\"not_found\"}}"), None);
        assert_eq!(result_of("not json"), None);
    }

    #[test]
    fn pane_requests_carry_the_origin() {
        let p = pane_params("w1:p3", 41, json!({ "state": "working", "message": null }));
        assert_eq!(
            p,
            json!({ "pane_id": "w1:p3", "source": "wassup", "agent": "wassup", "seq": 41, "state": "working", "message": null })
        );
        assert_eq!(pane_params("w1:p3", 1, json!({})).as_object().unwrap().len(), 4);
    }

    #[test]
    fn tab_labels() {
        let alone = json!({ "tab": { "tab_id": "t1", "pane_count": 1, "label": "notas" } });
        let split = json!({ "tab": { "tab_id": "t1", "pane_count": 2, "label": "notas" } });
        assert_eq!(label_if_alone(Some(&alone)), Some("notas".into()));
        assert_eq!(label_if_alone(Some(&json!({ "tab": { "pane_count": 1 } }))), Some("".into()));
        assert_eq!(label_if_alone(Some(&split)), None);
        assert_eq!(label_if_alone(None), None);
        assert_eq!(place_for(Some(&alone)), Place::Tab);
        assert_eq!(place_for(Some(&split)), Place::Pane);
        assert_eq!(place_for(None), Place::Tab);
    }

    #[test]
    fn splits() {
        let layout = |w: i64| {
            json!({ "layout": { "panes": [
                { "pane_id": "p1", "rect": { "x": 0, "y": 0, "width": 40, "height": 30 } },
                { "pane_id": "p2", "rect": { "x": 40, "y": 0, "width": w, "height": 30 } },
            ] } })
        };
        assert_eq!(split_direction(Some(&layout(120)), "p2"), "right");
        assert_eq!(split_direction(Some(&layout(100)), "p2"), "right");
        assert_eq!(split_direction(Some(&layout(99)), "p2"), "down");
        assert_eq!(split_direction(Some(&layout(120)), "p9"), "down");
        assert_eq!(split_direction(None, "p2"), "down");
    }

    #[test]
    fn next_chats() {
        // Tab t2 is number 1 and t1 number 2; the shell pane in t2 isn't wassup.
        let tabs = json!({ "tabs": [{ "tab_id": "t1", "number": 2 }, { "tab_id": "t2", "number": 1 }] });
        let panes = json!({ "panes": [
            { "pane_id": "a", "tab_id": "t1", "agent": "wassup" },
            { "pane_id": "b", "tab_id": "t2", "agent": "wassup" },
            { "pane_id": "c", "tab_id": "t2", "agent": "claude" },
            { "pane_id": "d", "tab_id": "t2", "agent": "wassup" },
            { "pane_id": "e", "tab_id": "t1" },
        ] });
        let next = |me| next_chat(Some(&tabs), Some(&panes), me).map(|(t, p)| format!("{t}/{p}"));
        assert_eq!(next("b").as_deref(), Some("t2/d"));
        assert_eq!(next("d").as_deref(), Some("t1/a"));
        assert_eq!(next("a").as_deref(), Some("t2/b"));
        assert_eq!(next("c"), None);
        let alone = json!({ "panes": [{ "pane_id": "a", "tab_id": "t1", "agent": "wassup" }] });
        assert_eq!(next_chat(Some(&tabs), Some(&alone), "a"), None);
        assert_eq!(next_chat(Some(&tabs), None, "a"), None);
        // Without the tabs, Herdr's order of the panes.
        assert_eq!(next_chat(None, Some(&panes), "a").map(|(_, p)| p).as_deref(), Some("b"));
    }

    #[test]
    fn exec_lines() {
        assert_eq!(exec_line(Path::new("/opt/wa"), "Rita Lemos"), "exec '/opt/wa' 'Rita Lemos'");
        assert_eq!(exec_line(Path::new("/opt/wa rs"), "Rita d'Vilarelho"), r"exec '/opt/wa rs' 'Rita d'\''Vilarelho'");
    }

    /// A stand-in for Herdr on a socket in the crate's target folder: it notes each request and answers with
    /// `answer(method, params)`.
    struct Fake {
        path: PathBuf,
        seen: Arc<Mutex<Vec<Value>>>,
    }

    impl Fake {
        fn new(name: &str, answer: fn(&str, &Value) -> Option<Value>) -> Fake {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("herdr-{name}-{}.sock", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path).unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    let mut line = String::new();
                    if BufReader::new(&stream).read_line(&mut line).is_err() {
                        continue;
                    }
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let Some(result) = answer(request["method"].as_str().unwrap(), &request["params"]) else {
                        // Silence: the client gives up.
                        std::thread::sleep(WAIT * 2);
                        continue;
                    };
                    log.lock().unwrap().push(request.clone());
                    let _ = writeln!(stream, "{}", json!({ "id": request["id"], "result": result }));
                }
            });
            Fake { path, seen }
        }

        fn herdr(&self) -> Herdr {
            let ids = Ids { socket: self.path.clone(), pane: "w1:p2".into(), tab: "w1:t1".into(), workspace: Some("w1".into()) };
            Herdr::start(ids).unwrap()
        }

        /// The requests seen, as "method params" without the sequence numbers.
        fn seen(&self) -> Vec<String> {
            let seen = self.seen.lock().unwrap();
            seen.iter()
                .map(|r| {
                    let mut params = r["params"].clone();
                    params.as_object_mut().unwrap().remove("seq");
                    format!("{} {}", r["method"].as_str().unwrap(), params)
                })
                .collect()
        }

        fn seqs(&self) -> Vec<u64> {
            self.seen.lock().unwrap().iter().filter_map(|r| r["params"]["seq"].as_u64()).collect()
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn ok(_: &str, _: &Value) -> Option<Value> {
        Some(json!({ "type": "ok" }))
    }

    /// A tab of its own, labelled "notas", one pane wide enough to split to the right.
    fn alone(method: &str, _: &Value) -> Option<Value> {
        Some(match method {
            "tab.get" => json!({ "type": "tab_info", "tab": { "tab_id": "w1:t1", "pane_count": 1, "label": "notas" } }),
            "tab.create" => json!({ "type": "tab_created", "root_pane": { "pane_id": "w1:p7" } }),
            _ => json!({ "type": "ok" }),
        })
    }

    /// A split tab, this pane 120 columns wide.
    fn split(method: &str, _: &Value) -> Option<Value> {
        Some(match method {
            "tab.get" => json!({ "type": "tab_info", "tab": { "tab_id": "w1:t1", "pane_count": 2, "label": "notas" } }),
            "pane.layout" => json!({ "type": "pane_layout", "layout": { "panes": [
                { "pane_id": "w1:p2", "rect": { "x": 0, "y": 0, "width": 120, "height": 40 } },
            ] } }),
            "pane.split" => json!({ "type": "pane_info", "pane": { "pane_id": "w1:p8" } }),
            "pane.get" => json!({ "type": "pane_info", "pane": { "pane_id": "w1:p2", "focused": true } }),
            _ => json!({ "type": "ok" }),
        })
    }

    #[test]
    fn reports_in_order_once() {
        let fake = Fake::new("reports", ok);
        let herdr = fake.herdr();
        herdr.report(State::Idle, None);
        herdr.report(State::Idle, None);
        herdr.report(State::Working, Some("Rita a escrever…"));
        herdr.done(Some("Clube de Leitura (2)"));
        herdr.report(State::Idle, Some("Clube de Leitura (2)"));
        assert!(herdr.wait(Duration::from_secs(5)));
        let report = |state: &str, message: Value| {
            let params = json!({ "pane_id": "w1:p2", "source": "wassup", "agent": "wassup", "state": state, "message": message });
            format!("pane.report_agent {params}")
        };
        assert_eq!(
            fake.seen(),
            [
                report("idle", Value::Null),
                report("working", "Rita a escrever…".into()),
                report("working", "Clube de Leitura (2)".into()),
                report("idle", "Clube de Leitura (2)".into()),
            ]
        );
        let seqs = fake.seqs();
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    }

    #[test]
    fn title_and_tab_then_release() {
        let fake = Fake::new("release", alone);
        let herdr = fake.herdr();
        herdr.title("Rita Lemos");
        herdr.title("Rita Lemos");
        herdr.tab_name(Some("Rita"));
        herdr.tab_name(Some("Rita"));
        herdr.release();
        let seen = fake.seen();
        assert_eq!(seen.len(), 6, "{seen:#?}");
        assert!(seen[0].starts_with("pane.report_metadata ") && seen[0].contains(r#""display_agent":"Rita Lemos""#));
        assert_eq!(seen[1], r#"tab.get {"tab_id":"w1:t1"}"#);
        assert_eq!(seen[2], r#"tab.rename {"label":"Rita","tab_id":"w1:t1"}"#);
        assert_eq!(seen[3], r#"tab.rename {"label":"notas","tab_id":"w1:t1"}"#);
        assert!(seen[4].starts_with("pane.release_agent "));
        assert!(seen[5].starts_with("pane.report_metadata ") && seen[5].contains(r#""clear_display_agent":true"#));
    }

    /// In a split tab the label isn't touched, nor put back.
    #[test]
    fn split_tab_keeps_its_label() {
        let fake = Fake::new("keeps", split);
        let herdr = fake.herdr();
        herdr.tab_name(Some("Rita"));
        herdr.tab_name(Some("Clube"));
        herdr.release();
        let seen = fake.seen();
        assert_eq!(seen.iter().filter(|r| r.starts_with("tab.")).count(), 1, "{seen:#?}");
        // No title was given: nothing to clear.
        assert_eq!(seen.last().map(|r| r.starts_with("pane.release_agent ")), Some(true));
    }

    /// Titles that come while one is being sent: only the last of them follows.
    #[test]
    fn titles_coalesce() {
        fn slow(method: &str, params: &Value) -> Option<Value> {
            if params["display_agent"] == "um" {
                std::thread::sleep(Duration::from_millis(200));
            }
            ok(method, params)
        }
        let fake = Fake::new("titles", slow);
        let herdr = fake.herdr();
        herdr.title("um");
        std::thread::sleep(Duration::from_millis(50));
        herdr.title("dois");
        herdr.title("três");
        assert!(herdr.wait(Duration::from_secs(5)));
        let names: Vec<String> = fake.seen.lock().unwrap().iter().map(|r| r["params"]["display_agent"].to_string()).collect();
        assert_eq!(names, [r#""um""#, r#""três""#]);
    }

    #[test]
    fn opens_in_a_pane_to_the_right() {
        let fake = Fake::new("pane", split);
        let herdr = fake.herdr();
        herdr.open_chat(Path::new("/opt/wa"), "Rita Lemos", false, None);
        assert!(herdr.wait(Duration::from_secs(5)));
        let seen = fake.seen.lock().unwrap().clone();
        let methods: Vec<&str> = seen.iter().map(|r| r["method"].as_str().unwrap()).collect();
        assert_eq!(methods, ["tab.get", "pane.layout", "pane.split", "pane.send_input"]);
        assert_eq!(seen[2]["params"]["target_pane_id"], "w1:p2");
        assert_eq!(seen[2]["params"]["direction"], "right");
        assert_eq!(seen[2]["params"]["focus"], false);
        assert_eq!(
            seen[3]["params"],
            json!({ "pane_id": "w1:p8", "text": "exec '/opt/wa' 'Rita Lemos'", "keys": ["enter"] })
        );
    }

    #[test]
    fn opens_in_a_tab() {
        let fake = Fake::new("tab", alone);
        let herdr = fake.herdr();
        herdr.open_chat(Path::new("/opt/wa"), "Clube de Leitura", true, Some(Place::Tab));
        assert!(herdr.wait(Duration::from_secs(5)));
        let seen = fake.seen.lock().unwrap().clone();
        let methods: Vec<&str> = seen.iter().map(|r| r["method"].as_str().unwrap()).collect();
        assert_eq!(methods, ["tab.create", "pane.send_input"]);
        assert_eq!(seen[0]["params"]["workspace_id"], "w1");
        assert_eq!(seen[0]["params"]["focus"], true);
        assert_eq!(seen[1]["params"]["pane_id"], "w1:p7");
    }

    #[test]
    fn focus_and_questions() {
        let fake = Fake::new("focus", split);
        let herdr = fake.herdr();
        assert_eq!(herdr.pane_focused(), Some(true));
        herdr.focus("w1:t3", Some("w1:p9"));
        herdr.focus("w1:t4", None);
        assert!(herdr.wait(Duration::from_secs(5)));
        assert_eq!(
            fake.seen(),
            [
                r#"pane.get {"pane_id":"w1:p2"}"#,
                r#"tab.focus {"tab_id":"w1:t3"}"#,
                r#"pane.focus {"pane_id":"w1:p9"}"#,
                r#"tab.focus {"tab_id":"w1:t4"}"#,
            ]
        );
    }

    #[test]
    fn focus_next_goes_round() {
        fn listed(method: &str, _: &Value) -> Option<Value> {
            Some(match method {
                "tab.list" => json!({ "type": "tab_list", "tabs": [
                    { "tab_id": "w1:t1", "number": 1 }, { "tab_id": "w1:t5", "number": 2 },
                ] }),
                "pane.list" => json!({ "type": "pane_list", "panes": [
                    { "pane_id": "w1:p6", "tab_id": "w1:t5", "agent": "wassup" },
                    { "pane_id": "w1:p2", "tab_id": "w1:t1", "agent": "wassup" },
                ] }),
                _ => json!({ "type": "ok" }),
            })
        }
        let fake = Fake::new("next", listed);
        assert!(fake.herdr().focus_next_chat());
        let seen = fake.seen();
        assert_eq!(seen[0], r#"tab.list {"workspace_id":"w1"}"#);
        assert_eq!(seen[1], r#"pane.list {"workspace_id":"w1"}"#);
        assert_eq!(seen[2..], [r#"tab.focus {"tab_id":"w1:t5"}"#, r#"pane.focus {"pane_id":"w1:p6"}"#]);
    }

    /// Herdr silent, or not there: None, within the half second.
    #[test]
    fn silence_and_absence() {
        let fake = Fake::new("silent", |_, _| None);
        let started = std::time::Instant::now();
        assert_eq!(call(&fake.path, "pane.get", json!({ "pane_id": "w1:p2" })), None);
        assert!(started.elapsed() < WAIT * 2, "{:?}", started.elapsed());
        let gone = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/herdr-nobody.sock");
        assert_eq!(call(&gone, "pane.get", json!({})), None);
    }
}
