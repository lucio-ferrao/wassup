import fs from 'node:fs'
import path from 'node:path'
import readline from 'node:readline'
import { PassThrough, Writable } from 'node:stream'
import { dirs } from './config.js'
import type { Decoded, RgbCell } from './image.js'
import { rgb256, type Rgb } from './rainbow.js'
import type { TermCaps } from './term.js'

/**
 * wa in a browser's tab (`wa --web-tab`, started by web.ts for each one): blessed draws as in a terminal, but to a
 * terminal that isn't there, and what goes to the page is its frame buffer, cell by cell, with the colours
 * resolved; the page draws it in HTML (web/app.js). The pictures go as pictures, placed over their cells.
 *
 * Both ways it's one message per line: the page's from the server on stdin (the size, the keys and the mouse as a
 * terminal would send them, and the acknowledgement of each frame), wa's to the server on descriptor 3. A frame goes
 * only once the page has drawn the one before: the next carries everything that changed meanwhile, so a page or a
 * link that lags behind skips frames instead of falling further behind.
 */

/** The page's colours, which it draws the cells that have none of their own in. */
export const WEB_FG = '#e9edef'
export const WEB_BG = '#0b141a'

type Row = [number, string][] | undefined
type Style = { fg: Rgb | null; bg: Rgb | null; flags: number }
/** Style flags as the page takes them. */
const BOLD = 1, ITALIC = 2, UNDERLINE = 4
/** blessed's attribute flags (italic is wa's, italic.ts). */
const B_BOLD = 1, B_UNDERLINE = 2, B_INVERSE = 8, B_INVISIBLE = 16, B_ITALIC = 32

const DEFAULT_ATTR = (0x1ff << 9) | 0x1ff
const hex = (c: Rgb) => `#${c.map(v => v.toString(16).padStart(2, '0')).join('')}`
const FG = [0xe9, 0xed, 0xef] as Rgb, BG = [0x0b, 0x14, 0x1a] as Rgb

/** A character the page draws in a cell of its own: past Latin, Greek and Cyrillic, where fonts fall back to others, but for the box-drawing and block ones, which every monospace font has. */
const ownCell = (ch: string) => { const cp = ch.codePointAt(0)!; return cp >= 0x2000 && !(cp >= 0x2500 && cp <= 0x259f) }

/** What a terminal's caps would be for the page: 24-bit colour, emoji and flags two cells wide, and its colours. */
export function webCaps(): TermCaps {
  return {
    kittyGraphics: false, utf8: true, answersQueries: false, version: null, kittyKeyboard: false, fg: WEB_FG, bg: WEB_BG,
    truecolor: true, flagWidth: 2, colors: 256, textSizing: false, sixel: false, cellPx: null, selectorNarrow: '',
  }
}

/**
 * The pictures of a frame, in place of a terminal's (image.ts): each one is told to the page once, by its file under
 * the media folder, which the page asks the server for, or else by its pixels; then where it goes, in cells.
 */
export class WebImages {
  private ids = new Map<string, number>()
  private next = 1
  /** Pictures not told yet, by id: a media file's path, or a PNG's data. */
  defs: Record<number, { file: string } | { png: string }> = {}
  shown: ({ id: number; x: number; y: number; w: number; h: number; from: number; to: number } | { emoji: string; x: number; y: number })[] = []

  clear() { this.shown = [] }

  /** As KittyImages.place: (col, row) 1-based, `cropTop`..`cropBottom` the picture's part in view. */
  place(file: string, d: Decoded, col: number, row: number, cols: number, rows: number, cropTop = 0, cropBottom = 1) {
    let id = this.ids.get(file)
    if (!id) {
      id = this.next++
      this.ids.set(file, id)
      const rel = path.relative(dirs.media, file)
      const servable = !rel.startsWith('..') && !path.isAbsolute(rel) && /\.(jpe?g|png|gif|webp)$/i.test(file)
      this.defs[id] = servable ? { file: rel } : { png: d.png.toString('base64') }
    }
    this.shown.push({ id, x: col - 1, y: row - 1, w: cols, h: rows, from: cropTop, to: cropBottom })
  }

  /** An emoji on its own, drawn twice its size in its four cells by two, (col, row) 1-based. */
  emoji(e: string, col: number, row: number) { this.shown.push({ emoji: e, x: col - 1, y: row - 1 }) }

  /** Nothing to let go: the page keeps what it was told. */
  dispose() {}
}

/** The page's end of wa: blessed's streams, the pictures, the 24-bit colours, and the frames. */
export class WebTab {
  readonly input = Object.assign(new PassThrough(), { isTTY: true, isRaw: true, setRawMode() { return this } })
  readonly output: Writable & { isTTY: boolean; columns: number; rows: number; bytesWritten: number }
  readonly images = new WebImages()
  /** The bubbles' cells in 24-bit colour, as the painter gives them (paintRgb), by y * 65536 + x. */
  private painted = new Map<number, string>()
  readonly painter = {
    snapshot: () => {},
    paint: (cells: RgbCell[]) => { this.painted = new Map(cells.map(c => [c.y * 65536 + c.x, c.sgr])) },
  }
  private styles = new Map<string, number>()
  private sentRows: string[] = []
  private sentSize = ''
  private sentPics = ''
  private waiting = false
  private pending = false
  private lines: () => Row[] = () => []

  constructor(cols: number, rows: number) {
    // Whatever blessed writes for a terminal goes nowhere; that it wrote is what it checks before some sequences.
    const out = Object.assign(new Writable({ write(_chunk, _enc, done) { out.bytesWritten++; done() } }), { isTTY: true, columns: cols, rows, bytesWritten: 0 })
    this.output = out
  }

  /** Tells the page something other than a frame: the title, the icon, a file to open, text to copy, leaving. */
  send(m: Record<string, unknown>) {
    const s = Buffer.from(`${JSON.stringify(m)}\n`)
    for (let at = 0; at < s.length;) {
      try { at += fs.writeSync(3, s, at) } catch (e) { if ((e as NodeJS.ErrnoException).code !== 'EAGAIN') throw e }
    }
  }

  /** blessed's buffer of the frame being drawn, for `frame`. */
  attach(lines: () => Row[]) { this.lines = lines }

  /** After each frame blessed draws: sent now, or once the page has drawn the one before. */
  frame() {
    this.pending = true
    if (!this.waiting) this.flush()
  }

  private onMessage(m: { t?: string; d?: string; cols?: number; rows?: number }) {
    if (m.t === 'in' && typeof m.d === 'string') this.input.write(Buffer.from(m.d, 'utf8'))
    else if (m.t === 'size' && m.cols && m.rows) {
      this.output.columns = m.cols
      this.output.rows = m.rows
      this.output.emit('resize')
    } else if (m.t === 'ack') {
      this.waiting = false
      if (this.pending) this.flush()
    }
  }

  private flush() {
    this.pending = false
    const lines = this.lines()
    const rows = lines.length, cols = lines[0]?.length ?? 0
    const m: Record<string, unknown> = { t: 'frame' }
    const size = `${cols}x${rows}`
    if (size !== this.sentSize) { this.sentSize = size; this.sentRows = []; m.cols = cols; m.rows = rows }
    const newStyles: Record<number, [string | null, string | null, number]> = {}
    const changed: [number, unknown[]][] = []
    for (let y = 0; y < rows; y++) {
      const segs = this.row(lines[y], y, cols, newStyles)
      const key = JSON.stringify(segs)
      if (this.sentRows[y] === key) continue
      this.sentRows[y] = key
      changed.push([y, segs])
    }
    if (Object.keys(newStyles).length) m.styles = newStyles
    if (changed.length) m.lines = changed
    const pics = JSON.stringify(this.images.shown)
    if (pics !== this.sentPics) {
      this.sentPics = pics
      m.pics = this.images.shown
      if (Object.keys(this.images.defs).length) { m.defs = this.images.defs; this.images.defs = {} }
    }
    if (Object.keys(m).length === 1) return
    this.waiting = true
    this.send(m)
  }

  /**
   * A row as the page draws it: runs of cells in one style, as [cells, text, style] one after the other. A wide
   * character (blessed's marker in the cell after it) is a run of its own two cells, and so is any character a
   * monospace font may not have (ownCell), so that whatever width the browser gives it, the cells after stay in place.
   */
  private row(row: Row, y: number, cols: number, newStyles: Record<number, [string | null, string | null, number]>): unknown[] {
    const runs: { w: number; text: string; id: number; open: boolean }[] = []
    let covered = false
    for (let x = 0; x < cols; x++) {
      const cell = row?.[x]
      let ch = cell?.[1] ?? ' '
      // The second cell of the wide character before; on its own (something drawn over the character), a blank.
      if (ch === '\x03' && covered) { covered = false; continue }
      if (!ch || ch < ' ') ch = ' '
      const sgr = this.painted.get(y * 65536 + x)
      const id = this.styleId(sgr ? fromSgr(sgr) : fromAttr(cell?.[0] ?? DEFAULT_ATTR), newStyles)
      const wide = row?.[x + 1]?.[1] === '\x03'
      covered = wide
      const last = runs[runs.length - 1]
      if (!wide && !ownCell(ch) && last?.open && last.id === id) { last.w++; last.text += ch; continue }
      runs.push({ w: wide ? 2 : 1, text: ch, id, open: !wide && !ownCell(ch) })
    }
    return runs.flatMap(r => [r.w, r.text, r.id])
  }

  private styleId(s: Style, newStyles: Record<number, [string | null, string | null, number]>): number {
    let fg = s.fg, bg = s.bg
    if (s.flags & B_INVERSE) { [fg, bg] = [bg ?? BG, fg ?? FG] }
    if (s.flags & B_INVISIBLE) fg = bg ?? BG
    const flags = (s.flags & B_BOLD ? BOLD : 0) | (s.flags & B_ITALIC ? ITALIC : 0) | (s.flags & B_UNDERLINE ? UNDERLINE : 0)
    const def: [string | null, string | null, number] = [fg && hex(fg), bg && hex(bg), flags]
    const key = def.join('|')
    let id = this.styles.get(key)
    if (id == null) { id = this.styles.size; this.styles.set(key, id); newStyles[id] = def }
    return id
  }

  /** Waits for the page's size, the first line on stdin, then takes the rest as they come; the server gone, wa goes. */
  static async open(): Promise<WebTab> {
    const lines = readline.createInterface({ input: process.stdin })
    let tab: WebTab | undefined
    return new Promise(resolve => {
      lines.on('line', l => {
        let m: { t?: string; cols?: number; rows?: number }
        try { m = JSON.parse(l) } catch { return }
        if (tab) return tab.onMessage(m)
        if (m.t !== 'size' || !m.cols || !m.rows) return
        tab = new WebTab(m.cols, m.rows)
        resolve(tab)
      })
      lines.on('close', () => process.exit(0))
    })
  }
}

/** blessed's attribute: background in the low 9 bits, foreground in the next 9 (0x1ff the default), flags above. */
function fromAttr(attr: number): Style {
  const bg = attr & 0x1ff, fg = (attr >> 9) & 0x1ff
  return { fg: fg === 0x1ff ? null : rgb256(fg), bg: bg === 0x1ff ? null : rgb256(bg), flags: attr >> 18 }
}

/** The painter's SGR codes (image.ts), which only set, after a reset: flags, and colours in 256 or 24 bits. */
function fromSgr(sgr: string): Style {
  const s: Style = { fg: null, bg: null, flags: 0 }
  const codes = sgr.split(';').map(Number)
  const FLAG: Record<number, number> = { 1: B_BOLD, 3: B_ITALIC, 4: B_UNDERLINE, 7: B_INVERSE, 8: B_INVISIBLE }
  for (let i = 0; i < codes.length; i++) {
    const c = codes[i]!
    if (c === 0) Object.assign(s, { fg: null, bg: null, flags: 0 })
    else if (FLAG[c]) s.flags |= FLAG[c]!
    else if (c === 39) s.fg = null
    else if (c === 49) s.bg = null
    else if (c === 38 || c === 48) {
      const color: Rgb | null = codes[i + 1] === 5 ? rgb256(codes[i + 2]!) : codes[i + 1] === 2 ? [codes[i + 2]!, codes[i + 3]!, codes[i + 4]!] : null
      i += codes[i + 1] === 5 ? 2 : 4
      if (c === 38) s.fg = color; else s.bg = color
    }
  }
  return s
}
