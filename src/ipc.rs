//! Several terminals, one connection, as the Node version's ipc.ts: the first wassup to start keeps the connection to
//! WhatsApp and listens on a socket; the others connect to it, send what they're asked to do and hear its events,
//! reading the chats from the shared database themselves (SQLite takes several readers). When the one with the
//! connection ends, the others elect a new one among themselves. One JSON line per message.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::i18n::s;
use crate::store::Store;
use crate::wa::{Backend, Conn, ToUi, Wa};

/// Whether a process still exists (signal 0 only checks).
pub fn alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 sends nothing; it only checks the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 || *libc::__errno_location() == libc::EPERM }
}

/// The socket, in the user's runtime folder, one per data folder, so different profiles don't meet.
fn sock_path(dir: &Path) -> PathBuf {
    let mut h = DefaultHasher::new();
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()).hash(&mut h);
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join(format!("wassup-{:08x}.sock", h.finish() as u32))
}

/// What a terminal asks of the one with the connection.
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "op", rename_all = "camelCase")]
enum Request {
    Send { chat: String, text: String, reply_to: Option<String>, mentions: Vec<String> },
    React { chat: String, id: String, emoji: String },
    Edit { chat: String, id: String, text: String },
    Delete { chat: String, id: String, for_everyone: bool },
    FetchOlder { chat: String },
    MarkRead { chat: String },
    EnsureMedia { chat: String, id: String },
    EnsurePreview { chat: String, id: String },
    EnsureProfile { jid: String },
    EnsureName { jid: String },
    SetComposing { chat: String, on: bool },
    SubscribePresence { chat: String },
    TouchPresence { pid: u32 },
    SetFocus { pid: u32, focused: bool },
    SendFile { chat: String, path: String, caption: String },
    DownloadAll { chat: String },
}

fn dispatch(b: &dyn Backend, r: Request) {
    match r {
        Request::Send { chat, text, reply_to, mentions } => b.send(&chat, &text, reply_to.as_deref(), &mentions),
        Request::React { chat, id, emoji } => b.react(&chat, &id, &emoji),
        Request::Edit { chat, id, text } => b.edit(&chat, &id, &text),
        Request::Delete { chat, id, for_everyone } => b.delete(&chat, &id, for_everyone),
        Request::FetchOlder { chat } => b.fetch_older(&chat),
        Request::MarkRead { chat } => b.mark_read(&chat),
        Request::EnsureMedia { chat, id } => b.ensure_media(&chat, &id),
        Request::EnsurePreview { chat, id } => b.ensure_preview(&chat, &id),
        Request::EnsureProfile { jid } => b.ensure_profile(&jid),
        Request::EnsureName { jid } => b.ensure_name(&jid),
        Request::SetComposing { chat, on } => b.set_composing(&chat, on),
        Request::SubscribePresence { chat } => b.subscribe_presence(&chat),
        Request::TouchPresence { pid } => b.touch_presence(pid),
        Request::SetFocus { pid, focused } => b.set_focus(pid, focused),
        Request::SendFile { chat, path, caption } => b.send_file(&chat, &path, &caption),
        Request::DownloadAll { chat } => b.download_all(&chat),
    }
}

/// Another process's connection, reached over the socket.
struct Remote {
    out: UnboundedSender<String>,
}

impl Remote {
    fn call(&self, r: Request) {
        if let Ok(line) = serde_json::to_string(&r) {
            let _ = self.out.send(line);
        }
    }
}

impl Backend for Remote {
    fn send(&self, chat: &str, text: &str, reply_to: Option<&str>, mentions: &[String]) {
        self.call(Request::Send { chat: chat.into(), text: text.into(), reply_to: reply_to.map(String::from), mentions: mentions.to_vec() });
    }
    fn react(&self, chat: &str, id: &str, emoji: &str) {
        self.call(Request::React { chat: chat.into(), id: id.into(), emoji: emoji.into() });
    }
    fn edit(&self, chat: &str, id: &str, text: &str) {
        self.call(Request::Edit { chat: chat.into(), id: id.into(), text: text.into() });
    }
    fn delete(&self, chat: &str, id: &str, for_everyone: bool) {
        self.call(Request::Delete { chat: chat.into(), id: id.into(), for_everyone });
    }
    fn fetch_older(&self, chat: &str) {
        self.call(Request::FetchOlder { chat: chat.into() });
    }
    fn mark_read(&self, chat: &str) {
        self.call(Request::MarkRead { chat: chat.into() });
    }
    fn ensure_media(&self, chat: &str, id: &str) {
        self.call(Request::EnsureMedia { chat: chat.into(), id: id.into() });
    }
    fn ensure_preview(&self, chat: &str, id: &str) {
        self.call(Request::EnsurePreview { chat: chat.into(), id: id.into() });
    }
    fn ensure_profile(&self, jid: &str) {
        self.call(Request::EnsureProfile { jid: jid.into() });
    }
    fn ensure_name(&self, jid: &str) {
        self.call(Request::EnsureName { jid: jid.into() });
    }
    fn set_composing(&self, chat: &str, on: bool) {
        self.call(Request::SetComposing { chat: chat.into(), on });
    }
    fn subscribe_presence(&self, chat: &str) {
        self.call(Request::SubscribePresence { chat: chat.into() });
    }
    fn touch_presence(&self, pid: u32) {
        self.call(Request::TouchPresence { pid });
    }
    fn set_focus(&self, pid: u32, focused: bool) {
        self.call(Request::SetFocus { pid, focused });
    }
    fn send_file(&self, chat: &str, path: &str, caption: &str) {
        self.call(Request::SendFile { chat: chat.into(), path: path.into(), caption: caption.into() });
    }
    fn download_all(&self, chat: &str) {
        self.call(Request::DownloadAll { chat: chat.into() });
    }
}

/// What the screen talks to: our own connection, or another process's; when that one goes, a new election swaps
/// what's behind without the screen knowing.
pub struct Proxy {
    inner: Mutex<Option<Arc<dyn Backend>>>,
    own: Mutex<Option<Wa>>,
    store: Arc<Store>,
    dir: PathBuf,
    ui: UnboundedSender<ToUi>,
    /// Asks for a new election, when the process with the connection is gone.
    again: UnboundedSender<()>,
}

impl Proxy {
    fn get(&self) -> Option<Arc<dyn Backend>> {
        self.inner.lock().unwrap().clone()
    }

    /// The connection closed, if it's ours, its state written.
    pub async fn stop(&self) {
        let own = self.own.lock().unwrap().take();
        // Only the process with the connection lets the socket go: the others' would leave it nowhere to be found.
        if let Some(wa) = own {
            let _ = std::fs::remove_file(sock_path(&self.dir));
            wa.stop().await;
        }
    }
}

macro_rules! forward {
    ($self:ident, $m:ident $(, $a:expr)*) => {
        match $self.get() {
            // What this terminal asked fails to its own screen, not to every terminal's.
            Some(b) => crate::wa::asking($self.ui.clone(), || b.$m($($a),*)),
            None => {
                let _ = $self.ui.send(ToUi::Error(s("notConnectedYet")));
            }
        }
    };
}

impl Backend for Proxy {
    fn send(&self, chat: &str, text: &str, reply_to: Option<&str>, mentions: &[String]) {
        forward!(self, send, chat, text, reply_to, mentions)
    }
    fn react(&self, chat: &str, id: &str, emoji: &str) {
        forward!(self, react, chat, id, emoji)
    }
    fn edit(&self, chat: &str, id: &str, text: &str) {
        forward!(self, edit, chat, id, text)
    }
    fn delete(&self, chat: &str, id: &str, for_everyone: bool) {
        forward!(self, delete, chat, id, for_everyone)
    }
    fn fetch_older(&self, chat: &str) {
        forward!(self, fetch_older, chat)
    }
    fn mark_read(&self, chat: &str) {
        if let Some(b) = self.get() {
            crate::wa::asking(self.ui.clone(), || b.mark_read(chat))
        }
    }
    fn ensure_media(&self, chat: &str, id: &str) {
        if let Some(b) = self.get() {
            crate::wa::asking(self.ui.clone(), || b.ensure_media(chat, id))
        }
    }
    fn ensure_preview(&self, chat: &str, id: &str) {
        if let Some(b) = self.get() {
            crate::wa::asking(self.ui.clone(), || b.ensure_preview(chat, id))
        }
    }
    fn ensure_profile(&self, jid: &str) {
        if let Some(b) = self.get() {
            crate::wa::asking(self.ui.clone(), || b.ensure_profile(jid))
        }
    }
    fn ensure_name(&self, jid: &str) {
        if let Some(b) = self.get() {
            crate::wa::asking(self.ui.clone(), || b.ensure_name(jid))
        }
    }
    fn set_composing(&self, chat: &str, on: bool) {
        if let Some(b) = self.get() {
            crate::wa::asking(self.ui.clone(), || b.set_composing(chat, on))
        }
    }
    fn subscribe_presence(&self, chat: &str) {
        if let Some(b) = self.get() {
            crate::wa::asking(self.ui.clone(), || b.subscribe_presence(chat))
        }
    }
    fn touch_presence(&self, pid: u32) {
        if let Some(b) = self.get() {
            crate::wa::asking(self.ui.clone(), || b.touch_presence(pid))
        }
    }
    fn set_focus(&self, pid: u32, focused: bool) {
        if let Some(b) = self.get() {
            crate::wa::asking(self.ui.clone(), || b.set_focus(pid, focused))
        }
    }
    fn send_file(&self, chat: &str, path: &str, caption: &str) {
        forward!(self, send_file, chat, path, caption)
    }
    fn download_all(&self, chat: &str) {
        forward!(self, download_all, chat)
    }
}

/// The connection for this process's screen: another terminal's if there is one, else its own, shared.
pub async fn connect(store: Arc<Store>, dir: &Path, ui: UnboundedSender<ToUi>) -> Result<Arc<Proxy>> {
    let (again, mut again_rx) = unbounded_channel::<()>();
    let proxy = Arc::new(Proxy { inner: Mutex::new(None), own: Mutex::new(None), store, dir: dir.to_path_buf(), ui, again });
    elect(proxy.clone()).await?;
    let p = proxy.clone();
    tokio::spawn(async move {
        while again_rx.recv().await.is_some() {
            if let Err(e) = elect(p.clone()).await {
                warn!("ipc: election: {e}");
            }
        }
    });
    Ok(proxy)
}

/// Connects to whatever process has the connection; with none, this one takes it and opens the socket.
async fn elect(proxy: Arc<Proxy>) -> Result<()> {
    let path = sock_path(&proxy.dir);
    for _ in 0..5 {
        match UnixStream::connect(&path).await {
            Ok(stream) => {
                info!("ipc: client of {}", path.display());
                client(proxy.clone(), stream);
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                let _ = std::fs::remove_file(&path);
            }
            Err(_) => {}
        }
        match UnixListener::bind(&path) {
            Ok(listener) => {
                info!("ipc: server on {}", path.display());
                return serve(proxy, listener).await;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(anyhow!(s("noServer")))
}

/// This process keeps the connection: its events go to its own screen and to every terminal connected.
async fn serve(proxy: Arc<Proxy>, listener: UnixListener) -> Result<()> {
    let (hub_tx, mut hub_rx) = unbounded_channel::<ToUi>();
    let (bcast, _) = broadcast::channel::<ToUi>(4096);
    // The last state of the connection and the QR, for a terminal that comes later.
    let last: Arc<Mutex<(Option<ToUi>, Option<ToUi>)>> = Arc::new(Mutex::new((None, None)));
    let (ui, b2, last2) = (proxy.ui.clone(), bcast.clone(), last.clone());
    tokio::spawn(async move {
        while let Some(ev) = hub_rx.recv().await {
            match &ev {
                ToUi::Connection(c, _) => {
                    let mut l = last2.lock().unwrap();
                    l.0 = Some(ev.clone());
                    if *c != Conn::Qr {
                        l.1 = None;
                    }
                }
                ToUi::Qr(_) => last2.lock().unwrap().1 = Some(ev.clone()),
                _ => {}
            }
            let _ = ui.send(ev.clone());
            let _ = b2.send(ev);
        }
    });
    let wa = Wa::start(proxy.store.clone(), &proxy.dir, hub_tx).await?;
    *proxy.own.lock().unwrap() = Some(wa.clone());
    *proxy.inner.lock().unwrap() = Some(Arc::new(wa.clone()));
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            let (wa, mut events, last) = (wa.clone(), bcast.subscribe(), last.clone());
            tokio::spawn(async move {
                info!("ipc: terminal connected");
                let (read, mut write) = stream.into_split();
                // What the requests of this terminal fail with goes only to it.
                let (own_tx, mut own_rx) = unbounded_channel::<ToUi>();
                let hello: Vec<ToUi> = {
                    let l = last.lock().unwrap();
                    [l.0.clone(), l.1.clone()].into_iter().flatten().collect()
                };
                let writer = tokio::spawn(async move {
                    for ev in hello {
                        if let Ok(line) = serde_json::to_string(&ev)
                            && write.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                                return;
                            }
                    }
                    loop {
                        let ev = tokio::select! {
                            r = events.recv() => match r {
                                Ok(ev) => ev,
                                Err(broadcast::error::RecvError::Lagged(_)) => ToUi::Chats,
                                Err(_) => break,
                            },
                            Some(ev) = own_rx.recv() => ev,
                        };
                        let Ok(line) = serde_json::to_string(&ev) else { continue };
                        if write.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                            break;
                        }
                    }
                });
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    match serde_json::from_str::<Request>(&line) {
                        Ok(r) => crate::wa::asking(own_tx.clone(), || dispatch(&wa, r)),
                        Err(e) => warn!("ipc: bad request: {e}"),
                    }
                }
                writer.abort();
                info!("ipc: terminal gone");
            });
        }
    });
    Ok(())
}

/// This process is a terminal of another's connection: its requests go out, the events come in; when the other
/// goes, a new election.
fn client(proxy: Arc<Proxy>, stream: UnixStream) {
    let (read, mut write) = stream.into_split();
    let (out_tx, mut out_rx) = unbounded_channel::<String>();
    *proxy.inner.lock().unwrap() = Some(Arc::new(Remote { out: out_tx }));
    tokio::spawn(async move {
        while let Some(line) = out_rx.recv().await {
            if write.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let mut lines = BufReader::new(read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match serde_json::from_str::<ToUi>(&line) {
                Ok(ev) => {
                    let _ = proxy.ui.send(ev);
                }
                Err(e) => warn!("ipc: bad event: {e}"),
            }
        }
        warn!("ipc: the connection's process is gone, new election");
        *proxy.inner.lock().unwrap() = None;
        let _ = proxy.ui.send(ToUi::Status(s("serverGone")));
        // A moment's difference between the terminals left, so one takes it and the others connect to it.
        let wait = 50 + (std::process::id() % 400) as u64;
        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
        let _ = proxy.again.send(());
    });
}
