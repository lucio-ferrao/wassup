// wassup in a browser: wa itself, running on the server (web.ts), sends its screen cell by cell (webtab.ts) and this
// page draws it in HTML; keys, clicks and the wheel go back as a terminal would send them. Signed in by passkey; a
// registration link (?invite=…) adds one.
import { browserSupportsWebAuthn, startAuthentication, startRegistration } from '/vendor/webauthn/index.js'

const pt = (navigator.language || '').toLowerCase().startsWith('pt')
const T = pt ? {
  login: 'Entra com a passkey deste dispositivo.', loginButton: 'Entrar com passkey',
  register: 'Regista uma passkey neste dispositivo: a impressão digital, a cara ou o PIN dele passam a abrir o wassup.', registerButton: 'Registar passkey',
  none: 'Ainda não há passkeys. Abre a ligação de registo que o «wa --web» mostra no terminal.',
  noWebauthn: 'Este navegador não suporta passkeys.',
  failed: 'Não deu: ', gone: 'A ligação ao wassup terminou.', again: 'Voltar a ligar',
  addDevice: 'Adicionar dispositivo', signOut: 'Sair',
  invite: 'Abre esta ligação no outro dispositivo, ou lê o código, nos próximos 15 minutos. Só serve uma vez.',
  attachment: 'Abrir anexo', link: 'Abrir ligação',
  // What the server's refusals mean, by the code it gives (web.ts).
  errors: {
    invite: 'Esta ligação de registo já não vale: expirou (dura 15 minutos) ou já foi usada. Pede outra.',
    challenge: 'Passou demasiado tempo desde que carregaste no botão. Tenta outra vez.',
    verify: 'A passkey não foi aceite. Tenta outra vez.',
    unknown: 'Esta passkey não está registada neste wassup.',
    none: 'Ainda não há passkeys registadas.',
    busy: 'Demasiadas tentativas. Espera um minuto.',
  },
} : {
  login: "Sign in with this device's passkey.", loginButton: 'Sign in with passkey',
  register: "Register a passkey on this device: its fingerprint, face or PIN will open wassup.", registerButton: 'Register passkey',
  none: 'There are no passkeys yet. Open the registration link "wa --web" shows in the terminal.',
  noWebauthn: "This browser doesn't support passkeys.",
  failed: "Didn't work: ", gone: 'The connection to wassup ended.', again: 'Connect again',
  addDevice: 'Add a device', signOut: 'Sign out',
  invite: 'Open this link on the other device, or scan the code, within 15 minutes. It works once.',
  attachment: 'Open attachment', link: 'Open link',
  errors: {
    invite: 'This registration link is no longer valid: it expired (it lasts 15 minutes) or was already used. Ask for another.',
    challenge: 'Too much time passed since you pressed the button. Try again.',
    verify: "The passkey wasn't accepted. Try again.",
    unknown: "This passkey isn't registered with this wassup.",
    none: 'There are no passkeys registered yet.',
    busy: 'Too many attempts. Wait a minute.',
  },
}

const $ = id => document.getElementById(id)
const touch = matchMedia('(pointer: coarse)').matches

/** The page's icon (the tab's favicon). */
function setIcon(href) {
  let link = document.querySelector('link[rel="icon"]')
  if (!link) { link = document.createElement('link'); link.rel = 'icon'; document.head.append(link) }
  link.href = href
}
const svgIcon = body => `data:image/svg+xml,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">${body}</svg>`)}`
const APP_ICON = svgIcon('<text x="32" y="52" font-size="52" text-anchor="middle">👾</text>')
const escape = s => String(s).replace(/[&<>"]/g, c => `&#${c.charCodeAt(0)};`)
/** A chat without a photo: its initials on its colour, as the chat list draws them. */
function initialsIcon(text, color) {
  return svgIcon(`<rect width="64" height="64" rx="14" fill="${/^#[0-9a-f]{6}$/i.test(color) ? color : '#555'}"/>` +
    `<text x="32" y="43" font-size="28" font-weight="700" font-family="sans-serif" text-anchor="middle" fill="#fff">${escape(text)}</text>`)
}

async function post(url, body) {
  const res = await fetch(url, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body ?? {}) })
  const data = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error(data.error || res.status)
  return data
}

/** The page before wa: a line and, when there's something to do, one button that does it. */
function door(text, button, action) {
  document.title = 'wassup'
  setIcon(APP_ICON)
  $('term').hidden = true
  $('door').hidden = false
  $('door-text').textContent = text
  $('door-error').textContent = ''
  const b = $('door-button')
  b.hidden = !button
  if (!button) return
  b.textContent = button
  b.onclick = async () => {
    b.disabled = true
    try { await action() } catch (e) { $('door-error').textContent = T.errors[e.message] ?? T.failed + (e.message || e) } finally { b.disabled = false }
  }
}

async function main() {
  const state = await (await fetch('/api/state')).json()
  const invite = new URLSearchParams(location.search).get('invite')
  if (state.in && !invite) return start()
  if (!browserSupportsWebAuthn()) return door(T.noWebauthn)
  if (invite) {
    return door(T.register, T.registerButton, async () => {
      const options = await post('/api/register/options', { invite })
      const response = await startRegistration({ optionsJSON: options })
      await post('/api/register/verify', { response })
      history.replaceState(null, '', '/')
      start()
    })
  }
  if (!state.passkeys) return door(T.none)
  door(T.login, T.loginButton, async () => {
    const options = await post('/api/login/options')
    const response = await startAuthentication({ optionsJSON: options })
    await post('/api/login/verify', response)
    start()
  })
}

/**
 * The box-drawing and block characters wa draws with (the big letters, rules, the QR code), drawn here rather than
 * by the font, which may not fill the line's height and leaves them broken: each in a cell 10 by 20, lines from its
 * centre to its sides (rounded corners for the arcs), blocks as its parts filled. And the ✖ that closes a tab, which
 * the fonts browsers fall back to draw far smaller than a terminal's: across most of its cell, heavy.
 */
const GLYPHS = (() => {
  const L = 'M0 10H5', R = 'M5 10H10', U = 'M5 0V10', D = 'M5 10V20'
  const path = d => `<path d="${d}"/>`, rect = (x, y, w, h) => `<rect x="${x}" y="${y}" width="${w}" height="${h}"/>`
  const UL = rect(0, 0, 5, 10), UR = rect(5, 0, 5, 10), LL = rect(0, 10, 5, 10), LR = rect(5, 10, 5, 10)
  const g = {
    '─': path(L + R), '│': path(U + D), '┌': path(R + D), '┐': path(L + D), '└': path(U + R), '┘': path(U + L),
    '├': path(U + D + R), '┤': path(U + D + L), '┬': path(L + R + D), '┴': path(L + R + U), '┼': path(L + R + U + D),
    '╴': path(L), '╵': path(U), '╶': path(R), '╷': path(D),
    '╭': path('M5 20V14Q5 10 9 10H10'), '╮': path('M5 20V14Q5 10 1 10H0'), '╯': path('M5 0V6Q5 10 1 10H0'), '╰': path('M5 0V6Q5 10 9 10H10'),
    '▀': rect(0, 0, 10, 10), '▄': rect(0, 10, 10, 10), '█': rect(0, 0, 10, 20), '▌': rect(0, 0, 5, 20), '▐': rect(5, 0, 5, 20), '▔': rect(0, 0, 10, 2.5),
    '✖': '<path class="x" d="M0.8 5.8L9.2 14.2M9.2 5.8L0.8 14.2"/>',
    '▖': LL, '▗': LR, '▘': UL, '▝': UR, '▙': UL + LL + LR, '▚': UL + LR, '▛': UL + UR + LL, '▜': UL + UR + LR, '▞': UR + LL, '▟': UR + LL + LR,
  }
  // Eighths: from the bottom (▁ to ▇) and from the left (▉ to ▏).
  for (let k = 1; k <= 7; k++) g[String.fromCodePoint(0x2580 + k)] = rect(0, 20 - 2.5 * k, 10, 2.5 * k)
  for (let k = 7; k >= 1; k--) g[String.fromCodePoint(0x2590 - k)] = rect(0, 0, 1.25 * k, 20)
  return g
})()

/** A run of cells as HTML: its text, but for the characters in GLYPHS, each repeat of one drawn in a single picture. */
function runHtml(cells, text, style, bold) {
  const cls = `s${style}${bold ? ' b' : ''}`
  const chars = [...text]
  if (chars.length !== cells || !chars.some(c => GLYPHS[c])) return `<span class="${cls}"${cells > 1 ? ` style="--n:${cells}"` : ''}>${escape(text)}</span>`
  let html = ''
  for (let i = 0; i < chars.length;) {
    let j = i + 1
    if (GLYPHS[chars[i]]) {
      while (chars[j] === chars[i]) j++
      const parts = Array.from({ length: j - i }, (_, k) => `<g transform="translate(${10 * k})">${GLYPHS[chars[i]]}</g>`).join('')
      html += `<span class="${cls}" style="--n:${j - i}"><svg viewBox="0 0 ${10 * (j - i)} 20" preserveAspectRatio="none">${parts}</svg></span>`
    } else {
      while (j < chars.length && !GLYPHS[chars[j]]) j++
      html += `<span class="${cls}"${j - i > 1 ? ` style="--n:${j - i}"` : ''}>${escape(chars.slice(i, j).join(''))}</span>`
    }
    i = j
  }
  return html
}

/** Keys as a terminal (xterm) sends them; null for those the browser keeps, and for text, which `input` brings. */
function keySeq(e) {
  if (e.metaKey || e.getModifierState?.('AltGraph')) return null
  const m = 1 + (e.shiftKey ? 1 : 0) + (e.altKey ? 2 : 0) + (e.ctrlKey ? 4 : 0)
  const csi = (final, n = '1') => (m > 1 ? `\x1b[${n};${m}${final}` : n === '1' ? `\x1b[${final}` : `\x1b[${n}${final}`)
  const ss3 = final => (m > 1 ? `\x1b[1;${m}${final}` : `\x1bO${final}`)
  const FN = { F5: 15, F6: 17, F7: 18, F8: 19, F9: 20, F10: 21, F11: 23, F12: 24 }
  switch (e.key) {
    case 'Enter': return e.altKey ? '\x1b\r' : '\r'
    case 'Backspace': return e.ctrlKey ? '\x08' : e.altKey ? '\x1b\x7f' : '\x7f'
    case 'Tab': return e.shiftKey ? '\x1b[Z' : '\t'
    case 'Escape': return '\x1b'
    case 'ArrowUp': return csi('A')
    case 'ArrowDown': return csi('B')
    case 'ArrowRight': return csi('C')
    case 'ArrowLeft': return csi('D')
    case 'Home': return csi('H')
    case 'End': return csi('F')
    case 'PageUp': return csi('~', '5')
    case 'PageDown': return csi('~', '6')
    case 'Delete': return csi('~', '3')
    case 'Insert': return csi('~', '2')
    case 'F1': return ss3('P')
    case 'F2': return ss3('Q')
    case 'F3': return ss3('R')
    case 'F4': return ss3('S')
  }
  if (FN[e.key]) return csi('~', String(FN[e.key]))
  if ([...e.key].length !== 1) return null
  if (e.ctrlKey && !e.altKey) {
    const k = e.key.toLowerCase()
    // Pasting, and copying what's selected, stay the browser's.
    if (k === 'v' || (k === 'c' && (e.shiftKey || String(getSelection())))) return null
    if (e.key === ' ') return '\0'
    const c = e.key.toUpperCase().charCodeAt(0)
    return c >= 64 && c <= 95 ? String.fromCharCode(c - 64) : e.key === '/' ? '\x1f' : null
  }
  if (e.altKey && !e.ctrlKey) {
    // Alt as Meta, also where Option gives another character (macOS): the key's own.
    const letter = /^Key([A-Z])$/.exec(e.code)?.[1], digit = /^Digit(\d)$/.exec(e.code)?.[1]
    return `\x1b${letter ? (e.shiftKey ? letter : letter.toLowerCase()) : digit ?? e.key}`
  }
  return null
}

/** wa's screen in the page, and wa on the server at the other end of a WebSocket. */
function start() {
  $('door').hidden = true
  $('term').hidden = false
  $('menu-button').hidden = false
  $('keys').hidden = !touch
  const term = $('term'), grid = $('grid'), picsLayer = $('pics'), input = $('input')
  term.style.fontSize = `${touch ? 13 : 15}px`

  const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/tab`)
  const say = m => { if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(m)) }
  const send = d => say({ t: 'in', d })

  // A cell's size: as wide as the font's characters, and exactly twice as tall, as wa takes a terminal's to be when
  // it sizes the pictures (image.ts, cellSize); the box-drawing characters fill it (GLYPHS) whatever the font's line.
  let cw = 9, ch = 18, cols = 0, rows = 0
  const measure = () => {
    const probe = document.createElement('span')
    probe.textContent = 'M'.repeat(50)
    probe.style.cssText = 'position:absolute;visibility:hidden;white-space:pre'
    term.append(probe)
    cw = probe.getBoundingClientRect().width / 50
    probe.remove()
    ch = 2 * cw
    term.style.setProperty('--cw', `${cw}px`)
    term.style.setProperty('--ch', `${ch}px`)
  }
  /** As many cells as fit; wa is told whenever that changes. */
  const fit = () => {
    const c = Math.max(10, Math.floor((term.clientWidth - 4) / cw)), r = Math.max(5, Math.floor((term.clientHeight - 4) / ch))
    if (c === cols && r === rows) return
    cols = c; rows = r
    say({ t: 'size', cols, rows })
  }
  measure()
  document.fonts?.ready.then(() => { measure(); fit() })
  new ResizeObserver(() => fit()).observe(term)

  // The frames: the rows that changed, in runs of [cells, text, style], the styles not seen before, and the pictures.
  const sheet = (() => { const s = document.createElement('style'); document.head.append(s); return s.sheet })()
  let lines = []
  const sources = new Map(), bold = new Set()
  const frame = m => {
    if (m.cols) {
      grid.textContent = ''
      lines = Array.from({ length: m.rows }, () => grid.appendChild(document.createElement('div')))
      grid.style.width = `${m.cols * cw}px`
    }
    for (const [id, [fg, bg, flags]] of Object.entries(m.styles ?? {})) {
      if (flags & 1) bold.add(Number(id))
      sheet.insertRule(`.s${id}{${fg ? `color:${fg};` : ''}${bg ? `background:${bg};` : ''}${flags & 1 ? 'font-weight:700;' : ''}${flags & 2 ? 'font-style:italic;' : ''}${flags & 4 ? 'text-decoration:underline;' : ''}}`, sheet.cssRules.length)
    }
    for (const [y, runs] of m.lines ?? []) {
      const line = lines[y]
      if (!line) continue
      let html = ''
      for (let i = 0; i < runs.length; i += 3) html += runHtml(runs[i], runs[i + 1], runs[i + 2], bold.has(runs[i + 2]))
      line.innerHTML = html
    }
    for (const [id, d] of Object.entries(m.defs ?? {})) sources.set(Number(id), d.file ? `/media?f=${encodeURIComponent(d.file)}` : `data:image/png;base64,${d.png}`)
    if (m.pics) pictures(m.pics)
    say({ t: 'ack' })
  }
  /** Each picture over its cells, cropped to the part in view; an emoji on its own, twice its size, in four cells by two. */
  const pictures = list => {
    list.forEach((p, i) => {
      let box = picsLayer.children[i]
      const kind = p.emoji ? 'emoji' : 'pic'
      if (!box || box.dataset.kind !== kind) {
        const fresh = document.createElement('div')
        fresh.dataset.kind = kind
        fresh.className = kind
        if (!p.emoji) fresh.append(document.createElement('img'))
        if (box) box.replaceWith(fresh); else picsLayer.append(fresh)
        box = fresh
      }
      Object.assign(box.style, { left: `${p.x * cw}px`, top: `${p.y * ch}px`, width: `${(p.emoji ? 4 : p.w) * cw}px`, height: `${(p.emoji ? 2 : p.h) * ch}px` })
      if (p.emoji) { if (box.textContent !== p.emoji) box.textContent = p.emoji; return }
      const img = box.firstChild, src = sources.get(p.id) ?? '', full = (p.h * ch) / Math.max(0.01, p.to - p.from)
      if (img.getAttribute('src') !== src) img.src = src
      Object.assign(img.style, { width: `${p.w * cw}px`, height: `${full}px`, top: `${-p.from * full}px` })
    })
    while (picsLayer.children.length > list.length) picsLayer.lastChild.remove()
  }

  // Leaving wa (Esc on the chat list, Ctrl+C) signs out; the connection ending otherwise offers to connect again.
  let leaving = false
  ws.onopen = () => { cols = rows = 0; fit(); if (document.hasFocus()) send('\x1b[I') }
  ws.onclose = () => { if (!leaving) door(T.gone, T.again, async () => location.reload()) }
  ws.onmessage = e => {
    const m = JSON.parse(e.data)
    if (m.t === 'frame') frame(m)
    else if (m.t === 'title') document.title = m.text || 'wassup'
    // The tab's icon, the chat's: its photo, its initials on its colour, or the app's.
    else if (m.t === 'icon') setIcon(m.photo ? `/media?f=${encodeURIComponent(m.photo)}` : m.initials ? initialsIcon(m.initials, m.color) : APP_ICON)
    else if (m.t === 'copy') copy(m.text)
    else if (m.t === 'open') open(`/media?f=${encodeURIComponent(m.file)}`, T.attachment)
    else if (m.t === 'link' && /^https?:\/\//i.test(m.url)) open(m.url, T.link)
    else if (m.t === 'logout') {
      leaving = true
      post('/api/logout').catch(() => {}).finally(() => location.replace('/'))
    }
  }

  // What wa copies (a message's text, a link) goes to the device's clipboard.
  const copy = text => {
    navigator.clipboard?.writeText(text).catch(() => {
      const area = document.createElement('textarea')
      area.value = text
      document.body.append(area)
      area.select()
      document.execCommand('copy')
      area.remove()
      input.focus({ preventScroll: true })
    })
  }
  // An attachment wa asks to open, from the server's media folder, or a link clicked in a message, in a tab of its
  // own, which doesn't get this one as its opener. A browser that blocks a tab opened after the click, once wa has
  // answered it, gets a link to click, for a few seconds.
  const open = (url, label) => {
    const tab = window.open(url, '_blank')
    if (tab) { tab.opener = null; return }
    const toast = $('toast')
    toast.href = url
    toast.textContent = label
    toast.hidden = false
    clearTimeout(toast.timer)
    toast.timer = setTimeout(() => { toast.hidden = true }, 10_000)
    toast.onclick = () => { toast.hidden = true; input.focus({ preventScroll: true }) }
  }

  // The keyboard, through a hidden text box: keys as a terminal sends them, text as typed, accents and the phone's
  // keyboard's words composed first; pasted text marked as such (bracketed paste), as wa asks.
  input.focus({ preventScroll: true })
  input.addEventListener('keydown', e => {
    if (e.isComposing || e.keyCode === 229) return
    const seq = keySeq(e)
    if (seq == null) return
    e.preventDefault()
    send(seq)
  })
  const typed = () => { const v = input.value; input.value = ''; if (v) send(v.replace(/\r?\n/g, '\r')) }
  input.addEventListener('beforeinput', e => {
    if (e.isComposing) return
    if (e.inputType === 'deleteContentBackward') { e.preventDefault(); send('\x7f') }
    else if (e.inputType === 'insertLineBreak' || e.inputType === 'insertParagraph') { e.preventDefault(); send('\r') }
  })
  input.addEventListener('input', e => { if (!e.isComposing) typed() })
  input.addEventListener('compositionend', typed)
  input.addEventListener('paste', e => {
    const text = e.clipboardData?.getData('text/plain')
    if (!text) return
    e.preventDefault()
    send(`\x1b[200~${text.replace(/\r?\n/g, '\r')}\x1b[201~`)
  })
  // Whether this window has the focus decides whether wa keeps the device online (focus reporting).
  addEventListener('focus', () => send('\x1b[I'))
  addEventListener('blur', () => send('\x1b[O'))

  // The mouse, in the terminal's terms (SGR): presses, releases, motion with and without a button, the wheel. With
  // Shift held it's the browser's, to select text and copy it.
  const cellAt = e => {
    const r = grid.getBoundingClientRect()
    return `${Math.min(cols, Math.max(1, Math.floor((e.clientX - r.left) / cw) + 1))};${Math.min(rows, Math.max(1, Math.floor((e.clientY - r.top) / ch) + 1))}`
  }
  const mods = e => (e.altKey ? 8 : 0) + (e.ctrlKey ? 16 : 0)
  let button = -1, at = ''
  term.addEventListener('mousedown', e => {
    if (e.shiftKey) return
    e.preventDefault()
    getSelection()?.removeAllRanges()
    input.focus({ preventScroll: true })
    button = e.button === 1 ? 1 : e.button === 2 ? 2 : 0
    at = cellAt(e)
    send(`\x1b[<${button + mods(e)};${at}M`)
  })
  addEventListener('mouseup', e => {
    if (button < 0) return
    send(`\x1b[<${button + mods(e)};${cellAt(e)}m`)
    button = -1
  })
  addEventListener('mousemove', e => {
    if (button < 0 && (e.shiftKey || !term.contains(e.target))) return
    const cell = cellAt(e)
    if (cell === at) return
    at = cell
    send(`\x1b[<${(button < 0 ? 35 : 32 + button) + mods(e)};${cell}M`)
  })
  term.addEventListener('contextmenu', e => { if (!e.shiftKey) e.preventDefault() })
  // A notch for each line's height the wheel turns, as xterm.js counted them.
  let wheel = 0
  term.addEventListener('wheel', e => {
    e.preventDefault()
    wheel += e.deltaMode === 1 ? e.deltaY * ch : e.deltaMode === 2 ? e.deltaY * rows * ch : e.deltaY
    const n = Math.max(-20, Math.min(20, Math.trunc(wheel / ch)))
    if (!n) return
    wheel -= n * ch
    send(`\x1b[<${n > 0 ? 65 : 64};${cellAt(e)}M`.repeat(Math.abs(n)))
  }, { passive: false })

  // A finger dragged on a touch screen scrolls as the wheel does: a notch for each line's height dragged, where the
  // finger is; down shows what's above, as on a phone. A tap stays a click.
  let lastY = null
  term.addEventListener('touchstart', e => { lastY = e.touches.length === 1 ? e.touches[0].clientY : null }, { passive: true })
  term.addEventListener('touchend', () => { lastY = null })
  term.addEventListener('touchmove', e => {
    if (lastY == null || e.touches.length !== 1) return
    e.preventDefault()
    const t = e.touches[0], steps = Math.trunc((t.clientY - lastY) / ch)
    if (!steps) return
    send(`\x1b[<${steps > 0 ? 64 : 65};${cellAt(t)}M`.repeat(Math.abs(steps)))
    lastY += steps * ch
  }, { passive: false })

  // The keys a phone's keyboard lacks, sent as the terminal would.
  const KEYS = { esc: '\x1b', tab: '\t', up: '\x1b[A', down: '\x1b[B', right: '\x1b[C', left: '\x1b[D', slash: '/' }
  for (const b of document.querySelectorAll('#keys button')) {
    b.addEventListener('pointerdown', e => { e.preventDefault(); send(KEYS[b.dataset.send]); input.focus({ preventScroll: true }) })
  }
}

// The menu: another device in, by a link of its own, or out.
$('menu-button').onclick = () => { $('invite').hidden = true; $('menu').showModal() }
$('close-menu').onclick = () => $('menu').close()
$('add-device').textContent = T.addDevice
$('sign-out').textContent = T.signOut
$('add-device').onclick = async () => {
  const { url, qr } = await post('/api/invite')
  $('invite-text').textContent = T.invite
  $('invite-qr').innerHTML = qr
  $('invite-link').href = url
  $('invite-link').textContent = url
  $('invite').hidden = false
}
$('sign-out').onclick = async () => { await post('/api/logout'); location.replace('/') }

main().catch(e => door(T.failed + (e.message || e)))
