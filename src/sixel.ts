import { loadSharp, type Decoded } from './image.js'

/**
 * Sixel graphics, for terminals that take them but not Kitty's (foot, among others). Unlike Kitty's placements, a
 * sixel image is painted into the cells themselves, as text is: what blessed later writes in one of those cells
 * eats that part of it, and what was painted stays where blessed believes its cells blank. So the pictures in view
 * are written after each frame, again wherever blessed has just written over them or they've moved, and the cells
 * of those that went are written back as blessed has them. Each is drawn to its cells' pixels (the terminal doesn't
 * scale it), its colours reduced to a palette by sharp, and kept encoded by size and cut.
 */

/** A picture's cells on the screen (0-based) and what's painted in them. */
interface Spot { id: string; key: string; x: number; y: number; cols: number; rows: number }
/** A picture wanted in this frame: its spot and how to encode it. */
interface Want extends Spot { png: Buffer; pw: number; ph: number; y0: number; y1: number }

type Cell = [number, string]
type Rows = (Cell[] | undefined)[]

/** Encoded pictures kept, most recently used last. */
const KEEP = 60

export class SixelImages {
  private shown: Spot[] = []
  private want: Want[] = []
  /** The pictures shown whose cells blessed is about to write over (snapshot), and the rows they were drawn on. */
  private damaged = new Set<string>()
  private rows = new Map<number, unknown>()
  private encoded = new Map<string, string | null>()
  private encoding = new Set<string>()

  /** `write` goes straight to the terminal; `redraw` asks for a frame, once a picture is encoded. */
  constructor(private write: (s: string) => void, private cell: { w: number; h: number }, private redraw: () => void) {
    // The cursor stays beside a picture just drawn rather than going down a line, which at the screen's last one
    // would scroll it.
    write('\x1b[?8452h')
  }

  /** At the start of each frame: the pictures wanted are placed again (place). */
  clear() {
    this.want = []
  }

  /**
   * A picture to draw with its top-left corner at cell (col,row), 1-based, over cols×rows cells; `cropTop` and
   * `cropBottom` the part of it in view, from 0 to 1, for one partly out of the panel.
   */
  place(path: string, d: Decoded, col: number, row: number, cols: number, rows: number, cropTop = 0, cropBottom = 1) {
    const fullRows = Math.max(rows, Math.round(rows / Math.max(0.01, cropBottom - cropTop)))
    const skip = Math.min(fullRows - rows, Math.round(cropTop * fullRows))
    const pw = cols * this.cell.w, ph = fullRows * this.cell.h
    const y0 = skip * this.cell.h, y1 = y0 + rows * this.cell.h
    const key = `${path}|${cols}x${fullRows}|${skip}+${rows}`
    this.want.push({ id: `${key}@${col},${row}`, key, x: col - 1, y: row - 1, cols, rows, png: d.png, pw, ph, y0, y1 })
  }

  /** Before blessed draws: the pictures shown whose cells it's about to write, or whose rows it moved. */
  snapshot(olines: Rows, lines: Rows) {
    this.damaged.clear()
    for (const s of this.shown) {
      for (let y = s.y; y < s.y + s.rows && !this.damaged.has(s.id); y++) {
        const o = olines[y], n = lines[y]
        if (!o || !n || o !== this.rows.get(y)) { this.damaged.add(s.id); break }
        for (let x = s.x; x < s.x + s.cols; x++) {
          const a = o[x], b = n[x]
          if (!a || !b || a[0] !== b[0] || a[1] !== b[1]) { this.damaged.add(s.id); break }
        }
      }
    }
  }

  /**
   * After blessed draws: the cells of the pictures that went, or moved, written back as blessed has them (`lines`,
   * `codeAttr` for their attributes); then the pictures wanted, those not already there intact, a damaged one
   * drawn whole over what blessed wrote. One still being encoded waits for the frame its encoding asks for.
   */
  flush(lines: Rows, olines: Rows, codeAttr: (attr: number) => string) {
    const wanted = new Set(this.want.map(w => w.id))
    let out = ''
    for (const s of this.shown) if (!wanted.has(s.id)) out += this.cells(s, lines, codeAttr)
    const kept = new Set(this.shown.filter(s => !this.damaged.has(s.id)).map(s => s.id))
    const now: Spot[] = []
    for (const w of this.want) {
      if (kept.has(w.id)) { now.push(w); continue }
      // Never past the screen's last row: the terminal would scroll it.
      if (w.y + w.rows > lines.length) continue
      const six = this.sixels(w)
      if (!six) continue
      out += `\x1b[${w.y + 1};${w.x + 1}H${six}`
      now.push(w)
    }
    this.shown = now.map(({ id, key, x, y, cols, rows }) => ({ id, key, x, y, cols, rows }))
    this.rows.clear()
    for (const s of this.shown) for (let y = s.y; y < s.y + s.rows; y++) this.rows.set(y, olines[y])
    if (out) this.write(`\x1b7${out}\x1b[0m\x1b8`)
  }

  /** Everything drawn is forgotten (a resize, which redraws the screen): the next frame draws it all again. */
  dispose() {
    this.shown = []
    this.rows.clear()
  }

  /** On exit: the cursor goes back to its usual place after a picture. */
  end() {
    this.write('\x1b[?8452l')
  }

  /** A spot's cells as blessed has them: its characters in their attributes, wide ones once. */
  private cells(s: Spot, lines: Rows, codeAttr: (attr: number) => string): string {
    let out = ''
    for (let y = s.y; y < s.y + s.rows; y++) {
      const row = lines[y]
      if (!row) continue
      out += `\x1b[${y + 1};${s.x + 1}H`
      for (let x = s.x; x < s.x + s.cols && x < row.length; x++) {
        const [attr, ch] = row[x]!
        // A wide character's second cell: written with the character, or a space when the character is outside.
        if (ch === '\x03' || ch === '\0') { if (x === s.x) out += `${codeAttr(attr)} `; continue }
        out += `${codeAttr(attr)}${ch}`
      }
    }
    return out
  }

  /** The picture's sixels, or null while it's encoded (the frame after asks again) or if it can't be. */
  private sixels(w: Want): string | null {
    const hit = this.encoded.get(w.key)
    if (hit !== undefined) { this.encoded.delete(w.key); this.encoded.set(w.key, hit); return hit }
    if (!this.encoding.has(w.key)) {
      this.encoding.add(w.key)
      void encodeSixel(w.png, w.pw, w.ph, w.y0, w.y1).catch(() => null).then(six => {
        this.encoding.delete(w.key)
        this.encoded.set(w.key, six)
        if (this.encoded.size > KEEP) this.encoded.delete(this.encoded.keys().next().value!)
        if (six) this.redraw()
      })
    }
    return null
  }
}

/** The picture in `png` as sixels, drawn `w`×`h` pixels, of which the rows `y0` to `y1`; null without sharp. */
export async function encodeSixel(png: Buffer, w: number, h: number, y0: number, y1: number): Promise<string | null> {
  const sharp = await loadSharp()
  if (!sharp) return null
  // Its colours reduced to a palette of 256 at most, dithered a little, as sixels number them.
  const cut = await sharp(png).resize(w, h, { fit: 'fill' }).extract({ left: 0, top: y0, width: w, height: y1 - y0 })
    .png({ palette: true, colors: 256, dither: 0.5 }).toBuffer()
  const { data, info } = await sharp(cut).ensureAlpha().raw().toBuffer({ resolveWithObject: true })
  return sixelOf(data, info.width, info.height)
}

/**
 * RGBA pixels as a sixel image: its palette (each colour once, up to 256; a pixel mostly transparent left out, so
 * what's under it stays), then six rows at a time, one pass per colour in them, runs of the same column written once
 * with their count.
 */
function sixelOf(px: Buffer, w: number, h: number): string {
  const colours: number[] = [], index = new Map<number, number>()
  const at = new Int16Array(w * h).fill(-1)
  for (let i = 0; i < w * h; i++) {
    if (px[i * 4 + 3]! < 128) continue
    const rgb = (px[i * 4]! << 16) | (px[i * 4 + 1]! << 8) | px[i * 4 + 2]!
    let c = index.get(rgb)
    if (c === undefined) {
      c = colours.length < 256 ? colours.push(rgb) - 1 : nearest(colours, rgb)
      index.set(rgb, c)
    }
    at[i] = c
  }
  const pct = (v: number) => Math.round((v * 100) / 255)
  let out = `\x1bP0;1;0q"1;1;${w};${h}`
  colours.forEach((rgb, i) => { out += `#${i};2;${pct(rgb >> 16)};${pct((rgb >> 8) & 255)};${pct(rgb & 255)}` })
  for (let y = 0; y < h; y += 6) {
    const used = new Set<number>()
    for (let i = y * w; i < Math.min(h, y + 6) * w; i++) if (at[i]! >= 0) used.add(at[i]!)
    const passes: string[] = []
    for (const c of used) {
      let line = '', run = 0, last = -1
      const emit = () => { if (run) line += run > 3 ? `!${run}${String.fromCharCode(63 + last)}` : String.fromCharCode(63 + last).repeat(run) }
      for (let x = 0; x < w; x++) {
        let bits = 0
        for (let k = 0; k < 6 && y + k < h; k++) if (at[(y + k) * w + x] === c) bits |= 1 << k
        if (bits === last) { run++; continue }
        emit()
        last = bits; run = 1
      }
      // The empty columns at the end of a pass aren't written.
      if (last !== 0) emit()
      passes.push(`#${c}${line}`)
    }
    out += passes.join('$') + '-'
  }
  return `${out}\x1b\\`
}

/** The palette's colour closest to `rgb`, for a picture with more than 256. */
function nearest(colours: number[], rgb: number): number {
  let best = 0, bestD = Infinity
  colours.forEach((c, i) => {
    const d = ((c >> 16) - (rgb >> 16)) ** 2 + (((c >> 8) & 255) - ((rgb >> 8) & 255)) ** 2 + ((c & 255) - (rgb & 255)) ** 2
    if (d < bestD) { bestD = d; best = i }
  })
  return best
}
