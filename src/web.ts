import { spawn } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import http from 'node:http'
import https from 'node:https'
import { createRequire } from 'node:module'
import path from 'node:path'
import readline from 'node:readline'
import type { Readable } from 'node:stream'
import { fileURLToPath } from 'node:url'
import QRCode from 'qrcode'
import { WebSocketServer, type WebSocket } from 'ws'
import {
  generateAuthenticationOptions, generateRegistrationOptions, verifyAuthenticationResponse, verifyRegistrationResponse,
  type AuthenticationResponseJSON, type RegistrationResponseJSON,
} from '@simplewebauthn/server'
import { dirs } from './config.js'
import { store } from './db.js'
import { t } from './i18n.js'
import { logger } from './log.js'

/**
 * wassup in a browser: `wa --web` serves a page where each tab runs wa itself (`wa --web-tab`, webtab.ts) over a
 * WebSocket: the same program, so the same screen, which wa sends cell by cell and the page draws in HTML. Over
 * plain HTTP on localhost only, or, given a certificate and its key (Let's Encrypt's, from certbot), over HTTPS on
 * every interface, read again when renewed.
 *
 * Entry is by passkey (WebAuthn), with no password: the device's fingerprint, face or PIN, nothing to steal or guess,
 * and useless on another site. The first one is registered through a single-use link printed on start, valid 15
 * minutes; the next ones through links made from a device already in. A session is a random token in an HttpOnly,
 * SameSite=Strict cookie (Secure over HTTPS), kept hashed in the state table and dropped after 30 days unused.
 */

const ROOT = fileURLToPath(new URL('..', import.meta.url))
const MAIN = fileURLToPath(new URL('./main.ts', import.meta.url))
const PAGE = fileURLToPath(new URL('./web/', import.meta.url))

/** Passkeys, sessions (by the token's hash) and the one user's id, in the state table. */
interface Passkey { id: string; publicKey: string; counter: number; transports?: string[]; created: number; device: string }
type Sessions = Record<string, { created: number; seen: number }>
const PASSKEYS = 'web:passkeys', SESSIONS = 'web:sessions', USER = 'web:user'
const IDLE = 30 * 86400_000
const INVITE_MS = 15 * 60_000
const CHALLENGE_MS = 5 * 60_000

/** Registration links not used yet, and the challenges handed out, by the id in their cookie: in memory only. */
const invites = new Map<string, number>()
const challenges = new Map<string, { challenge: string; expires: number; invite?: string }>()
/** Attempts at signing in or registering in the last minute: past 30, refused. */
let attempts: number[] = []

const random = (bytes = 32) => crypto.randomBytes(bytes).toString('base64url')
const hash = (token: string) => crypto.createHash('sha256').update(token).digest('hex')

// The browser's files: the page's own, and simplewebauthn's from its package.
const require = createRequire(import.meta.url)
/** A package's folder, from its entry point up to its package.json (some don't export that file). */
function pkgRoot(name: string): string {
  let dir = path.dirname(require.resolve(name))
  while (!fs.existsSync(path.join(dir, 'package.json')) || JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8')).name !== name) dir = path.dirname(dir)
  return dir
}
const WEBAUTHN = path.join(pkgRoot('@simplewebauthn/browser'), 'esm')
const TYPES: Record<string, string> = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.svg': 'image/svg+xml', '.json': 'application/json',
  '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.png': 'image/png', '.gif': 'image/gif', '.webp': 'image/webp',
  '.mp4': 'video/mp4', '.webm': 'video/webm', '.ogg': 'audio/ogg', '.opus': 'audio/ogg', '.mp3': 'audio/mpeg',
  '.m4a': 'audio/mp4', '.pdf': 'application/pdf', '.txt': 'text/plain; charset=utf-8',
}
const CSP = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; " +
  "media-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"

/** Where the page is published over HTTPS: the certificate's name and the port (startWeb); unset on localhost. */
let published: URL | undefined

/**
 * The origin the browser is on: the one published, whatever host a request names; on localhost, plain HTTP, a
 * secure context there too. The passkeys' relying party is its host name, so a passkey made for one name doesn't
 * sign in on another.
 */
function origin(req: http.IncomingMessage): { origin: string; rpID: string; secure: boolean } {
  if (published) return { origin: published.origin, rpID: published.hostname, secure: true }
  const url = new URL(`http://${req.headers.host ?? 'localhost'}`)
  return { origin: url.origin, rpID: url.hostname, secure: false }
}

function cookies(req: http.IncomingMessage): Record<string, string> {
  return Object.fromEntries((req.headers.cookie ?? '').split(';').map(c => c.trim().split('=')).filter(p => p.length === 2) as [string, string][])
}

function cookie(name: string, value: string, maxAge: number, secure: boolean): string {
  return `${name}=${value}; Path=/; HttpOnly; SameSite=Strict; Max-Age=${maxAge}${secure ? '; Secure' : ''}`
}

/** The session the request carries, touched (at most once an hour, as it's written to the database), or none. */
function session(req: http.IncomingMessage): string | null {
  const token = cookies(req).wa_s
  if (!token) return null
  const all = store.getState<Sessions>(SESSIONS) ?? {}
  const s = all[hash(token)]
  if (!s || Date.now() - s.seen > IDLE) return null
  if (Date.now() - s.seen > 3600_000) { s.seen = Date.now(); store.setState(SESSIONS, all) }
  return token
}

function newSession(): string {
  const token = random()
  const all = store.getState<Sessions>(SESSIONS) ?? {}
  for (const [h, s] of Object.entries(all)) if (Date.now() - s.seen > IDLE) delete all[h]
  all[hash(token)] = { created: Date.now(), seen: Date.now() }
  store.setState(SESSIONS, all)
  return token
}

/** A registration link's token still valid; `use` spends it. */
function inviteValid(token: string | undefined, use = false): boolean {
  const expires = token ? invites.get(token) : undefined
  if (!expires || expires < Date.now()) return false
  if (use) invites.delete(token!)
  return true
}

function newInvite(): string {
  const token = random(24)
  invites.set(token, Date.now() + INVITE_MS)
  return token
}

/** A short name for the device a passkey was made on, from its browser's user agent. */
function deviceOf(req: http.IncomingMessage): string {
  const ua = req.headers['user-agent'] ?? ''
  const os = /iPhone|iPad/.test(ua) ? 'iOS' : /Android/.test(ua) ? 'Android' : /Mac OS/.test(ua) ? 'macOS' : /Windows/.test(ua) ? 'Windows' : /Linux/.test(ua) ? 'Linux' : '?'
  const browser = /Firefox\//.test(ua) ? 'Firefox' : /Edg\//.test(ua) ? 'Edge' : /Chrome\//.test(ua) ? 'Chrome' : /Safari\//.test(ua) ? 'Safari' : '?'
  return `${browser} · ${os}`
}

function send(res: http.ServerResponse, code: number, body: unknown, headers: Record<string, string | string[]> = {}) {
  res.writeHead(code, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store', ...headers })
  res.end(JSON.stringify(body))
}

function readJson(req: http.IncomingMessage): Promise<unknown> {
  return new Promise((resolve, reject) => {
    let size = 0
    const parts: Buffer[] = []
    req.on('data', (d: Buffer) => { size += d.length; if (size > 64 * 1024) { reject(new Error('too big')); req.destroy() } else parts.push(d) })
    req.on('end', () => { try { resolve(parts.length ? JSON.parse(Buffer.concat(parts).toString()) : {}) } catch (e) { reject(e) } })
    req.on('error', reject)
  })
}

function file(res: http.ServerResponse, at: string, headers: Record<string, string> = {}) {
  fs.stat(at, (e, st) => {
    if (e || !st.isFile()) { res.writeHead(404); return res.end() }
    res.writeHead(200, { 'Content-Type': TYPES[path.extname(at).toLowerCase()] ?? 'application/octet-stream', 'Content-Length': st.size, ...headers })
    fs.createReadStream(at).pipe(res)
  })
}

async function api(req: http.IncomingMessage, res: http.ServerResponse, route: string) {
  const o = origin(req)
  // Requests that change something come from the page itself: the browser says where from.
  if (req.method === 'POST' && req.headers.origin !== o.origin) return send(res, 403, { error: 'origin' })
  const passkeys = store.getState<Passkey[]>(PASSKEYS) ?? []
  const signedIn = session(req)

  if (route === 'state') return send(res, 200, { in: !!signedIn, passkeys: passkeys.length })
  if (req.method !== 'POST') return send(res, 405, { error: 'method' })

  if (route === 'logout') {
    if (signedIn) { const all = store.getState<Sessions>(SESSIONS) ?? {}; delete all[hash(signedIn)]; store.setState(SESSIONS, all) }
    return send(res, 200, { ok: true }, { 'Set-Cookie': cookie('wa_s', '', 0, o.secure) })
  }
  if (route === 'invite') {
    if (!signedIn) return send(res, 401, { error: 'session' })
    const url = `${o.origin}/?invite=${newInvite()}`
    return send(res, 200, { url, qr: await QRCode.toString(url, { type: 'svg', margin: 1 }) })
  }

  attempts = attempts.filter(a => Date.now() - a < 60_000)
  if (attempts.length >= 30) return send(res, 429, { error: 'busy' })
  attempts.push(Date.now())
  const body = await readJson(req).catch(() => null) as Record<string, unknown> | null
  if (!body) return send(res, 400, { error: 'body' })

  const challengeFor = (id: string | undefined) => {
    const c = id ? challenges.get(id) : undefined
    if (id) challenges.delete(id)
    return c && c.expires > Date.now() ? c : undefined
  }
  const handOut = (challenge: string, invite?: string) => {
    for (const [k, c] of challenges) if (c.expires < Date.now()) challenges.delete(k)
    const id = random(16)
    challenges.set(id, { challenge, expires: Date.now() + CHALLENGE_MS, invite })
    return cookie('wa_c', id, CHALLENGE_MS / 1000, o.secure)
  }
  const signIn = () => cookie('wa_s', newSession(), IDLE / 1000, o.secure)

  if (route === 'register/options') {
    const invite = typeof body.invite === 'string' ? body.invite : undefined
    if (!signedIn && !inviteValid(invite)) return send(res, 403, { error: 'invite' })
    let user = store.getState<string>(USER)
    if (!user) { user = random(16); store.setState(USER, user) }
    const options = await generateRegistrationOptions({
      rpName: 'wassup', rpID: o.rpID, userName: 'wassup', userID: Buffer.from(user, 'base64url'), attestationType: 'none',
      excludeCredentials: passkeys.map(p => ({ id: p.id, transports: p.transports as never })),
      authenticatorSelection: { residentKey: 'required', userVerification: 'required' },
    })
    return send(res, 200, options, { 'Set-Cookie': handOut(options.challenge, invite) })
  }
  if (route === 'register/verify') {
    const c = challengeFor(cookies(req).wa_c)
    if (!c) return send(res, 400, { error: 'challenge' })
    if (!signedIn && !inviteValid(c.invite)) return send(res, 403, { error: 'invite' })
    const v = await verifyRegistrationResponse({
      response: body.response as RegistrationResponseJSON, expectedChallenge: c.challenge, expectedOrigin: o.origin,
      expectedRPID: o.rpID, requireUserVerification: true,
    }).catch(e => { logger.warn({ e: String(e) }, 'web: registration'); return null })
    if (!v?.verified) return send(res, 400, { error: 'verify' })
    if (c.invite) inviteValid(c.invite, true)
    const { credential } = v.registrationInfo
    passkeys.push({
      id: credential.id, publicKey: Buffer.from(credential.publicKey).toString('base64url'), counter: credential.counter,
      transports: credential.transports, created: Date.now(), device: deviceOf(req),
    })
    store.setState(PASSKEYS, passkeys)
    return send(res, 200, { ok: true }, { 'Set-Cookie': signIn() })
  }
  if (route === 'login/options') {
    if (!passkeys.length) return send(res, 409, { error: 'none' })
    const options = await generateAuthenticationOptions({ rpID: o.rpID, userVerification: 'required' })
    return send(res, 200, options, { 'Set-Cookie': handOut(options.challenge) })
  }
  if (route === 'login/verify') {
    const c = challengeFor(cookies(req).wa_c)
    const response = body as unknown as AuthenticationResponseJSON
    const key = passkeys.find(p => p.id === response.id)
    if (!c || !key) return send(res, 400, { error: c ? 'unknown' : 'challenge' })
    const v = await verifyAuthenticationResponse({
      response, expectedChallenge: c.challenge, expectedOrigin: o.origin, expectedRPID: o.rpID, requireUserVerification: true,
      credential: { id: key.id, publicKey: Buffer.from(key.publicKey, 'base64url'), counter: key.counter, transports: key.transports as never },
    }).catch(e => { logger.warn({ e: String(e) }, 'web: sign-in'); return null })
    if (!v?.verified) return send(res, 400, { error: 'verify' })
    key.counter = v.authenticationInfo.newCounter
    store.setState(PASSKEYS, passkeys)
    return send(res, 200, { ok: true }, { 'Set-Cookie': signIn() })
  }
  send(res, 404, { error: 'route' })
}

async function handle(req: http.IncomingMessage, res: http.ServerResponse) {
  const url = new URL(req.url ?? '/', 'http://localhost')
  res.setHeader('Content-Security-Policy', CSP)
  res.setHeader('X-Content-Type-Options', 'nosniff')
  res.setHeader('Referrer-Policy', 'no-referrer')
  // Once over HTTPS, the browser never comes back over plain HTTP to this name.
  if (origin(req).secure) res.setHeader('Strict-Transport-Security', 'max-age=31536000')
  if (url.pathname.startsWith('/api/')) return api(req, res, url.pathname.slice(5))
  if (url.pathname === '/' || url.pathname === '/index.html') return file(res, path.join(PAGE, 'index.html'), { 'Cache-Control': 'no-store' })
  if (url.pathname === '/app.js' || url.pathname === '/app.css') return file(res, path.join(PAGE, url.pathname.slice(1)), { 'Cache-Control': 'no-cache' })
  // simplewebauthn's modules, which import each other relatively: only files inside its folder.
  if (url.pathname.startsWith('/vendor/webauthn/')) {
    const at = path.resolve(WEBAUTHN, decodeURIComponent(url.pathname.slice('/vendor/webauthn/'.length)))
    if (at.startsWith(WEBAUTHN + path.sep) && at.endsWith('.js')) return file(res, at, { 'Cache-Control': 'no-cache' })
  }
  // An attachment wa asked the page to open, or a picture it shows (webtab.ts): signed in, and only from the media
  // folder.
  if (url.pathname === '/media') {
    if (!session(req)) { res.writeHead(401); return res.end() }
    const at = path.resolve(dirs.media, url.searchParams.get('f') ?? '')
    if (!at.startsWith(path.resolve(dirs.media) + path.sep)) { res.writeHead(403); return res.end() }
    return file(res, at, { 'Cache-Control': 'private, max-age=3600', 'Content-Disposition': `inline; filename*=UTF-8''${encodeURIComponent(path.basename(at))}` })
  }
  res.writeHead(404)
  res.end()
}

/**
 * A browser's tab: wa itself (`wa --web-tab`), as wide and tall as the tab says, and its messages both ways, one per
 * line: the page's to its stdin, checked first, and its own, from descriptor 3, as they come (webtab.ts). No Herdr
 * nor tmux, whatever the server was started in.
 */
function tab(ws: WebSocket) {
  let child: ReturnType<typeof spawn> | undefined
  const env: Record<string, string> = {}
  for (const [k, v] of Object.entries(process.env)) if (v != null && !k.startsWith('HERDR_') && k !== 'TMUX') env[k] = v
  env.TERM = 'xterm-256color'
  const ping = setInterval(() => ws.ping(), 30_000)
  ws.on('message', raw => {
    let m: { t?: string; d?: string; cols?: number; rows?: number }
    try { m = JSON.parse(String(raw)) } catch { return }
    let line: object
    if (m.t === 'size') line = { t: 'size', cols: Math.max(10, Math.min(500, Math.floor(m.cols ?? 80))), rows: Math.max(5, Math.min(300, Math.floor(m.rows ?? 24))) }
    else if (m.t === 'in' && typeof m.d === 'string') line = { t: 'in', d: m.d }
    else if (m.t === 'ack') line = { t: 'ack' }
    else return
    if (!child && m.t === 'size') {
      child = spawn(process.execPath, ['--import', 'tsx', MAIN, '--web-tab'], { cwd: ROOT, env, stdio: ['pipe', 'ignore', 'pipe', 'pipe'] })
      readline.createInterface({ input: child.stdio[3] as Readable, crlfDelay: Infinity }).on('line', l => ws.send(l))
      readline.createInterface({ input: child.stderr! }).on('line', l => logger.warn({ l }, 'web: tab'))
      // Once its output is all read: the last thing wa says may be to sign out.
      child.on('close', () => ws.close(1000))
    }
    child?.stdin?.write(`${JSON.stringify(line)}\n`)
  })
  ws.on('close', () => { clearInterval(ping); child?.kill() })
}

/** A certificate and its key, as files (certbot's fullchain.pem and privkey.pem). */
export interface Tls { cert: string; key: string }

/**
 * Serves wassup on `port`: over HTTPS on every interface with `tls`, over HTTP on localhost only without; prints
 * where, and a registration link while there's no passkey (or `invite`).
 */
export async function startWeb(port: number, invite: boolean, tls?: Tls) {
  const serve = (req: http.IncomingMessage, res: http.ServerResponse) => { handle(req, res).catch(e => { logger.warn({ e: String(e) }, 'web'); if (!res.headersSent) res.writeHead(500); res.end() }) }
  const read = (t: Tls) => ({ cert: fs.readFileSync(t.cert), key: fs.readFileSync(t.key) })
  const server = tls ? https.createServer(read(tls), serve) : http.createServer(serve)
  if (tls) {
    // Let's Encrypt's certificates last 90 days, and certbot renews them in place: read again when they change.
    let seen = fs.statSync(tls.cert).mtimeMs
    setInterval(() => {
      try {
        const now = fs.statSync(tls.cert).mtimeMs
        if (now === seen) return
        ;(server as https.Server).setSecureContext(read(tls))
        seen = now
        logger.info('web: certificate read again')
      } catch (e) { logger.warn({ e: String(e) }, 'web: certificate') }
    }, 3600_000).unref()
  }
  const wss = new WebSocketServer({ noServer: true, maxPayload: 1 << 20 })
  server.on('upgrade', (req, socket, head) => {
    const url = new URL(req.url ?? '/', 'http://localhost')
    // Only the page itself, signed in: the browser says where from, and the cookie who.
    if (url.pathname !== '/tab' || req.headers.origin !== origin(req).origin || !session(req)) {
      socket.end('HTTP/1.1 401 Unauthorized\r\n\r\n')
      return
    }
    wss.handleUpgrade(req, socket, head, ws => tab(ws))
  })
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(port, tls ? '::' : '127.0.0.1', resolve) })
  // Published under the certificate's name; on localhost, not 127.0.0.1, as a passkey's relying party is a domain
  // name, never an address.
  const certName = tls ? new crypto.X509Certificate(fs.readFileSync(tls.cert)).subjectAltName?.match(/DNS:([^,\s]+)/)?.[1] : undefined
  if (tls && !certName) throw new Error('web: the certificate names no domain')
  published = tls ? new URL(`https://${certName}:${port}`) : undefined
  const base = published?.origin ?? `http://localhost:${port}`
  const say = (s: string) => process.stdout.write(`${s}\n`)
  say(t('webListening', base))
  if (invite || !(store.getState<Passkey[]>(PASSKEYS) ?? []).length) say(t('webInvite', `${base}/?invite=${newInvite()}`))
}
