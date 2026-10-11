/**
 * Each chat's state, as one emoji the local model picks from its latest messages (llm.mood), for the terminals to
 * show after the chat's name. Worked out in the server process, one queue for every terminal, and calmly, as the model
 * takes one request at a time and the writing suggestions must find it free:
 * - only chats with messages since their state was worked out, quiet for QUIET since the last one, and open in a
 *   terminal or with messages in the last day (RECENT), never one whose last message is over a week old (OLD);
 *   from the most recent to the oldest;
 * - one request at a time, a look every PAUSE;
 * - never while I write in a terminal (setComposing), and a request under way gives way when I start (it goes back
 *   to the queue);
 * - the model, asleep, is woken by the request itself, and its requests keep it awake while the queue goes through.
 * The messages go from the latest back, as many as fit in the model's context, short of a margin for the instructions
 * and the answer. Kept in the state table, `mood:<jid>`: the emoji ("" for none) and the time of the last message it
 * was worked out for.
 */
import { store, type MessageRow } from './db.js'
import { logger } from './log.js'
import { llmEnabled, modelInfo, mood } from './llm.js'

export interface Mood { emoji: string; upTo: number }

const QUIET = 120
const RECENT = 24 * 3600
const OLD = 7 * 24 * 3600
const PAUSE = 30_000
/** No request while I typed in a terminal this recently, in ms. */
const WRITING = 30_000
/** Tokens kept for the instructions and the answer, and how many characters a token stands for, short of the truth. */
const MARGIN = 2048
const CHARS_PER_TOKEN = 3
/** A request taking longer than this is given up. */
const TIMEOUT = 180_000

export class MoodKeeper {
  private timer: NodeJS.Timeout | undefined
  private busy: AbortController | undefined
  private writingAt = 0

  /** `line`: a message as the model reads it ("who: text"), or null to leave it out; `told`: a chat's state changed. */
  constructor(private line: (row: MessageRow) => string | null, private told: (jid: string) => void) {}

  start() {
    if (!llmEnabled || this.timer) return
    this.timer = setInterval(() => void this.tick(), PAUSE)
  }

  stop() {
    clearInterval(this.timer)
    this.timer = undefined
    this.busy?.abort()
  }

  /** I'm writing in a terminal: no request for a while, and the one under way gives way. */
  writing() {
    this.writingAt = Date.now()
    this.busy?.abort()
  }

  private async tick() {
    if (this.busy || Date.now() - this.writingAt < WRITING) return
    const chat = this.next()
    if (!chat) return
    const info = await modelInfo()
    if (!info.ctx || this.busy || Date.now() - this.writingAt < WRITING) return
    const budget = Math.max(1000, (info.ctx - MARGIN) * CHARS_PER_TOKEN)
    const rows = store.listMessages(chat.jid, -1)
    const lines: string[] = []
    let used = 0
    for (let i = rows.length - 1; i >= 0; i--) {
      const l = this.line(rows[i]!)
      if (!l) continue
      if (used + l.length + 1 > budget) break
      lines.unshift(l)
      used += l.length + 1
    }
    const abort = new AbortController()
    this.busy = abort
    const timeout = setTimeout(() => abort.abort(), TIMEOUT)
    try {
      const emoji = lines.length ? await mood(lines, abort.signal) : null
      store.setState(`mood:${chat.jid}`, { emoji: emoji ?? '', upTo: chat.last_ts } satisfies Mood)
      logger.info({ chat: chat.jid, lines: lines.length, emoji }, 'mood')
      this.told(chat.jid)
    } catch (e) {
      // Given way to the writing, or the server down: the chat stays in the queue.
      if (!abort.signal.aborted) logger.warn({ e: String(e) }, 'mood')
    } finally {
      clearTimeout(timeout)
      this.busy = undefined
    }
  }

  /** The next chat due, if any (see the conditions above). */
  private next() {
    const now = Date.now() / 1000
    const open = new Set(store.listState<{ tabs?: string[] }>('tabs:').flatMap(r => r.value.tabs ?? []))
    const due = store.listChats().filter(c => !c.archived && c.last_ts && now - c.last_ts >= QUIET && now - c.last_ts <= OLD
      && (open.has(c.jid) || now - c.last_ts < RECENT)
      && (store.getState<Mood>(`mood:${c.jid}`)?.upTo ?? 0) < c.last_ts)
    due.sort((a, b) => b.last_ts - a.last_ts)
    return due[0]
  }
}
