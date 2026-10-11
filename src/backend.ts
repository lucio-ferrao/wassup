import type { EventEmitter } from 'node:events'
import type { MessageRow } from './db.js'
import type { ConnState, WaEvents } from './wa.js'

/**
 * What the UI needs from whoever talks to WhatsApp. `Wa` (our own connection, in the server process) and
 * `RemoteWa` (another process's client, over the socket) implement it; the UI doesn't know which one it has.
 */
export interface Backend extends EventEmitter<WaEvents> {
  readonly me: string
  readonly state: ConnState
  readonly qr: string | undefined
  /** `mentions`: the jids mentioned in a group, each written in the text as "@" and its number. */
  send(chatJid: string, text: string, replyTo?: string, mentions?: string[]): Promise<void>
  react(chatJid: string, msgId: string, emoji: string): Promise<void>
  edit(chatJid: string, msgId: string, text: string): Promise<void>
  /** One of my messages for everyone (`forEveryone`), or any for me only (see Wa.deleteMessage). */
  deleteMessage(chatJid: string, msgId: string, forEveryone: boolean): Promise<void>
  sendFile(chatJid: string, filePath: string, caption?: string): Promise<void>
  markRead(chatJid: string): Promise<void>
  subscribePresence(chatJid: string): void
  setComposing(chatJid: string, on: boolean): void
  /** Activity in this terminal (its pid), which keeps the device online. */
  touchPresence(terminal: number): void
  /** This terminal (its pid) gained or lost the focus. */
  setFocus(terminal: number, focused: boolean): void
  ensureMedia(row: MessageRow): void
  /** Fetches a link preview's image, if it has one (see Wa.ensurePreview). */
  ensurePreview(row: MessageRow): void
  /** Asks WhatsApp for a chat's picture, "about" and group size, at most once a day (see Wa.ensureProfile). */
  ensureProfile(jid: string): void
  /** Asks the phone again for a chat's latest messages when its person has no name here (see Wa.ensureName). */
  ensureName(jid: string): void
  downloadAll(chatJid: string): Promise<{ copied: number; pending: number }>
  /** Asks the phone for messages older than the oldest stored for this chat; resolves with how many came, 0 for none. */
  fetchOlder(chatJid: string): Promise<number>
  stop(): Promise<void>
}
