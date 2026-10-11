import { EventEmitter } from 'node:events'
import { silenceConsole, logger } from './log.js'
import { t } from './i18n.js'
import { Wa, type WaEvents } from './wa.js'
import { Ui } from './ui.js'
import { probeTerminal } from './term.js'
import { IpcServer, RemoteWa } from './ipc.js'
import type { Backend } from './backend.js'
import type { MessageRow } from './db.js'

silenceConsole()
process.on('uncaughtException', e => logger.error({ e: e.stack ?? String(e) }, 'uncaughtException'))
process.on('unhandledRejection', e => logger.error({ e: e instanceof Error ? e.stack : String(e) }, 'unhandledRejection'))

/**
 * The UI always talks to this proxy; behind it is either our own connection (this process is the server) or
 * another process's client. When the server disappears, the election repeats and what's behind it swaps without
 * the UI noticing.
 */
class BackendProxy extends EventEmitter<WaEvents> implements Backend {
  private inner: Backend | undefined
  private server: IpcServer | undefined
  get me() { return this.inner?.me ?? '' }
  get state() { return this.inner?.state ?? 'connecting' }
  get qr() { return this.inner?.qr }
  // Before the election finishes there's no backend: actions that aren't possible fail with a message, the others are ignored.
  private ready(): Backend { if (!this.inner) throw new Error(t('notConnectedYet')); return this.inner }
  send(jid: string, text: string, replyTo?: string) { return this.ready().send(jid, text, replyTo) }
  react(jid: string, msgId: string, emoji: string) { return this.ready().react(jid, msgId, emoji) }
  edit(jid: string, msgId: string, text: string) { return this.ready().edit(jid, msgId, text) }
  deleteMessage(jid: string, msgId: string, forEveryone: boolean) { return this.ready().deleteMessage(jid, msgId, forEveryone) }
  sendFile(jid: string, file: string, caption?: string) { return this.ready().sendFile(jid, file, caption) }
  async markRead(jid: string) { await this.inner?.markRead(jid) }
  subscribePresence(jid: string) { this.inner?.subscribePresence(jid) }
  setComposing(jid: string, on: boolean) { this.inner?.setComposing(jid, on) }
  touchPresence(terminal: number) { this.inner?.touchPresence(terminal) }
  setFocus(terminal: number, focused: boolean) { this.inner?.setFocus(terminal, focused) }
  ensureMedia(row: MessageRow) { this.inner?.ensureMedia(row) }
  ensurePreview(row: MessageRow) { this.inner?.ensurePreview(row) }
  ensureProfile(jid: string) { this.inner?.ensureProfile(jid) }
  ensureName(jid: string) { this.inner?.ensureName(jid) }
  downloadAll(jid: string) { return this.ready().downloadAll(jid) }
  async fetchOlder(jid: string) { return this.ready().fetchOlder(jid) }
  async stop() { this.server?.close(); await this.inner?.stop() }

  /** Connects to whatever server exists; if there's none, this process becomes the server. */
  async elect() {
    for (let attempt = 0; attempt < 5; attempt++) {
      const remote = await RemoteWa.connect()
      if (remote) {
        this.use(remote)
        remote.once('lost', () => {
          logger.warn('ipc: server lost, new election')
          this.emit('status', t('serverGone'))
          this.elect().catch(e => logger.error({ e }, 'elect'))
        })
        return
      }
      const wa = new Wa()
      const server = new IpcServer(wa)
      try {
        await server.listen()
      } catch (e) {
        // Another process opened the socket at this instant: try again as a client. Any other error is final.
        if ((e as NodeJS.ErrnoException).code !== 'EADDRINUSE') throw e
        logger.info('ipc: socket busy, trying as client')
        continue
      }
      this.server = server
      this.use(wa)
      wa.start().catch(e => logger.error({ e }, 'start'))
      return
    }
    throw new Error(t('noServer'))
  }

  private use(b: Backend) {
    this.inner = b
    for (const ev of ['connection', 'chats', 'messages', 'notify', 'status', 'typing', 'reaction', 'presence', 'available', 'groupOnline', 'profile', 'mood'] as const) {
      b.on(ev, ((...args: unknown[]) => (this.emit as (ev: string, ...a: unknown[]) => boolean)(ev, ...args)) as never)
    }
    this.emit('connection', b.state)
  }
}

const caps = await probeTerminal()
const backend = new BackendProxy()
new Ui(backend, caps, process.argv.slice(2).join(' ').trim() || undefined)
await backend.elect()
