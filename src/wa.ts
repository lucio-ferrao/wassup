import { EventEmitter } from 'node:events'
import fs from 'node:fs'
import path from 'node:path'
import makeWASocket, {
  Browsers, BufferJSON, DisconnectReason, downloadContentFromMessage, downloadMediaMessage, fetchLatestBaileysVersion, getContentType,
  isJidBroadcast, isJidGroup, isJidNewsletter, isJidStatusBroadcast, jidNormalizedUser, makeCacheableSignalKeyStore,
  normalizeMessageContent, toNumber, useMultiFileAuthState,
  proto, type AnyMessageContent, type GroupMetadata, type WAMessage, type WAMessageKey, type WASocket,
} from 'baileys'
import type { Boom } from '@hapi/boom'
import { dirs } from './config.js'
import { t } from './i18n.js'
import { logger } from './log.js'
import { store, type MessageRow } from './db.js'
import { MoodKeeper } from './mood.js'

export type ConnState = 'connecting' | 'qr' | 'open' | 'closed'

export interface WaEvents {
  connection: [state: ConnState, detail?: string]
  chats: []
  messages: [chatJid: string]
  notify: [chatJid: string, row: MessageRow]
  status: [text: string]
  /** Who is typing or recording in a chat (canonical jids); empty list when nobody. */
  typing: [chatJid: string, who: string[]]
  /** A reaction just received or sent (empty emoji: removed). */
  reaction: [chatJid: string, msgId: string, senderJid: string, emoji: string]
  /** Whether the person of a one-to-one chat is online, as far as WhatsApp lets this device know. */
  presence: [chatJid: string, online: boolean]
  /** How many of a group's followed members are online (see subscribePresence). */
  groupOnline: [groupJid: string, count: number]
  /** A chat's photo, "about" or group size has just been stored (ensureProfile). */
  profile: [jid: string]
  /** A chat's state, an emoji, has just been worked out and stored (MoodKeeper). */
  mood: [jid: string]
  /** Whether this device shows as online to the others: from activity in a terminal until two idle minutes. */
  available: [on: boolean]
  /** Only from the remote client: the server process disappeared. */
  lost: []
}

/** Canonical jid: number instead of lid whenever we know it, without device suffix. */
export function canonicalJid(jid?: string | null, alt?: string | null): string {
  if (!jid) return ''
  if (isJidGroup(jid) || isJidBroadcast(jid) || isJidNewsletter(jid)) return jid
  const norm = jidNormalizedUser(jid)
  if (norm.endsWith('@lid')) {
    if (alt && !alt.endsWith('@lid')) {
      const pn = jidNormalizedUser(alt)
      store.setLid(norm, pn)
      return pn
    }
    return store.getPn(norm) ?? norm
  }
  if (alt && alt.endsWith('@lid')) store.setLid(jidNormalizedUser(alt), norm)
  return norm
}

/** Whether a process still exists (signal 0 only checks). */
function alive(pid: number): boolean {
  try { process.kill(pid, 0); return true } catch { return false }
}

export function jidUser(jid: string): string {
  return jid.split('@')[0] ?? jid
}

/** WhatsApp sends the masked number ("+351∙∙∙∙∙∙∙35") as `name` when the contact isn't saved: that's not a name. */
const looksLikeNumber = (s: string) => /^[+\d\s∙·.()-]+$/.test(s)

/** My own jid and profile name, as the server keeps them on connecting, read again every 10 s (myself). */
let mine = { jid: '', name: '', at: 0 }
function myself() {
  if (Date.now() - mine.at > 10000) mine = { jid: store.getState<string>('me') ?? '', name: store.getState<string>('meName') ?? '', at: Date.now() }
  return mine
}

export function contactName(jid: string): string {
  // My own chat, with myself: my profile name and "(eu)", where WhatsApp, as for anyone not saved, gives a masked number.
  const me = myself()
  if (me.jid && canonicalJid(jid) === me.jid) return me.name ? `${me.name} (${t('me')})` : t('me')
  const c = store.getContact(jid)
  if (c?.name && !looksLikeNumber(c.name)) return c.name
  if (c?.notify) return c.notify
  if (c?.name) return c.name
  if (jid.endsWith('@lid')) return `lid:${jidUser(jid)}`
  return `+${jidUser(jid)}`
}

/** "@" and a number (a lid's or a phone's) starting a word: a mention as WhatsApp sends it, not part of an address. */
const MENTION = /(?<![\p{L}\p{N}._%+-])@(\d{6,})(?!\d)/gu

/** Who a mention's number is, when it's a known contact: their jid (a lid's phone number when known) and name. */
function mentioned(digits: string): { jid: string; name: string } | null {
  for (const jid of [canonicalJid(`${digits}@lid`), `${digits}@s.whatsapp.net`]) {
    const name = contactName(jid)
    if (!name.startsWith('lid:') && !name.startsWith('+')) return { jid, name }
  }
  return null
}

/**
 * Mentions with the person's first name in place of the number when they're a known contact: "@123456789012345"
 * becomes "@Ana". `show` draws each one found from the jid and first name; by default "@" and the name.
 */
export function withMentions(text: string, show: (jid: string, first: string) => string = (_jid, first) => `@${first}`): string {
  return text.replace(MENTION, (m, digits: string) => {
    const who = mentioned(digits)
    return who ? show(who.jid, who.name.split(' ')[0]!) : m
  })
}

/** The id of the message a pin or unpin is about (pinInChatMessage.key.id), from the message as it came. */
export function pinTarget(row: Pick<MessageRow, 'type' | 'raw'>): string | null {
  if (row.type !== 'pinInChat') return null
  return /"pinInChatMessage":\{"key":\{[^}]*?"id":"([^"]+)"/.exec(row.raw)?.[1] ?? null
}

/**
 * The words for a message of a kind the client has no drawing of its own for (stored as "[kind]"): a pin, an album's
 * announcement, a round video, a message still on its way, a group invite, a business message, or anything else.
 */
export function typeLabel(row: Pick<MessageRow, 'type' | 'raw'>): string {
  switch (row.type) {
    case 'pinInChat': return row.raw.includes('"UNPIN_FOR_ALL"') ? t('unpinned') : t('pinned')
    case 'album': return t('album')
    case 'ptv': return t('videoNote')
    case 'placeholder': return t('waitingMessage')
    case 'groupInvite': return t('groupInvite')
    case 'interactive': case 'template': return t('businessMessage')
    default: return t('unsupported')
  }
}

export function chatName(jid: string): string {
  const chat = store.getChat(jid)
  if (chat?.name) return chat.name
  if (isJidGroup(jid)) return t('group', jidUser(jid))
  return contactName(jid)
}

/**
 * A short name for the chat, for a tab's label or the prompt: a contact's first name (with `two`, the first two when
 * there are more than two, see firstNames), a group's whole name, and nothing when all we have is a number or a lid.
 */
export function shortName(jid: string, two = false): string | null {
  const name = chatName(jid)
  if (isJidGroup(jid)) return store.getChat(jid)?.name ?? null
  if (looksLikeNumber(name) || name.startsWith('lid:')) return null
  return firstNames(name, two)
}

/** Words joining the parts of a Portuguese name, which aren't names themselves ("João da Silva"). */
const PARTICLES = new Set(['de', 'da', 'do', 'das', 'dos', 'e'])

/**
 * A name's first word; with `two`, when it has more than two names, its first two, with any particle between them
 * kept ("Ana Maria Costa": "Ana Maria", "João da Silva Santos": "João da Silva", "Rita Lemos": "Rita").
 */
export function firstNames(name: string, two: boolean): string | null {
  const words = name.split(/\s+/).filter(Boolean)
  const names = words.flatMap((w, i) => PARTICLES.has(w.toLowerCase()) ? [] : [i])
  if (two && names.length > 2) return words.slice(0, names[1]! + 1).join(' ')
  return words[0] ?? null
}

const EXT_BY_MIME: Record<string, string> = {
  'image/jpeg': 'jpg', 'image/png': 'png', 'image/webp': 'webp', 'image/gif': 'gif',
  'video/mp4': 'mp4', 'video/3gpp': '3gp',
  'audio/ogg': 'ogg', 'audio/ogg; codecs=opus': 'ogg', 'audio/mpeg': 'mp3', 'audio/mp4': 'm4a', 'audio/aac': 'aac',
  'application/pdf': 'pdf', 'text/plain': 'txt',
}
const MIME_BY_EXT: Record<string, string> = Object.fromEntries(Object.entries(EXT_BY_MIME).map(([m, e]) => [e, m.split(';')[0]!]))
Object.assign(MIME_BY_EXT, {
  jpeg: 'image/jpeg', doc: 'application/msword', docx: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
  xls: 'application/vnd.ms-excel', xlsx: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
  zip: 'application/zip', csv: 'text/csv', json: 'application/json', md: 'text/markdown',
})

function extFor(mime: string | null, name: string | null): string {
  if (mime && EXT_BY_MIME[mime]) return EXT_BY_MIME[mime]!
  const fromName = name ? path.extname(name).slice(1) : ''
  if (fromName) return fromName
  return mime ? (mime.split('/')[1] ?? 'bin').split(';')[0]! : 'bin'
}

export function mediaDir(chatJid: string): string {
  return path.join(dirs.media, jidUser(chatJid))
}
export function thumbPath(chatJid: string, id: string): string {
  return path.join(mediaDir(chatJid), `${id}.thumb.jpg`)
}
/** A link's preview image, and the mark left when it has none to get (`.none`), so it isn't asked for again. */
export function previewPath(chatJid: string, id: string): string {
  return path.join(mediaDir(chatJid), `${id}.link.jpg`)
}
/** Whether a message's link preview carries an image, to fetch (thumbnailDirectPath) or inline (jpegThumbnail). */
export function hasPreviewImage(row: MessageRow): boolean {
  return row.raw.includes('"matchedText"') && (row.raw.includes('"thumbnailDirectPath"') || row.raw.includes('"jpegThumbnail"'))
}
/** Where a message's attachment is on disk: `media_path` is kept relative to the media folder, so the folder can move. */
export function mediaFile(row: MessageRow): string | null {
  return row.media_path ? path.join(dirs.media, row.media_path) : null
}

export interface Parsed {
  id: string
  chatJid: string
  senderJid: string
  fromMe: boolean
  ts: number
  type: string
  text: string
  pushName: string | null
  quoted: string | null
  mediaMime: string | null
  mediaName: string | null
  mediaW: number | null
  mediaH: number | null
  status: number | null
  thumb: Uint8Array | null
}

function quotedSnippet(msg: proto.IMessage | null | undefined): string {
  const c = normalizeMessageContent(msg ?? undefined)
  if (!c) return ''
  const kind = getContentType(c)
  switch (kind) {
    case 'conversation': return c.conversation ?? ''
    case 'extendedTextMessage': return c.extendedTextMessage?.text ?? ''
    case 'imageMessage': return `[${t('image')}] ${c.imageMessage?.caption ?? ''}`.trim()
    case 'videoMessage': return `[${t('video')}] ${c.videoMessage?.caption ?? ''}`.trim()
    case 'stickerMessage': return `[${t('sticker')}]`
    case 'audioMessage': return `[${t('audio')}]`
    case 'documentMessage': return `[${t('file')}] ${c.documentMessage?.fileName ?? ''}`.trim()
    case 'locationMessage': return `[${t('location')}]`
    case 'contactMessage': return `[${t('contact')}] ${c.contactMessage?.displayName ?? ''}`.trim()
    default: return kind ? `[${kind.replace(/Message$/, '')}]` : ''
  }
}

export interface Reaction { chatJid: string; msgId: string; senderJid: string; emoji: string }

/**
 * A reaction (emoji on a previous message) is not a message: it's stored in its own table. Returns what was
 * stored, for the UI to redraw and animate, or null if the WAMessage isn't a reaction.
 */
export function storeReaction(m: WAMessage, meJid: string): Reaction | null {
  const r = normalizeMessageContent(m.message ?? undefined)?.reactionMessage
  if (!r?.key?.id || !m.key?.remoteJid) return null
  const chatJid = canonicalJid(m.key.remoteJid, m.key.remoteJidAlt)
  if (!chatJid) return null
  const fromMe = !!m.key.fromMe
  const senderJid = fromMe ? meJid : isJidGroup(chatJid) ? canonicalJid(m.key.participant || m.participant, m.key.participantAlt) : chatJid
  const ts = r.senderTimestampMs ? toNumber(r.senderTimestampMs) : toNumber(m.messageTimestamp) * 1000
  const emoji = r.text ?? ''
  store.setReaction(chatJid, r.key.id, senderJid, emoji, ts)
  return { chatJid, msgId: r.key.id, senderJid, emoji }
}

/** Translates a WAMessage from baileys into the row we store. Returns null for what isn't a visible message. */
export function parseMessage(m: WAMessage, meJid: string): Parsed | null {
  const key = m.key
  if (!key?.id || !key.remoteJid) return null
  const chatJid = canonicalJid(key.remoteJid, key.remoteJidAlt)
  if (!chatJid || isJidStatusBroadcast(chatJid) || isJidNewsletter(chatJid) || isJidBroadcast(chatJid)) return null
  const isGroup = isJidGroup(chatJid)
  const fromMe = !!key.fromMe
  // In a group the sender is the key's participant; messages from the history sync carry it in the message's own
  // `participant` instead.
  const senderJid = fromMe ? meJid : isGroup ? canonicalJid(key.participant || m.participant, key.participantAlt) : chatJid

  const content = normalizeMessageContent(m.message ?? undefined)
  if (!content) return null
  const ctype = getContentType(content)
  if (!ctype) return null

  const p: Parsed = {
    id: key.id, chatJid, senderJid, fromMe, ts: toNumber(m.messageTimestamp) || Math.floor(Date.now() / 1000),
    type: 'text', text: '', pushName: m.pushName ?? null, quoted: null,
    mediaMime: null, mediaName: null, mediaW: null, mediaH: null, status: m.status ?? null, thumb: null,
  }
  let ctx: proto.IContextInfo | null | undefined

  switch (ctype) {
    case 'conversation':
      p.text = content.conversation ?? ''
      break
    case 'extendedTextMessage':
      p.text = content.extendedTextMessage?.text ?? ''
      ctx = content.extendedTextMessage?.contextInfo
      break
    case 'imageMessage': {
      const im = content.imageMessage!
      p.type = 'image'; p.text = im.caption ?? ''; p.mediaMime = im.mimetype ?? 'image/jpeg'
      p.mediaW = im.width ?? null; p.mediaH = im.height ?? null; p.thumb = im.jpegThumbnail ?? null; ctx = im.contextInfo
      break
    }
    case 'videoMessage': {
      const v = content.videoMessage!
      p.type = v.gifPlayback ? 'gif' : 'video'; p.text = v.caption ?? ''; p.mediaMime = v.mimetype ?? 'video/mp4'
      p.mediaW = v.width ?? null; p.mediaH = v.height ?? null; p.thumb = v.jpegThumbnail ?? null; ctx = v.contextInfo
      break
    }
    case 'stickerMessage': {
      const s = content.stickerMessage!
      p.type = 'sticker'; p.mediaMime = s.mimetype ?? 'image/webp'; p.mediaW = s.width ?? null; p.mediaH = s.height ?? null
      break
    }
    case 'documentMessage': {
      const d = content.documentMessage!
      p.type = 'document'; p.text = d.caption ?? ''; p.mediaMime = d.mimetype ?? 'application/octet-stream'
      p.mediaName = d.fileName ?? null; p.thumb = d.jpegThumbnail ?? null; ctx = d.contextInfo
      break
    }
    case 'audioMessage': {
      const a = content.audioMessage!
      p.type = a.ptt ? 'voice' : 'audio'; p.mediaMime = a.mimetype ?? 'audio/ogg; codecs=opus'
      p.text = a.seconds ? `${a.seconds}s` : ''; ctx = a.contextInfo
      break
    }
    case 'locationMessage':
    case 'liveLocationMessage': {
      const l = content.locationMessage ?? content.liveLocationMessage!
      const loc = content.locationMessage
      p.type = 'location'
      p.text = [loc?.name, loc?.address, `https://maps.google.com/?q=${l.degreesLatitude},${l.degreesLongitude}`].filter(Boolean).join(' · ')
      break
    }
    case 'contactMessage':
      p.type = 'contact'; p.text = content.contactMessage?.displayName ?? ''
      break
    case 'contactsArrayMessage':
      p.type = 'contact'; p.text = (content.contactsArrayMessage?.contacts ?? []).map(c => c.displayName).filter(Boolean).join(', ')
      break
    case 'pollCreationMessage':
    case 'pollCreationMessageV2':
    case 'pollCreationMessageV3': {
      const poll = (content as Record<string, proto.Message.IPollCreationMessage | null | undefined>)[ctype]
      p.type = 'poll'; p.text = [poll?.name ?? '', ...(poll?.options ?? []).map(o => `  ○ ${o.optionName ?? ''}`)].join('\n')
      break
    }
    case 'reactionMessage':
      return null
    case 'protocolMessage': {
      const pm = content.protocolMessage!
      const targetId = pm.key?.id
      if (targetId && pm.type === 0) {
        if (store.getMessage(chatJid, targetId)) store.setType(chatJid, targetId, 'deleted', '')
      } else if (targetId && pm.type === 14) {
        const edited = parseMessage({ key: { ...key, id: targetId }, message: pm.editedMessage, messageTimestamp: m.messageTimestamp } as WAMessage, meJid)
        if (edited && store.getMessage(chatJid, targetId)) store.setType(chatJid, targetId, edited.type, `${edited.text}\n${t('edited')}`)
      }
      return null
    }
    case 'senderKeyDistributionMessage':
    case 'messageContextInfo':
    // Encrypted content for another message (targetMessageKey), not a message of its own.
    case 'secretEncryptedMessage':
      return null
    default:
      p.type = ctype.replace(/Message$/, '')
      p.text = `[${p.type}]`
  }

  if (ctx?.quotedMessage) {
    const who = canonicalJid(ctx.participant) || (fromMe ? chatJid : meJid)
    p.quoted = `${who}\t${quotedSnippet(ctx.quotedMessage).split('\n')[0]}`
  }
  return p
}

export class Wa extends EventEmitter<WaEvents> {
  sock: WASocket | undefined
  /** Who is typing in each chat, and the timer that forgets it if the "stopped" signal never arrives. */
  private typing = new Map<string, { who: Set<string>; timer: NodeJS.Timeout }>()
  /** Whether this device announced itself "available" to WhatsApp, and the timer that sets it back to unavailable. */
  private available = false
  /**
   * Who is online, per one-to-one chat, and the chats whose presence was asked for. WhatsApp only sends presence
   * while this device is available, and only from people who share it: when this device goes unavailable the
   * states would go stale, so they're dropped, and asked for again when it's back.
   */
  private online = new Map<string, boolean>()
  private subscribed = new Set<string>()
  /** The chats whose profile is waiting to be asked of WhatsApp (ensureProfile), and whether one is being asked. */
  private profileQueue: string[] = []
  private profileAsked = new Set<string>()
  private profileBusy = false
  /** Per group, the members whose presence is followed, and how many of them are online. */
  private groupMembers = new Map<string, string[]>()
  private groupOnline = new Map<string, number>()
  /** Which terminals (by pid) have the focus, for those whose terminal reports it. */
  private focus = new Map<number, boolean>()
  private presenceTimer: NodeJS.Timeout | undefined
  me = ''
  state: ConnState = 'connecting'
  qr: string | undefined
  private groupCache = new Map<string, GroupMetadata>()
  private downloading = new Set<string>()
  private stopped = false
  /** Each chat's state as an emoji, worked out calmly by the local model for the terminals (mood.ts). */
  private moods = new MoodKeeper(row => this.moodLine(row), jid => this.emit('mood', jid))

  async start() {
    // Only the server process writes, so the stored delivery states are put right here, once per start.
    const fixed = store.repairStatuses()
    if (fixed.read || fixed.groupSent) logger.info(fixed, 'delivery states repaired')
    const senders = this.repairSenders()
    if (senders) logger.info({ senders }, 'group senders repaired')
    // Once, the chats already here whose person is a WhatsApp Business account, from the messages that say so.
    if (!store.getState<boolean>('businessScanned')) {
      for (const jid of store.businessChats()) store.setState(`biz:${jid}`, true)
      store.setState('businessScanned', true)
    }
    await this.connect()
  }

  /**
   * Group messages stored with no sender, which showed as "+": the history sync names it in the message's own
   * `participant`, which used to go unread (parseMessage). Their sender is taken from the message as it came, a lid
   * turned into the number when it's known, and its pushName goes to the contact. Safe to repeat: only rows still
   * without a sender are read.
   */
  private repairSenders(): number {
    let n = 0
    store.transaction(() => {
      for (const row of store.messagesWithoutSender()) {
        let m: WAMessage
        try { m = JSON.parse(row.raw, BufferJSON.reviver) as WAMessage } catch { continue }
        const sender = canonicalJid(m.key?.participant || m.participant, m.key?.participantAlt)
        if (!sender) continue
        store.setSender(row.chat_jid, row.id, sender)
        if (m.pushName) store.upsertContact(sender, null, m.pushName)
        n++
      }
    })
    return n
  }

  /**
   * Read receipts in one-to-one chats: whoever read one of my messages read the earlier ones too, which a receipt
   * that went missing or arrived out of order would otherwise leave behind. Takes my messages read now, per chat,
   * and moves my sent or delivered ones up to the latest of them to read, once per chat and batch.
   */
  private settleReads(rows: Iterable<MessageRow | undefined | null>) {
    const latest = new Map<string, number>()
    for (const r of rows) {
      if (!r || !r.from_me || (r.status ?? 0) < 4 || this.isGroup(r.chat_jid)) continue
      latest.set(r.chat_jid, Math.max(latest.get(r.chat_jid) ?? 0, r.ts))
    }
    for (const [chat, ts] of latest) store.markReadBefore(chat, ts)
  }

  /**
   * On exit: tells WhatsApp this device is no longer available, if it had said it was, so the phone goes back to
   * notifying right away instead of waiting for the server to notice the connection is gone, and closes the socket
   * properly. Each step is bounded, so quitting never hangs on a dead connection.
   */
  async stop() {
    this.stopped = true
    this.moods.stop()
    if (this.presenceTimer) { clearTimeout(this.presenceTimer); this.presenceTimer = undefined }
    const sock = this.sock
    if (!sock) return
    const bounded = (p: Promise<unknown>, ms: number) => Promise.race([p.catch(e => logger.warn({ e: String(e) }, 'stop')), new Promise(r => setTimeout(r, ms))])
    if (this.available) { this.available = false; await bounded(sock.sendPresenceUpdate('unavailable'), 1000) }
    await bounded(sock.end(undefined), 1500)
  }

  private isGroup(jid: string): boolean {
    return !!isJidGroup(jid)
  }

  private setState(s: ConnState, detail?: string) {
    this.state = s
    this.emit('connection', s, detail)
  }

  private async connect() {
    const { state, saveCreds } = await useMultiFileAuthState(dirs.auth)
    const { version } = await fetchLatestBaileysVersion()
    const sock = makeWASocket({
      version,
      logger,
      auth: { creds: state.creds, keys: makeCacheableSignalKeyStore(state.keys, logger) },
      browser: Browsers.ubuntu('Chrome'),
      markOnlineOnConnect: false,
      syncFullHistory: process.env.WA_FULL_HISTORY === '1',
      shouldIgnoreJid: jid => isJidBroadcast(jid) || isJidStatusBroadcast(jid) || isJidNewsletter(jid),
      getMessage: async (key: WAMessageKey) => {
        const row = key.id ? store.findMessage(key.id) : undefined
        if (!row) return undefined
        const raw = JSON.parse(row.raw, BufferJSON.reviver) as WAMessage
        return raw.message ?? undefined
      },
      cachedGroupMetadata: async jid => this.groupCache.get(jid),
    })
    this.sock = sock
    this.setState('connecting')

    sock.ev.on('creds.update', saveCreds)
    sock.ev.on('presence.update', ({ id, presences }) => {
      logger.info({ id, presences }, 'presence')
      const chatJid = canonicalJid(id)
      for (const [participant, p] of Object.entries(presences)) {
        const who = canonicalJid(participant)
        const active = p.lastKnownPresence === 'composing' || p.lastKnownPresence === 'recording'
        this.setTyping(chatJid, who, active && who !== this.me)
        // Typing, recording and paused are all online; only "unavailable" isn't.
        if (who !== this.me && !this.isGroup(chatJid)) {
          const on = p.lastKnownPresence !== 'unavailable'
          // When they were last seen, kept for the chat's panel (Ui.drawBorder): now while online, and on going
          // offline the time WhatsApp gives, when the person shares it, or now if they were online until then.
          const seen = on ? Math.floor(Date.now() / 1000) : p.lastSeen ?? (this.online.get(chatJid) ? Math.floor(Date.now() / 1000) : undefined)
          if (seen) store.setState(`seen:${chatJid}`, seen)
          this.setOnline(chatJid, on)
        }
      }
    })

    sock.ev.on('connection.update', async update => {
      const { connection, lastDisconnect, qr } = update
      // Once the notifications kept while offline are through (WhatsApp's own app-state syncs among them), the
      // address book is asked for again if it's due, and then the contacts known only by lid are resolved.
      if (update.receivedPendingNotifications) {
        this.resyncContacts().catch(e => logger.warn({ e }, 'resyncContacts'))
          .then(() => this.resolveLidContacts()).catch(e => logger.warn({ e }, 'resolveLidContacts'))
      }
      if (qr) {
        this.qr = qr
        this.setState('qr')
      }
      if (connection === 'open') {
        this.qr = undefined
        this.me = jidNormalizedUser(sock.user?.id ?? '')
        if (sock.user?.lid) store.setLid(jidNormalizedUser(sock.user.lid), this.me)
        // Who I am, for every process: my own chat goes by my profile name (contactName).
        store.setState('me', this.me)
        if (sock.user?.name) store.setState('meName', sock.user.name)
        // Baileys announces "unavailable" on connect; it's activity in the terminal that sets it back to available.
        this.markAvailable(false)
        // Profiles asked for while it was down are asked now.
        void this.nextProfile()
        this.moods.start()
        this.setState('open', this.me)
        this.refreshGroups().catch(e => logger.warn({ e }, 'refreshGroups'))
      } else if (connection === 'close') {
        const code = (lastDisconnect?.error as Boom | undefined)?.output?.statusCode
        const loggedOut = code === DisconnectReason.loggedOut
        logger.warn({ code, err: lastDisconnect?.error?.message }, 'connection closed')
        this.dropOnline()
        this.markAvailable(false)
        if (this.stopped) return
        if (code === DisconnectReason.connectionReplaced) {
          // Another instance connected with these credentials. Reconnecting here would just kick it out and get kicked out again.
          this.setState('closed', t('anotherInstance'))
          return
        }
        if (loggedOut) {
          fs.rmSync(dirs.auth, { recursive: true, force: true })
          fs.mkdirSync(dirs.auth, { recursive: true })
          this.setState('closed', t('loggedOut'))
        } else {
          this.setState('closed', t('closedReconnecting', code ?? '?'))
        }
        setTimeout(() => this.connect().catch(e => logger.error({ e }, 'reconnect')), loggedOut ? 500 : 2000)
      }
    })

    sock.ev.on('messaging-history.set', ({ chats, contacts, messages, lidPnMappings, progress, syncType, peerDataRequestSessionId }) => {
      let stored = 0
      store.transaction(() => {
        for (const m of lidPnMappings ?? []) store.setLid(jidNormalizedUser(m.lid), jidNormalizedUser(m.pn))
        for (const c of contacts) this.upsertContact(c)
        for (const c of chats) this.upsertChat(c)
        const rows: MessageRow[] = []
        for (const m of messages) {
          if (storeReaction(m, this.me)) continue
          const row = this.storeMessage(m, false)
          if (row) { stored++; rows.push(row) }
        }
        this.settleReads(rows)
      })
      this.emit('chats')
      this.emit('messages', '*')
      // An answer to fetchOlder goes back to whoever asked; the progress line is only for the syncs WhatsApp starts.
      const asked = peerDataRequestSessionId ? this.older.get(peerDataRequestSessionId) : undefined
      if (asked) { this.older.delete(peerDataRequestSessionId!); asked(stored) }
      if (syncType !== proto.HistorySync.HistorySyncType.ON_DEMAND) {
        this.emit('status', t('historyProgress', messages.length, chats.length, progress != null ? ` (${progress}%)` : ''))
      }
    })

    sock.ev.on('messaging-history.status', ({ status }) => {
      if (status === 'complete') this.emit('status', t('historyDone'))
    })

    sock.ev.on('contacts.upsert', cs => { store.transaction(() => cs.forEach(c => this.upsertContact(c))); this.emit('chats') })
    sock.ev.on('contacts.update', cs => { store.transaction(() => cs.forEach(c => c.id && this.upsertContact(c as { id: string }))); this.emit('chats') })
    sock.ev.on('chats.upsert', cs => { store.transaction(() => cs.forEach(c => this.upsertChat(c))); this.emit('chats') })
    sock.ev.on('chats.update', cs => {
      store.transaction(() => {
        for (const c of cs) {
          if (!c.id) continue
          const jid = canonicalJid(c.id, c.pnJid ?? undefined)
          const cur = store.getChat(jid)
          if (!cur) { this.upsertChat(c); continue }
          store.upsertChat({
            jid, name: c.name ?? null, isGroup: this.isGroup(jid),
            lastTs: c.conversationTimestamp ? toNumber(c.conversationTimestamp) : cur.last_ts,
            unread: c.unreadCount ?? cur.unread, archived: c.archived ?? cur.archived === 1,
          })
        }
      })
      this.emit('chats')
    })
    sock.ev.on('lid-mapping.update', m => store.setLid(jidNormalizedUser(m.lid), jidNormalizedUser(m.pn)))

    sock.ev.on('messages.upsert', ({ messages, type }) => {
      const touched = new Set<string>()
      const reactions: Reaction[] = []
      store.transaction(() => {
        const rows: MessageRow[] = []
        for (const m of messages) {
          const reacted = storeReaction(m, this.me)
          if (reacted) { touched.add(reacted.chatJid); if (type === 'notify') reactions.push(reacted); continue }
          const row = this.storeMessage(m, type === 'notify')
          if (!row) continue
          rows.push(row)
          touched.add(row.chat_jid)
          if (type === 'notify' && !row.from_me) this.emit('notify', row.chat_jid, row)
        }
        this.settleReads(rows)
      })
      for (const jid of touched) this.emit('messages', jid)
      for (const r of reactions) this.emit('reaction', r.chatJid, r.msgId, r.senderJid, r.emoji)
      if (touched.size) this.emit('chats')
    })

    sock.ev.on('messages.update', updates => {
      const touched = new Set<string>()
      store.transaction(() => {
        const rows: (MessageRow | undefined)[] = []
        for (const u of updates) {
          if (!u.key.id || !u.key.remoteJid) continue
          const chatJid = canonicalJid(u.key.remoteJid, u.key.remoteJidAlt)
          if (u.update.status == null) continue
          store.setStatus(chatJid, u.key.id, u.update.status)
          rows.push(store.getMessage(chatJid, u.key.id))
          touched.add(chatJid)
        }
        this.settleReads(rows)
      })
      for (const jid of touched) this.emit('messages', jid)
    })

    // Receipts in groups come one participant at a time, here rather than in messages.update. Any of them for a
    // message of mine means the server took it: it leaves pending for sent. Delivered and read in a group need
    // every participant, which isn't tracked.
    sock.ev.on('message-receipt.update', receipts => {
      const touched = new Set<string>()
      store.transaction(() => {
        for (const { key } of receipts) {
          if (!key.id || !key.remoteJid || !this.isGroup(key.remoteJid)) continue
          const row = store.getMessage(key.remoteJid, key.id)
          if (!row?.from_me || (row.status ?? 0) >= 2) continue
          store.setStatus(key.remoteJid, key.id, 2)
          touched.add(key.remoteJid)
        }
      })
      for (const jid of touched) this.emit('messages', jid)
    })

    sock.ev.on('messages.delete', del => {
      if ('all' in del) return
      for (const k of del.keys) {
        if (!k.id || !k.remoteJid) continue
        store.setType(canonicalJid(k.remoteJid, k.remoteJidAlt), k.id, 'deleted', '')
        this.emit('messages', canonicalJid(k.remoteJid, k.remoteJidAlt))
      }
    })

    sock.ev.on('groups.upsert', gs => { gs.forEach(g => this.cacheGroup(g)); this.emit('chats') })
    sock.ev.on('groups.update', gs => {
      for (const g of gs) if (g.id && g.subject) store.setChatName(g.id, g.subject)
      this.emit('chats')
    })
  }

  private cacheGroup(g: GroupMetadata) {
    this.groupCache.set(g.id, g)
    store.touchChat(g.id, true, 0)
    if (g.subject) store.setChatName(g.id, g.subject)
  }

  /**
   * The address book from scratch, once per database: names that came before the masked numbers stopped replacing
   * them (see upsertContact) were lost, and only a snapshot of the collection sends every contact again. Forgetting
   * the collection's version is what makes WhatsApp send the snapshot instead of the patches since the last sync.
   * It runs after the pending notifications and under baileys' own app-state lock, so it doesn't cross another sync
   * of the same collection.
   */
  private async resyncContacts() {
    const key = 'contactsResynced'
    if (store.getState<boolean>(key)) return
    const sock = this.sock!
    await sock.appStatePatchMutex.mutex(async () => {
      await sock.authState.keys.set({ 'app-state-sync-version': { critical_unblock_low: null } })
      await sock.resyncAppState(['critical_unblock_low'], false)
    })
    store.setState(key, true)
    logger.info('contacts resynced')
  }

  /**
   * Contacts with a name that WhatsApp delivered only by lid: we ask baileys' mapping repository for each one's
   * number and also store the name under the number, which is how chats are keyed.
   */
  private async resolveLidContacts() {
    const pending = store.lidContactsUnmapped()
    if (!pending.length) return
    const mappings = await this.sock!.signalRepository.lidMapping.getPNsForLIDs(pending.map(c => c.jid))
    let resolved = 0
    store.transaction(() => {
      for (const m of mappings ?? []) {
        const lid = jidNormalizedUser(m.lid), pn = jidNormalizedUser(m.pn)
        const c = pending.find(k => k.jid === lid)
        if (!c) continue
        store.setLid(lid, pn)
        store.upsertContact(pn, c.name, c.notify)
        resolved++
      }
    })
    logger.info({ pending: pending.length, resolved }, 'lid contacts resolved')
    if (resolved) this.emit('chats')
  }

  private async refreshGroups() {
    const groups = await this.sock!.groupFetchAllParticipating()
    store.transaction(() => Object.values(groups).forEach(g => this.cacheGroup(g)))
    this.emit('chats')
  }

  private upsertContact(c: { id: string; lid?: string; phoneNumber?: string; name?: string | null; notify?: string | null }) {
    let jid = canonicalJid(c.id, c.phoneNumber)
    if (c.lid && c.phoneNumber) store.setLid(jidNormalizedUser(c.lid), jidNormalizedUser(c.phoneNumber))
    if (jid.endsWith('@lid') && c.phoneNumber) jid = jidNormalizedUser(c.phoneNumber)
    if (!jid) return
    // The history sync names each chat with the masked number when it has nothing better, even for contacts saved in
    // the address book: stored as the name, it would replace the real one that came before. With no name stored yet
    // it's kept, better than none ("lid:…" for someone known only by lid).
    const masked = !!c.name && looksLikeNumber(c.name)
    const name = masked && store.getContact(jid)?.name ? null : c.name ?? null
    store.upsertContact(jid, name, c.notify ?? null)
  }

  private upsertChat(c: { id?: string | null; name?: string | null; unreadCount?: number | null; conversationTimestamp?: number | Long | null; archived?: boolean | null; pnJid?: string | null; lidJid?: string | null }) {
    const jid = canonicalJid(c.id, c.pnJid ?? undefined)
    if (!c.id || !jid || isJidStatusBroadcast(jid) || isJidNewsletter(jid) || isJidBroadcast(jid)) return
    if (c.lidJid && !jid.endsWith('@lid')) store.setLid(jidNormalizedUser(c.lidJid), jid)
    store.upsertChat({
      jid, name: c.name ?? null, isGroup: this.isGroup(jid),
      lastTs: c.conversationTimestamp ? toNumber(c.conversationTimestamp) : 0,
      unread: c.unreadCount ?? 0, archived: !!c.archived,
    })
  }

  /** Stores the message (and the thumbnail, if any) and returns the row; `live` marks new messages as unread. */
  private storeMessage(m: WAMessage, live: boolean): MessageRow | null {
    const p = parseMessage(m, this.me)
    if (!p) return null
    const existed = store.hasMessage(p.chatJid, p.id)
    const row: Omit<MessageRow, 'media_err'> = {
      id: p.id, chat_jid: p.chatJid, sender_jid: p.senderJid, from_me: p.fromMe ? 1 : 0, ts: p.ts, type: p.type, text: p.text,
      push_name: p.pushName, quoted: p.quoted, media_path: null, media_mime: p.mediaMime, media_name: p.mediaName,
      media_w: p.mediaW, media_h: p.mediaH, status: p.status, raw: JSON.stringify(m, BufferJSON.replacer),
    }
    store.upsertMessage(row)
    store.touchChat(p.chatJid, this.isGroup(p.chatJid), p.ts)
    if (p.pushName && !p.fromMe) store.upsertContact(p.senderJid, null, p.pushName)
    // A message from a WhatsApp Business account carries its verified name: the chat's number is a business one.
    if (m.verifiedBizName && !p.fromMe && !this.isGroup(p.chatJid) && !store.getState<boolean>(`biz:${p.chatJid}`)) store.setState(`biz:${p.chatJid}`, true)
    if (p.thumb && p.thumb.length > 0) {
      const tp = thumbPath(p.chatJid, p.id)
      if (!fs.existsSync(tp)) {
        fs.mkdirSync(mediaDir(p.chatJid), { recursive: true })
        fs.writeFileSync(tp, p.thumb)
      }
    }
    if (live && !existed && !p.fromMe) store.bumpUnread(p.chatJid)
    return store.getMessage(p.chatJid, p.id)!
  }

  /** Sends text; with `replyTo` (id of a message in this chat) it goes as a reply, with the quote. */
  async send(chatJid: string, text: string, replyTo?: string, mentions?: string[]) {
    const quoted = replyTo ? this.rawMessage(chatJid, replyTo) : undefined
    const sent = await this.sock!.sendMessage(chatJid, mentions?.length ? { text, mentions } : { text }, quoted ? { quoted } : undefined)
    if (sent) { store.transaction(() => this.storeMessage(sent, false)); this.emit('messages', chatJid); this.emit('chats') }
  }

  /** Reacts to a message with an emoji; empty removes the previous reaction. */
  async react(chatJid: string, msgId: string, emoji: string) {
    const target = this.rawMessage(chatJid, msgId)
    if (!target) throw new Error(t('unknownMessage'))
    const sent = await this.sock!.sendMessage(chatJid, { react: { text: emoji, key: target.key } })
    const stored = sent && storeReaction(sent, this.me)
    if (stored) { this.emit('messages', chatJid); this.emit('reaction', stored.chatJid, stored.msgId, stored.senderJid, stored.emoji) }
  }

  /** Replaces the text of one of my messages; in the database it's marked like edits that arrive from others. */
  async edit(chatJid: string, msgId: string, text: string) {
    const target = this.rawMessage(chatJid, msgId)
    if (!target?.key.fromMe) throw new Error(t('onlyOwnEdit'))
    await this.sock!.sendMessage(chatJid, { text, edit: target.key })
    store.setType(chatJid, msgId, 'text', `${text}\n${t('edited')}`)
    this.emit('messages', chatJid)
    this.emit('chats')
  }

  /**
   * Deletes a message: one of mine for everyone (WhatsApp leaves "mensagem apagada" in its place, on both sides), or
   * any for me only, which goes from here and, through the app state, from the phone.
   */
  async deleteMessage(chatJid: string, msgId: string, forEveryone: boolean) {
    const row = store.getMessage(chatJid, msgId)
    const target = this.rawMessage(chatJid, msgId)
    if (!row || !target) throw new Error(t('messageGone'))
    if (forEveryone) {
      if (!target.key.fromMe) throw new Error(t('onlyOwnDelete'))
      await this.sock!.sendMessage(chatJid, { delete: target.key })
      store.setType(chatJid, msgId, 'deleted', '')
    } else {
      await this.sock!.chatModify({ deleteForMe: { deleteMedia: false, key: target.key, timestamp: row.ts } }, chatJid)
      store.removeMessage(chatJid, msgId)
    }
    this.emit('messages', chatJid)
    this.emit('chats')
  }

  private rawMessage(chatJid: string, id: string): WAMessage | undefined {
    const row = store.getMessage(chatJid, id)
    return row ? (JSON.parse(row.raw, BufferJSON.reviver) as WAMessage) : undefined
  }

  async sendFile(chatJid: string, filePath: string, caption?: string) {
    const ext = path.extname(filePath).slice(1).toLowerCase()
    const mimetype = MIME_BY_EXT[ext] ?? 'application/octet-stream'
    let content: AnyMessageContent
    if (['jpg', 'jpeg', 'png', 'webp'].includes(ext)) content = { image: { url: filePath }, caption }
    else if (ext === 'mp4') content = { video: { url: filePath }, caption }
    else if (['mp3', 'ogg', 'm4a'].includes(ext)) content = { audio: { url: filePath }, mimetype }
    else content = { document: { url: filePath }, mimetype, fileName: path.basename(filePath), caption }
    const sent = await this.sock!.sendMessage(chatJid, content)
    if (sent) {
      store.transaction(() => {
        const row = this.storeMessage(sent, false)
        if (row && row.media_mime) {
          const dir = mediaDir(chatJid)
          fs.mkdirSync(dir, { recursive: true })
          const file = path.join(dir, `${row.id}.${ext || extFor(mimetype, filePath)}`)
          fs.copyFileSync(filePath, file)
          store.setMedia(chatJid, row.id, path.relative(dirs.media, file), null, null)
        }
      })
      this.emit('messages', chatJid); this.emit('chats')
    }
  }

  /** Registers (or removes) who's typing in a chat and notifies if the list changed. */
  private setTyping(chatJid: string, who: string, active: boolean) {
    const cur = this.typing.get(chatJid)
    const had = cur?.who.has(who) ?? false
    if (active === had && !active) return
    if (cur) clearTimeout(cur.timer)
    const set = cur?.who ?? new Set<string>()
    if (active) set.add(who); else set.delete(who)
    if (set.size) {
      // If the "stopped" signal gets lost, forget it after 15 seconds without updates.
      this.typing.set(chatJid, { who: set, timer: setTimeout(() => { this.typing.delete(chatJid); this.emit('typing', chatJid, []) }, 15000) })
    } else this.typing.delete(chatJid)
    if (active !== had) this.emit('typing', chatJid, [...set])
  }

  /**
   * Someone is using a terminal: announces this device as available, which is the condition for WhatsApp to send
   * who's typing, and goes back to unavailable after 2 minutes without activity, so the phone starts notifying
   * again. Any connected terminal extends the deadline, except one that said it doesn't have the focus.
   */
  touchPresence(terminal?: number) {
    if (terminal != null && this.focus.get(terminal) === false) return
    if (this.presenceTimer) clearTimeout(this.presenceTimer)
    this.presenceTimer = setTimeout(() => { this.presenceTimer = undefined; this.setAvailable(false) }, 120000)
    this.setAvailable(true)
  }

  private setAvailable(on: boolean) {
    if (on === this.available || !this.sock) return
    this.markAvailable(on)
    const sock = this.sock
    sock.sendPresenceUpdate(on ? 'available' : 'unavailable')
      .then(() => { if (on) for (const jid of this.subscribed) sock.presenceSubscribe(jid).catch(e => logger.warn({ e, jid }, 'presenceSubscribe')) })
      .catch(e => logger.warn({ e }, 'sendPresenceUpdate'))
    if (!on) this.dropOnline()
  }

  /**
   * A terminal (by pid) gained or lost the focus, as its terminal reports it, the way WhatsApp is online only while
   * it's in front: losing it takes this device offline at once, unless another live terminal has it. Gaining it
   * doesn't make it online by itself: a key or the mouse does, as when opening.
   */
  setFocus(terminal: number, focused: boolean) {
    this.focus.set(terminal, focused)
    if (focused) return
    for (const [pid, f] of this.focus) if (f && pid !== terminal && alive(pid)) return
    if (this.presenceTimer) { clearTimeout(this.presenceTimer); this.presenceTimer = undefined }
    this.setAvailable(false)
  }

  /** Records this device's own presence and tells the terminals, which show it in the cursor. */
  private markAvailable(on: boolean) {
    if (on === this.available) return
    this.available = on
    this.emit('available', on)
  }

  private setOnline(chatJid: string, on: boolean) {
    if ((this.online.get(chatJid) ?? false) === on) return void this.online.set(chatJid, on)
    this.online.set(chatJid, on)
    this.emit('presence', chatJid, on)
    for (const [group, members] of this.groupMembers) if (members.includes(chatJid)) this.countGroup(group)
  }

  /** Recounts a group's followed members online, and says so when the count changed (or `always`). */
  private countGroup(group: string, always = false) {
    const n = (this.groupMembers.get(group) ?? []).filter(j => this.online.get(j)).length
    if (!always && n === (this.groupOnline.get(group) ?? 0)) return
    this.groupOnline.set(group, n)
    this.emit('groupOnline', group, n)
  }

  /** Nothing more will arrive (this device unavailable, or the connection gone): everyone known online goes back to unknown. */
  private dropOnline() {
    for (const [jid, on] of this.online) if (on) this.emit('presence', jid, false)
    this.online.clear()
    for (const [group, n] of this.groupOnline) if (n) this.emit('groupOnline', group, 0)
    this.groupOnline.clear()
  }

  /** Tells the chat that we're typing (or that we stopped): it's the "typing…" that the other person sees. */
  setComposing(chatJid: string, on: boolean) {
    // While I write, the local model is left to the writing suggestions.
    if (on) this.moods.writing()
    this.sock?.sendPresenceUpdate(on ? 'composing' : 'paused', chatJid).catch(e => logger.warn({ e, chatJid }, 'setComposing'))
  }

  /** A message as the model reads it to work out the chat's state: who wrote it and its text, or what it is. */
  private moodLine(row: MessageRow): string | null {
    if (row.type === 'deleted' || row.type === 'secretEncrypted' || row.type === 'reaction') return null
    const who = row.from_me ? t('me') : this.isGroup(row.chat_jid) ? contactName(row.sender_jid) : chatName(row.chat_jid)
    const text = (row.text || `[${typeLabel(row)}]`).replace(/\s+/g, ' ').slice(0, 500)
    return `${who}: ${text}`
  }

  /**
   * Asks WhatsApp for a chat's presence (online, typing, recording); without this nothing arrives. What's already
   * known is said again, for a terminal that opens the chat after another one did.
   */
  subscribePresence(chatJid: string) {
    this.subscribed.add(chatJid)
    this.sock?.presenceSubscribe(chatJid).catch(e => logger.warn({ e, chatJid }, 'presenceSubscribe'))
    if (this.online.get(chatJid)) this.emit('presence', chatJid, true)
    // A group's presence only brings who's typing in it: who's online is each member's own, so the members are
    // followed one by one, but only the 30 who wrote most recently, to keep the subscriptions few.
    if (this.isGroup(chatJid)) {
      const members = store.recentSenders(chatJid, 30).filter(j => j !== this.me)
      this.groupMembers.set(chatJid, members)
      for (const m of members) {
        if (this.subscribed.has(m)) continue
        this.subscribed.add(m)
        this.sock?.presenceSubscribe(m).catch(e => logger.warn({ e, jid: m }, 'presenceSubscribe'))
      }
      this.countGroup(chatJid, true)
    }
    // And this device's own presence, which such a terminal doesn't know either.
    this.emit('available', this.available)
  }

  async markRead(chatJid: string) {
    const chat = store.getChat(chatJid)
    if (!chat || chat.unread === 0) return
    const rows = store.unreadIncoming(chatJid, Math.max(chat.unread, 1))
    const keys = rows.map(r => (JSON.parse(r.raw, BufferJSON.reviver) as WAMessage).key)
    store.clearUnread(chatJid)
    this.emit('chats')
    if (keys.length && this.sock) await this.sock.readMessages(keys).catch(e => logger.warn({ e }, 'readMessages'))
  }

  /** Link previews being fetched, by chat and message. */
  private previewing = new Set<string>()

  /**
   * Gets a link preview's image in the background, in the same queue as the attachments: the full-size one the
   * sender's phone uploaded (encrypted, at thumbnailDirectPath), or else the small one inside the message. When
   * neither comes (the upload expired, an old message), a `.none` mark stops it being asked for again.
   * Notifies via 'messages' when done either way.
   */
  /**
   * A one-to-one chat whose person has no name here, neither in the address book nor as a profile name (only their
   * number, or the masked one WhatsApp gives for someone not saved): its latest messages are asked of the phone again,
   * at most once a day, as its copy may carry the profile name the messages as they came didn't. What comes is stored
   * as any history is, the profile name going to the contact (storeMessage).
   */
  ensureName(jid: string) {
    const sock = this.sock
    if (!sock || this.state !== 'open' || this.isGroup(jid)) return
    const c = store.getContact(jid)
    if (c?.notify || (c?.name && !looksLikeNumber(c.name))) return
    const asked = `nameAsked:${jid}`
    if (Date.now() - (store.getState<number>(asked) ?? 0) < 86400_000) return
    const newest = store.listMessages(jid, 1)[0]
    if (!newest) return
    store.setState(asked, Date.now())
    const { key } = JSON.parse(newest.raw, BufferJSON.reviver) as WAMessage
    sock.fetchMessageHistory(50, { remoteJid: key.remoteJid, fromMe: key.fromMe, id: key.id }, newest.ts)
      .catch(e => logger.warn({ e: String(e), jid }, 'ensureName'))
  }

  /**
   * A chat's picture, "about" and, for a group, how many members it has (for a person, whether the number is a
   * WhatsApp Business account), asked of WhatsApp for the chat list at most once a day, one chat at a time; the
   * picture, whole (shown small in the list and large when clicked), goes to the chat's media folder. A picture or
   * "about" hidden by privacy, or missing, is stored as none; a request that fails is tried again the next day.
   */
  ensureProfile(jid: string) {
    const known = store.getProfile(jid)
    if (known && Date.now() / 1000 - known.fetched < 86400) return
    if (!this.profileAsked.has(jid)) { this.profileAsked.add(jid); this.profileQueue.push(jid) }
    void this.nextProfile()
  }

  private async nextProfile() {
    const sock = this.sock
    if (this.profileBusy || !sock || this.state !== 'open') return
    const jid = this.profileQueue.shift()
    if (!jid) return
    this.profileBusy = true
    const fetched = Math.floor(Date.now() / 1000)
    try {
      let about: string | null = null, members: number | null = null, avatar = ''
      if (jid.endsWith('@g.us')) members = (await sock.groupMetadata(jid)).participants.length
      else {
        about = ((await sock.fetchStatus(jid))?.[0] as { status?: { status?: string | null } } | undefined)?.status?.status?.trim() || null
        // Whether the number is a WhatsApp Business account: it has a business profile.
        const business = await sock.getBusinessProfile(jid).then(b => !!b, () => undefined)
        if (business !== undefined) store.setState(`biz:${jid}`, business)
      }
      const url = await sock.profilePictureUrl(jid, 'image').catch(() => undefined)
      if (url) {
        const res = await fetch(url)
        if (res.ok) {
          fs.mkdirSync(mediaDir(jid), { recursive: true })
          fs.writeFileSync(path.join(mediaDir(jid), 'profile.jpg'), Buffer.from(await res.arrayBuffer()))
          avatar = 'profile.jpg'
        }
      }
      store.setProfile({ jid, about, members, avatar, fetched })
      // Its own event, not 'chats': the list draws only that chat's rows again, instead of being made anew (and
      // pulled back to the selection) at every photo that arrives.
      this.emit('profile', jid)
    } catch (e) {
      logger.debug({ e: String(e), jid }, 'profile')
      store.setProfile({ jid, about: null, members: null, avatar: '', fetched })
    } finally {
      this.profileAsked.delete(jid)
      this.profileBusy = false
      void this.nextProfile()
    }
  }

  ensurePreview(row: MessageRow) {
    const k = `${row.chat_jid}/${row.id}`, file = previewPath(row.chat_jid, row.id)
    if (this.previewing.has(k) || !this.sock || fs.existsSync(file) || fs.existsSync(`${file}.none`)) return
    this.previewing.add(k)
    this.downloadQueue = this.downloadQueue.then(() => this.previewOne(row, file))
  }

  private async previewOne(row: MessageRow, file: string) {
    let buf: Buffer | undefined
    try {
      const raw = JSON.parse(row.raw, BufferJSON.reviver) as WAMessage
      const x = normalizeMessageContent(raw.message)?.extendedTextMessage
      if (x?.thumbnailDirectPath && x.mediaKey?.length) {
        try {
          const stream = await downloadContentFromMessage({ mediaKey: x.mediaKey, directPath: x.thumbnailDirectPath, url: undefined }, 'thumbnail-link')
          const chunks: Buffer[] = []
          for await (const c of stream) chunks.push(c as Buffer)
          buf = Buffer.concat(chunks)
        } catch (e) { logger.warn({ e: (e as Error)?.message, id: row.id }, 'link preview download failed') }
      }
      if (!buf?.length && x?.jpegThumbnail?.length) buf = Buffer.from(x.jpegThumbnail)
      fs.mkdirSync(mediaDir(row.chat_jid), { recursive: true })
      if (buf?.length) fs.writeFileSync(file, buf)
      else fs.writeFileSync(`${file}.none`, '')
    } catch (e) {
      logger.warn({ e: (e as Error)?.message, id: row.id }, 'link preview')
    } finally {
      this.previewing.delete(`${row.chat_jid}/${row.id}`)
      this.emit('messages', row.chat_jid)
    }
  }

  /**
   * Downloads the attachment in the background if we don't have it yet; notifies via 'messages' once it's on disk.
   * One download at a time: opening an old chat used to request dozens at once and clogged the connection and CPU.
   */
  ensureMedia(row: MessageRow) {
    if (!row.media_mime || row.media_path || row.media_err || !this.sock) return
    const k = `${row.chat_jid}/${row.id}`
    if (this.downloading.has(k)) return
    this.downloading.add(k)
    const sock = this.sock
    this.downloadQueue = this.downloadQueue.then(() => this.downloadOne(row, sock))
  }

  private downloadQueue: Promise<void> = Promise.resolve()

  private async downloadOne(row: MessageRow, sock: WASocket) {
    const k = `${row.chat_jid}/${row.id}`
    try {
      const raw = JSON.parse(row.raw, BufferJSON.reviver) as WAMessage
      const buf = await downloadMediaMessage(raw, 'buffer', {}, { logger, reuploadRequest: sock.updateMediaMessage })
      const dir = mediaDir(row.chat_jid)
      fs.mkdirSync(dir, { recursive: true })
      const file = path.join(dir, `${row.id}.${extFor(row.media_mime, row.media_name)}`)
      fs.writeFileSync(file, buf as Buffer)
      store.setMedia(row.chat_jid, row.id, path.relative(dirs.media, file), row.media_w, row.media_h)
    } catch (e) {
      logger.warn({ e: (e as Error)?.message, id: row.id }, 'download failed')
      store.setMediaErr(row.chat_jid, row.id)
    } finally {
      this.downloading.delete(k)
      this.emit('messages', row.chat_jid)
    }
  }

  /** Pending fetchOlder requests, by the session id the phone answers with. */
  private older = new Map<string, (stored: number) => void>()

  /**
   * Asks the phone for up to 50 messages older than the oldest stored for this chat: WhatsApp's on-demand history,
   * which only reaches back past what this device already holds (anchored anywhere else, the phone answers empty).
   * Resolves with how many came, 0 once the phone has nothing older; fails if it doesn't answer within 30 s, so a
   * later try can ask again.
   */
  async fetchOlder(chatJid: string): Promise<number> {
    const sock = this.sock
    if (!sock || this.state !== 'open') throw new Error(t('notConnectedYet'))
    const oldest = store.oldestMessage(chatJid)
    if (!oldest) return 0
    const { key } = JSON.parse(oldest.raw, BufferJSON.reviver) as WAMessage
    const session = await sock.fetchMessageHistory(50, { remoteJid: key.remoteJid, fromMe: key.fromMe, id: key.id }, oldest.ts)
    const stored = await new Promise<number>((resolve, reject) => {
      const timer = setTimeout(() => { this.older.delete(session); reject(new Error('fetchOlder: no answer from the phone')) }, 30000)
      this.older.set(session, n => { clearTimeout(timer); resolve(n) })
    })
    // Only what lands before the old anchor counts: messages that were already here don't move it.
    const now = store.oldestMessage(chatJid)
    return now && now.ts < oldest.ts ? stored : 0
  }

  /** Copies all the chat's attachments to ~/Downloads/wa/<chat>/, downloading whatever is missing. */
  async downloadAll(chatJid: string): Promise<{ copied: number; pending: number }> {
    const out = path.join(dirs.downloads, chatName(chatJid).replace(/[^\p{L}\p{N} _.-]/gu, '_'))
    fs.mkdirSync(out, { recursive: true })
    let copied = 0, pending = 0
    for (const row of store.listMedia(chatJid)) {
      const file = mediaFile(row)
      if (!file) { if (!row.media_err) { this.ensureMedia(row); pending++ }; continue }
      const name = row.media_name ?? path.basename(file)
      const dest = path.join(out, `${new Date(row.ts * 1000).toISOString().slice(0, 10)}_${name}`)
      if (!fs.existsSync(dest)) { fs.copyFileSync(file, dest); copied++ }
    }
    return { copied, pending }
  }
}
