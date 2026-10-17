//! What wassup keeps of the chats: their names, their messages (each also as it came, to answer, react to or
//! mark it read as WhatsApp addressed it), where each attachment was saved, and the screen's state. whatsapp-rust
//! keeps its own session (keys, devices) in a database of its own; this one is only the client's view, as wassup's
//! db.ts. Several processes share it (one connected, the other terminals reading), so a write waits for another
//! instead of failing.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use crate::i18n::{s, t};

/// A message as the screen draws it.
#[derive(Clone, Debug, Default)]
pub struct Msg {
    pub chat: String,
    pub id: String,
    pub from_me: bool,
    /// Who sent it, in a group; the chat itself otherwise.
    pub sender: String,
    pub push_name: String,
    /// Seconds since the epoch.
    pub ts: i64,
    /// "text", "image", "sticker", "video", "gif", "voice", "audio", "document", "location", "contact", "poll",
    /// "deleted", or a kind the screen has no drawing of its own for ("pinInChat", "album", "ptv", …).
    pub kind: String,
    /// The text, or an attachment's caption, a voice message's length, a location's name and map link.
    pub text: String,
    /// The small JPEG the message carries, for pictures, videos and documents.
    pub thumb: Option<Vec<u8>>,
    /// The attachment's own message (protobuf), to download it from.
    pub media: Option<Vec<u8>>,
    /// Where the downloaded attachment is, once it is; its type and file name as sent; whether it couldn't be had.
    pub media_path: Option<String>,
    pub media_mime: Option<String>,
    pub media_name: Option<String>,
    pub media_err: bool,
    pub w: u32,
    pub h: u32,
    /// Mine: -1 failed to send, 1 on its way, 2 at the server, 3 delivered, 4 read (or played); 0 for theirs.
    pub status: i32,
    /// The message it answers: who sent it and its first line, tab between.
    pub quoted: Option<String>,
    /// The message as it came (wa::Message, encoded), and how WhatsApp addressed it: its chat and sender as they
    /// were (a hidden id, perhaps), for answering, reacting and receipts. Not read with the chat's messages.
    pub raw: Option<Vec<u8>>,
    pub key_chat: String,
    pub key_sender: String,
    /// A text with a link whose preview carries a picture.
    pub preview: bool,
}

/// A chat as the list draws it.
#[derive(Clone, Debug)]
pub struct Chat {
    pub jid: String,
    pub name: String,
    pub last_ts: i64,
    /// -1 when marked unread on the phone with nothing new: a bell, no count.
    pub unread: i32,
    pub archived: bool,
}

/// What WhatsApp says of a chat, asked at most once a day: its picture's file (None with none to be seen), its
/// "about", a group's size, and when that was.
#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub avatar: Option<String>,
    pub about: Option<String>,
    pub members: Option<u32>,
    pub fetched: i64,
}

/// How sure a name is: a contact's own name in the phone wins over the name people give themselves.
pub const NAME_PUSH: i32 = 1;
pub const NAME_CONTACT: i32 = 2;

/// WhatsApp sends the masked number ("+351∙∙∙∙∙∙∙35") as the name of someone not saved: that's not a name.
pub fn looks_like_number(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || " +∙·.()-".contains(c))
}

pub struct Store {
    db: Mutex<Connection>,
}

/// An attachment's or a photo's file as kept: its place in the media folder, so the data folder can move. Only the
/// media folder's: one anywhere else isn't kept (None), and is had again.
fn rel(path: &str) -> Option<String> {
    let p = Path::new(path);
    let inside = if p.is_absolute() { p.strip_prefix(crate::config_dir().join("media")).ok() } else { Some(p) };
    let kept = inside.filter(|p| !p.as_os_str().is_empty() && p.components().all(|c| matches!(c, std::path::Component::Normal(_))));
    if kept.is_none() {
        log::warn!("store: {path} is outside the media folder, not kept");
    }
    kept.map(|p| p.to_string_lossy().into_owned())
}

/// A file kept by its place in the media folder, where that folder is now.
fn abs(path: Option<String>) -> Option<String> {
    path.map(|p| crate::config_dir().join("media").join(p).to_string_lossy().into_owned())
}

const MSG_COLS: &str = "chat, id, from_me, sender, push_name, ts, kind, text, thumb, media, media_path, w, h, status, quoted, media_mime, media_name, media_err, key_chat, key_sender, preview";

fn msg_row(r: &rusqlite::Row) -> rusqlite::Result<Msg> {
    Ok(Msg {
        chat: r.get(0)?,
        id: r.get(1)?,
        from_me: r.get(2)?,
        sender: r.get(3)?,
        push_name: r.get(4)?,
        ts: r.get(5)?,
        kind: r.get(6)?,
        text: r.get(7)?,
        thumb: r.get(8)?,
        media: r.get(9)?,
        media_path: abs(r.get(10)?),
        w: r.get(11)?,
        h: r.get(12)?,
        status: r.get(13)?,
        quoted: r.get(14)?,
        media_mime: r.get(15)?,
        media_name: r.get(16)?,
        media_err: r.get(17)?,
        key_chat: r.get(18)?,
        key_sender: r.get(19)?,
        preview: r.get(20)?,
        raw: None,
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(3))?;
        // fold(), for searching without accents or case (text.rs).
        db.create_scalar_function("fold", 1, rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
            Ok(crate::text::fold(&ctx.get::<String>(0)?))
        })?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS chats (
               jid TEXT PRIMARY KEY, name TEXT, last_ts INTEGER NOT NULL DEFAULT 0,
               unread INTEGER NOT NULL DEFAULT 0, archived INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS names (jid TEXT PRIMARY KEY, name TEXT NOT NULL, rank INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS messages (
               chat TEXT NOT NULL, id TEXT NOT NULL, from_me INTEGER NOT NULL, sender TEXT NOT NULL,
               push_name TEXT NOT NULL DEFAULT '', ts INTEGER NOT NULL, kind TEXT NOT NULL,
               text TEXT NOT NULL DEFAULT '', thumb BLOB, media BLOB, media_path TEXT,
               w INTEGER NOT NULL DEFAULT 0, h INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY (chat, id));
             CREATE INDEX IF NOT EXISTS messages_chat_ts ON messages (chat, ts);
             CREATE TABLE IF NOT EXISTS lids (lid TEXT PRIMARY KEY, pn TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS avatars (jid TEXT PRIMARY KEY, path TEXT, fetched INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS reactions (
               chat TEXT NOT NULL, msg_id TEXT NOT NULL, sender TEXT NOT NULL, emoji TEXT NOT NULL,
               PRIMARY KEY (chat, msg_id, sender));",
        )?;
        // Columns added after the first version: a database made by it gets them (the error, if it has them, ignored).
        // Mine kept before the state was: delivered, at least, as they're from the history.
        if db.execute("ALTER TABLE messages ADD COLUMN status INTEGER NOT NULL DEFAULT 0", []).is_ok() {
            db.execute("UPDATE messages SET status = 3 WHERE from_me = 1", [])?;
        }
        for col in [
            "messages ADD COLUMN quoted TEXT",
            "messages ADD COLUMN raw BLOB",
            "messages ADD COLUMN key_chat TEXT NOT NULL DEFAULT ''",
            "messages ADD COLUMN key_sender TEXT NOT NULL DEFAULT ''",
            "messages ADD COLUMN media_mime TEXT",
            "messages ADD COLUMN media_name TEXT",
            "messages ADD COLUMN media_err INTEGER NOT NULL DEFAULT 0",
            "messages ADD COLUMN preview INTEGER NOT NULL DEFAULT 0",
            "reactions ADD COLUMN ts INTEGER NOT NULL DEFAULT 0",
            "avatars ADD COLUMN about TEXT",
            "avatars ADD COLUMN members INTEGER",
        ] {
            let _ = db.execute(&format!("ALTER TABLE {col}"), []);
        }
        // The photos a first version kept were the small ones ("avatar-…"): asked again, whole, as wassup keeps them.
        let _ = db.execute("UPDATE avatars SET fetched = 0 WHERE path LIKE '%avatar-%'", []);
        // Files a first version kept by their whole path, which breaks once the data folder moves: by their place in
        // the media folder, as every other is kept (rel); one outside a media folder isn't kept, and is had again.
        for (table, key, column) in [("messages", "chat || ' ' || id", "media_path"), ("avatars", "jid", "path")] {
            let whole: Vec<(String, String)> = db
                .prepare(&format!("SELECT {key}, {column} FROM {table} WHERE {column} LIKE '/%'"))
                .and_then(|mut q| q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
                .unwrap_or_default();
            for (k, old) in whole {
                let new = old.rfind("/media/").map(|at| old[at + "/media/".len()..].to_string()).filter(|n| !n.is_empty());
                let _ = db.execute(&format!("UPDATE {table} SET {column} = ?1 WHERE {key} = ?2"), params![new, k]);
            }
        }
        Ok(Self { db: Mutex::new(db) })
    }

    /// Several writes at once, in one transaction: a history chunk brings thousands of messages.
    pub fn batch<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// A chat seen, with its name if it came with one; its last activity only moves forward.
    pub fn touch_chat(db: &Connection, jid: &str, name: Option<&str>, ts: i64) -> Result<()> {
        db.execute(
            "INSERT INTO chats (jid, name, last_ts) VALUES (?1, ?2, ?3)
             ON CONFLICT (jid) DO UPDATE SET name = COALESCE(?2, name), last_ts = MAX(last_ts, ?3)",
            params![jid, name.filter(|n| !n.is_empty()), ts],
        )?;
        Ok(())
    }

    pub fn set_unread(db: &Connection, jid: &str, unread: i32) -> Result<()> {
        db.execute("UPDATE chats SET unread = ?2 WHERE jid = ?1", params![jid, unread])?;
        Ok(())
    }

    pub fn set_archived(db: &Connection, jid: &str, archived: bool) -> Result<()> {
        db.execute("UPDATE chats SET archived = ?2 WHERE jid = ?1", params![jid, archived])?;
        Ok(())
    }

    /// A group's new name, as it changes.
    pub fn set_chat_name(db: &Connection, jid: &str, name: &str) -> Result<()> {
        db.execute("UPDATE chats SET name = ?2 WHERE jid = ?1", params![jid, name])?;
        Ok(())
    }

    /// Someone's hidden id ("…@lid") and their number ("…@s.whatsapp.net"): WhatsApp calls the same chat by either,
    /// and here it's always by the number once it's known. A pair seen for the first time brings what was kept under
    /// the hidden id over to the number.
    pub fn set_lid(db: &Connection, lid: &str, pn: &str) -> Result<()> {
        if !lid.ends_with("@lid") || !pn.ends_with("@s.whatsapp.net") {
            return Ok(());
        }
        if db.execute("INSERT OR IGNORE INTO lids (lid, pn) VALUES (?1, ?2)", params![lid, pn])? == 0 {
            return Ok(());
        }
        db.execute("UPDATE OR IGNORE messages SET chat = ?2 WHERE chat = ?1", params![lid, pn])?;
        db.execute("DELETE FROM messages WHERE chat = ?1", params![lid])?;
        db.execute("UPDATE messages SET sender = ?2 WHERE sender = ?1", params![lid, pn])?;
        db.execute(
            "INSERT INTO chats (jid, name, last_ts, unread, archived)
               SELECT ?2, name, last_ts, unread, archived FROM chats WHERE jid = ?1
             ON CONFLICT (jid) DO UPDATE SET name = COALESCE(chats.name, excluded.name),
               last_ts = MAX(chats.last_ts, excluded.last_ts), unread = chats.unread + excluded.unread",
            params![lid, pn],
        )?;
        db.execute("DELETE FROM chats WHERE jid = ?1", params![lid])?;
        db.execute("UPDATE OR IGNORE reactions SET chat = ?2 WHERE chat = ?1", params![lid, pn])?;
        db.execute("UPDATE OR IGNORE reactions SET sender = ?2 WHERE sender = ?1", params![lid, pn])?;
        db.execute("INSERT OR IGNORE INTO names (jid, name, rank) SELECT ?2, name, rank FROM names WHERE jid = ?1", params![lid, pn])?;
        Ok(())
    }

    /// The id a chat or a person is kept by: their number, when a hidden id's is known.
    pub fn canonical(db: &Connection, jid: &str) -> String {
        if !jid.ends_with("@lid") {
            return jid.to_string();
        }
        db.query_row("SELECT pn FROM lids WHERE lid = ?1", [jid], |r| r.get(0)).optional().ok().flatten().unwrap_or_else(|| jid.to_string())
    }

    /// Someone's hidden id, when their number's is known: groups mention people by it now.
    pub fn lid_of(&self, pn: &str) -> Option<String> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT lid FROM lids WHERE pn = ?1", [pn], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn canonical_of(&self, jid: &str) -> String {
        Self::canonical(&self.db.lock().unwrap(), jid)
    }

    /// Hidden ids with a name kept under them and no number known: to ask WhatsApp for theirs.
    pub fn unmapped_lids(&self) -> Vec<String> {
        let db = self.db.lock().unwrap();
        let Ok(mut q) = db.prepare("SELECT jid FROM names WHERE jid LIKE '%@lid' AND jid NOT IN (SELECT lid FROM lids)") else { return vec![] };
        q.query_map([], |r| r.get(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// A name for someone, kept unless a surer one is already there; a masked number only when there's none.
    pub fn set_name(db: &Connection, jid: &str, name: &str, rank: i32) -> Result<()> {
        if name.trim().is_empty() {
            return Ok(());
        }
        if looks_like_number(name) {
            db.execute("INSERT OR IGNORE INTO names (jid, name, rank) VALUES (?1, ?2, 0)", params![jid, name])?;
            return Ok(());
        }
        db.execute(
            "INSERT INTO names (jid, name, rank) VALUES (?1, ?2, ?3)
             ON CONFLICT (jid) DO UPDATE SET name = ?2, rank = ?3 WHERE ?3 >= rank",
            params![jid, name, rank],
        )?;
        Ok(())
    }

    /// Whether a message is kept.
    pub fn has_message(db: &Connection, chat: &str, id: &str) -> bool {
        db.query_row("SELECT 1 FROM messages WHERE chat = ?1 AND id = ?2", params![chat, id], |_| Ok(())).optional().ok().flatten().is_some()
    }

    /// Stores a message, or another copy of one kept (which updates it: its state only moves forward, its
    /// attachment's file stays); returns whether it was new. Its chat moves to its time.
    pub fn add_message(db: &Connection, m: &Msg) -> Result<bool> {
        let existed = Self::has_message(db, &m.chat, &m.id);
        db.execute(
            "INSERT INTO messages (chat, id, from_me, sender, push_name, ts, kind, text, thumb, media, w, h, status, quoted,
                                   raw, key_chat, key_sender, media_mime, media_name, media_path, preview)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
             ON CONFLICT (chat, id) DO UPDATE SET
               kind = excluded.kind, text = excluded.text,
               push_name = CASE WHEN excluded.push_name = '' THEN messages.push_name ELSE excluded.push_name END,
               quoted = COALESCE(excluded.quoted, messages.quoted),
               thumb = COALESCE(excluded.thumb, messages.thumb), media = COALESCE(excluded.media, messages.media),
               media_path = COALESCE(messages.media_path, excluded.media_path),
               media_mime = COALESCE(excluded.media_mime, messages.media_mime),
               media_name = COALESCE(excluded.media_name, messages.media_name),
               w = CASE WHEN excluded.w > 0 THEN excluded.w ELSE messages.w END,
               h = CASE WHEN excluded.h > 0 THEN excluded.h ELSE messages.h END,
               status = CASE
                 WHEN excluded.status = -1 THEN CASE WHEN messages.status <= 1 THEN -1 ELSE messages.status END
                 ELSE MAX(messages.status, excluded.status) END,
               raw = COALESCE(excluded.raw, messages.raw), preview = MAX(messages.preview, excluded.preview),
               key_chat = CASE WHEN excluded.key_chat = '' THEN messages.key_chat ELSE excluded.key_chat END,
               key_sender = CASE WHEN excluded.key_sender = '' THEN messages.key_sender ELSE excluded.key_sender END",
            params![
                m.chat, m.id, m.from_me, m.sender, m.push_name, m.ts, m.kind, m.text, m.thumb, m.media, m.w, m.h, m.status,
                m.quoted, m.raw, m.key_chat, m.key_sender, m.media_mime, m.media_name, m.media_path.as_deref().and_then(rel), m.preview
            ],
        )?;
        Self::touch_chat(db, &m.chat, None, m.ts)?;
        Ok(!existed)
    }

    /// A message's delivery state, which only moves forward: receipts arrive out of order (a delivery receipt from
    /// another of their devices after the read one). Failing to send is the exception, and only replaces pending.
    pub fn set_status(db: &Connection, chat: &str, id: &str, status: i32) -> Result<()> {
        db.execute(
            "UPDATE messages SET status = CASE WHEN ?3 = -1 THEN CASE WHEN status <= 1 THEN -1 ELSE status END
                                               ELSE MAX(status, ?3) END
             WHERE chat = ?1 AND id = ?2 AND from_me = 1",
            params![chat, id, status],
        )?;
        Ok(())
    }

    /// Someone who read one of my messages read the ones before it too: my sent or delivered ones up to `ts`.
    pub fn mark_read_before(db: &Connection, chat: &str, ts: i64) -> Result<()> {
        db.execute("UPDATE messages SET status = 4 WHERE chat = ?1 AND from_me = 1 AND status IN (2, 3) AND ts <= ?2", params![chat, ts])?;
        Ok(())
    }

    /// The same rules on what's kept, once per start: one-to-one, mine before one of mine read are read; in groups,
    /// mine pending with any message after them were taken by the server. Returns how many moved.
    pub fn repair_statuses(&self) -> usize {
        let db = self.db.lock().unwrap();
        let read = db
            .execute(
                "UPDATE messages SET status = 4 WHERE from_me = 1 AND status IN (2, 3) AND chat NOT LIKE '%@g.us'
                   AND ts <= (SELECT MAX(n.ts) FROM messages n WHERE n.chat = messages.chat AND n.from_me = 1 AND n.status >= 4)",
                [],
            )
            .unwrap_or(0);
        let sent = db
            .execute(
                "UPDATE messages SET status = 2 WHERE from_me = 1 AND status = 1 AND chat LIKE '%@g.us'
                   AND EXISTS (SELECT 1 FROM messages n WHERE n.chat = messages.chat AND n.ts > messages.ts)",
                [],
            )
            .unwrap_or(0);
        read + sent
    }

    /// Someone's reaction to a message; an empty one takes theirs away. The most recent wins (`ts` in milliseconds),
    /// whatever order they arrive in.
    pub fn set_reaction(db: &Connection, chat: &str, msg_id: &str, sender: &str, emoji: &str, ts: i64) -> Result<()> {
        if emoji.is_empty() {
            db.execute("DELETE FROM reactions WHERE chat = ?1 AND msg_id = ?2 AND sender = ?3 AND ts <= ?4", params![chat, msg_id, sender, ts])?;
        } else {
            db.execute(
                "INSERT INTO reactions (chat, msg_id, sender, emoji, ts) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (chat, msg_id, sender) DO UPDATE SET emoji = ?4, ts = ?5 WHERE ?5 >= reactions.ts",
                params![chat, msg_id, sender, emoji, ts],
            )?;
        }
        Ok(())
    }

    /// My reaction to a message, if any.
    pub fn my_reaction(&self, chat: &str, msg_id: &str) -> Option<String> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT emoji FROM reactions WHERE chat = ?1 AND msg_id = ?2 AND sender = 'me'", [chat, msg_id], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    /// A chat's reactions, by message: each emoji and how many reacted with it, in the order they came.
    pub fn reactions(&self, chat: &str) -> Result<std::collections::HashMap<String, Vec<(String, usize)>>> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare("SELECT msg_id, emoji FROM reactions WHERE chat = ?1 ORDER BY ts, rowid")?;
        let mut out: std::collections::HashMap<String, Vec<(String, usize)>> = std::collections::HashMap::new();
        for row in q.query_map([chat], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (id, emoji) = row?;
            let list = out.entry(id).or_default();
            match list.iter_mut().find(|(e, _)| *e == emoji) {
                Some((_, n)) => *n += 1,
                None => list.push((emoji, 1)),
            }
        }
        Ok(out)
    }

    /// A message deleted for everyone: nothing of it is shown.
    pub fn revoke(db: &Connection, chat: &str, id: &str) -> Result<()> {
        db.execute(
            "UPDATE messages SET kind = 'deleted', text = '', quoted = NULL, thumb = NULL, media = NULL WHERE chat = ?1 AND id = ?2",
            params![chat, id],
        )?;
        Ok(())
    }

    /// A message deleted only for me: gone from here, its reactions with it.
    pub fn forget(db: &Connection, chat: &str, id: &str) -> Result<()> {
        db.execute("DELETE FROM messages WHERE chat = ?1 AND id = ?2", params![chat, id])?;
        db.execute("DELETE FROM reactions WHERE chat = ?1 AND msg_id = ?2", params![chat, id])?;
        Ok(())
    }

    /// A message edited: its new kind and text, marked as edited, as wassup marks it.
    pub fn edit(db: &Connection, chat: &str, id: &str, kind: &str, text: &str) -> Result<()> {
        db.execute(
            "UPDATE messages SET kind = ?3, text = ?4 WHERE chat = ?1 AND id = ?2",
            params![chat, id, kind, format!("{text}\n{}", s("edited"))],
        )?;
        Ok(())
    }

    /// A value kept between sessions and shared by the terminals (who I am, each terminal's tabs, the moods…).
    pub fn state(&self, key: &str) -> Option<String> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT value FROM state WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_state(&self, key: &str, value: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT INTO state (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_state(&self, key: &str) {
        let _ = self.db.lock().unwrap().execute("DELETE FROM state WHERE key = ?1", [key]);
    }

    /// Every value whose key starts with `prefix`.
    pub fn states(&self, prefix: &str) -> Vec<(String, String)> {
        let db = self.db.lock().unwrap();
        let like = format!("{}%", prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let Ok(mut q) = db.prepare("SELECT key, value FROM state WHERE key LIKE ?1 ESCAPE '\\'") else { return vec![] };
        q.query_map([like], |r| Ok((r.get(0)?, r.get(1)?))).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// A chat's profile as last asked of WhatsApp.
    pub fn profile(&self, jid: &str) -> Option<Profile> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT path, about, members, fetched FROM avatars WHERE jid = ?1", [jid], |r| {
            Ok(Profile { avatar: abs(r.get(0)?), about: r.get(1)?, members: r.get(2)?, fetched: r.get(3)? })
        })
        .optional()
        .ok()
        .flatten()
    }

    pub fn set_profile(&self, jid: &str, p: &Profile) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT INTO avatars (jid, path, about, members, fetched) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (jid) DO UPDATE SET path = ?2, about = ?3, members = ?4, fetched = ?5",
            params![jid, p.avatar.as_deref().and_then(rel), p.about, p.members, p.fetched],
        )?;
        Ok(())
    }

    /// A chat's photo's file, when it has one.
    pub fn avatar(&self, jid: &str) -> Option<String> {
        self.profile(jid).and_then(|p| p.avatar)
    }

    pub fn bump_unread(db: &Connection, jid: &str) -> Result<()> {
        db.execute("UPDATE chats SET unread = unread + 1 WHERE jid = ?1", params![jid])?;
        Ok(())
    }

    /// An attachment downloaded (its file, and its size when it's a picture), or not to be had.
    pub fn set_media(&self, chat: &str, id: &str, path: &str, w: u32, h: u32) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE messages SET media_path = ?3, media_err = 0, w = CASE WHEN ?4 > 0 THEN ?4 ELSE w END,
                                 h = CASE WHEN ?5 > 0 THEN ?5 ELSE h END WHERE chat = ?1 AND id = ?2",
            params![chat, id, rel(path), w, h],
        )?;
        Ok(())
    }

    pub fn set_media_err(&self, chat: &str, id: &str) -> Result<()> {
        self.db.lock().unwrap().execute("UPDATE messages SET media_err = 1 WHERE chat = ?1 AND id = ?2", params![chat, id])?;
        Ok(())
    }

    /// Who a mention's digits are, when they're someone with a name: their id (a hidden id's number when known)
    /// and their name, as wassup's mentioned.
    pub fn mention_name(&self, digits: &str) -> Option<(String, String)> {
        let db = self.db.lock().unwrap();
        let state = |k: &str| db.query_row("SELECT value FROM state WHERE key = ?1", [k], |r| r.get::<_, String>(0)).optional().ok().flatten();
        for jid in [Self::canonical(&db, &format!("{digits}@lid")), format!("{digits}@s.whatsapp.net")] {
            // A mention of me: my profile name.
            if state("me").is_some_and(|m| m == jid) {
                return Some((jid, state("meName").filter(|n| !n.is_empty()).unwrap_or_else(|| s("me"))));
            }
            if let Some(name) = Self::known_name(&db, &jid) {
                return Some((jid, name));
            }
        }
        None
    }

    /// Someone's name as kept, a real one only (not a masked number).
    fn known_name(db: &Connection, jid: &str) -> Option<String> {
        let named: Option<String> = db
            .query_row("SELECT name FROM names WHERE jid = ?1 AND rank > 0", [jid], |r| r.get(0))
            .optional()
            .ok()
            .flatten();
        named.or_else(|| {
            db.query_row("SELECT name FROM chats WHERE jid = ?1 AND name IS NOT NULL", [jid], |r| r.get::<_, String>(0))
                .optional()
                .ok()
                .flatten()
                .filter(|n| !looks_like_number(n))
        })
    }

    /// A chat's own name as kept (a group's subject), if it has one.
    pub fn chat_name(&self, jid: &str) -> Option<String> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT name FROM chats WHERE jid = ?1 AND name IS NOT NULL", [jid], |r| r.get(0)).optional().ok().flatten()
    }

    /// Whether someone has a real name here (in the address book or one they gave themselves).
    pub fn has_name(&self, jid: &str) -> bool {
        Self::known_name(&self.db.lock().unwrap(), jid).is_some()
    }

    /// The name to show for someone: theirs as kept; my own chat, my profile name and "(eu)"; or else their number
    /// ("lid:…" for a hidden id).
    pub fn name_of(&self, jid: &str) -> String {
        let db = self.db.lock().unwrap();
        Self::resolve(&db, jid)
    }

    fn resolve(db: &Connection, jid: &str) -> String {
        let state = |k: &str| db.query_row("SELECT value FROM state WHERE key = ?1", [k], |r| r.get::<_, String>(0)).optional().ok().flatten();
        if let Some(me) = state("me").filter(|m| !m.is_empty() && *m == Self::canonical(db, jid)) {
            let _ = me;
            return match state("meName").filter(|n| !n.is_empty()) {
                Some(n) => format!("{n} ({})", s("me")),
                None => s("me"),
            };
        }
        if let Some(n) = Self::known_name(db, jid) {
            return n;
        }
        if crate::text::is_group(jid) {
            return t("group", &[&jid.split('@').next().unwrap_or(jid)]);
        }
        let masked: Option<String> = db.query_row("SELECT name FROM names WHERE jid = ?1", [jid], |r| r.get(0)).optional().ok().flatten();
        if let Some(m) = masked {
            return m;
        }
        if jid.ends_with("@lid") {
            return format!("lid:{}", jid.split('@').next().unwrap_or(jid));
        }
        crate::text::number_of(jid)
    }

    /// The chats with any activity, the latest first; archived ones too, marked. One query for them all: a name
    /// is worked out as `resolve` does, from what came with each row.
    pub fn chats(&self) -> Result<Vec<Chat>> {
        let db = self.db.lock().unwrap();
        let state = |k: &str| db.query_row("SELECT value FROM state WHERE key = ?1", [k], |r| r.get::<_, String>(0)).optional().ok().flatten();
        let me = state("me").unwrap_or_default();
        let me_name = state("meName").filter(|n| !n.is_empty());
        let me_lid: Option<String> = db.query_row("SELECT lid FROM lids WHERE pn = ?1", [&me], |r| r.get(0)).optional().ok().flatten();
        let mut q = db.prepare(
            "SELECT c.jid, c.last_ts, c.unread, c.archived, n.name, n.rank, c.name
               FROM chats c LEFT JOIN names n ON n.jid = c.jid
              WHERE c.last_ts > 0 ORDER BY c.last_ts DESC",
        )?;
        let rows = q.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i32>(2)?, r.get::<_, bool>(3)?, r.get::<_, Option<String>>(4)?, r.get::<_, Option<i32>>(5)?, r.get::<_, Option<String>>(6)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (jid, last_ts, unread, archived, named, rank, chat_name) = row?;
            if jid.ends_with("@broadcast") || jid.ends_with("@newsletter") {
                continue;
            }
            let name = if !me.is_empty() && (jid == me || me_lid.as_deref() == Some(jid.as_str())) {
                match &me_name {
                    Some(n) => format!("{n} ({})", s("me")),
                    None => s("me"),
                }
            } else if let Some(n) = named.clone().filter(|_| rank.unwrap_or(0) > 0) {
                n
            } else if let Some(n) = chat_name.filter(|n| !looks_like_number(n)) {
                n
            } else if crate::text::is_group(&jid) {
                t("group", &[&jid.split('@').next().unwrap_or(&jid)])
            } else if let Some(n) = named {
                n
            } else if jid.ends_with("@lid") {
                format!("lid:{}", jid.split('@').next().unwrap_or(&jid))
            } else {
                crate::text::number_of(&jid)
            };
            out.push(Chat { jid, name, last_ts, unread, archived });
        }
        Ok(out)
    }

    /// The messages with every one of `words` in them (already folded), in one chat or in all, the most recent
    /// `limit`, oldest first.
    pub fn search(&self, chat: Option<&str>, words: &[String], limit: usize) -> Result<Vec<Msg>> {
        if words.is_empty() {
            return Ok(vec![]);
        }
        let db = self.db.lock().unwrap();
        let mut sql = format!("SELECT * FROM (SELECT {MSG_COLS} FROM messages WHERE text != '' AND kind != 'deleted'");
        let mut args: Vec<String> = Vec::new();
        if let Some(c) = chat {
            args.push(c.to_string());
            sql += &format!(" AND chat = ?{}", args.len());
        }
        for w in words {
            args.push(format!("%{}%", w.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
            sql += &format!(" AND fold(text) LIKE ?{} ESCAPE '\\'", args.len());
        }
        sql += &format!(" ORDER BY ts DESC LIMIT {limit}) ORDER BY ts ASC");
        let mut q = db.prepare(&sql)?;
        let rows = q.query_map(rusqlite::params_from_iter(args.iter()), msg_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// How many of a chat's messages are as recent as `ts` or more: how many to draw for one that old to be there.
    pub fn count_since(&self, chat: &str, ts: i64) -> usize {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT count(*) FROM messages WHERE chat = ?1 AND ts >= ?2", params![chat, ts], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize
    }

    /// How many messages a chat has kept.
    pub fn count(&self, chat: &str) -> usize {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT count(*) FROM messages WHERE chat = ?1", [chat], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize
    }

    /// A chat's latest messages, oldest first.
    pub fn messages(&self, chat: &str, limit: usize) -> Result<Vec<Msg>> {
        let db = self.db.lock().unwrap();
        let mut q = db.prepare(&format!("SELECT * FROM (SELECT {MSG_COLS} FROM messages WHERE chat = ?1 ORDER BY ts DESC LIMIT ?2) ORDER BY ts ASC"))?;
        let rows = q.query_map(params![chat, limit as i64], msg_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One message, with how it came.
    pub fn message(&self, chat: &str, id: &str) -> Option<Msg> {
        let db = self.db.lock().unwrap();
        db.query_row(&format!("SELECT {MSG_COLS}, raw FROM messages WHERE chat = ?1 AND id = ?2"), params![chat, id], |r| {
            let mut m = msg_row(r)?;
            m.raw = r.get(21)?;
            Ok(m)
        })
        .optional()
        .ok()
        .flatten()
    }

    /// A chat's oldest message kept, to ask the phone for those before it.
    pub fn oldest(&self, chat: &str) -> Option<Msg> {
        let id: Option<String> = {
            let db = self.db.lock().unwrap();
            db.query_row("SELECT id FROM messages WHERE chat = ?1 ORDER BY ts ASC LIMIT 1", [chat], |r| r.get(0)).optional().ok().flatten()
        };
        self.message(chat, &id?)
    }

    /// A chat's latest message.
    pub fn newest(&self, chat: &str) -> Option<Msg> {
        let id: Option<String> = {
            let db = self.db.lock().unwrap();
            db.query_row("SELECT id FROM messages WHERE chat = ?1 ORDER BY ts DESC LIMIT 1", [chat], |r| r.get(0)).optional().ok().flatten()
        };
        self.message(chat, &id?)
    }

    /// The other people who wrote in a chat, most recent first.
    pub fn recent_senders(&self, chat: &str, limit: usize) -> Vec<String> {
        let db = self.db.lock().unwrap();
        let Ok(mut q) = db.prepare("SELECT sender FROM messages WHERE chat = ?1 AND from_me = 0 AND sender != '' GROUP BY sender ORDER BY MAX(ts) DESC LIMIT ?2") else {
            return vec![];
        };
        q.query_map(params![chat, limit as i64], |r| r.get(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// The text of every message I wrote, for the spelling to accept my own words.
    pub fn my_texts(&self) -> Vec<String> {
        let db = self.db.lock().unwrap();
        let Ok(mut q) = db.prepare("SELECT text FROM messages WHERE from_me = 1 AND kind = 'text' AND text != ''") else { return vec![] };
        q.query_map([], |r| r.get(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// Every name known, for the spelling to accept them.
    pub fn names(&self) -> Vec<String> {
        let db = self.db.lock().unwrap();
        let Ok(mut q) = db.prepare(
            "SELECT name FROM names UNION SELECT name FROM chats WHERE name IS NOT NULL
             UNION SELECT DISTINCT push_name FROM messages WHERE push_name != ''",
        ) else {
            return vec![];
        };
        q.query_map([], |r| r.get(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// A chat's messages with an attachment, oldest first.
    pub fn media_messages(&self, chat: &str) -> Vec<Msg> {
        let db = self.db.lock().unwrap();
        let Ok(mut q) = db.prepare(&format!("SELECT {MSG_COLS} FROM messages WHERE chat = ?1 AND media IS NOT NULL ORDER BY ts ASC")) else { return vec![] };
        q.query_map([chat], msg_row).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// A chat read: its count goes to zero; returns the messages of the others it had unread, newest first, to tell
    /// WhatsApp which were read.
    pub fn mark_read(&self, chat: &str) -> Result<Vec<Msg>> {
        let ids: Vec<String> = {
            let db = self.db.lock().unwrap();
            let unread: i32 = db.query_row("SELECT unread FROM chats WHERE jid = ?1", [chat], |r| r.get(0)).optional()?.unwrap_or(0);
            if unread == 0 {
                return Ok(vec![]);
            }
            db.execute("UPDATE chats SET unread = 0 WHERE jid = ?1", [chat])?;
            let mut q = db.prepare("SELECT id FROM messages WHERE chat = ?1 AND from_me = 0 ORDER BY ts DESC LIMIT ?2")?;
            q.query_map(params![chat, unread.max(1)], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        Ok(ids.iter().filter_map(|id| self.message(chat, id)).collect())
    }

    /// The one-to-one chats where a message came from a WhatsApp Business account, once per database.
    pub fn business_chats(&self) -> Vec<String> {
        let db = self.db.lock().unwrap();
        let Ok(mut q) = db.prepare("SELECT key FROM state WHERE key LIKE 'biz:%' AND value = 'true'") else { return vec![] };
        q.query_map([], |r| r.get::<_, String>(0)).map(|rows| rows.flatten().map(|k| k[4..].to_string()).collect()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Files kept by their whole path come back by their place in the media folder, wherever that folder was; one
    /// outside a media folder isn't kept; those kept by their place stay.
    #[test]
    fn files_kept_inside_the_media_folder() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("store-paths-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wassup.db");
        let _ = std::fs::remove_file(&path);
        drop(Store::open(&path).unwrap());
        let db = Connection::open(&path).unwrap();
        let files = [
            ("1", "/home/rita/.config/wassup-old/media/123456789012345@s.whatsapp.net/A1.jpg"),
            ("2", "/srv/elsewhere/foto.jpg"),
            ("3", "123456789012345@s.whatsapp.net/A3.jpg"),
        ];
        for (id, p) in files {
            db.execute("INSERT INTO messages (chat, id, from_me, sender, ts, kind, media_path) VALUES ('c', ?1, 0, '', 0, 'image', ?2)", params![id, p]).unwrap();
        }
        db.execute("INSERT INTO avatars (jid, path, fetched) VALUES ('c', '/home/rita/.config/wassup-old/media/c/profile.jpg', 0)", []).unwrap();
        drop(db);
        let store = Store::open(&path).unwrap();
        let db = store.db.lock().unwrap();
        let kept: Vec<Option<String>> = db.prepare("SELECT media_path FROM messages ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        assert_eq!(kept, [Some("123456789012345@s.whatsapp.net/A1.jpg".into()), None, Some("123456789012345@s.whatsapp.net/A3.jpg".into())]);
        let photo: Option<String> = db.query_row("SELECT path FROM avatars", [], |r| r.get(0)).unwrap();
        assert_eq!(photo.as_deref(), Some("c/profile.jpg"));
        drop(db);
        assert_eq!(rel("../wassup.db"), None);
        assert_eq!(rel("/etc/passwd"), None);
        assert_eq!(rel("c/A1.jpg").as_deref(), Some("c/A1.jpg"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
