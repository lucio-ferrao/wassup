import { createHash } from 'node:crypto'
import { EventEmitter } from 'node:events'
import fs from 'node:fs'
import os from 'node:os'
import net from 'node:net'
import path from 'node:path'
import { dirs } from './config.js'
import { t } from './i18n.js'
import { logger } from './log.js'
import { store, type MessageRow } from './db.js'
import type { Backend } from './backend.js'
import type { ConnState, WaEvents } from './wa.js'

/**
 * Several `wa` processes: the first one is the server (WhatsApp connection, database writes) and opens this
 * socket; the others connect to it as clients. Only the actions that need the connection and the events the
 * server broadcasts go over the socket; reads (chats, messages, names) are done by each process directly on
 * SQLite, which accepts several readers in WAL mode. Protocol: one JSON line per message.
 */
// A Unix socket path has a 108-byte limit: it lives in the user's runtime directory (or the temp one), with a
// suffix derived from the data folder so different profiles (WA_HOME) don't collide.
export const sockPath = path.join(process.env.XDG_RUNTIME_DIR ?? os.tmpdir(), `wa-${createHash('sha1').update(dirs.base).digest('hex').slice(0, 8)}.sock`)

type Request = { id: number; op: string; args: unknown[] }
type Reply = { id: number; ok: boolean; result?: unknown; error?: string }
type Event = { event: keyof WaEvents | 'hello'; args: unknown[] }

const slimRow = (row: MessageRow): MessageRow => ({ ...row, raw: '' })

function lines(socket: net.Socket, onLine: (obj: unknown) => void) {
  let buf = ''
  socket.setEncoding('utf8')
  socket.on('data', chunk => {
    buf += chunk
    let i: number
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i); buf = buf.slice(i + 1)
      if (!line) continue
      try { onLine(JSON.parse(line)) } catch (e) { logger.warn({ e: (e as Error).message }, 'ipc: invalid line') }
    }
  })
}

const sendJson = (socket: net.Socket, obj: unknown) => { if (!socket.destroyed) socket.write(JSON.stringify(obj) + '\n') }

// ---------- server ----------

export class IpcServer {
  private server: net.Server
  private clients = new Set<net.Socket>()

  constructor(private wa: Backend) {
    this.server = net.createServer(socket => this.accept(socket))
    const forward = (event: keyof WaEvents) => (...args: unknown[]) => {
      // The 'connection' event also carries the current QR: the client has no other way to know it.
      const payload: Event = { event, args: event === 'notify' ? [args[0], slimRow(args[1] as MessageRow)] : event === 'connection' ? [args[0], args[1], wa.qr] : args }
      for (const c of this.clients) sendJson(c, payload)
    }
    for (const ev of ['connection', 'chats', 'messages', 'notify', 'status', 'typing', 'reaction', 'presence', 'available', 'groupOnline', 'profile', 'mood'] as const) wa.on(ev, forward(ev) as never)
  }

  /** Opens the socket. Fails with EADDRINUSE if another process just opened it: the caller should then connect as a client. */
  listen(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.server.once('error', reject)
      this.server.listen(sockPath, () => { this.server.off('error', reject); logger.info({ sockPath }, 'ipc: server'); resolve() })
    })
  }

  private accept(socket: net.Socket) {
    this.clients.add(socket)
    logger.info({ clients: this.clients.size }, 'ipc: client connected')
    sendJson(socket, { event: 'hello', args: [{ me: this.wa.me, state: this.wa.state, qr: this.wa.qr }] } satisfies Event)
    lines(socket, obj => { this.handle(socket, obj as Request).catch(e => logger.warn({ e }, 'ipc: request')) })
    socket.on('close', () => { this.clients.delete(socket) })
    socket.on('error', e => logger.warn({ e: e.message }, 'ipc: client socket'))
  }

  private async handle(socket: net.Socket, req: Request) {
    const reply = (r: Omit<Reply, 'id'>) => sendJson(socket, { id: req.id, ...r } satisfies Reply)
    try {
      const a = req.args as string[]
      switch (req.op) {
        case 'send': await this.wa.send(a[0]!, a[1]!, a[2] ?? undefined, (req.args[3] as string[] | null) ?? undefined); return reply({ ok: true })
        case 'react': await this.wa.react(a[0]!, a[1]!, a[2] ?? ''); return reply({ ok: true })
        case 'edit': await this.wa.edit(a[0]!, a[1]!, a[2]!); return reply({ ok: true })
        case 'deleteMessage': await this.wa.deleteMessage(a[0]!, a[1]!, a[2] === 'all'); return reply({ ok: true })
        case 'sendFile': await this.wa.sendFile(a[0]!, a[1]!, a[2]); return reply({ ok: true })
        case 'markRead': await this.wa.markRead(a[0]!); return reply({ ok: true })
        case 'subscribePresence': this.wa.subscribePresence(a[0]!); return reply({ ok: true })
        case 'setComposing': this.wa.setComposing(a[0]!, a[1] === 'on'); return reply({ ok: true })
        case 'touchPresence': this.wa.touchPresence(Number(a[0])); return reply({ ok: true })
        case 'setFocus': this.wa.setFocus(Number(a[0]), a[1] === 'on'); return reply({ ok: true })
        case 'ensureMedia': { const row = store.getMessage(a[0]!, a[1]!); if (row) this.wa.ensureMedia(row); return reply({ ok: true }) }
        case 'ensurePreview': { const row = store.getMessage(a[0]!, a[1]!); if (row) this.wa.ensurePreview(row); return reply({ ok: true }) }
        case 'ensureProfile': this.wa.ensureProfile(a[0]!); return reply({ ok: true })
        case 'downloadAll': return reply({ ok: true, result: await this.wa.downloadAll(a[0]!) })
        case 'fetchOlder': return reply({ ok: true, result: await this.wa.fetchOlder(a[0]!) })
        default: return reply({ ok: false, error: t('unknownOp', req.op) })
      }
    } catch (e) {
      reply({ ok: false, error: (e as Error).message })
    }
  }

  close() {
    for (const c of this.clients) c.destroy()
    this.server.close()
    try { fs.unlinkSync(sockPath) } catch { /* no longer exists */ }
  }
}

// ---------- client ----------

export class RemoteWa extends EventEmitter<WaEvents> implements Backend {
  me = ''
  state: ConnState = 'connecting'
  qr: string | undefined
  private nextId = 1
  private waiting = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>()

  private constructor(private socket: net.Socket) {
    super()
    lines(socket, obj => this.receive(obj as Reply | Event))
    socket.on('close', () => {
      for (const w of this.waiting.values()) w.reject(new Error(t('serverEnded')))
      this.waiting.clear()
      this.emit('lost')
    })
    socket.on('error', e => logger.warn({ e: e.message }, 'ipc: server socket'))
  }

  /** Connects to the server; null if none is listening (socket missing or dead). */
  static connect(): Promise<RemoteWa | null> {
    return new Promise(resolve => {
      const socket = net.connect(sockPath)
      socket.once('connect', () => resolve(new RemoteWa(socket)))
      socket.once('error', (e: NodeJS.ErrnoException) => {
        if (e.code === 'ECONNREFUSED') { try { fs.unlinkSync(sockPath) } catch { /* no longer exists */ } }
        resolve(null)
      })
    })
  }

  private receive(msg: Reply | Event) {
    if ('event' in msg) {
      if (msg.event === 'hello') {
        const h = msg.args[0] as { me: string; state: ConnState; qr?: string }
        this.me = h.me; this.state = h.state; this.qr = h.qr
        this.emit('connection', this.state)
        return
      }
      if (msg.event === 'connection') {
        const [state, detail, qr] = msg.args as [ConnState, string?, string?]
        this.state = state
        if (state === 'open' && detail) this.me = detail
        this.qr = state === 'qr' ? qr : undefined
        msg.args = [state, detail]
      }
      ;(this.emit as (ev: string, ...a: unknown[]) => boolean)(msg.event, ...msg.args)
      return
    }
    const w = this.waiting.get(msg.id)
    if (!w) return
    this.waiting.delete(msg.id)
    msg.ok ? w.resolve(msg.result) : w.reject(new Error(msg.error ?? t('serverError')))
  }

  private call<T>(op: string, ...args: unknown[]): Promise<T> {
    const id = this.nextId++
    return new Promise<T>((resolve, reject) => {
      this.waiting.set(id, { resolve: resolve as (v: unknown) => void, reject })
      sendJson(this.socket, { id, op, args } satisfies Request)
    })
  }

  send(chatJid: string, text: string, replyTo?: string, mentions?: string[]) { return this.call<void>('send', chatJid, text, replyTo, mentions) }
  react(chatJid: string, msgId: string, emoji: string) { return this.call<void>('react', chatJid, msgId, emoji) }
  edit(chatJid: string, msgId: string, text: string) { return this.call<void>('edit', chatJid, msgId, text) }
  deleteMessage(chatJid: string, msgId: string, forEveryone: boolean) { return this.call<void>('deleteMessage', chatJid, msgId, forEveryone ? 'all' : 'me') }
  sendFile(chatJid: string, filePath: string, caption?: string) { return this.call<void>('sendFile', chatJid, filePath, caption) }
  markRead(chatJid: string) { return this.call<void>('markRead', chatJid) }
  subscribePresence(chatJid: string) { this.call('subscribePresence', chatJid).catch(() => {}) }
  setComposing(chatJid: string, on: boolean) { this.call('setComposing', chatJid, on ? 'on' : 'off').catch(() => {}) }
  touchPresence(terminal: number) { this.call('touchPresence', String(terminal)).catch(() => {}) }
  setFocus(terminal: number, focused: boolean) { this.call('setFocus', String(terminal), focused ? 'on' : 'off').catch(() => {}) }
  ensureMedia(row: MessageRow) { this.call('ensureMedia', row.chat_jid, row.id).catch(() => {}) }
  ensurePreview(row: MessageRow) { this.call('ensurePreview', row.chat_jid, row.id).catch(() => {}) }
  ensureProfile(jid: string) { this.call('ensureProfile', jid).catch(() => {}) }
  downloadAll(chatJid: string) { return this.call<{ copied: number; pending: number }>('downloadAll', chatJid) }
  fetchOlder(chatJid: string) { return this.call<number>('fetchOlder', chatJid) }
  async stop() { this.socket.destroy() }
}
