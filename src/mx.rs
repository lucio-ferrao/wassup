//! Matrix beside WhatsApp: the rooms of one Matrix account come in as chats of the same store, read and answered
//! from the same screen. The process with the WhatsApp connection keeps this one too (wa.rs hands it the chats that
//! are Matrix rooms); `wa matrix login` makes the session once, with the recovery key, so this device can read what
//! is encrypted. The session and matrix-sdk's own stores (keys, rooms) are in `matrix/` of the data folder.
//!
//! A room is a chat by its id and a suffix that says what it is, as WhatsApp's jids do: "@mx" for a direct chat,
//! "@mxg" for any other room, which the screen draws as a group.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use log::{info, warn};
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::encryption::{BackupDownloadStrategy, EncryptionSettings};
use matrix_sdk::room::MessagesOptions;
use matrix_sdk::ruma::api::client::receipt::create_receipt::v3::ReceiptType;
use matrix_sdk::ruma::events::reaction::{OriginalSyncReactionEvent, ReactionEventContent};
use matrix_sdk::ruma::events::relation::{Annotation, InReplyTo, Reply};
use matrix_sdk::ruma::events::receipt::{ReceiptEventContent, ReceiptThread, ReceiptType as Read};
use matrix_sdk::ruma::events::typing::TypingEventContent;
use matrix_sdk::ruma::events::room::redaction::OriginalSyncRoomRedactionEvent;
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent, Relation, ReplacementMetadata, RoomMessageEventContent};
use matrix_sdk::ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent, SyncEphemeralRoomEvent, SyncMessageLikeEvent};
use matrix_sdk::ruma::{EventId, OwnedUserId, RoomId, UserId};
use matrix_sdk::store::RoomLoadSettings;
use matrix_sdk::attachment::AttachmentConfig;
use matrix_sdk::media::{MediaFormat, MediaRequestParameters};
use matrix_sdk::ruma::events::room::MediaSource;
use matrix_sdk::ruma::events::room::message::TextMessageEventContent;
use matrix_sdk::{Client, Room, RoomMemberships};
use tokio::sync::mpsc::UnboundedSender;

use crate::i18n::{s, t};
use crate::store::{self, Msg, Store};
use crate::wa::ToUi;

const DIRECT: &str = "@mx";
const GROUP: &str = "@mxg";

/// Whether a chat is a Matrix room.
pub fn is_matrix(chat: &str) -> bool {
    chat.ends_with(DIRECT) || chat.ends_with(GROUP)
}

/// Whether a chat or someone is Matrix's: a room, or a user ("@name:server", which no WhatsApp jid starts as).
pub fn is_matrix_id(id: &str) -> bool {
    is_matrix(id) || id.starts_with('@')
}

/// Whether a chat is a Matrix room other than a direct chat.
pub fn is_group(chat: &str) -> bool {
    chat.ends_with(GROUP)
}

fn chat_of(room: &RoomId, direct: bool) -> String {
    format!("{room}{}", if direct { DIRECT } else { GROUP })
}

fn room_of(chat: &str) -> Option<&RoomId> {
    let id = chat.strip_suffix(GROUP).or_else(|| chat.strip_suffix(DIRECT))?;
    <&RoomId>::try_from(id).ok()
}

fn session_path(dir: &Path) -> PathBuf {
    dir.join("matrix").join("session.json")
}

/// The client for the session's homeserver, its stores in the data folder. Keys missing to read a message are
/// asked of the backup as it fails, and this device signs itself with the account's keys once recovered.
async fn client(dir: &Path, user: &UserId) -> Result<Client> {
    // reqwest is built with no TLS provider of its own: the one the rest uses (ring), once for the process.
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let settings = EncryptionSettings {
        auto_enable_cross_signing: true,
        backup_download_strategy: BackupDownloadStrategy::AfterDecryptionFailure,
        auto_enable_backups: true,
    };
    Client::builder()
        .server_name(user.server_name())
        .sqlite_store(dir.join("matrix").join("store"), None)
        .with_encryption_settings(settings)
        .build()
        .await
        .context("matrix client")
}

fn ask(prompt: &str, hidden: bool) -> Result<String> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    // SAFETY: tcgetattr/tcsetattr on standard input with a termios read just before; echo is put back after.
    let saved = hidden.then(|| unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        (libc::tcgetattr(0, &mut t) == 0).then(|| {
            let mut quiet = t;
            quiet.c_lflag &= !libc::ECHO;
            libc::tcsetattr(0, libc::TCSANOW, &quiet);
            t
        })
    });
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    if let Some(Some(t)) = saved {
        // SAFETY: as above, the settings read before.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &t) };
        println!();
    }
    read?;
    Ok(line.trim().to_string())
}

/// `wa matrix login`: the account and its password, then the recovery key (Enter for none), asked in the terminal.
/// The process with the connection takes the session when it next starts.
pub async fn login(dir: &Path) -> Result<()> {
    let path = session_path(dir);
    if path.exists() {
        return Err(anyhow!(t("mxHasSession", &[&path.display()])));
    }
    std::fs::create_dir_all(path.parent().unwrap_or(dir))?;
    let user: OwnedUserId = ask(&s("mxUser"), false)?.parse().map_err(|e| anyhow!("{e}"))?;
    let password = ask(&s("mxPassword"), true)?;
    let client = client(dir, &user).await?;
    client.matrix_auth().login_username(&user, &password).initial_device_display_name("wassup").send().await.context("login")?;
    let session = client.matrix_auth().session().context("no session after login")?;
    // Only mine to read, as all of the data folder (setup's umask).
    std::fs::write(&path, serde_json::to_vec(&session)?)?;
    // The recovery needs what the account keeps on the server, which the first sync brings.
    client.sync_once(SyncSettings::default()).await.context("first sync")?;
    let key = ask(&s("mxRecovery"), true)?;
    if !key.is_empty() {
        client.encryption().recovery().recover(&key).await.context("recovery")?;
    }
    println!("{}", t("mxDone", &[&user]));
    Ok(())
}

/// The Matrix connection, alive while the process with the WhatsApp one is.
#[derive(Clone)]
pub struct Mx {
    inner: Arc<Inner>,
}

struct Inner {
    client: Client,
    me: OwnedUserId,
    store: Arc<Store>,
    tx: UnboundedSender<ToUi>,
    /// Where attachments go (wa.rs's media folder), and those being downloaded, each once.
    media: PathBuf,
    /// Voice messages to read, once downloaded (wa.rs's voice_loop).
    voices: UnboundedSender<(String, String)>,
    downloading: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl Mx {
    /// Restores the session made by `wa matrix login`, and syncs from then on; None with no session.
    pub async fn start(dir: &Path, store: Arc<Store>, tx: UnboundedSender<ToUi>, voices: UnboundedSender<(String, String)>) -> Result<Option<Self>> {
        let path = session_path(dir);
        if !path.exists() {
            return Ok(None);
        }
        let session: MatrixSession = serde_json::from_slice(&std::fs::read(&path)?)?;
        let me = session.meta.user_id.clone();
        let client = client(dir, &me).await?;
        client.matrix_auth().restore_session(session, RoomLoadSettings::default()).await.context("matrix session")?;
        let inner = Inner { client: client.clone(), me, store, tx, media: dir.join("media"), voices, downloading: Default::default() };
        let mx = Self { inner: Arc::new(inner) };
        let on = mx.clone();
        client.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, room: Room| {
            let mx = on.clone();
            async move { mx.on_message(&room, &ev).await }
        });
        let on = mx.clone();
        client.add_event_handler(move |ev: OriginalSyncReactionEvent, room: Room| {
            let mx = on.clone();
            async move { mx.on_reaction(&room, &ev, false).await }
        });
        let on = mx.clone();
        client.add_event_handler(move |ev: OriginalSyncRoomRedactionEvent, room: Room| {
            let mx = on.clone();
            async move { mx.on_redaction(&room, &ev).await }
        });
        let on = mx.clone();
        client.add_event_handler(move |ev: SyncEphemeralRoomEvent<TypingEventContent>, room: Room| {
            let mx = on.clone();
            async move { mx.on_typing(&room, &ev.content).await }
        });
        let on = mx.clone();
        client.add_event_handler(move |ev: SyncEphemeralRoomEvent<ReceiptEventContent>, room: Room| {
            let mx = on.clone();
            async move { mx.on_receipt(&room, &ev.content).await }
        });
        tokio::spawn(mx.clone().catch_up());
        tokio::spawn(async move {
            loop {
                if let Err(e) = client.sync(SyncSettings::default()).await {
                    warn!("matrix sync: {e}");
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
        info!("matrix: {}", mx.inner.me);
        Ok(Some(mx))
    }

    fn emit(&self, ev: ToUi) {
        let _ = self.inner.tx.send(ev);
    }

    fn room(&self, chat: &str) -> Option<Room> {
        self.inner.client.get_room(room_of(chat)?)
    }

    /// The rooms this store has no message of yet, their latest messages: the sync only brings what's new since the
    /// last one, and the first was `wa matrix login`'s.
    async fn catch_up(self) {
        for room in self.inner.client.joined_rooms() {
            let chat = chat_of(room.room_id(), room.is_direct().await.unwrap_or(false));
            if self.inner.store.count(&chat) != 0 {
                self.read_so_far(&room, &chat).await;
                continue;
            }
            // From the latest again: a try before whose messages couldn't all be read (keys not yet had from the
            // backup) moved on and kept nothing.
            self.inner.store.delete_state(&format!("mxBefore:{chat}"));
            if let Err(e) = self.older(&room, &chat).await {
                warn!("matrix catching up: {e}");
            }
            self.read_so_far(&room, &chat).await;
        }
        self.emit(ToUi::Chats);
    }

    /// How far the other side of a direct chat has read, as the server last said, for mine kept before now.
    async fn read_so_far(&self, room: &Room, chat: &str) {
        if is_group(chat) {
            return;
        }
        let Ok(members) = room.members(RoomMemberships::JOIN).await else { return };
        for m in members.iter().filter(|m| m.user_id() != self.inner.me) {
            for thread in [ReceiptThread::Unthreaded, ReceiptThread::Main] {
                if let Ok(Some((event, _))) = room.load_user_receipt(Read::Read, &thread, m.user_id()).await {
                    self.read_up_to(chat, event.as_str());
                }
            }
        }
    }

    /// The other side of a direct chat read up to a message: mine up to it are read. Groups keep mine as sent:
    /// read there would need everyone's, as WhatsApp's.
    fn read_up_to(&self, chat: &str, event: &str) {
        if is_group(chat) {
            return;
        }
        let Some(m) = self.inner.store.message(chat, event) else { return };
        if self.inner.store.batch(|db| Store::mark_read_before(db, chat, m.ts)).is_ok() {
            self.emit(ToUi::Messages(chat.to_string()));
        }
    }

    async fn on_receipt(&self, room: &Room, content: &ReceiptEventContent) {
        let chat = chat_of(room.room_id(), room.is_direct().await.unwrap_or(false));
        for (event, receipts) in &content.0 {
            let by_others = receipts.get(&Read::Read).is_some_and(|users| users.keys().any(|u| *u != self.inner.me));
            if by_others {
                self.read_up_to(&chat, event.as_str());
            }
        }
    }

    /// Who is typing in a room, as the server says it whole each time (and stops it by itself).
    async fn on_typing(&self, room: &Room, content: &TypingEventContent) {
        let chat = chat_of(room.room_id(), room.is_direct().await.unwrap_or(false));
        let who: Vec<String> = content.user_ids.iter().filter(|u| **u != self.inner.me).map(|u| u.to_string()).collect();
        self.emit(ToUi::Typing(chat, who));
    }

    /// Tells a room I'm typing, or stopped.
    pub fn set_typing(&self, chat: &str, on: bool) {
        let Some(room) = self.room(chat) else { return };
        tokio::spawn(async move {
            if let Err(e) = room.typing_notice(on).await {
                warn!("matrix typing: {e}");
            }
        });
    }

    /// A message of a room as the store keeps it; an edit changes the one it replaces. Returns the chat, and the
    /// message's id when it's new and someone else's; `history` for one fetched from the past, which is no news.
    async fn keep(&self, room: &Room, ev: &OriginalSyncRoomMessageEvent, history: bool) -> Option<(String, Option<String>)> {
        let direct = room.is_direct().await.unwrap_or(false);
        let chat = chat_of(room.room_id(), direct);
        let name = room.display_name().await.map(|n| n.to_string()).unwrap_or_default();
        let from_me = ev.sender == self.inner.me;
        let sender = ev.sender.to_string();
        let sender_name = match room.get_member_no_sync(&ev.sender).await {
            Ok(Some(m)) => m.name().to_string(),
            _ => ev.sender.localpart().to_string(),
        };
        let ts = i64::from(ev.origin_server_ts.as_secs());
        // The message it answers, as the screen shows it: who (none for mine) and its first line. Only if kept.
        let replied = match &ev.content.relates_to {
            Some(Relation::Reply(r)) => Some(&r.in_reply_to.event_id),
            Some(Relation::Thread(t)) if !t.is_falling_back => t.in_reply_to.as_ref().map(|r| &r.event_id),
            _ => None,
        };
        let quoted = replied.and_then(|id| self.inner.store.message(&chat, id.as_str())).map(|q| {
            let who = if q.from_me { String::new() } else { q.sender };
            format!("{who}\t{}", q.text.lines().next().unwrap_or(""))
        });
        let r = self.inner.store.batch(|db| {
            Store::touch_chat(db, &chat, None, 0)?;
            if !name.is_empty() {
                Store::set_chat_name(db, &chat, &name)?;
            }
            if !from_me {
                Store::set_name(db, &sender, &sender_name, store::NAME_CONTACT)?;
            }
            if let Some(Relation::Replacement(r)) = &ev.content.relates_to {
                let new = shown(&r.new_content.msgtype);
                Store::edit(db, &chat, r.event_id.as_str(), new.kind, &new.text)?;
                return Ok(None);
            }
            let Shown { kind, text, media, mime, name: file, w, h } = shown(&ev.content.msgtype);
            let text = if replied.is_some() { without_fallback(&text).to_string() } else { text };
            let m = Msg {
                chat: chat.clone(),
                id: ev.event_id.to_string(),
                from_me,
                sender: sender.clone(),
                push_name: sender_name.clone(),
                ts,
                kind: kind.into(),
                text,
                // Mine in the room is as far as Matrix goes before it's read: delivered, two dots in its time.
                status: if from_me { 3 } else { 0 },
                quoted: quoted.clone(),
                media,
                media_mime: mime,
                media_name: file,
                w,
                h,
                ..Default::default()
            };
            let added = Store::add_message(db, &m)? && !from_me && !history;
            if added {
                Store::bump_unread(db, &chat)?;
            }
            Ok(Some(added.then(|| m.id.clone())))
        });
        match r {
            Ok(new) => Some((chat, new.flatten())),
            Err(e) => {
                warn!("matrix message: {e}");
                None
            }
        }
    }

    /// A reaction to a message: one per person and message, as the store keeps them (the latest wins). Which
    /// message and whose it was is kept by the reaction's id, for when it's taken away.
    async fn on_reaction(&self, room: &Room, ev: &OriginalSyncReactionEvent, history: bool) {
        let chat = chat_of(room.room_id(), room.is_direct().await.unwrap_or(false));
        let target = ev.content.relates_to.event_id.to_string();
        let who = if ev.sender == self.inner.me { "me".to_string() } else { ev.sender.to_string() };
        let emoji = ev.content.relates_to.key.clone();
        let ts = i64::from(ev.origin_server_ts.get());
        let kept = self.inner.store.batch(|db| Store::set_reaction(db, &chat, &target, &who, &emoji, ts));
        let _ = self.inner.store.set_state(&format!("mxReaction:{}", ev.event_id), &format!("{chat}\t{target}\t{who}\t{ts}"));
        if kept.is_ok() {
            self.emit(ToUi::Messages(chat.clone()));
            if !history {
                self.emit(ToUi::Reaction(chat, target, who, emoji));
            }
        }
    }

    /// Something taken away: a message kept shows as deleted, as WhatsApp's; a reaction kept is removed.
    async fn on_redaction(&self, room: &Room, ev: &OriginalSyncRoomRedactionEvent) {
        let Some(gone) = ev.redacts.as_ref().or(ev.content.redacts.as_ref()) else { return };
        let chat = chat_of(room.room_id(), room.is_direct().await.unwrap_or(false));
        if self.inner.store.message(&chat, gone.as_str()).is_some() {
            if self.inner.store.batch(|db| Store::revoke(db, &chat, gone.as_str())).is_ok() {
                self.emit(ToUi::Messages(chat));
                self.emit(ToUi::Chats);
            }
            return;
        }
        let key = format!("mxReaction:{gone}");
        let Some(kept) = self.inner.store.state(&key) else { return };
        let mut parts = kept.split('\t');
        let (Some(chat), Some(target), Some(who), Some(ts)) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return };
        let ts: i64 = ts.parse().unwrap_or(0);
        if self.inner.store.batch(|db| Store::set_reaction(db, chat, target, who, "", ts)).is_ok() {
            self.inner.store.delete_state(&key);
            self.emit(ToUi::Messages(chat.to_string()));
            self.emit(ToUi::Reaction(chat.to_string(), target.to_string(), who.to_string(), String::new()));
        }
    }

    async fn on_message(&self, room: &Room, ev: &OriginalSyncRoomMessageEvent) {
        let Some((chat, new)) = self.keep(room, ev, false).await else { return };
        self.emit(ToUi::Messages(chat.clone()));
        if let Some(id) = new {
            // A voice message that just came is downloaded at once, to be read.
            if self.inner.store.message(&chat, &id).is_some_and(|m| m.kind == "voice") {
                self.ensure_media(&chat, &id);
            }
            self.emit(ToUi::Notify(chat, id));
        }
        self.emit(ToUi::Chats);
    }

    /// Sends text, answering `reply_to` (a message of the room) when given.
    pub fn send(&self, chat: &str, text: &str, reply_to: Option<&str>) {
        let Some(room) = self.room(chat) else { return self.refuse("mxNoRoom") };
        let mut content = RoomMessageEventContent::text_plain(text);
        if let Some(id) = reply_to.and_then(|id| EventId::parse(id).ok()) {
            content.relates_to = Some(Relation::Reply(Reply::new(InReplyTo::new(id))));
        }
        let tx = self.inner.tx.clone();
        // What's sent comes back in the sync, and is kept then.
        tokio::spawn(async move {
            if let Err(e) = room.send(content).await {
                warn!("matrix send: {e}");
                let _ = tx.send(ToUi::Error(format!("{}: {e}", s("error"))));
            }
        });
    }

    /// Reacts to a message; empty takes mine away. Mine is one per message, as WhatsApp's: a new one takes the
    /// one before away. Both come back in the sync, and are kept then.
    pub fn react(&self, chat: &str, id: &str, emoji: &str) {
        let Some(room) = self.room(chat) else { return self.refuse("mxNoRoom") };
        let Ok(target) = EventId::parse(id) else { return self.refuse("unknownMessage") };
        // My reaction to it so far, by the id it was kept under.
        let mine = self.inner.store.states("mxReaction:").into_iter().find_map(|(key, value)| {
            let mut parts = value.split('\t');
            (parts.next() == Some(chat) && parts.next() == Some(id) && parts.next() == Some("me")).then(|| key["mxReaction:".len()..].to_string())
        });
        let (emoji, tx) = (emoji.to_string(), self.inner.tx.clone());
        tokio::spawn(async move {
            if let Some(old) = mine.and_then(|m| EventId::parse(m).ok())
                && let Err(e) = room.redact(&old, None, None).await {
                    warn!("matrix unreact: {e}");
                }
            if emoji.is_empty() {
                return;
            }
            if let Err(e) = room.send(ReactionEventContent::new(Annotation::new(target, emoji))).await {
                warn!("matrix react: {e}");
                let _ = tx.send(ToUi::Error(format!("{}: {e}", s("error"))));
            }
        });
    }

    /// Downloads a message's attachment (decrypting it, in an encrypted room) into the media folder; marked as not
    /// to be had when it can't.
    pub fn ensure_media(&self, chat: &str, id: &str) {
        let key = format!("{chat}/{id}");
        if !self.inner.downloading.lock().unwrap().insert(key.clone()) {
            return;
        }
        let (mx, chat, id) = (self.clone(), chat.to_string(), id.to_string());
        tokio::spawn(async move {
            let r: Result<()> = async {
                let m = mx.inner.store.message(&chat, &id).context("no message")?;
                if m.media_path.is_some() {
                    return Ok(());
                }
                let source: MediaSource = serde_json::from_slice(m.media.as_deref().context("no attachment")?)?;
                let request = MediaRequestParameters { source, format: MediaFormat::File };
                let bytes = mx.inner.client.media().get_media_content(&request, true).await?;
                let dir = crate::wa::media_dir(&mx.inner.media, &chat);
                std::fs::create_dir_all(&dir)?;
                let file = dir.join(format!("{}.{}", crate::wa::safe(&id), crate::wa::ext_for(m.media_mime.as_deref(), m.media_name.as_deref())));
                std::fs::write(&file, bytes)?;
                mx.inner.store.set_media(&chat, &id, &file.to_string_lossy(), m.w, m.h)?;
                if m.kind == "voice" {
                    let _ = mx.inner.voices.send((chat.clone(), id.clone()));
                }
                Ok(())
            }
            .await;
            if let Err(e) = r {
                warn!("matrix download {id}: {e}");
                let _ = mx.inner.store.set_media_err(&chat, &id);
            }
            mx.inner.downloading.lock().unwrap().remove(&key);
            mx.emit(ToUi::Messages(chat));
        });
    }

    /// Sends a file, with a caption; the room's encryption, if it has it, is matrix-sdk's. Kept when it comes back
    /// in the sync, with the file here as its attachment.
    pub fn send_file(&self, chat: &str, path: &str, caption: &str) {
        let Some(room) = self.room(chat) else { return self.refuse("mxNoRoom") };
        let (mx, chat, path, caption) = (self.clone(), chat.to_string(), PathBuf::from(path), caption.to_string());
        tokio::spawn(async move {
            let r: Result<()> = async {
                let data = tokio::fs::read(&path).await?;
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                let mime: mime::Mime = crate::wa::mime_for(&ext).parse()?;
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
                let caption = (!caption.is_empty()).then(|| TextMessageEventContent::plain(caption));
                let sent = room.send_attachment(name, &mime, data, AttachmentConfig::new().caption(caption)).await?;
                // The file sent is the attachment kept: no need to download it back.
                let id = sent.event_id.to_string();
                let dir = crate::wa::media_dir(&mx.inner.media, &chat);
                std::fs::create_dir_all(&dir)?;
                let file = dir.join(format!("{}.{}", crate::wa::safe(&id), if ext.is_empty() { "bin".into() } else { ext }));
                std::fs::copy(&path, &file)?;
                let _ = mx.inner.store.set_media(&chat, &id, &file.to_string_lossy(), 0, 0);
                mx.emit(ToUi::Messages(chat.clone()));
                Ok(())
            }
            .await;
            if let Err(e) = r {
                warn!("matrix send file: {e}");
                mx.emit(ToUi::Error(format!("{}: {e}", s("error"))));
            }
        });
    }

    /// Changes the text of a message of mine; the change comes back in the sync, and is kept then.
    pub fn edit(&self, chat: &str, id: &str, text: &str) {
        let Some(room) = self.room(chat) else { return self.refuse("mxNoRoom") };
        let Ok(target) = EventId::parse(id) else { return self.refuse("unknownMessage") };
        let content = RoomMessageEventContent::text_plain(text).make_replacement(ReplacementMetadata::new(target, None));
        let tx = self.inner.tx.clone();
        tokio::spawn(async move {
            if let Err(e) = room.send(content).await {
                warn!("matrix edit: {e}");
                let _ = tx.send(ToUi::Error(format!("{}: {e}", s("error"))));
            }
        });
    }

    /// Deletes a message: for everyone, taken away from the room (it shows as deleted once the sync says so); for
    /// me, only from this store, as Matrix has no deleting for one person.
    pub fn delete(&self, chat: &str, id: &str, for_everyone: bool) {
        if !for_everyone {
            if self.inner.store.batch(|db| Store::forget(db, chat, id)).is_ok() {
                self.emit(ToUi::Messages(chat.to_string()));
                self.emit(ToUi::Chats);
            }
            return;
        }
        let Some(room) = self.room(chat) else { return self.refuse("mxNoRoom") };
        let Ok(target) = EventId::parse(id) else { return self.refuse("unknownMessage") };
        let tx = self.inner.tx.clone();
        tokio::spawn(async move {
            if let Err(e) = room.redact(&target, None, None).await {
                warn!("matrix delete: {e}");
                let _ = tx.send(ToUi::Error(format!("{}: {e}", s("error"))));
            }
        });
    }

    /// What the store marked read: the room is told up to its latest.
    pub fn mark_read(&self, chat: &str, rows: &[Msg]) {
        let (Some(room), Some(last)) = (self.room(chat), rows.iter().max_by_key(|m| m.ts)) else { return };
        let Ok(id) = EventId::parse(&last.id) else { return };
        tokio::spawn(async move {
            if let Err(e) = room.send_single_receipt(ReceiptType::Read, ReceiptThread::Unthreaded, id).await {
                warn!("matrix receipt: {e}");
            }
        });
    }

    /// Older messages of a room, fifty at a time from where the last asking stopped (the latest, the first time).
    pub fn fetch_older(&self, chat: &str, sink: UnboundedSender<ToUi>) {
        let Some(room) = self.room(chat) else {
            let _ = sink.send(ToUi::Older(chat.to_string(), None));
            return;
        };
        let (mx, chat) = (self.clone(), chat.to_string());
        tokio::spawn(async move {
            match mx.older(&room, &chat).await {
                Ok(added) => {
                    let _ = sink.send(ToUi::Older(chat, Some(added)));
                }
                Err(e) => {
                    warn!("matrix older: {e}");
                    let _ = sink.send(ToUi::Older(chat, None));
                }
            }
        });
    }

    /// Fifty messages of a room before where the last asking stopped, kept; how many.
    async fn older(&self, room: &Room, chat: &str) -> Result<usize> {
        // Where the last asking stopped; empty when it reached the room's start.
        let key = format!("mxBefore:{chat}");
        let before = self.inner.store.state(&key);
        if before.as_deref() == Some("") {
            return Ok(0);
        }
        let mut options = MessagesOptions::backward();
        options.from = before;
        options.limit = 50u32.into();
        let got = room.messages(options).await?;
        let (mut added, mut locked) = (0, 0);
        // Oldest first: what a reply answers, or a reaction is to, is kept before it.
        for event in got.chunk.iter().rev() {
            match event.raw().deserialize() {
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Original(ev)))) => {
                    if self.keep(room, &ev, true).await.is_some() {
                        added += 1;
                    }
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::Reaction(SyncMessageLikeEvent::Original(ev)))) => {
                    self.on_reaction(room, &ev, true).await;
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomEncrypted(_))) => locked += 1,
                _ => {}
            }
        }
        info!("matrix older {chat}: {} events, {added} kept, {locked} not decrypted", got.chunk.len());
        let _ = self.inner.store.set_state(&key, got.end.as_deref().unwrap_or(""));
        self.emit(ToUi::Messages(chat.to_string()));
        Ok(added)
    }

    fn refuse(&self, key: &str) {
        self.emit(ToUi::Error(s(key)));
    }
}

/// A reply's text without the quote older clients put before it ("> <@who> what they said", then a blank line).
fn without_fallback(text: &str) -> &str {
    if !text.starts_with("> ") {
        return text;
    }
    match text.find("\n\n") {
        Some(i) if text[..i].lines().all(|l| l.starts_with('>')) => &text[i + 2..],
        _ => text,
    }
}

/// What the store keeps of a message: its kind and text, and an attachment's source (kept as JSON in `media`, to
/// download it from), type, name and size.
#[derive(Default)]
struct Shown {
    kind: &'static str,
    text: String,
    media: Option<Vec<u8>>,
    mime: Option<String>,
    name: Option<String>,
    w: u32,
    h: u32,
}

/// An attachment's caption: its body when it has a file name besides (else the body is the file's name).
fn caption(body: &str, filename: &Option<String>) -> String {
    match filename {
        Some(f) if f != body => body.to_string(),
        _ => String::new(),
    }
}

fn size(w: Option<matrix_sdk::ruma::UInt>, h: Option<matrix_sdk::ruma::UInt>) -> (u32, u32) {
    let n = |v: Option<matrix_sdk::ruma::UInt>| v.and_then(|v| u32::try_from(u64::from(v)).ok()).unwrap_or(0);
    (n(w), n(h))
}

fn shown(m: &MessageType) -> Shown {
    let attached = |kind: &'static str, text: String, source: &MediaSource, mime: Option<String>, name: String, (w, h): (u32, u32)| Shown {
        kind,
        text,
        media: serde_json::to_vec(source).ok(),
        mime,
        name: Some(name),
        w,
        h,
    };
    match m {
        MessageType::Text(c) => Shown { kind: "text", text: c.body.clone(), ..Default::default() },
        MessageType::Notice(c) => Shown { kind: "text", text: c.body.clone(), ..Default::default() },
        MessageType::Emote(c) => Shown { kind: "text", text: format!("* {}", c.body), ..Default::default() },
        MessageType::Image(c) => {
            let info = c.info.as_deref();
            let name = c.filename.clone().unwrap_or_else(|| c.body.clone());
            attached("image", caption(&c.body, &c.filename), &c.source, info.and_then(|i| i.mimetype.clone()), name, size(info.and_then(|i| i.width), info.and_then(|i| i.height)))
        }
        MessageType::Video(c) => {
            let info = c.info.as_deref();
            let name = c.filename.clone().unwrap_or_else(|| c.body.clone());
            attached("video", caption(&c.body, &c.filename), &c.source, info.and_then(|i| i.mimetype.clone()), name, size(info.and_then(|i| i.width), info.and_then(|i| i.height)))
        }
        MessageType::Audio(c) => {
            let info = c.info.as_deref();
            let name = c.filename.clone().unwrap_or_else(|| c.body.clone());
            // A voice message says its length, as WhatsApp's.
            let (kind, text) = match (c.voice.is_some(), info.and_then(|i| i.duration)) {
                (true, Some(d)) => ("voice", format!("{}s", d.as_secs())),
                (true, None) => ("voice", String::new()),
                (false, _) => ("audio", caption(&c.body, &c.filename)),
            };
            attached(kind, text, &c.source, info.and_then(|i| i.mimetype.clone()), name, (0, 0))
        }
        MessageType::File(c) => {
            let info = c.info.as_deref();
            let name = c.filename.clone().unwrap_or_else(|| c.body.clone());
            attached("document", caption(&c.body, &c.filename), &c.source, info.and_then(|i| i.mimetype.clone()), name, (0, 0))
        }
        MessageType::Location(c) => Shown { kind: "text", text: format!("📍 {}", c.body), ..Default::default() },
        other => Shown { kind: "text", text: other.body().to_string(), ..Default::default() },
    }
}
