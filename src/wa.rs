//! The WhatsApp side, as the Node version's wa.ts: whatsapp-rust as a linked device of the phone, its events turned into the
//! store's rows, and what the terminals ask of it (sending, reading, attachments, profiles, being online). The
//! screen talks to it through `Backend`, which a terminal without the connection reaches over a socket (ipc.rs).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use whatsapp_rust::prelude::*;
use whatsapp_rust::waproto::buffa::Message as _;

use crate::i18n::{s, t};
use crate::store::{self, Msg, Store};

/// The connection's state, as the terminals show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Conn {
    Connecting,
    Qr,
    Open,
    Closed,
}

/// What the terminals hear from WhatsApp (wassup's WaEvents).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ToUi {
    /// The connection's state, with what to say about it (why it closed, my number once open).
    Connection(Conn, String),
    /// A code to show as QR, for the phone to link this device.
    Qr(String),
    /// The chats changed (names, order, counts).
    Chats,
    /// A chat's messages changed ("*" for any).
    Messages(String),
    /// A message just came from someone else (chat, id): for the notifications.
    Notify(String, String),
    /// A line for the status bar (the history's progress).
    Status(String),
    /// Who is typing or recording in a chat; nobody when empty.
    Typing(String, Vec<String>),
    /// A reaction just received or sent (chat, message, who, emoji; empty when taken away).
    Reaction(String, String, String, String),
    /// Whether the person of a one-to-one chat is online.
    Presence(String, bool),
    /// How many of a group's followed members are online.
    GroupOnline(String, usize),
    /// A chat's photo, "about" or size was just stored.
    Profile(String),
    /// A chat's state, an emoji, was just worked out and stored.
    Mood(String),
    /// Whether this device shows as online to the others.
    Available(bool),
    /// Older messages the phone sent for a chat that asked (how many); None when it couldn't be asked or didn't
    /// answer, to be asked again later.
    Older(String, Option<usize>),
    /// Something asked failed: what to say.
    Error(String),
}

/// What a terminal asks of the connection: here, or over the socket to the process that has it (ipc.rs). Messages
/// are named by chat and id; the connection reads them from the store. Failures come back as `ToUi::Error`.
pub trait Backend: Send + Sync {
    /// Sends text, answering `reply_to` (a message of the same chat), mentioning `mentions` (jids).
    fn send(&self, chat: &str, text: &str, reply_to: Option<&str>, mentions: &[String]);
    /// Reacts to a message with an emoji; empty takes mine away.
    fn react(&self, chat: &str, id: &str, emoji: &str);
    fn edit(&self, chat: &str, id: &str, text: &str);
    fn delete(&self, chat: &str, id: &str, for_everyone: bool);
    /// Asks the phone for messages before the oldest kept.
    fn fetch_older(&self, chat: &str);
    fn mark_read(&self, chat: &str);
    /// Downloads a message's attachment, if it isn't here; `ToUi::Messages` once it is (or can't be).
    fn ensure_media(&self, chat: &str, id: &str);
    /// Gets a link preview's picture.
    fn ensure_preview(&self, chat: &str, id: &str);
    /// A chat's photo, "about" and size, at most once a day.
    fn ensure_profile(&self, jid: &str);
    /// Asks the phone again for the latest messages of someone with no name, at most once a day.
    fn ensure_name(&self, jid: &str);
    /// Tells a chat I'm typing (or stopped).
    fn set_composing(&self, chat: &str, on: bool);
    /// Asks for a chat's presence (and its group's members').
    fn subscribe_presence(&self, chat: &str);
    /// Someone wrote in terminal `pid`: online, until two idle minutes.
    fn touch_presence(&self, pid: u32);
    /// Terminal `pid` gained or lost the focus.
    fn set_focus(&self, pid: u32, focused: bool);
    /// Sends a file.
    fn send_file(&self, chat: &str, path: &str, caption: &str);
    /// Copies the chat's attachments to ~/Downloads/wa/<chat>/.
    fn download_all(&self, chat: &str);
}

thread_local! {
    /// The terminal a request came from, while it's being taken (ipc.rs): what it fails with goes to it alone.
    static ASKER: std::cell::RefCell<Option<UnboundedSender<ToUi>>> = const { std::cell::RefCell::new(None) };
}

/// Takes a terminal's request, its failures told to it.
pub fn asking<T>(tx: UnboundedSender<ToUi>, f: impl FnOnce() -> T) -> T {
    ASKER.with(|a| *a.borrow_mut() = Some(tx));
    let r = f();
    ASKER.with(|a| *a.borrow_mut() = None);
    r
}

/// Where a message's attachment and its link preview go.
pub fn media_dir(media: &Path, chat: &str) -> PathBuf {
    media.join(chat.split('@').next().unwrap_or(chat))
}

pub fn preview_path(media: &Path, chat: &str, id: &str) -> PathBuf {
    media_dir(media, chat).join(format!("{}.link.jpg", safe(id)))
}

fn safe(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// The file extension for an attachment: by its type, else its name's, else the type's second half.
fn ext_for(mime: Option<&str>, name: Option<&str>) -> String {
    let known = match mime.map(|m| m.split(';').next().unwrap_or(m).trim()) {
        Some("image/jpeg") => Some("jpg"),
        Some("image/png") => Some("png"),
        Some("image/webp") => Some("webp"),
        Some("image/gif") => Some("gif"),
        Some("video/mp4") => Some("mp4"),
        Some("video/3gpp") => Some("3gp"),
        Some("audio/ogg") => Some("ogg"),
        Some("audio/mpeg") => Some("mp3"),
        Some("audio/mp4") => Some("m4a"),
        Some("audio/aac") => Some("aac"),
        Some("application/pdf") => Some("pdf"),
        Some("text/plain") => Some("txt"),
        _ => None,
    };
    if let Some(e) = known {
        return e.into();
    }
    if let Some(e) = name.and_then(|n| Path::new(n).extension()).and_then(|e| e.to_str()).filter(|e| !e.is_empty()) {
        return e.to_string();
    }
    mime.and_then(|m| m.split('/').nth(1)).map(|e| e.split(';').next().unwrap_or(e).to_string()).unwrap_or_else(|| "bin".into())
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp4" => "video/mp4",
        "3gp" => "video/3gpp",
        "ogg" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "zip" => "application/zip",
        "csv" => "text/csv",
        "json" => "application/json",
        "md" => "text/markdown",
        _ => "application/octet-stream",
    }
}

/// A picture's small copy (32 pixels wide, as WhatsApp's) and size, for sending it.
fn preview_of_image(data: &[u8]) -> (Option<Vec<u8>>, Option<u32>, Option<u32>) {
    let Ok(img) = image::load_from_memory(data) else { return (None, None, None) };
    let mut out = Vec::new();
    let small = img.resize(32, u32::MAX, image::imageops::FilterType::Triangle);
    let ok = small.to_rgb8().write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Jpeg).is_ok();
    (ok.then_some(out), Some(img.width()), Some(img.height()))
}

/// A video's first frame, small, when ffmpeg is there.
fn frame_of_video(path: &Path) -> Option<Vec<u8>> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-ss", "0", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", "scale=32:-2", "-f", "mjpeg", "-"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    (out.status.success() && !out.stdout.is_empty()).then_some(out.stdout)
}

/// An audio's length in seconds: an Ogg Opus one's from its last page, others with ffprobe when it's there.
fn audio_seconds(path: &Path, data: &[u8]) -> Option<u32> {
    if data.starts_with(b"OggS") {
        // The last page's granule position, at 48 kHz, less the pre-skip in the Opus header.
        let last = data.windows(4).rposition(|w| w == b"OggS")?;
        let granule = u64::from_le_bytes(data.get(last + 6..last + 14)?.try_into().ok()?);
        let head = data.windows(8).position(|w| w == b"OpusHead")?;
        let pre_skip = u16::from_le_bytes(data.get(head + 10..head + 12)?.try_into().ok()?) as u64;
        return Some((granule.saturating_sub(pre_skip) / 48000) as u32);
    }
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().ok().map(|s| s.round() as u32)
}

/// A message's content as the store keeps it.
#[derive(Default)]
struct Content {
    kind: String,
    text: String,
    thumb: Option<Vec<u8>>,
    media: Option<Vec<u8>>,
    mime: Option<String>,
    name: Option<String>,
    w: u32,
    h: u32,
    /// The message it answers: who sent that (as WhatsApp named them), and its first line.
    quoted: Option<(String, String)>,
    preview: bool,
}

/// The message inside its wrappers, as Baileys' normalizeMessageContent: whatsapp-rust's own unwrapping, then an
/// album's child or a group status, again, a few times over.
fn base_of(m: &wa::Message) -> &wa::Message {
    let mut cur = m.get_base_message();
    for _ in 0..5 {
        let inner = cur
            .associated_child_message
            .as_option()
            .and_then(|w| w.message.as_option())
            .or_else(|| cur.group_status_message.as_option().and_then(|w| w.message.as_option()))
            .or_else(|| cur.group_status_message_v2.as_option().and_then(|w| w.message.as_option()));
        match inner {
            Some(i) => cur = i.get_base_message(),
            None => break,
        }
    }
    cur
}

/// The context (quote, mentions) of a message, whatever its kind.
fn context(base: &wa::Message) -> Option<&wa::ContextInfo> {
    base.extended_text_message.as_option().and_then(|m| m.context_info.as_option())
        .or_else(|| base.image_message.as_option().and_then(|m| m.context_info.as_option()))
        .or_else(|| base.video_message.as_option().and_then(|m| m.context_info.as_option()))
        .or_else(|| base.audio_message.as_option().and_then(|m| m.context_info.as_option()))
        .or_else(|| base.document_message.as_option().and_then(|m| m.context_info.as_option()))
        .or_else(|| base.sticker_message.as_option().and_then(|m| m.context_info.as_option()))
        .or_else(|| base.location_message.as_option().and_then(|m| m.context_info.as_option()))
        .or_else(|| base.contact_message.as_option().and_then(|m| m.context_info.as_option()))
}

/// What a quoted message says in a line, as wassup's quotedSnippet.
fn snippet(message: &wa::Message) -> String {
    let base = base_of(message);
    let tag = |k: &str, extra: Option<&str>| format!("[{}] {}", s(k), extra.unwrap_or("")).trim().to_string();
    if let Some(t) = base.conversation.as_deref() {
        return t.to_string();
    }
    if let Some(x) = base.extended_text_message.as_option() {
        return x.text.clone().unwrap_or_default();
    }
    if let Some(m) = base.image_message.as_option() {
        return tag("image", m.caption.as_deref());
    }
    if let Some(m) = base.video_message.as_option() {
        return tag("video", m.caption.as_deref());
    }
    if base.sticker_message.as_option().is_some() {
        return tag("sticker", None);
    }
    if base.audio_message.as_option().is_some() {
        return tag("audio", None);
    }
    if let Some(m) = base.document_message.as_option() {
        return tag("file", m.file_name.as_deref());
    }
    if base.location_message.as_option().is_some() {
        return tag("location", None);
    }
    if let Some(m) = base.contact_message.as_option() {
        return tag("contact", m.display_name.as_deref());
    }
    content(message).map(|c| format!("[{}]", c.kind)).unwrap_or_default()
}

/// A message's content; None for what isn't a message of its own (reactions, edits, keys, receipts).
fn content(message: &wa::Message) -> Option<Content> {
    let base = base_of(message);
    let quoted = context(base).and_then(|ctx| {
        let q = ctx.quoted_message.as_option()?;
        Some((ctx.participant.clone().unwrap_or_default(), snippet(q).lines().next().unwrap_or("").to_string()))
    });
    let c = |kind: &str, text: String| Content { kind: kind.into(), text, quoted: quoted.clone(), ..Default::default() };
    if let Some(img) = base.image_message.as_option() {
        return Some(Content {
            thumb: img.jpeg_thumbnail.clone(),
            media: Some(img.encode_to_vec()),
            mime: Some(img.mimetype.clone().unwrap_or_else(|| "image/jpeg".into())),
            w: img.width.unwrap_or(0),
            h: img.height.unwrap_or(0),
            ..c("image", img.caption.clone().unwrap_or_default())
        });
    }
    if let Some(st) = base.sticker_message.as_option() {
        return Some(Content {
            media: Some(st.encode_to_vec()),
            mime: Some(st.mimetype.clone().unwrap_or_else(|| "image/webp".into())),
            w: st.width.unwrap_or(0),
            h: st.height.unwrap_or(0),
            ..c("sticker", String::new())
        });
    }
    let video = base.video_message.as_option().map(|v| (v, false)).or_else(|| base.ptv_message.as_option().map(|v| (v, true)));
    if let Some((v, ptv)) = video {
        let kind = if ptv { "ptv" } else if v.gif_playback == Some(true) { "gif" } else { "video" };
        return Some(Content {
            thumb: v.jpeg_thumbnail.clone(),
            media: Some(v.encode_to_vec()),
            mime: Some(v.mimetype.clone().unwrap_or_else(|| "video/mp4".into())),
            w: v.width.unwrap_or(0),
            h: v.height.unwrap_or(0),
            ..c(kind, if ptv { String::new() } else { v.caption.clone().unwrap_or_default() })
        });
    }
    if let Some(a) = base.audio_message.as_option() {
        return Some(Content {
            media: Some(a.encode_to_vec()),
            mime: Some(a.mimetype.clone().unwrap_or_else(|| "audio/ogg; codecs=opus".into())),
            ..c(if a.ptt == Some(true) { "voice" } else { "audio" }, a.seconds.map(|s| format!("{s}s")).unwrap_or_default())
        });
    }
    if let Some(d) = base.document_message.as_option() {
        return Some(Content {
            thumb: d.jpeg_thumbnail.clone(),
            media: Some(d.encode_to_vec()),
            mime: Some(d.mimetype.clone().unwrap_or_else(|| "application/octet-stream".into())),
            name: d.file_name.clone().or_else(|| d.title.clone()),
            ..c("document", d.caption.clone().unwrap_or_default())
        });
    }
    if let Some(l) = base.location_message.as_option() {
        let map = format!("https://maps.google.com/?q={},{}", l.degrees_latitude.unwrap_or(0.0), l.degrees_longitude.unwrap_or(0.0));
        let parts: Vec<String> = [l.name.clone(), l.address.clone(), Some(map)].into_iter().flatten().filter(|p| !p.is_empty()).collect();
        return Some(c("location", parts.join(" · ")));
    }
    if let Some(l) = base.live_location_message.as_option() {
        let map = format!("https://maps.google.com/?q={},{}", l.degrees_latitude.unwrap_or(0.0), l.degrees_longitude.unwrap_or(0.0));
        return Some(c("location", map));
    }
    if let Some(k) = base.contact_message.as_option() {
        return Some(c("contact", k.display_name.clone().unwrap_or_default()));
    }
    if let Some(ks) = base.contacts_array_message.as_option() {
        let names: Vec<String> = ks.contacts.iter().filter_map(|k| k.display_name.clone()).filter(|n| !n.is_empty()).collect();
        return Some(c("contact", names.join(", ")));
    }
    let poll = base.poll_creation_message.as_option()
        .or_else(|| base.poll_creation_message_v2.as_option())
        .or_else(|| base.poll_creation_message_v3.as_option());
    if let Some(p) = poll {
        let mut text = p.name.clone().unwrap_or_default();
        for o in &p.options {
            text.push_str(&format!("\n  ○ {}", o.option_name.clone().unwrap_or_default()));
        }
        return Some(c("poll", text));
    }
    if let Some(p) = base.pin_in_chat_message.as_option() {
        use wa::message::pin_in_chat_message::Type;
        let target = p.key.as_option().and_then(|k| k.id.clone()).unwrap_or_default();
        return Some(c(if p.r#type == Some(Type::UNPIN_FOR_ALL) { "unpin" } else { "pin" }, target));
    }
    if base.album_message.as_option().is_some() {
        return Some(c("album", String::new()));
    }
    if base.placeholder_message.as_option().is_some() {
        return Some(c("placeholder", String::new()));
    }
    if base.group_invite_message.as_option().is_some() {
        return Some(c("groupInvite", String::new()));
    }
    if base.interactive_message.as_option().is_some() || base.template_message.as_option().is_some() {
        return Some(c("interactive", String::new()));
    }
    if let Some(t) = base.text_content().filter(|t| !t.is_empty()) {
        // A link's preview with a picture, fetched when the message comes into view.
        let preview = base.extended_text_message.as_option().is_some_and(|x| {
            x.matched_text.is_some() && (x.thumbnail_direct_path.is_some() || x.jpeg_thumbnail.as_ref().is_some_and(|j| !j.is_empty()))
        });
        return Some(Content { preview, ..c("text", t.to_string()) });
    }
    // What's left that isn't plumbing (keys, context, reactions, edits, encrypted parts of others) is a message of a
    // kind not drawn here: it shows as such.
    let mut rest = base.clone();
    rest.sender_key_distribution_message = Default::default();
    rest.message_context_info = Default::default();
    rest.reaction_message = Default::default();
    rest.protocol_message = Default::default();
    rest.secret_encrypted_message = Default::default();
    rest.poll_update_message = Default::default();
    rest.keep_in_chat_message = Default::default();
    rest.enc_reaction_message = Default::default();
    if rest.encode_to_vec().is_empty() {
        return None;
    }
    Some(c("unsupported", String::new()))
}

/// A message as kept: `chat` and `sender` as the store names them, `key_chat` and `key_sender` as WhatsApp did.
#[allow(clippy::too_many_arguments)]
fn to_msg(db: &rusqlite::Connection, chat: &str, id: &str, from_me: bool, sender: &str, push_name: &str, ts: i64, message: &wa::Message, key_chat: &str, key_sender: &str) -> Option<Msg> {
    let c = content(message)?;
    let quoted = c.quoted.map(|(who, text)| format!("{}\t{text}", Store::canonical(db, &who)));
    Some(Msg {
        chat: chat.into(),
        id: id.into(),
        from_me,
        sender: sender.into(),
        push_name: push_name.into(),
        ts,
        kind: c.kind,
        text: c.text,
        thumb: c.thumb.filter(|t| !t.is_empty()),
        media: c.media,
        media_mime: c.mime,
        media_name: c.name,
        w: c.w,
        h: c.h,
        status: if from_me { 2 } else { 0 },
        quoted,
        raw: Some(message.encode_to_vec()),
        key_chat: key_chat.into(),
        key_sender: key_sender.into(),
        preview: c.preview,
        ..Default::default()
    })
}

/// A reaction, a deletion or an edit: applied to the message it's about, in place of being a message. Returns the
/// reaction applied, for the screen; Some(None) for another such action.
fn apply_action(db: &rusqlite::Connection, chat: &str, sender: &str, ts_ms: i64, message: &wa::Message) -> Result<Option<Option<(String, String)>>> {
    let base = base_of(message);
    if let Some(r) = base.reaction_message.as_option() {
        let Some(id) = r.key.as_option().and_then(|k| k.id.clone()) else { return Ok(Some(None)) };
        let emoji = r.text.clone().unwrap_or_default();
        let ts = r.sender_timestamp_ms.unwrap_or(ts_ms);
        Store::set_reaction(db, chat, &id, sender, &emoji, ts)?;
        return Ok(Some(Some((id, emoji))));
    }
    if let Some(p) = base.protocol_message.as_option() {
        use wa::message::protocol_message::Type;
        let Some(id) = p.key.as_option().and_then(|k| k.id.as_deref()) else { return Ok(Some(None)) };
        if p.r#type == Some(Type::REVOKE) {
            Store::revoke(db, chat, id)?;
        } else if p.r#type == Some(Type::MESSAGE_EDIT)
            && let Some(c) = p.edited_message.as_option().and_then(content)
                && Store::has_message(db, chat, id) {
                    Store::edit(db, chat, id, &c.kind, &c.text)?;
                }
        return Ok(Some(None));
    }
    Ok(None)
}

/// What the moods' keeper reads and writes: the chats, their latest lines, their kept moods.
struct Moods {
    store: Arc<Store>,
    tx: UnboundedSender<ToUi>,
}

impl crate::mood::MoodHost for Moods {
    fn chats(&self) -> Vec<crate::mood::MoodChat> {
        let open: HashSet<String> = self
            .store
            .states("tabs:")
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_str::<serde_json::Value>(&v).ok())
            .flat_map(|v| v.get("tabs").and_then(|t| t.as_array()).cloned().unwrap_or_default())
            .filter_map(|j| j.as_str().map(String::from))
            .collect();
        self.store
            .chats()
            .unwrap_or_default()
            .into_iter()
            .map(|c| {
                let up_to = self.store.state(&format!("mood:{}", c.jid)).and_then(|v| serde_json::from_str::<crate::mood::Mood>(&v).ok()).map(|m| m.up_to).unwrap_or(0);
                crate::mood::MoodChat { open: open.contains(&c.jid), jid: c.jid, last_ts: c.last_ts, up_to, archived: c.archived }
            })
            .collect()
    }

    fn lines(&self, jid: &str, budget: usize) -> Vec<String> {
        let lang = crate::llm::Lang::from_env();
        let mut out = Vec::new();
        let mut used = 0;
        // As many of the latest as fit in the budget: read in growing pages until they do or there are no more.
        let total = self.store.count(jid);
        let mut limit = 200;
        let msgs = loop {
            let msgs = self.store.messages(jid, limit).unwrap_or_default();
            let chars: usize = msgs.iter().map(|m| m.text.chars().count() + 24).sum();
            if chars >= budget || limit >= total {
                break msgs;
            }
            limit *= 4;
        };
        for m in msgs.iter().rev() {
            let who = if crate::text::is_group(jid) { self.store.name_of(&m.sender) } else { self.store.name_of(jid) };
            let kind = match m.kind.as_str() {
                "pin" | "unpin" => "pinInChat",
                k => k,
            };
            // A pin's text is the id of the message it's about: nothing for the model.
            let text = if kind == "pinInChat" { "" } else { m.text.as_str() };
            let line = crate::mood::mood_line(&crate::mood::MoodMsg { kind, from_me: m.from_me, who: &who, text, unpinned: m.kind == "unpin" }, lang);
            if let Some(l) = line {
                used += l.chars().count() + 1;
                out.push(l);
                if used > budget {
                    break;
                }
            }
        }
        out.reverse();
        out
    }

    fn save(&self, jid: &str, mood: &crate::mood::Mood) {
        if let Ok(v) = serde_json::to_string(mood) {
            let _ = self.store.set_state(&format!("mood:{jid}"), &v);
        }
    }

    fn changed(&self, jid: &str) {
        let _ = self.tx.send(ToUi::Mood(jid.to_string()));
    }
}

/// The connection's own state, behind the handle the terminals share.
struct Inner {
    client: Mutex<Option<Arc<Client>>>,
    handle: tokio::sync::Mutex<Option<BotHandle>>,
    store: Arc<Store>,
    dir: PathBuf,
    media: PathBuf,
    tx: UnboundedSender<ToUi>,
    live: Mutex<Live>,
    /// The downloads and the profiles asked for, one at a time each.
    downloads: UnboundedSender<Job>,
    profiles: UnboundedSender<String>,
    /// Asks for a new connection, after the phone unlinked this device.
    relink: UnboundedSender<()>,
    /// Each chat's state as an emoji, worked out calmly by the local model.
    moods: crate::mood::MoodKeeper,
}

/// What changes as the connection lives: who's typing, who's online, this device's own presence, what's asked.
#[derive(Default)]
struct Live {
    state: Option<Conn>,
    me: String,
    /// Who is typing in each chat, and a counter that lets a timer know it's still the latest word.
    typing: HashMap<String, (HashSet<String>, u64)>,
    typing_gen: u64,
    available: bool,
    /// A counter for the two idle minutes' timer, so a newer touch cancels an older timer.
    presence_gen: u64,
    online: HashMap<String, bool>,
    subscribed: HashSet<String>,
    group_members: HashMap<String, Vec<String>>,
    group_online: HashMap<String, usize>,
    focus: HashMap<u32, bool>,
    downloading: HashSet<String>,
    profile_asked: HashSet<String>,
    stopped: bool,
    /// Older messages asked of the phone, by the request's id: the chat, its oldest message's time and how many it
    /// had then, and the terminal that asked.
    older: HashMap<String, (String, i64, usize, UnboundedSender<ToUi>)>,
}

enum Job {
    Media(String, String),
    Preview(String, String),
}

/// The live connection, and what the terminals can ask of it.
#[derive(Clone)]
pub struct Wa {
    inner: Arc<Inner>,
}

impl Wa {
    /// Connects (pairing by QR the first time, shown through `tx`), with the session kept in `dir`.
    pub async fn start(store: Arc<Store>, dir: &Path, tx: UnboundedSender<ToUi>) -> Result<Self> {
        let media = dir.join("media");
        std::fs::create_dir_all(&media)?;
        // Only the process with the connection writes, so the stored delivery states are put right here, once.
        let fixed = store.repair_statuses();
        if fixed > 0 {
            info!("delivery states repaired: {fixed}");
        }
        let (dl_tx, dl_rx) = unbounded_channel();
        let (pr_tx, pr_rx) = unbounded_channel();
        let (relink_tx, mut relink_rx) = unbounded_channel::<()>();
        let moods = crate::mood::MoodKeeper::new(crate::llm::Llm::from_env(crate::llm::Lang::from_env()), Moods { store: store.clone(), tx: tx.clone() });
        let wa = Self {
            inner: Arc::new(Inner {
                client: Mutex::new(None),
                handle: tokio::sync::Mutex::new(None),
                store,
                dir: dir.to_path_buf(),
                media,
                tx,
                live: Mutex::new(Live::default()),
                downloads: dl_tx,
                profiles: pr_tx,
                relink: relink_tx,
                moods,
            }),
        };
        let again = wa.clone();
        tokio::spawn(async move {
            while relink_rx.recv().await.is_some() {
                if let Err(e) = again.connect().await {
                    warn!("reconnect: {e}");
                }
            }
        });
        tokio::spawn(wa.clone().download_loop(dl_rx));
        tokio::spawn(wa.clone().profile_loop(pr_rx));
        wa.connect().await?;
        Ok(wa)
    }

    fn emit(&self, ev: ToUi) {
        let _ = self.inner.tx.send(ev);
    }

    fn client(&self) -> Option<Arc<Client>> {
        self.inner.client.lock().unwrap().clone()
    }

    fn store(&self) -> &Store {
        &self.inner.store
    }

    fn set_state(&self, state: Conn, detail: String) {
        self.inner.live.lock().unwrap().state = Some(state);
        self.emit(ToUi::Connection(state, detail));
    }

    pub fn is_open(&self) -> bool {
        self.inner.live.lock().unwrap().state == Some(Conn::Open)
    }

    fn me(&self) -> String {
        self.inner.live.lock().unwrap().me.clone()
    }

    async fn connect(&self) -> Result<()> {
        let session = self.inner.dir.join("session.db");
        let backend = SqliteStore::new(session.to_str().context("session path")?).await.context("session store")?;
        self.set_state(Conn::Connecting, String::new());
        let mut props = whatsapp_rust::wacore::store::DevicePropsOverride::new().with_os("wassup");
        // WA_FULL_HISTORY=1: the phone sends the whole history on linking, not only the recent months.
        if std::env::var("WA_FULL_HISTORY").as_deref() == Ok("1") {
            props = props.with_require_full_sync(true);
        }
        let (qr_wa, conn_wa, out_wa, ev_wa) = (self.clone(), self.clone(), self.clone(), self.clone());
        let bot = Bot::builder()
            .with_backend(backend)
            // How this device shows among the phone's linked devices.
            .with_device_props(props)
            .on_qr_code(move |code, _timeout| {
                let wa = qr_wa.clone();
                async move {
                    wa.set_state(Conn::Qr, String::new());
                    wa.emit(ToUi::Qr(code));
                }
            })
            .on_connected(move |client| {
                let wa = conn_wa.clone();
                async move { wa.on_connected(client).await }
            })
            .on_logged_out(move |_| {
                let wa = out_wa.clone();
                async move { wa.on_logged_out().await }
            })
            .on_event(move |event, _client| {
                let wa = ev_wa.clone();
                async move { wa.on_event(&event) }
            })
            .build()
            .await
            .context("whatsapp client")?;
        let handle = bot.spawn();
        *self.inner.client.lock().unwrap() = Some(handle.client());
        *self.inner.handle.lock().await = Some(handle);
        Ok(())
    }

    async fn on_connected(&self, client: Arc<Client>) {
        let me = client.pn().map(|j| j.to_non_ad_string()).unwrap_or_default();
        if let Some(lid) = client.lid() {
            let _ = self.store().batch(|db| Store::set_lid(db, &lid.to_non_ad_string(), &me));
        }
        // Who I am, for every process: my own chat goes by my profile name.
        let _ = self.store().set_state("me", &me);
        let name = client.push_name();
        if !name.is_empty() {
            let _ = self.store().set_state("meName", &name);
        }
        self.inner.live.lock().unwrap().me = me.clone();
        // whatsapp-rust announces "available" on connecting; it's someone writing in a terminal that makes it so.
        let available = self.inner.live.lock().unwrap().available;
        if !available {
            let _ = client.presence().set_unavailable().await;
        }
        self.set_state(Conn::Open, me);
        self.emit(ToUi::Available(available));
        self.inner.moods.start();
        // The groups' names, all at once.
        match client.groups().get_participating().await {
            Ok(groups) => {
                let r = self.store().batch(|db| {
                    for (jid, meta) in &groups {
                        Store::touch_chat(db, &jid.to_non_ad_string(), Some(&meta.subject), 0)?;
                    }
                    Ok(())
                });
                if let Err(e) = r {
                    warn!("groups: {e}");
                }
                self.emit(ToUi::Chats);
            }
            Err(e) => warn!("groups: {e}"),
        }
        self.resolve_lids(&client).await;
        // What was asked while it was down is asked now.
        let subscribed: Vec<String> = self.inner.live.lock().unwrap().subscribed.iter().cloned().collect();
        if available {
            for jid in subscribed {
                self.subscribe_one(&jid);
            }
        }
    }

    /// Names kept under a hidden id with no number known: the number asked of whatsapp-rust's own mapping, and the
    /// name moved to it.
    async fn resolve_lids(&self, client: &Arc<Client>) {
        let pending = self.store().unmapped_lids();
        let mut resolved = 0;
        for lid in &pending {
            let Ok(jid) = lid.parse::<Jid>() else { continue };
            if let Ok(Some(entry)) = client.get_lid_pn_entry(&jid).await {
                let pn = format!("{}@s.whatsapp.net", entry.phone_number);
                if self.store().batch(|db| Store::set_lid(db, lid, &pn)).is_ok() {
                    resolved += 1;
                }
            }
        }
        if !pending.is_empty() {
            info!("lid contacts: {} pending, {resolved} resolved", pending.len());
        }
        if resolved > 0 {
            self.emit(ToUi::Chats);
        }
    }

    /// Unlinked from the phone: the session goes, and a new QR comes to link again.
    async fn on_logged_out(&self) {
        self.drop_online();
        self.set_state(Conn::Closed, s("loggedOut"));
        self.restart(Duration::from_millis(500), true);
    }

    /// A new connection after `wait`, the old one let go; its session wiped when the phone unlinked this device.
    fn restart(&self, wait: Duration, wipe: bool) {
        if self.inner.live.lock().unwrap().stopped {
            return;
        }
        let wa = self.clone();
        tokio::spawn(async move {
            if let Some(h) = wa.inner.handle.lock().await.take() {
                let _ = tokio::time::timeout(Duration::from_secs(3), h.shutdown()).await;
            }
            *wa.inner.client.lock().unwrap() = None;
            if wipe {
                for suffix in ["", "-wal", "-shm"] {
                    let _ = std::fs::remove_file(wa.inner.dir.join(format!("session.db{suffix}")));
                }
            }
            tokio::time::sleep(wait).await;
            let _ = wa.inner.relink.send(());
        });
    }

    /// whatsapp-rust says "available" by itself (a new connection's first profile name, a change of it): back to
    /// unavailable unless someone is writing here.
    fn keep_unavailable(&self) {
        if self.inner.live.lock().unwrap().available {
            return;
        }
        if let Some(c) = self.client() {
            tokio::spawn(async move {
                let _ = c.presence().set_unavailable().await;
            });
        }
    }

    /// Offline first, then the connection closed, its state written.
    pub async fn stop(&self) {
        self.inner.live.lock().unwrap().stopped = true;
        self.inner.moods.stop();
        let available = std::mem::take(&mut self.inner.live.lock().unwrap().available);
        if let (true, Some(c)) = (available, self.client()) {
            let _ = tokio::time::timeout(Duration::from_secs(1), c.presence().set_unavailable()).await;
        }
        if let Some(handle) = self.inner.handle.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_millis(1500), handle.shutdown()).await;
        }
    }

    /// A message of the store, with how it came.
    fn row(&self, chat: &str, id: &str) -> Option<Msg> {
        self.store().message(chat, id)
    }

    /// The key WhatsApp knows a kept message by: its chat and sender as they came.
    fn key_of(&self, m: &Msg) -> wa::MessageKey {
        let chat = if m.key_chat.is_empty() { m.chat.clone() } else { m.key_chat.clone() };
        let sender = if m.key_sender.is_empty() { m.sender.clone() } else { m.key_sender.clone() };
        wa::MessageKey {
            remote_jid: Some(chat.clone()),
            from_me: Some(m.from_me),
            id: Some(m.id.clone()),
            participant: (crate::text::is_group(&m.chat) && !m.from_me).then_some(sender),
        }
    }

    fn jid_of(m: &Msg) -> Option<Jid> {
        let chat = if m.key_chat.is_empty() { &m.chat } else { &m.key_chat };
        chat.parse().ok()
    }

    /// Where what's asked now fails to: the terminal asking, or this one's screen.
    fn sink(&self) -> UnboundedSender<ToUi> {
        ASKER.with(|a| a.borrow().clone()).unwrap_or_else(|| self.inner.tx.clone())
    }

    fn fail_to(sink: &UnboundedSender<ToUi>, what: &str, e: impl std::fmt::Display) {
        warn!("{what}: {e}");
        let _ = sink.send(ToUi::Error(format!("{}: {e}", s("error"))));
    }

    fn refuse(&self, key: &str) {
        let _ = self.sink().send(ToUi::Error(s(key)));
    }

    /// The client, when connected; else says so.
    fn with_client(&self) -> Option<Arc<Client>> {
        let c = self.client().filter(|_| self.is_open());
        if c.is_none() {
            self.refuse("noConnection");
        }
        c
    }

    fn subscribe_one(&self, jid: &str) {
        let Some(client) = self.client() else { return };
        let Ok(j) = jid.parse::<Jid>() else { return };
        tokio::spawn(async move {
            if let Err(e) = client.presence().subscribe(j).await {
                warn!("presence subscribe: {e}");
            }
        });
    }

    fn set_available(&self, on: bool) {
        {
            let mut live = self.inner.live.lock().unwrap();
            if live.available == on {
                return;
            }
            live.available = on;
        }
        self.emit(ToUi::Available(on));
        let Some(client) = self.client().filter(|_| self.is_open()) else { return };
        let wa = self.clone();
        tokio::spawn(async move {
            let r = if on { client.presence().set_available().await } else { client.presence().set_unavailable().await };
            if let Err(e) = r {
                warn!("presence: {e}");
            }
            // Back online, what's followed is asked again: nothing came while unavailable.
            if on {
                let subscribed: Vec<String> = wa.inner.live.lock().unwrap().subscribed.iter().cloned().collect();
                for jid in subscribed {
                    wa.subscribe_one(&jid);
                }
            }
        });
        if !on {
            self.drop_online();
        }
    }

    /// Nothing more will arrive (unavailable, or the connection gone): everyone known online goes back to unknown.
    fn drop_online(&self) {
        let (online, groups) = {
            let mut live = self.inner.live.lock().unwrap();
            let online: Vec<String> = live.online.drain().filter(|(_, on)| *on).map(|(j, _)| j).collect();
            let groups: Vec<String> = live.group_online.drain().filter(|(_, n)| *n > 0).map(|(g, _)| g).collect();
            (online, groups)
        };
        for jid in online {
            self.emit(ToUi::Presence(jid, false));
        }
        for g in groups {
            self.emit(ToUi::GroupOnline(g, 0));
        }
    }

    fn set_online(&self, chat: &str, on: bool) {
        let groups: Vec<String> = {
            let mut live = self.inner.live.lock().unwrap();
            let was = live.online.insert(chat.to_string(), on).unwrap_or(false);
            if was == on {
                return;
            }
            live.group_members.iter().filter(|(_, m)| m.iter().any(|j| j == chat)).map(|(g, _)| g.clone()).collect()
        };
        self.emit(ToUi::Presence(chat.to_string(), on));
        for g in groups {
            self.count_group(&g, false);
        }
    }

    fn count_group(&self, group: &str, always: bool) {
        let n = {
            let mut live = self.inner.live.lock().unwrap();
            let n = live.group_members.get(group).map(|m| m.iter().filter(|j| live.online.get(*j).copied().unwrap_or(false)).count()).unwrap_or(0);
            if !always && live.group_online.get(group).copied().unwrap_or(0) == n {
                return;
            }
            live.group_online.insert(group.to_string(), n);
            n
        };
        self.emit(ToUi::GroupOnline(group.to_string(), n));
    }

    /// Who's typing in a chat, forgotten after 15 s without word in case the "stopped" never comes.
    fn set_typing(&self, chat: &str, who: &str, active: bool) {
        let (changed, list, generation) = {
            let mut live = self.inner.live.lock().unwrap();
            live.typing_gen += 1;
            let generation = live.typing_gen;
            let entry = live.typing.entry(chat.to_string()).or_default();
            let had = entry.0.contains(who);
            if active {
                entry.0.insert(who.to_string());
            } else {
                entry.0.remove(who);
            }
            entry.1 = generation;
            let list: Vec<String> = entry.0.iter().cloned().collect();
            if list.is_empty() {
                live.typing.remove(chat);
            }
            (had != active, list, generation)
        };
        if !list.is_empty() {
            let (wa, chat) = (self.clone(), chat.to_string());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(15)).await;
                let expired = {
                    let mut live = wa.inner.live.lock().unwrap();
                    let stale = live.typing.get(&chat).is_some_and(|(_, g)| *g == generation);
                    if stale {
                        live.typing.remove(&chat);
                    }
                    stale
                };
                if expired {
                    wa.emit(ToUi::Typing(chat, vec![]));
                }
            });
        }
        if changed {
            self.emit(ToUi::Typing(chat.to_string(), list));
        }
    }

    /// One-to-one: whoever read one of my messages read the ones before it too.
    fn settle_reads(db: &rusqlite::Connection, rows: &[(String, i64)]) -> Result<()> {
        let mut latest: HashMap<&str, i64> = HashMap::new();
        for (chat, ts) in rows {
            if !crate::text::is_group(chat) {
                let e = latest.entry(chat.as_str()).or_insert(*ts);
                *e = (*e).max(*ts);
            }
        }
        for (chat, ts) in latest {
            Store::mark_read_before(db, chat, ts)?;
        }
        Ok(())
    }

    /// A history chunk into the store, in one transaction: each conversation's name, unread count and messages,
    /// the names people gave themselves, and the hidden ids' numbers.
    fn store_history(&self, hs: &wa::HistorySync) -> Result<usize> {
        let me = self.me();
        self.store().batch(|db| {
            let mut count = 0;
            for m in &hs.phone_number_to_lid_mappings {
                if let (Some(pn), Some(lid)) = (&m.pn_jid, &m.lid_jid) {
                    Store::set_lid(db, lid, pn)?;
                }
            }
            for conv in &hs.conversations {
                if let Some(pn) = &conv.pn_jid {
                    Store::set_lid(db, &conv.id, pn)?;
                }
                if let Some(lid) = &conv.lid_jid {
                    Store::set_lid(db, lid, &conv.id)?;
                }
            }
            for p in &hs.pushnames {
                if let (Some(id), Some(name)) = (&p.id, &p.pushname) {
                    Store::set_name(db, &Store::canonical(db, id), name, store::NAME_PUSH)?;
                }
            }
            let mut read: Vec<(String, i64)> = Vec::new();
            for conv in &hs.conversations {
                let jid = Store::canonical(db, &conv.id);
                let jid = jid.as_str();
                if jid.ends_with("@broadcast") || jid.ends_with("@newsletter") {
                    continue;
                }
                let name = conv.name.as_deref().or(conv.display_name.as_deref());
                let ts = conv.conversation_timestamp.or(conv.last_msg_timestamp).unwrap_or(0) as i64;
                Store::touch_chat(db, jid, name, ts)?;
                if let Some(unread) = conv.unread_count {
                    Store::set_unread(db, jid, unread as i32)?;
                }
                if let Some(archived) = conv.archived {
                    Store::set_archived(db, jid, archived)?;
                }
                for hm in &conv.messages {
                    let Some(wmi) = hm.message.as_option() else { continue };
                    let Some(key) = wmi.key.as_option() else { continue };
                    let Some(message) = wmi.message.as_option() else { continue };
                    let from_me = key.from_me.unwrap_or(false);
                    let key_chat = key.remote_jid.clone().unwrap_or_else(|| conv.id.clone());
                    let key_sender = key.participant.clone().or_else(|| wmi.participant.clone()).unwrap_or_else(|| if from_me { me.clone() } else { key_chat.clone() });
                    let sender = if from_me { me.clone() } else { Store::canonical(db, &key_sender) };
                    let push = wmi.push_name.clone().unwrap_or_default();
                    if !from_me && !push.is_empty() {
                        Store::set_name(db, &sender, &push, store::NAME_PUSH)?;
                    }
                    if !from_me && !crate::text::is_group(jid) && wmi.verified_biz_name.is_some() {
                        let _ = db.execute("INSERT OR REPLACE INTO state (key, value) VALUES (?1, 'true')", [format!("biz:{jid}")]);
                    }
                    let ts = wmi.message_timestamp.unwrap_or(0) as i64;
                    let id = key.id.clone().unwrap_or_default();
                    let who = if from_me { "me".to_string() } else { sender.clone() };
                    if apply_action(db, jid, &who, ts * 1000, message)?.is_some() {
                        continue;
                    }
                    if let Some(mut m) = to_msg(db, jid, &id, from_me, &sender, &push, ts, message, &key_chat, &key_sender) {
                        // Mine as far as they got: at the server, delivered, read (played counts as read).
                        if from_me {
                            m.status = wmi.status.map(|s| (s as i32).min(4)).unwrap_or(2);
                            if m.status >= 4 {
                                read.push((jid.to_string(), ts));
                            }
                        }
                        Store::add_message(db, &m)?;
                        count += 1;
                    }
                    // The reactions it had, each by whoever reacted.
                    for r in &wmi.reactions {
                        let Some(k) = r.key.as_option() else { continue };
                        let who = if k.from_me == Some(true) { "me".to_string() } else { Store::canonical(db, &k.participant.clone().or_else(|| k.remote_jid.clone()).unwrap_or_default()) };
                        Store::set_reaction(db, jid, &id, &who, r.text.as_deref().unwrap_or(""), r.sender_timestamp_ms.unwrap_or(ts * 1000))?;
                    }
                }
            }
            Self::settle_reads(db, &read)?;
            Ok(count)
        })
    }

    /// Every event from WhatsApp that changes what's shown.
    fn on_event(&self, event: &Event) {
        match event {
            Event::Messages(batch) => self.on_messages(batch.iter().map(|im| (&*im.info, &im.message))),
            Event::HistorySync(lazy) => {
                let Some(hs) = lazy.get() else { return };
                let on_demand = lazy.sync_type() == 6;
                // The answer to an older stretch asked for goes to whoever asked; others (asked for a name) to no one.
                let asked = lazy.peer_data_request_session_id().and_then(|id| self.inner.live.lock().unwrap().older.remove(id));
                let stored = match self.store_history(hs) {
                    Ok(n) => n,
                    Err(e) => {
                        warn!("history: {e}");
                        0
                    }
                };
                info!("history: {} conversations, {stored} messages", hs.conversations.len());
                self.emit(ToUi::Chats);
                self.emit(ToUi::Messages("*".into()));
                if let Some((chat, oldest_ts, before, sink)) = asked {
                    let moved = self.store().oldest(&chat).is_some_and(|m| m.ts < oldest_ts);
                    let added = if moved { self.store().count(&chat).saturating_sub(before) } else { 0 };
                    let _ = sink.send(ToUi::Older(chat, Some(added)));
                }
                if !on_demand {
                    let progress = hs.progress.map(|p| format!(" ({p}%)")).unwrap_or_default();
                    self.emit(ToUi::Status(t("historyProgress", &[&stored, &hs.conversations.len(), &progress])));
                    if hs.progress == Some(100) {
                        self.emit(ToUi::Status(s("historyDone")));
                    }
                }
            }
            Event::ContactUpdate(c) => {
                if let (Some(lid), Some(pn)) = (&c.action.lid_jid, &c.action.pn_jid) {
                    let _ = self.store().batch(|db| Store::set_lid(db, lid, pn));
                }
                let name = c.action.full_name.clone().or_else(|| c.action.first_name.clone());
                if let Some(name) = name {
                    let _ = self.store().batch(|db| Store::set_name(db, &Store::canonical(db, &c.jid.to_non_ad_string()), &name, store::NAME_CONTACT));
                    self.emit(ToUi::Chats);
                }
            }
            Event::PushNameUpdate(p) => {
                let _ = self.store().batch(|db| Store::set_name(db, &Store::canonical(db, &p.jid.to_non_ad_string()), &p.new_push_name, store::NAME_PUSH));
                self.emit(ToUi::Chats);
            }
            Event::SelfPushNameUpdated(p) => {
                let _ = self.store().set_state("meName", &p.new_name);
                self.emit(ToUi::Chats);
                self.keep_unavailable();
            }
            Event::OfflineSyncCompleted(_) => self.keep_unavailable(),
            Event::TemporaryBan(b) => {
                self.drop_online();
                self.set_state(Conn::Closed, t("closedReconnecting", &[&format!("{:?}", b.code)]));
                self.restart(Duration::from_secs(2), false);
            }
            Event::ClientOutdated(_) => {
                self.drop_online();
                self.set_state(Conn::Closed, t("closedReconnecting", &[&"outdated"]));
                self.restart(Duration::from_secs(2), false);
            }
            Event::ConnectFailure(f) => {
                self.drop_online();
                self.set_state(Conn::Closed, t("closedReconnecting", &[&format!("{:?}", f.reason)]));
                self.restart(Duration::from_secs(2), false);
            }
            Event::Receipt(r) => self.on_receipt(r),
            // Read on another device, the count goes; marked unread there, it's -1, as WhatsApp gives it: 🔔, no count.
            Event::MarkChatAsReadUpdate(u) => {
                let chat = self.store().canonical_of(&u.jid.to_non_ad_string());
                match u.action.read {
                    Some(true) => {
                        let _ = self.store().batch(|db| Store::set_unread(db, &chat, 0));
                    }
                    Some(false) => {
                        let _ = self.store().batch(|db| {
                            db.execute("UPDATE chats SET unread = -1 WHERE jid = ?1 AND unread = 0", [&chat])?;
                            Ok(())
                        });
                    }
                    None => {}
                }
                self.emit(ToUi::Chats);
            }
            Event::ArchiveUpdate(u) => {
                let chat = self.store().canonical_of(&u.jid.to_non_ad_string());
                let archived = u.action.archived.unwrap_or(false);
                let _ = self.store().batch(|db| Store::set_archived(db, &chat, archived));
                self.emit(ToUi::Chats);
            }
            // Deleted for me on the phone: it shows as deleted, as wassup shows it.
            Event::DeleteMessageForMeUpdate(u) => {
                let chat = self.store().canonical_of(&u.chat_jid.to_non_ad_string());
                let _ = self.store().batch(|db| Store::revoke(db, &chat, &u.message_id));
                self.emit(ToUi::Messages(chat));
            }
            Event::GroupUpdate(g) => {
                use whatsapp_rust::wacore::stanza::groups::GroupNotificationAction as A;
                let jid = g.group_jid.to_non_ad_string();
                if let A::Subject { subject, .. } = &g.action {
                    let _ = self.store().batch(|db| {
                        Store::touch_chat(db, &jid, None, 0)?;
                        Store::set_chat_name(db, &jid, subject)
                    });
                    self.emit(ToUi::Chats);
                }
            }
            Event::PictureUpdate(p) => {
                let jid = self.store().canonical_of(&p.jid.to_non_ad_string());
                if let Some(mut prof) = self.store().profile(&jid) {
                    prof.fetched = 0;
                    let _ = self.store().set_profile(&jid, &prof);
                }
                self.ensure_profile(&jid);
            }
            Event::UserAboutUpdate(u) => {
                let jid = self.store().canonical_of(&u.jid.to_non_ad_string());
                if let Some(mut prof) = self.store().profile(&jid) {
                    prof.about = Some(u.status.clone()).filter(|s| !s.is_empty());
                    let _ = self.store().set_profile(&jid, &prof);
                    self.emit(ToUi::Profile(jid));
                }
            }
            Event::ChatPresence(p) => {
                use whatsapp_rust::wacore::types::presence::ChatPresence;
                let chat = self.store().canonical_of(&p.source.chat.to_non_ad_string());
                let who = self.store().canonical_of(&p.source.sender.to_non_ad_string());
                let active = matches!(p.state, ChatPresence::Composing);
                let me = self.me();
                self.set_typing(&chat, &who, active && who != me);
                // Typing, recording and paused are all online.
                if who != me && !crate::text::is_group(&chat) {
                    let _ = self.store().set_state(&format!("seen:{chat}"), &chrono::Utc::now().timestamp().to_string());
                    self.set_online(&chat, true);
                }
            }
            Event::Presence(p) => {
                let jid = self.store().canonical_of(&p.from.to_non_ad_string());
                if jid == self.me() || crate::text::is_group(&jid) {
                    return;
                }
                let on = !p.unavailable;
                // When they were last seen: now while online; going offline, the time WhatsApp gives, or now if they
                // were online until then.
                let was = self.inner.live.lock().unwrap().online.get(&jid).copied().unwrap_or(false);
                let now = chrono::Utc::now().timestamp();
                let seen = if on { Some(now) } else { p.last_seen.map(|t| t.timestamp()).or(was.then_some(now)) };
                if let Some(seen) = seen {
                    let _ = self.store().set_state(&format!("seen:{jid}"), &seen.to_string());
                }
                self.set_online(&jid, on);
            }
            Event::StreamReplaced(_) => {
                // Another instance connected with this account: reconnecting would only throw it out, and be thrown out.
                self.drop_online();
                self.set_state(Conn::Closed, s("anotherInstance"));
            }
            Event::Disconnected(_) => {
                self.drop_online();
                if std::mem::take(&mut self.inner.live.lock().unwrap().available) {
                    self.emit(ToUi::Available(false));
                }
                if self.inner.live.lock().unwrap().state != Some(Conn::Closed) {
                    self.set_state(Conn::Closed, t("closedReconnecting", &[&"?"]));
                }
            }
            Event::Connected(_) => {}
            _ => {}
        }
    }

    /// Live messages: stored (or applied, for reactions, deletions and edits), counted unread, and told.
    fn on_messages<'a>(&self, batch: impl Iterator<Item = (&'a whatsapp_rust::wacore::types::message::MessageInfo, &'a Arc<wa::Message>)>) {
        let me = self.me();
        let mut touched: Vec<String> = Vec::new();
        let mut notify: Vec<(String, String)> = Vec::new();
        let mut reactions: Vec<(String, String, String, String)> = Vec::new();
        for (info, message) in batch {
            let src = &info.source;
            let from_me = src.is_from_me;
            let key_chat = src.chat.to_non_ad_string();
            let key_sender = src.sender.to_non_ad_string();
            let r = self.store().batch(|db| {
                // A chat or a sender by hidden id comes with their number beside it: the pair is kept, and they're
                // kept by the number.
                let alt = if from_me { src.recipient_alt.as_ref() } else { src.sender_alt.as_ref() };
                if let Some(alt) = alt.filter(|a| a.is_pn())
                    && src.chat.is_lid() {
                        Store::set_lid(db, &key_chat, &alt.to_non_ad_string())?;
                    }
                if let Some(alt) = src.sender_alt.as_ref().filter(|a| a.is_pn())
                    && src.sender.is_lid() {
                        Store::set_lid(db, &key_sender, &alt.to_non_ad_string())?;
                    }
                // Addressed by number, the hidden id beside it: the pair is kept too.
                if let Some(alt) = src.sender_alt.as_ref().filter(|a| a.is_lid())
                    && src.sender.is_pn() {
                        Store::set_lid(db, &alt.to_non_ad_string(), &key_sender)?;
                    }
                if let Some(alt) = src.recipient_alt.as_ref().filter(|a| a.is_lid())
                    && src.chat.is_pn() {
                        Store::set_lid(db, &alt.to_non_ad_string(), &key_chat)?;
                    }
                let chat = Store::canonical(db, &key_chat);
                if chat.ends_with("@broadcast") || chat.ends_with("@newsletter") {
                    return Ok(None);
                }
                let sender = if from_me { me.clone() } else { Store::canonical(db, &key_sender) };
                if !from_me && !info.push_name.is_empty() {
                    Store::set_name(db, &sender, &info.push_name, store::NAME_PUSH)?;
                }
                // A message from a WhatsApp Business account carries its verified name.
                if !from_me && !src.is_group && info.verified_name.is_some() {
                    db.execute("INSERT OR REPLACE INTO state (key, value) VALUES (?1, 'true')", [format!("biz:{chat}")])?;
                }
                let who = if from_me { "me".to_string() } else { sender.clone() };
                let ts = info.timestamp.timestamp();
                if let Some(reacted) = apply_action(db, &chat, &who, ts * 1000, message)? {
                    return Ok(Some((chat, None, reacted.map(|(id, e)| (id, who, e)))));
                }
                let Some(m) = to_msg(db, &chat, &info.id, from_me, &sender, &info.push_name, ts, message, &key_chat, &key_sender) else {
                    return Ok(Some((chat, None, None)));
                };
                let added = Store::add_message(db, &m)?;
                if added && !from_me {
                    Store::bump_unread(db, &chat)?;
                }
                Ok(Some((chat, (added && !from_me).then(|| m.id.clone()), None)))
            });
            match r {
                Ok(Some((chat, new, reacted))) => {
                    if let Some(id) = new {
                        notify.push((chat.clone(), id));
                    }
                    if let Some((id, who, e)) = reacted {
                        reactions.push((chat.clone(), id, who, e));
                    }
                    if !touched.contains(&chat) {
                        touched.push(chat);
                    }
                }
                Ok(None) => {}
                Err(e) => warn!("message: {e}"),
            }
        }
        for chat in &touched {
            self.emit(ToUi::Messages(chat.clone()));
        }
        for (chat, id) in notify {
            self.emit(ToUi::Notify(chat, id));
        }
        for (c, id, who, e) in reactions {
            self.emit(ToUi::Reaction(c, id, who, e));
        }
        if !touched.is_empty() {
            self.emit(ToUi::Chats);
        }
    }

    /// Mine delivered, read or played. In a group a receipt comes from one participant: any of them means the
    /// server took it, and only that is told (delivered and read need everyone, which isn't followed). Read by me on
    /// another device, the chat's count goes.
    fn on_receipt(&self, r: &whatsapp_rust::wacore::types::events::Receipt) {
        use whatsapp_rust::wacore::types::presence::ReceiptType;
        let chat = self.store().canonical_of(&r.source.chat.to_non_ad_string());
        let status = match r.r#type {
            ReceiptType::Delivered => 3,
            ReceiptType::Read | ReceiptType::Played => 4,
            ReceiptType::ReadSelf | ReceiptType::PlayedSelf => {
                let _ = self.store().batch(|db| Store::set_unread(db, &chat, 0));
                self.emit(ToUi::Chats);
                return;
            }
            _ => return,
        };
        let group = crate::text::is_group(&chat);
        let ids: Vec<String> = r.message_ids.iter().map(|i| i.to_string()).collect();
        let rows: Vec<Msg> = ids.iter().filter_map(|id| self.store().message(&chat, id)).collect();
        let _ = self.store().batch(|db| {
            let mut read = Vec::new();
            for m in rows.iter().filter(|m| m.from_me) {
                let s = if group { 2 } else { status };
                Store::set_status(db, &chat, &m.id, s)?;
                if s >= 4 {
                    read.push((chat.clone(), m.ts));
                }
            }
            Self::settle_reads(db, &read)
        });
        self.emit(ToUi::Messages(chat));
    }

    /// The downloads, one at a time: opening an old chat asks for dozens at once, which clogged the connection.
    async fn download_loop(self, mut rx: UnboundedReceiver<Job>) {
        while let Some(job) = rx.recv().await {
            match job {
                Job::Media(chat, id) => {
                    // One that expired on WhatsApp's servers waits for the phone to send it again, out of the queue,
                    // so the others don't wait behind it.
                    if self.download_one(&chat, &id).await {
                        let wa = self.clone();
                        tokio::spawn(async move {
                            wa.reupload_one(&chat, &id).await;
                            wa.inner.live.lock().unwrap().downloading.remove(&format!("{chat}/{id}"));
                            wa.emit(ToUi::Messages(chat));
                        });
                        continue;
                    }
                    self.inner.live.lock().unwrap().downloading.remove(&format!("{chat}/{id}"));
                    self.emit(ToUi::Messages(chat));
                }
                Job::Preview(chat, id) => {
                    self.preview_one(&chat, &id).await;
                    self.inner.live.lock().unwrap().downloading.remove(&format!("link:{chat}/{id}"));
                    self.emit(ToUi::Messages(chat));
                }
            }
        }
    }

    async fn fetch_media(client: &Arc<Client>, kind: &str, media: &[u8], direct_path: Option<&str>) -> Result<Vec<u8>> {
        macro_rules! get {
            ($t:ty) => {{
                let mut m = <$t>::decode_from_slice(media).map_err(|e| anyhow!("{e}"))?;
                if let Some(p) = direct_path {
                    m.direct_path = Some(p.to_string());
                }
                client.download(&m).await
            }};
        }
        match kind {
            "sticker" => get!(wa::message::StickerMessage),
            "image" => get!(wa::message::ImageMessage),
            "video" | "gif" | "ptv" => get!(wa::message::VideoMessage),
            "voice" | "audio" => get!(wa::message::AudioMessage),
            "document" => get!(wa::message::DocumentMessage),
            _ => Err(anyhow!("no attachment")),
        }
    }

    /// The media key of an attachment's message, for asking the phone to send it again.
    fn media_key(kind: &str, media: &[u8]) -> Option<Vec<u8>> {
        match kind {
            "sticker" => wa::message::StickerMessage::decode_from_slice(media).ok()?.media_key,
            "image" => wa::message::ImageMessage::decode_from_slice(media).ok()?.media_key,
            "video" | "gif" | "ptv" => wa::message::VideoMessage::decode_from_slice(media).ok()?.media_key,
            "voice" | "audio" => wa::message::AudioMessage::decode_from_slice(media).ok()?.media_key,
            "document" => wa::message::DocumentMessage::decode_from_slice(media).ok()?.media_key,
            _ => None,
        }
    }

    /// An attachment into the chat's media folder; one expired on WhatsApp's servers is asked of the phone again;
    /// one that can't be had is marked so.
    /// Downloads an attachment; true when it failed in a way the phone can mend (it's asked again, apart).
    async fn download_one(&self, chat: &str, id: &str) -> bool {
        let (Some(m), Some(client)) = (self.row(chat, id), self.client()) else { return false };
        let Some(media) = m.media.clone() else { return false };
        match Self::fetch_media(&client, &m.kind, &media, None).await {
            Ok(bytes) => {
                self.keep_media(&m, &bytes);
                false
            }
            Err(_) if Self::media_key(&m.kind, &media).is_some() => true,
            Err(e) => {
                warn!("download {chat}/{id}: {e}");
                let _ = self.store().set_media_err(chat, id);
                false
            }
        }
    }

    fn keep_media(&self, m: &Msg, bytes: &[u8]) {
        let dir = media_dir(&self.inner.media, &m.chat);
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join(format!("{}.{}", safe(&m.id), ext_for(m.media_mime.as_deref(), m.media_name.as_deref())));
        if std::fs::write(&file, bytes).is_ok() {
            let _ = self.store().set_media(&m.chat, &m.id, &file.to_string_lossy(), m.w, m.h);
        }
    }

    /// An attachment expired on WhatsApp's servers: the phone is asked to send it again, and it's downloaded from
    /// where it says; marked as not to be had when it can't.
    async fn reupload_one(&self, chat: &str, id: &str) {
        let (Some(m), Some(client)) = (self.row(chat, id), self.client()) else { return };
        let Some(media) = m.media.clone() else { return };
        let mut got: Result<Vec<u8>> = Err(anyhow!("no media key"));
        {
            if let (Some(key), Some(jid)) = (Self::media_key(&m.kind, &media), Self::jid_of(&m)) {
                let participant = (crate::text::is_group(&m.chat) && !m.from_me).then(|| m.key_sender.parse::<Jid>().ok()).flatten();
                let req = whatsapp_rust::MediaReuploadRequest {
                    msg_id: &m.id,
                    chat_jid: &jid,
                    media_key: &key,
                    is_from_me: m.from_me,
                    participant: participant.as_ref(),
                };
                got = match client.media_reupload().request(&req).await {
                    Ok(whatsapp_rust::MediaRetryResult::Success { direct_path }) => Self::fetch_media(&client, &m.kind, &media, Some(&direct_path)).await,
                    Ok(other) => Err(anyhow!("{other:?}")),
                    Err(e) => Err(anyhow!("{e}")),
                };
            }
        }
        match got {
            Ok(bytes) => self.keep_media(&m, &bytes),
            Err(e) => {
                warn!("download {chat}/{id}: {e}");
                let _ = self.store().set_media_err(chat, id);
            }
        }
    }

    /// A link preview's picture: the full one the sender's phone uploaded, or else the small one in the message; a
    /// mark when neither comes, so it isn't asked again.
    async fn preview_one(&self, chat: &str, id: &str) {
        let file = preview_path(&self.inner.media, chat, id);
        let Some(m) = self.row(chat, id) else { return };
        let x = m.raw.as_deref().and_then(|r| wa::Message::decode_from_slice(r).ok()).and_then(|msg| base_of(&msg).extended_text_message.as_option().cloned());
        let mut bytes: Option<Vec<u8>> = None;
        if let (Some(x), Some(client)) = (&x, self.client())
            && let (Some(path), Some(key), Some(sha), Some(enc)) = (&x.thumbnail_direct_path, &x.media_key, &x.thumbnail_sha256, &x.thumbnail_enc_sha256) {
                let params = whatsapp_rust::download::DownloadParams::encrypted(path.clone(), key, sha, enc, 0, whatsapp_rust::download::MediaType::LinkThumbnail);
                match client.download_from_params(&params).await {
                    Ok(b) if !b.is_empty() => bytes = Some(b),
                    Ok(_) => {}
                    Err(e) => warn!("link preview {id}: {e}"),
                }
            }
        if bytes.is_none() {
            bytes = x.and_then(|x| x.jpeg_thumbnail).filter(|b| !b.is_empty());
        }
        let _ = std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")));
        match bytes {
            Some(b) => {
                let _ = std::fs::write(&file, b);
            }
            None => {
                let _ = std::fs::write(file.with_extension("jpg.none"), b"");
            }
        }
    }

    /// The profiles, one at a time.
    async fn profile_loop(self, mut rx: UnboundedReceiver<String>) {
        while let Some(jid) = rx.recv().await {
            while !self.is_open() {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            self.profile_one(&jid).await;
            self.inner.live.lock().unwrap().profile_asked.remove(&jid);
        }
    }

    async fn profile_one(&self, jid: &str) {
        let Some(client) = self.client() else { return };
        let Ok(who) = jid.parse::<Jid>() else { return };
        let fetched = chrono::Utc::now().timestamp();
        let mut p = store::Profile { fetched, ..Default::default() };
        if crate::text::is_group(jid) {
            if let Ok(meta) = client.groups().get_metadata(&who).await {
                p.members = Some(meta.participants.len() as u32);
            }
        } else if let Ok(info) = client.contacts().get_user_info(std::slice::from_ref(&who)).await
            && let Some(u) = info.values().next() {
                p.about = u.status.clone().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
                let _ = self.store().set_state(&format!("biz:{jid}"), if u.is_business { "true" } else { "false" });
            }
        if let Ok(Some(pic)) = client.contacts().get_profile_picture(&who, false).await {
            let dir = media_dir(&self.inner.media, jid);
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join("profile.jpg");
            let got = tokio::task::spawn_blocking({
                let (url, path) = (pic.url.clone(), path.clone());
                move || -> Result<()> {
                    let bytes = ureq::get(&url).call()?.body_mut().with_config().limit(20 << 20).read_to_vec()?;
                    std::fs::write(&path, bytes)?;
                    Ok(())
                }
            })
            .await;
            match got {
                Ok(Ok(())) => p.avatar = Some(path.to_string_lossy().into_owned()),
                Ok(Err(e)) => warn!("photo download: {e}"),
                Err(e) => warn!("photo download: {e}"),
            }
        }
        let _ = self.store().set_profile(jid, &p);
        self.emit(ToUi::Profile(jid.to_string()));
    }

    /// A message to send, with its mentions and the message it answers.
    fn compose(&self, text: &str, reply: Option<&Msg>, mentions: &[String]) -> wa::Message {
        let mut ctx = wa::ContextInfo::default();
        if let Some(q) = reply {
            let quoted = q.raw.as_deref().and_then(|r| wa::Message::decode_from_slice(r).ok()).unwrap_or_else(|| wa::Message::text(q.text.clone()));
            let participant = if q.from_me { self.me() } else if q.key_sender.is_empty() { q.sender.clone() } else { q.key_sender.clone() };
            ctx = whatsapp_rust::wacore::proto_helpers::build_quote_context(q.id.clone(), participant, &quoted);
        }
        ctx.mentioned_jid = mentions.to_vec();
        if reply.is_none() && mentions.is_empty() {
            wa::Message::text(text.to_string())
        } else {
            wa::Message::text_with_context(text.to_string(), ctx)
        }
    }

    /// A message just sent into the store, as a copy of it will come back.
    fn keep_sent(&self, chat: &str, id: &str, message: &wa::Message) {
        let me = self.me();
        let ts = chrono::Utc::now().timestamp();
        let r = self.store().batch(|db| {
            if let Some(mut m) = to_msg(db, chat, id, true, &me, "", ts, message, chat, &me) {
                m.status = 1;
                Store::add_message(db, &m)?;
            }
            Ok(())
        });
        if let Err(e) = r {
            warn!("sent: {e}");
        }
        self.emit(ToUi::Messages(chat.to_string()));
        self.emit(ToUi::Chats);
    }
}

impl Backend for Wa {
    fn send(&self, chat: &str, text: &str, reply_to: Option<&str>, mentions: &[String]) {
        let Some(client) = self.with_client() else { return };
        let reply = reply_to.and_then(|id| self.row(chat, id));
        let message = self.compose(text, reply.as_ref(), mentions);
        let (wa, chat, sink) = (self.clone(), chat.to_string(), self.sink());
        tokio::spawn(async move {
            let Ok(jid) = chat.parse::<Jid>() else { return };
            match client.send_message(jid, message.clone()).await {
                Ok(sent) => {
                    wa.keep_sent(&chat, &sent.message_id.to_string(), &message);
                    let _ = wa.store().batch(|db| Store::set_status(db, &chat, &sent.message_id.to_string(), 2));
                }
                Err(e) => Self::fail_to(&sink, "send", e),
            }
        });
    }

    fn react(&self, chat: &str, id: &str, emoji: &str) {
        let Some(m) = self.row(chat, id) else { return self.refuse("unknownMessage") };
        let Some(client) = self.with_client() else { return };
        let key = self.key_of(&m);
        let (wa, chat, id, emoji, sink) = (self.clone(), chat.to_string(), id.to_string(), emoji.to_string(), self.sink());
        tokio::spawn(async move {
            let Some(jid) = Self::jid_of(&m) else { return };
            match client.send_reaction(jid, key, &emoji).await {
                Ok(_) => {
                    let ts = chrono::Utc::now().timestamp_millis();
                    let _ = wa.store().batch(|db| Store::set_reaction(db, &chat, &id, "me", &emoji, ts));
                    wa.emit(ToUi::Messages(chat.clone()));
                    wa.emit(ToUi::Reaction(chat, id, "me".into(), emoji));
                }
                Err(e) => Self::fail_to(&sink, "reaction", e),
            }
        });
    }

    fn edit(&self, chat: &str, id: &str, text: &str) {
        let Some(m) = self.row(chat, id).filter(|m| m.from_me) else { return self.refuse("onlyOwnEdit") };
        let Some(client) = self.with_client() else { return };
        let (wa, chat, id, text, sink) = (self.clone(), chat.to_string(), id.to_string(), text.to_string(), self.sink());
        tokio::spawn(async move {
            let Some(jid) = Self::jid_of(&m) else { return };
            match client.edit_message(jid, id.clone(), wa::Message::text(text.clone())).await {
                Ok(_) => {
                    let _ = wa.store().batch(|db| Store::edit(db, &chat, &id, "text", &text));
                    wa.emit(ToUi::Messages(chat));
                    wa.emit(ToUi::Chats);
                }
                Err(e) => Self::fail_to(&sink, "edit", e),
            }
        });
    }

    fn delete(&self, chat: &str, id: &str, for_everyone: bool) {
        let Some(m) = self.row(chat, id) else { return self.refuse("messageGone") };
        if for_everyone && !m.from_me {
            return self.refuse("onlyOwnDelete");
        }
        let Some(client) = self.with_client() else { return };
        let (wa, sink) = (self.clone(), self.sink());
        tokio::spawn(async move {
            let Some(jid) = Self::jid_of(&m) else { return };
            let done = if for_everyone {
                client
                    .revoke_message(jid, m.id.clone(), whatsapp_rust::send::RevokeType::Sender)
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|_| wa.store().batch(|db| Store::revoke(db, &m.chat, &m.id)).map_err(|e| e.to_string()))
            } else {
                let sender = (crate::text::is_group(&m.chat) && !m.from_me).then(|| m.key_sender.parse::<Jid>().ok()).flatten();
                client
                    .chat_actions()
                    .delete_message_for_me(&jid, sender.as_ref(), &m.id, m.from_me, false, Some(m.ts))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|_| wa.store().batch(|db| Store::forget(db, &m.chat, &m.id)).map_err(|e| e.to_string()))
            };
            match done {
                Ok(()) => {
                    wa.emit(ToUi::Messages(m.chat));
                    wa.emit(ToUi::Chats);
                }
                Err(e) => Self::fail_to(&sink, "delete", e),
            }
        });
    }

    /// Asks the phone for up to 50 messages before the oldest kept; its answer comes back to whoever asked, by the
    /// request's id, and only what lands before the old anchor counts. Unanswered in 30 s, it can be asked again.
    fn fetch_older(&self, chat: &str) {
        let sink = self.sink();
        let Some(client) = self.client().filter(|_| self.is_open()) else {
            let _ = sink.send(ToUi::Error(s("notConnectedYet")));
            let _ = sink.send(ToUi::Older(chat.to_string(), None));
            return;
        };
        let Some(oldest) = self.store().oldest(chat) else {
            let _ = sink.send(ToUi::Older(chat.to_string(), Some(0)));
            return;
        };
        let (wa, chat, count) = (self.clone(), chat.to_string(), self.store().count(chat));
        tokio::spawn(async move {
            let Some(jid) = Self::jid_of(&oldest) else { return };
            match client.fetch_message_history(&jid, &oldest.id, oldest.from_me, oldest.ts * 1000, 50).await {
                Ok(session) => {
                    wa.inner.live.lock().unwrap().older.insert(session.clone(), (chat.clone(), oldest.ts, count, sink.clone()));
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    if wa.inner.live.lock().unwrap().older.remove(&session).is_some() {
                        warn!("older messages: no answer from the phone");
                        let _ = sink.send(ToUi::Older(chat, None));
                    }
                }
                Err(e) => {
                    warn!("older messages: {e}");
                    let _ = sink.send(ToUi::Older(chat, None));
                }
            }
        });
    }

    fn mark_read(&self, chat: &str) {
        let Ok(rows) = self.store().mark_read(chat) else { return };
        self.emit(ToUi::Chats);
        if rows.is_empty() {
            return;
        }
        let Some(client) = self.client().filter(|_| self.is_open()) else { return };
        tokio::spawn(async move {
            // By chat and sender as WhatsApp named them.
            type Group = ((String, Option<String>), Vec<String>);
            let mut groups: Vec<Group> = Vec::new();
            for m in rows {
                let chat = if m.key_chat.is_empty() { m.chat.clone() } else { m.key_chat.clone() };
                let sender = crate::text::is_group(&m.chat).then(|| if m.key_sender.is_empty() { m.sender.clone() } else { m.key_sender.clone() });
                let k = (chat, sender);
                match groups.iter_mut().find(|(g, _)| *g == k) {
                    Some((_, ids)) => ids.push(m.id),
                    None => groups.push((k, vec![m.id])),
                }
            }
            for ((chat, sender), ids) in groups {
                let Ok(jid) = chat.parse::<Jid>() else { continue };
                let sender = sender.and_then(|s| s.parse::<Jid>().ok());
                let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
                if let Err(e) = client.mark_as_read(&jid, sender.as_ref(), &ids).await {
                    warn!("read receipts: {e}");
                }
            }
        });
    }

    fn ensure_media(&self, chat: &str, id: &str) {
        let key = format!("{chat}/{id}");
        if self.client().is_none() || !self.inner.live.lock().unwrap().downloading.insert(key) {
            return;
        }
        let _ = self.inner.downloads.send(Job::Media(chat.into(), id.into()));
    }

    fn ensure_preview(&self, chat: &str, id: &str) {
        let file = preview_path(&self.inner.media, chat, id);
        if file.exists() || file.with_extension("jpg.none").exists() || self.client().is_none() {
            return;
        }
        if !self.inner.live.lock().unwrap().downloading.insert(format!("link:{chat}/{id}")) {
            return;
        }
        let _ = self.inner.downloads.send(Job::Preview(chat.into(), id.into()));
    }

    fn ensure_profile(&self, jid: &str) {
        if self.store().profile(jid).is_some_and(|p| chrono::Utc::now().timestamp() - p.fetched < 86400) {
            return;
        }
        if self.inner.live.lock().unwrap().profile_asked.insert(jid.to_string()) {
            let _ = self.inner.profiles.send(jid.to_string());
        }
    }

    fn ensure_name(&self, jid: &str) {
        if crate::text::is_group(jid) || self.store().has_name(jid) {
            return;
        }
        let Some(client) = self.client().filter(|_| self.is_open()) else { return };
        let asked = format!("nameAsked:{jid}");
        let now = chrono::Utc::now().timestamp();
        if self.store().state(&asked).and_then(|v| v.parse::<i64>().ok()).is_some_and(|t| now - t < 86400) {
            return;
        }
        let Some(newest) = self.store().newest(jid) else { return };
        let _ = self.store().set_state(&asked, &now.to_string());
        tokio::spawn(async move {
            let Some(j) = Self::jid_of(&newest) else { return };
            if let Err(e) = client.fetch_message_history(&j, &newest.id, newest.from_me, newest.ts * 1000, 50).await {
                warn!("name: {e}");
            }
        });
    }

    fn set_composing(&self, chat: &str, on: bool) {
        // While I write, the local model is left to the writing suggestions.
        if on {
            self.inner.moods.writing();
        }
        let Some(client) = self.client().filter(|_| self.is_open()) else { return };
        let Ok(jid) = chat.parse::<Jid>() else { return };
        tokio::spawn(async move {
            let r = if on { client.chatstate().send_composing(&jid).await } else { client.chatstate().send_paused(&jid).await };
            if let Err(e) = r {
                warn!("composing: {e}");
            }
        });
    }

    fn subscribe_presence(&self, chat: &str) {
        let me = self.me();
        let (known, members) = {
            let mut live = self.inner.live.lock().unwrap();
            live.subscribed.insert(chat.to_string());
            (live.online.get(chat).copied().unwrap_or(false), None::<Vec<String>>)
        };
        let _ = members;
        self.subscribe_one(chat);
        if known {
            self.emit(ToUi::Presence(chat.to_string(), true));
        }
        // A group's presence only brings who's typing in it: who's online is each member's own, so the 30 who wrote
        // most recently are followed one by one.
        if crate::text::is_group(chat) {
            let members: Vec<String> = self.store().recent_senders(chat, 30).into_iter().filter(|j| *j != me).collect();
            let new: Vec<String> = {
                let mut live = self.inner.live.lock().unwrap();
                live.group_members.insert(chat.to_string(), members.clone());
                members.into_iter().filter(|m| live.subscribed.insert(m.clone())).collect()
            };
            for m in new {
                self.subscribe_one(&m);
            }
            self.count_group(chat, true);
        }
        let available = self.inner.live.lock().unwrap().available;
        self.emit(ToUi::Available(available));
    }

    fn touch_presence(&self, pid: u32) {
        let generation = {
            let mut live = self.inner.live.lock().unwrap();
            if live.focus.get(&pid) == Some(&false) {
                return;
            }
            live.presence_gen += 1;
            live.presence_gen
        };
        let wa = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(120)).await;
            if wa.inner.live.lock().unwrap().presence_gen == generation {
                wa.set_available(false);
            }
        });
        self.set_available(true);
    }

    fn set_focus(&self, pid: u32, focused: bool) {
        {
            let mut live = self.inner.live.lock().unwrap();
            live.focus.insert(pid, focused);
            if focused {
                return;
            }
            let others = live.focus.iter().any(|(p, f)| *f && *p != pid && crate::ipc::alive(*p));
            if others {
                return;
            }
            live.presence_gen += 1;
        }
        self.set_available(false);
    }

    fn send_file(&self, chat: &str, path: &str, caption: &str) {
        let Some(client) = self.with_client() else { return };
        let (wa, chat, path, caption, sink) = (self.clone(), chat.to_string(), PathBuf::from(path), caption.to_string(), self.sink());
        tokio::spawn(async move {
            let r: Result<()> = async {
                let jid: Jid = chat.parse().map_err(|e| anyhow!("{e}"))?;
                let data = tokio::fs::read(&path).await?;
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                let mime = mime_for(&ext).to_string();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
                use whatsapp_rust::download::MediaType;
                let media_type = match ext.as_str() {
                    "jpg" | "jpeg" | "png" | "webp" => MediaType::Image,
                    "mp4" => MediaType::Video,
                    "mp3" | "ogg" | "m4a" => MediaType::Audio,
                    _ => MediaType::Document,
                };
                let up = client.upload(data.clone(), media_type, Default::default()).await?;
                let caption = Some(caption).filter(|c| !c.is_empty());
                // What the other side shows before downloading: a picture's size and small copy, a video's first
                // frame (with ffmpeg), a voice's length.
                let (thumb, w, h) = match media_type {
                    MediaType::Image => preview_of_image(&data),
                    MediaType::Video => (frame_of_video(&path), None, None),
                    _ => (None, None, None),
                };
                let seconds = if media_type == MediaType::Audio { audio_seconds(&path, &data) } else { None };
                let message = match media_type {
                    MediaType::Image => wa::Message {
                        image_message: whatsapp_rust::waproto::buffa::MessageField::some(wa::message::ImageMessage {
                            url: Some(up.url.clone()), direct_path: Some(up.direct_path.clone()), media_key: Some(up.media_key.to_vec()),
                            file_sha256: Some(up.file_sha256.to_vec()), file_enc_sha256: Some(up.file_enc_sha256.to_vec()),
                            file_length: Some(up.file_length), mimetype: Some(mime.clone()), caption,
                            jpeg_thumbnail: thumb.clone(), width: w, height: h, ..Default::default()
                        }),
                        ..Default::default()
                    },
                    MediaType::Video => wa::Message {
                        video_message: whatsapp_rust::waproto::buffa::MessageField::some(wa::message::VideoMessage {
                            url: Some(up.url.clone()), direct_path: Some(up.direct_path.clone()), media_key: Some(up.media_key.to_vec()),
                            file_sha256: Some(up.file_sha256.to_vec()), file_enc_sha256: Some(up.file_enc_sha256.to_vec()),
                            file_length: Some(up.file_length), mimetype: Some(mime.clone()), caption, jpeg_thumbnail: thumb.clone(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    MediaType::Audio => wa::Message {
                        audio_message: whatsapp_rust::waproto::buffa::MessageField::some(wa::message::AudioMessage {
                            url: Some(up.url.clone()), direct_path: Some(up.direct_path.clone()), media_key: Some(up.media_key.to_vec()),
                            file_sha256: Some(up.file_sha256.to_vec()), file_enc_sha256: Some(up.file_enc_sha256.to_vec()),
                            file_length: Some(up.file_length), mimetype: Some(mime.clone()), seconds, ..Default::default()
                        }),
                        ..Default::default()
                    },
                    _ => wa::Message {
                        document_message: whatsapp_rust::waproto::buffa::MessageField::some(wa::message::DocumentMessage {
                            url: Some(up.url.clone()), direct_path: Some(up.direct_path.clone()), media_key: Some(up.media_key.to_vec()),
                            file_sha256: Some(up.file_sha256.to_vec()), file_enc_sha256: Some(up.file_enc_sha256.to_vec()),
                            file_length: Some(up.file_length), mimetype: Some(mime.clone()), file_name: Some(name.clone()), caption,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                };
                let sent = client.send_message(jid, message.clone()).await?;
                let id = sent.message_id.to_string();
                wa.keep_sent(&chat, &id, &message);
                let dir = media_dir(&wa.inner.media, &chat);
                std::fs::create_dir_all(&dir)?;
                let file = dir.join(format!("{}.{}", safe(&id), if ext.is_empty() { ext_for(Some(&mime), Some(&name)) } else { ext }));
                std::fs::copy(&path, &file)?;
                wa.store().set_media(&chat, &id, &file.to_string_lossy(), 0, 0)?;
                wa.emit(ToUi::Messages(chat.clone()));
                Ok(())
            }
            .await;
            if let Err(e) = r {
                Self::fail_to(&sink, "send file", e);
            }
        });
    }

    fn download_all(&self, chat: &str) {
        let name: String = self.store().name_of(chat).chars().map(|c| if c.is_alphanumeric() || " _.-".contains(c) { c } else { '_' }).collect();
        let out = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Downloads/wa").join(name);
        if let Err(e) = std::fs::create_dir_all(&out) {
            return Self::fail_to(&self.sink(), "download all", e);
        }
        let (mut copied, mut pending) = (0, 0);
        for m in self.store().media_messages(chat) {
            let Some(file) = m.media_path.clone() else {
                if !m.media_err {
                    self.ensure_media(chat, &m.id);
                    pending += 1;
                }
                continue;
            };
            let base = m.media_name.clone().unwrap_or_else(|| Path::new(&file).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
            let day = chrono::DateTime::from_timestamp(m.ts, 0).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
            let dest = out.join(format!("{day}_{base}"));
            if !dest.exists() && std::fs::copy(&file, &dest).is_ok() {
                copied += 1;
            }
        }
        let _ = self.sink().send(ToUi::Status(format!("{copied} → {} ({pending} …)", out.display())));
    }
}
