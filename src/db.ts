import { DatabaseSync } from 'node:sqlite'
import { dirs } from './config.js'

export interface ChatRow {
  jid: string
  name: string | null
  is_group: number
  last_ts: number
  unread: number
  archived: number
}

export interface ContactRow {
  jid: string
  name: string | null
  notify: string | null
}

/** What WhatsApp says of a chat, asked at most once a day (Wa.ensureProfile): its picture, "about" and group size. */
export interface ProfileRow {
  jid: string
  about: string | null
  members: number | null
  /** The picture's file in the chat's media folder; '' with none, or none to be seen. */
  avatar: string
  fetched: number
}

export interface ReactionRow {
  chat_jid: string
  msg_id: string
  sender_jid: string
  emoji: string
}

export interface MessageRow {
  id: string
  chat_jid: string
  sender_jid: string
  from_me: number
  ts: number
  type: string
  text: string
  push_name: string | null
  quoted: string | null
  media_path: string | null // relative to dirs.media
  media_mime: string | null
  media_name: string | null
  media_w: number | null
  media_h: number | null
  media_err: number
  status: number | null
  raw: string
}

// timeout: several processes share the database (server writes, clients read and save their tabs); instead of SQLITE_BUSY, it waits.
const db = new DatabaseSync(dirs.db, { timeout: 3000 })
/** Text for comparing without accents or case, as format.ts's fold; in SQL as fold(), for searching messages. */
const fold = (s: string) => s.normalize('NFD').replace(/\p{M}/gu, '').toLowerCase()
db.function('fold', { deterministic: true }, (s: unknown) => (typeof s === 'string' ? fold(s) : null))
db.exec(`
  PRAGMA journal_mode = WAL;
  PRAGMA synchronous = NORMAL;
  CREATE TABLE IF NOT EXISTS chats (
    jid TEXT PRIMARY KEY,
    name TEXT,
    is_group INTEGER NOT NULL DEFAULT 0,
    last_ts INTEGER NOT NULL DEFAULT 0,
    unread INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0
  );
  CREATE TABLE IF NOT EXISTS contacts (
    jid TEXT PRIMARY KEY,
    name TEXT,
    notify TEXT
  );
  CREATE TABLE IF NOT EXISTS lids (
    lid TEXT PRIMARY KEY,
    pn TEXT NOT NULL
  );
  CREATE TABLE IF NOT EXISTS messages (
    id TEXT NOT NULL,
    chat_jid TEXT NOT NULL,
    sender_jid TEXT NOT NULL,
    from_me INTEGER NOT NULL,
    ts INTEGER NOT NULL,
    type TEXT NOT NULL,
    text TEXT NOT NULL DEFAULT '',
    push_name TEXT,
    quoted TEXT,
    media_path TEXT,
    media_mime TEXT,
    media_name TEXT,
    media_w INTEGER,
    media_h INTEGER,
    media_err INTEGER NOT NULL DEFAULT 0,
    status INTEGER,
    raw TEXT NOT NULL,
    PRIMARY KEY (chat_jid, id)
  );
  CREATE INDEX IF NOT EXISTS messages_chat_ts ON messages (chat_jid, ts);
  CREATE TABLE IF NOT EXISTS reactions (
    chat_jid TEXT NOT NULL,
    msg_id TEXT NOT NULL,
    sender_jid TEXT NOT NULL,
    emoji TEXT NOT NULL,
    ts INTEGER NOT NULL,
    PRIMARY KEY (chat_jid, msg_id, sender_jid)
  );
  CREATE TABLE IF NOT EXISTS state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
  );
  CREATE TABLE IF NOT EXISTS profiles (
    jid TEXT PRIMARY KEY,
    about TEXT,
    members INTEGER,
    avatar TEXT NOT NULL DEFAULT '',
    fetched INTEGER NOT NULL
  );
`)

const q = {
  upsertChat: db.prepare(`
    INSERT INTO chats (jid, name, is_group, last_ts, unread, archived) VALUES (?, ?, ?, ?, ?, ?)
    ON CONFLICT(jid) DO UPDATE SET
      name = COALESCE(excluded.name, chats.name),
      is_group = excluded.is_group,
      last_ts = MAX(chats.last_ts, excluded.last_ts),
      unread = excluded.unread,
      archived = excluded.archived`),
  touchChat: db.prepare(`
    INSERT INTO chats (jid, is_group, last_ts) VALUES (?, ?, ?)
    ON CONFLICT(jid) DO UPDATE SET last_ts = MAX(chats.last_ts, excluded.last_ts)`),
  setChatName: db.prepare(`UPDATE chats SET name = ? WHERE jid = ?`),
  bumpUnread: db.prepare(`UPDATE chats SET unread = unread + 1 WHERE jid = ?`),
  clearUnread: db.prepare(`UPDATE chats SET unread = 0 WHERE jid = ?`),
  listChats: db.prepare(`SELECT * FROM chats WHERE last_ts > 0 ORDER BY last_ts DESC`),
  getChat: db.prepare(`SELECT * FROM chats WHERE jid = ?`),
  upsertContact: db.prepare(`
    INSERT INTO contacts (jid, name, notify) VALUES (?, ?, ?)
    ON CONFLICT(jid) DO UPDATE SET
      name = COALESCE(excluded.name, contacts.name),
      notify = COALESCE(excluded.notify, contacts.notify)`),
  getContact: db.prepare(`SELECT * FROM contacts WHERE jid = ?`),
  setLid: db.prepare(`INSERT OR REPLACE INTO lids (lid, pn) VALUES (?, ?)`),
  getPn: db.prepare(`SELECT pn FROM lids WHERE lid = ?`),
  getLid: db.prepare(`SELECT lid FROM lids WHERE pn = ?`),
  getProfile: db.prepare(`SELECT * FROM profiles WHERE jid = ?`),
  listProfiles: db.prepare(`SELECT * FROM profiles`),
  setProfile: db.prepare(`INSERT OR REPLACE INTO profiles (jid, about, members, avatar, fetched) VALUES (?, ?, ?, ?, ?)`),
  upsertMessage: db.prepare(`
    INSERT INTO messages (id, chat_jid, sender_jid, from_me, ts, type, text, push_name, quoted,
      media_path, media_mime, media_name, media_w, media_h, status, raw)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(chat_jid, id) DO UPDATE SET
      type = excluded.type,
      text = excluded.text,
      push_name = COALESCE(excluded.push_name, messages.push_name),
      quoted = COALESCE(excluded.quoted, messages.quoted),
      media_path = COALESCE(messages.media_path, excluded.media_path),
      media_mime = COALESCE(excluded.media_mime, messages.media_mime),
      media_name = COALESCE(excluded.media_name, messages.media_name),
      media_w = COALESCE(excluded.media_w, messages.media_w),
      media_h = COALESCE(excluded.media_h, messages.media_h),
      -- A state only moves forward (see setStatus): another copy of the message mustn't take it back.
      status = CASE
        WHEN excluded.status IS NULL THEN messages.status
        WHEN messages.status IS NULL THEN excluded.status
        WHEN excluded.status = 0 THEN CASE WHEN messages.status <= 1 THEN 0 ELSE messages.status END
        ELSE MAX(messages.status, excluded.status) END,
      raw = excluded.raw`),
  hasMessage: db.prepare(`SELECT 1 FROM messages WHERE chat_jid = ? AND id = ?`),
  getMessage: db.prepare(`SELECT * FROM messages WHERE chat_jid = ? AND id = ?`),
  findMessage: db.prepare(`SELECT * FROM messages WHERE id = ? LIMIT 1`),
  listMessages: db.prepare(`SELECT * FROM (SELECT * FROM messages WHERE chat_jid = ? ORDER BY ts DESC LIMIT ?) ORDER BY ts ASC`),
  countMessages: db.prepare(`SELECT COUNT(*) AS n FROM messages WHERE chat_jid = ?`),
  countMessagesSince: db.prepare(`SELECT COUNT(*) AS n FROM messages WHERE chat_jid = ? AND ts >= ?`),
  myTexts: db.prepare(`SELECT text FROM messages WHERE from_me = 1 AND type = 'text' AND text <> ''`),
  names: db.prepare(`SELECT name FROM contacts WHERE name IS NOT NULL UNION SELECT notify FROM contacts WHERE notify IS NOT NULL UNION SELECT name FROM chats WHERE name IS NOT NULL`),
  recentSenders: db.prepare(`SELECT sender_jid FROM messages WHERE chat_jid = ? AND from_me = 0 AND sender_jid <> '' GROUP BY sender_jid ORDER BY MAX(ts) DESC LIMIT ?`),
  oldestMessage: db.prepare(`SELECT * FROM messages WHERE chat_jid = ? ORDER BY ts ASC LIMIT 1`),
  listMedia: db.prepare(`SELECT * FROM messages WHERE chat_jid = ? AND media_mime IS NOT NULL ORDER BY ts ASC`),
  unreadIncoming: db.prepare(`SELECT * FROM messages WHERE chat_jid = ? AND from_me = 0 ORDER BY ts DESC LIMIT ?`),
  setMedia: db.prepare(`UPDATE messages SET media_path = ?, media_w = ?, media_h = ?, media_err = 0 WHERE chat_jid = ? AND id = ?`),
  setMediaErr: db.prepare(`UPDATE messages SET media_err = 1 WHERE chat_jid = ? AND id = ?`),
  setStatus: db.prepare(`UPDATE messages SET status = CASE
      WHEN :s = 0 THEN CASE WHEN COALESCE(status, 0) <= 1 THEN 0 ELSE status END
      ELSE MAX(COALESCE(status, 0), :s) END
    WHERE chat_jid = :chat AND id = :id`),
  markReadBefore: db.prepare(`UPDATE messages SET status = 4 WHERE chat_jid = ? AND from_me = 1 AND status IN (2, 3) AND ts <= ?`),
  // Startup repair, safe to repeat: one-to-one, my sent or delivered messages before one of mine that was read;
  // groups, my pending messages followed by any other message in the group, which the server must have taken.
  repairRead: db.prepare(`UPDATE messages SET status = 4 WHERE from_me = 1 AND status IN (2, 3) AND chat_jid NOT LIKE '%@g.us'
    AND ts <= (SELECT MAX(n.ts) FROM messages n WHERE n.chat_jid = messages.chat_jid AND n.from_me = 1 AND n.status >= 4)`),
  noSender: db.prepare(`SELECT chat_jid, id, raw FROM messages WHERE chat_jid LIKE '%@g.us' AND from_me = 0 AND sender_jid = ''`),
  setSender: db.prepare(`UPDATE messages SET sender_jid = ? WHERE chat_jid = ? AND id = ?`),
  repairGroupPending: db.prepare(`UPDATE messages SET status = 2 WHERE from_me = 1 AND status = 1 AND chat_jid LIKE '%@g.us'
    AND EXISTS (SELECT 1 FROM messages n WHERE n.chat_jid = messages.chat_jid AND n.ts > messages.ts)`),
  setType: db.prepare(`UPDATE messages SET type = ?, text = ? WHERE chat_jid = ? AND id = ?`),
  removeMessage: db.prepare(`DELETE FROM messages WHERE chat_jid = ? AND id = ?`),
  businessChats: db.prepare(`SELECT DISTINCT chat_jid FROM messages WHERE from_me = 0 AND chat_jid NOT LIKE '%@g.us' AND raw LIKE '%"verifiedBizName"%'`),
  removeReactions: db.prepare(`DELETE FROM reactions WHERE chat_jid = ? AND msg_id = ?`),
  lastMessage: db.prepare(`SELECT * FROM messages WHERE chat_jid = ? AND type != 'secretEncrypted' ORDER BY ts DESC LIMIT 1`),
  setReaction: db.prepare(`
    INSERT INTO reactions (chat_jid, msg_id, sender_jid, emoji, ts) VALUES (?, ?, ?, ?, ?)
    ON CONFLICT(chat_jid, msg_id, sender_jid) DO UPDATE SET emoji = excluded.emoji, ts = excluded.ts WHERE excluded.ts >= reactions.ts`),
  clearReaction: db.prepare(`DELETE FROM reactions WHERE chat_jid = ? AND msg_id = ? AND sender_jid = ? AND ts <= ?`),
  listReactions: db.prepare(`SELECT chat_jid, msg_id, sender_jid, emoji FROM reactions WHERE chat_jid = ? ORDER BY ts ASC`),
  lidContactsUnmapped: db.prepare(`SELECT * FROM contacts k WHERE k.jid LIKE '%@lid' AND (k.name IS NOT NULL OR k.notify IS NOT NULL) AND NOT EXISTS (SELECT 1 FROM lids l WHERE l.lid = k.jid)`),
  getState: db.prepare(`SELECT value FROM state WHERE key = ?`),
  listState: db.prepare(`SELECT key, value FROM state WHERE key LIKE ? ESCAPE '\\'`),
  setState: db.prepare(`INSERT OR REPLACE INTO state (key, value) VALUES (?, ?)`),
  deleteState: db.prepare(`DELETE FROM state WHERE key = ?`),
}

export const store = {
  transaction<T>(fn: () => T): T {
    db.exec('BEGIN')
    try {
      const r = fn()
      db.exec('COMMIT')
      return r
    } catch (e) {
      db.exec('ROLLBACK')
      throw e
    }
  },

  upsertChat(c: { jid: string; name?: string | null; isGroup: boolean; lastTs: number; unread: number; archived: boolean }) {
    q.upsertChat.run(c.jid, c.name ?? null, c.isGroup ? 1 : 0, c.lastTs, c.unread, c.archived ? 1 : 0)
  },
  touchChat(jid: string, isGroup: boolean, ts: number) {
    q.touchChat.run(jid, isGroup ? 1 : 0, ts)
  },
  setChatName(jid: string, name: string) {
    q.setChatName.run(name, jid)
  },
  bumpUnread(jid: string) {
    q.bumpUnread.run(jid)
  },
  clearUnread(jid: string) {
    q.clearUnread.run(jid)
  },
  listChats(): ChatRow[] {
    return q.listChats.all() as unknown as ChatRow[]
  },
  getChat(jid: string): ChatRow | undefined {
    return q.getChat.get(jid) as unknown as ChatRow | undefined
  },

  upsertContact(jid: string, name?: string | null, notify?: string | null) {
    q.upsertContact.run(jid, name ?? null, notify ?? null)
  },
  getContact(jid: string): ContactRow | undefined {
    return q.getContact.get(jid) as unknown as ContactRow | undefined
  },
  getProfile(jid: string): ProfileRow | undefined {
    return q.getProfile.get(jid) as unknown as ProfileRow | undefined
  },
  listProfiles(): ProfileRow[] {
    return q.listProfiles.all() as unknown as ProfileRow[]
  },
  setProfile(p: ProfileRow) {
    q.setProfile.run(p.jid, p.about, p.members, p.avatar, p.fetched)
  },

  setLid(lid: string, pn: string) {
    q.setLid.run(lid, pn)
  },
  getPn(lid: string): string | undefined {
    return (q.getPn.get(lid) as { pn: string } | undefined)?.pn
  },
  getLid(pn: string): string | undefined {
    return (q.getLid.get(pn) as { lid: string } | undefined)?.lid
  },

  upsertMessage(m: Omit<MessageRow, 'media_err'>) {
    q.upsertMessage.run(m.id, m.chat_jid, m.sender_jid, m.from_me, m.ts, m.type, m.text, m.push_name, m.quoted,
      m.media_path, m.media_mime, m.media_name, m.media_w, m.media_h, m.status, m.raw)
  },
  hasMessage(chat: string, id: string): boolean {
    return q.hasMessage.get(chat, id) != null
  },
  getMessage(chat: string, id: string): MessageRow | undefined {
    return q.getMessage.get(chat, id) as unknown as MessageRow | undefined
  },
  findMessage(id: string): MessageRow | undefined {
    return q.findMessage.get(id) as unknown as MessageRow | undefined
  },
  /** The chat's latest `limit` messages, oldest first; -1 for all of them. */
  listMessages(chat: string, limit = 300): MessageRow[] {
    return q.listMessages.all(chat, limit) as unknown as MessageRow[]
  },
  /** The other people who wrote in a chat, most recent first. */
  recentSenders(chat: string, limit: number): string[] {
    return (q.recentSenders.all(chat, limit) as { sender_jid: string }[]).map(r => r.sender_jid)
  },
  countMessages(chat: string): number {
    return (q.countMessages.get(chat) as { n: number }).n
  },
  /** The text of every message I wrote. */
  myTexts(): string[] {
    return (q.myTexts.all() as { text: string }[]).map(r => r.text)
  },
  /** Every name known: contacts' (as saved and as they call themselves) and chats'. */
  names(): string[] {
    return (q.names.all() as { name: string }[]).map(r => r.name)
  },
  /** How many of a chat's messages are from `ts` on: how far back the panel has to draw to reach one of that time. */
  /**
   * Messages whose text has every word of `query` somewhere, in any order, ignoring accents and case: in `chat`, or
   * in all of them; the most recent first.
   */
  searchMessages(query: string, chat: string | null, limit: number): MessageRow[] {
    const words = fold(query).split(/\s+/).filter(Boolean)
    if (!words.length) return []
    const like = words.map(() => `fold(text) LIKE ? ESCAPE '\\'`).join(' AND ')
    const sql = `SELECT * FROM messages WHERE ${chat ? 'chat_jid = ? AND ' : ''}type NOT IN ('secretEncrypted', 'deleted') AND ${like} ORDER BY ts DESC LIMIT ?`
    const args = [...(chat ? [chat] : []), ...words.map(w => `%${w.replace(/[\\%_]/g, '\\$&')}%`), limit]
    return db.prepare(sql).all(...args) as unknown as MessageRow[]
  },
  countMessagesSince(chat: string, ts: number): number {
    return (q.countMessagesSince.get(chat, ts) as { n: number }).n
  },
  oldestMessage(chat: string): MessageRow | undefined {
    return q.oldestMessage.get(chat) as unknown as MessageRow | undefined
  },
  listMedia(chat: string): MessageRow[] {
    return q.listMedia.all(chat) as unknown as MessageRow[]
  },
  unreadIncoming(chat: string, limit: number): MessageRow[] {
    return q.unreadIncoming.all(chat, limit) as unknown as MessageRow[]
  },
  setMedia(chat: string, id: string, path: string, w: number | null, h: number | null) {
    q.setMedia.run(path, w, h, chat, id)
  },
  setMediaErr(chat: string, id: string) {
    q.setMediaErr.run(chat, id)
  },
  /**
   * A message's delivery state, which only moves forward: receipts arrive out of order (a delivery receipt from
   * another of their devices after the read one), and taking the last as it came put read messages back to
   * delivered or sent. The error state is the exception, and only replaces pending, so a failed send still shows.
   */
  setStatus(chat: string, id: string, status: number) {
    q.setStatus.run({ s: status, chat, id })
  },
  /** Someone who read one of my messages read the ones before it too: my sent or delivered ones up to `ts` become read. */
  markReadBefore(chat: string, ts: number) {
    q.markReadBefore.run(chat, ts)
  },
  /** The same rules on what's already stored; returns how many messages moved. */
  /** Group messages from others stored with no sender, as they came (`raw`). */
  messagesWithoutSender(): { chat_jid: string; id: string; raw: string }[] {
    return q.noSender.all() as { chat_jid: string; id: string; raw: string }[]
  },
  setSender(chat: string, id: string, sender: string) {
    q.setSender.run(sender, chat, id)
  },
  repairStatuses(): { read: number; groupSent: number } {
    return { read: Number(q.repairRead.run().changes), groupSent: Number(q.repairGroupPending.run().changes) }
  },
  /** The one-to-one chats where a message came from a WhatsApp Business account (its verified name in it). */
  businessChats(): string[] {
    return (q.businessChats.all() as { chat_jid: string }[]).map(r => r.chat_jid)
  },
  /** A message deleted for me: it goes, and its reactions with it. */
  removeMessage(chat: string, id: string) {
    q.removeMessage.run(chat, id)
    q.removeReactions.run(chat, id)
  },
  setType(chat: string, id: string, type: string, text: string) {
    q.setType.run(type, text, chat, id)
  },

  /** Contacts with a name saved by lid, without a known number: candidates to resolve with WhatsApp. */
  lidContactsUnmapped(): ContactRow[] {
    return q.lidContactsUnmapped.all() as unknown as ContactRow[]
  },
  lastMessage(chat: string): MessageRow | undefined {
    return q.lastMessage.get(chat) as unknown as MessageRow | undefined
  },

  /** Someone's reaction to a message; empty emoji removes it. The most recent one wins, whatever order they arrive in. */
  setReaction(chat: string, msgId: string, sender: string, emoji: string, ts: number) {
    if (emoji) q.setReaction.run(chat, msgId, sender, emoji, ts)
    else q.clearReaction.run(chat, msgId, sender, ts)
  },
  listReactions(chat: string): ReactionRow[] {
    return q.listReactions.all(chat) as unknown as ReactionRow[]
  },

  /** UI state (open tabs, etc.), as JSON per key. */
  getState<T>(key: string): T | undefined {
    const row = q.getState.get(key) as { value: string } | undefined
    if (!row) return undefined
    try { return JSON.parse(row.value) as T } catch { return undefined }
  },
  deleteState(key: string) {
    q.deleteState.run(key)
  },
  setState(key: string, value: unknown) {
    q.setState.run(key, JSON.stringify(value))
  },
  /** All records whose key starts with `prefix`. */
  listState<T>(prefix: string): { key: string; value: T }[] {
    const rows = q.listState.all(prefix.replace(/[%_\\]/g, '\\$&') + '%') as unknown as { key: string; value: string }[]
    const out: { key: string; value: T }[] = []
    for (const r of rows) { try { out.push({ key: r.key, value: JSON.parse(r.value) as T }) } catch { /* ignore */ } }
    return out
  },

  close() {
    db.close()
  },
}
