import { fileURLToPath } from 'node:url'
import type blessed from 'blessed'
import { emoticonify } from './emoji.js'
import { loadSharp } from './image.js'
import { mix, nearest256, type Rgb } from './rainbow.js'

/**
 * An emoji sent or received alone, as a message or a reaction, makes a big copy of it rise through the message
 * panel: it's born over the message itself, grows and rises, like Instagram used to do in chat. The drawing is the
 * terminal's own: the glyph is rasterized with sharp from the system's emoji font (fontconfig's "emoji" family,
 * the one terminals fall back to), so it has the same shape and colors as the small one in the message; where the
 * system has no colour emoji font, from the Noto Color Emoji that comes with wassup (BUNDLED_EMOJI).
 * It draws over everything, only on the cells of the glyph, writing directly into blessed's screen buffer from
 * an empty element that is the last to render.
 */

/** A pixel of the glyph at the drawn size: color and opacity (0..1); null where there's no ink. */
interface Px { r: number; g: number; b: number; a: number }
/** The glyph trimmed to its ink, as rasterized: RGBA bytes, row by row. */
interface Glyph { w: number; h: number; rgba: Uint8Array }

/**
 * Pixels per cell: eighths vertically and quarters horizontally, which in a cell twice as tall as it is wide gives
 * square pixels; that's what the eighth blocks (▁▂▃▄▅▆▇) and quarter blocks (▎▌▊) let you draw.
 */
const PW = 4, PH = 8
/** Max height in lines; the width follows the glyph's proportion (about as many pixels wide as tall). */
const MAX_H = 8
/** Size the glyph is rasterized at, in pixels: twice the biggest drawn size, so shrinking it keeps the edges smooth. */
const RASTER_PX = 128
const LIFE_MS = 4200
const FRAME_MS = 20

/**
 * One emoji: a pictograph (or a pair of regional indicators, a flag), with its presentation selector, skin tone, and
 * the pictographs joined to it (❤️‍🔥, 👨‍👩‍👧).
 */
const EMOJI = '(?:\\p{Extended_Pictographic}|\\p{Regional_Indicator}{2})(?:\\uFE0F|\\p{Emoji_Modifier}|\\u200D\\p{Extended_Pictographic}\\uFE0F?)*'
const ONE = new RegExp(`^${EMOJI}$`, 'u')
const ONLY = new RegExp(`^\\s*(?:${EMOJI}\\s*)+$`, 'u')

/** A single emoji with nothing else (or a smiley that stands for one, like "<3" or ":*"): the emoji, or nothing. */
export function reaction(text: string): string | null {
  const t = emoticonify(text.trim())
  return ONE.test(t) ? t : null
}

/** Text made of emoji alone, one or more, with nothing but spaces between them. */
export function emojiOnly(text: string): boolean {
  return ONLY.test(text)
}

/** Rasterized glyphs by emoji: null when sharp isn't there or couldn't draw it; absent while loading. */
const glyphs = new Map<string, Glyph | null>()
const loading = new Map<string, Promise<void>>()
/**
 * Noto Color Emoji, the vector one (Noto-COLRv1.ttf from googlefonts/noto-emoji v2.051, OFL-1.1, see fonts/LICENSE):
 * for a system with no colour emoji font, whose glyph comes out empty or grey (a Mac, whose Apple Color Emoji
 * fontconfig may not see; a server with no fonts).
 */
const BUNDLED_EMOJI = fileURLToPath(new URL('./fonts/Noto-COLRv1.ttf', import.meta.url))

/** Whether any of the glyph's ink has colour: a monochrome fallback font draws it all in grey. */
function colourful(g: Glyph): boolean {
  for (let i = 0; i < g.rgba.length; i += 4) {
    const [r, gr, b, a] = [g.rgba[i]!, g.rgba[i + 1]!, g.rgba[i + 2]!, g.rgba[i + 3]!]
    if (a > 128 && Math.abs(r - b) + Math.abs(gr - b) > 30) return true
  }
  return false
}

function rasterize(emoji: string): Promise<void> {
  const p = loading.get(emoji)
  if (p) return p
  const job = (async () => {
    try {
      const sharp = await loadSharp()
      if (!sharp) { glyphs.set(emoji, null); return }
      const draw = async (font: { font: string; fontfile?: string }) => {
        const { data, info } = await sharp({ text: { text: emoji, rgba: true, width: RASTER_PX, height: RASTER_PX, ...font } })
          .ensureAlpha().raw().toBuffer({ resolveWithObject: true })
        return trim(new Uint8Array(data.buffer, data.byteOffset, data.byteLength), info.width, info.height)
      }
      let glyph = await draw({ font: 'emoji' }).catch(() => null)
      if (!glyph || !colourful(glyph)) glyph = (await draw({ font: 'Noto Color Emoji', fontfile: BUNDLED_EMOJI }).catch(() => null)) ?? glyph
      glyphs.set(emoji, glyph)
    } catch {
      glyphs.set(emoji, null)
    } finally {
      loading.delete(emoji)
    }
  })()
  loading.set(emoji, job)
  return job
}
/**
 * The emoji as a square PNG, its glyph centred on a transparent ground with a little room around it, for Kitty's
 * graphics (ui.ts draws emoji on their own twice their size with it): null when it can't be drawn.
 */
export async function emojiPng(emoji: string): Promise<{ png: Buffer; size: number } | null> {
  if (!glyphs.has(emoji)) await rasterize(emoji)
  const g = glyphs.get(emoji), sharp = await loadSharp()
  if (!g || !sharp) return null
  const size = Math.round(Math.max(g.w, g.h) * 1.08)
  const x = Math.floor((size - g.w) / 2), y = Math.floor((size - g.h) / 2)
  const png = await sharp(Buffer.from(g.rgba.buffer, g.rgba.byteOffset, g.rgba.byteLength), { raw: { width: g.w, height: g.h, channels: 4 } })
    .extend({ left: x, right: size - g.w - x, top: y, bottom: size - g.h - y, background: { r: 0, g: 0, b: 0, alpha: 0 } })
    .png().toBuffer()
  return { png, size }
}

/** The glyph cut down to the box of its ink (alpha above a hair), or null if it drew nothing. */
function trim(rgba: Uint8Array, w: number, h: number): Glyph | null {
  let x0 = w, y0 = h, x1 = -1, y1 = -1
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) if (rgba[(y * w + x) * 4 + 3]! > 8) {
    if (x < x0) x0 = x; if (x > x1) x1 = x; if (y < y0) y0 = y; if (y > y1) y1 = y
  }
  if (x1 < 0) return null
  const tw = x1 - x0 + 1, th = y1 - y0 + 1
  const out = new Uint8Array(tw * th * 4)
  for (let y = 0; y < th; y++) out.set(rgba.subarray(((y0 + y) * w + x0) * 4, ((y0 + y) * w + x0 + tw) * 4), y * tw * 4)
  return { w: tw, h: th, rgba: out }
}

/** The glyph with `H` pixels of height (width in proportion), each pixel the average of the source area it covers; one per height, so growth is pixel by pixel. */
const shapeCache = new Map<string, (Px | null)[][]>()
function makeShape(emoji: string, g: Glyph, H: number): (Px | null)[][] {
  const key = `${emoji}:${H}`
  const hit = shapeCache.get(key)
  if (hit) return hit
  const W = Math.max(1, Math.round(H * g.w / g.h))
  const rows: (Px | null)[][] = []
  for (let r = 0; r < H; r++) {
    const row: (Px | null)[] = []
    const sy0 = Math.floor(r * g.h / H), sy1 = Math.max(sy0 + 1, Math.floor((r + 1) * g.h / H))
    for (let c = 0; c < W; c++) {
      const sx0 = Math.floor(c * g.w / W), sx1 = Math.max(sx0 + 1, Math.floor((c + 1) * g.w / W))
      let red = 0, green = 0, blue = 0, alpha = 0, n = 0
      for (let y = sy0; y < sy1; y++) for (let x = sx0; x < sx1; x++) {
        const o = (y * g.w + x) * 4, a = g.rgba[o + 3]! / 255
        red += g.rgba[o]! * a; green += g.rgba[o + 1]! * a; blue += g.rgba[o + 2]! * a; alpha += a; n++
      }
      // Ink is a pixel at least half covered; its color is that of the ink alone, the opacity blends it with the background.
      row.push(alpha / n < 0.5 ? null : { r: red / alpha, g: green / alpha, b: blue / alpha, a: alpha / n })
    }
    rows.push(row)
  }
  shapeCache.set(key, rows)
  return rows
}
/**
 * Blocks used to approximate a 4×8-pixel cell: the mask says which pixels the character paints in the foreground
 * color (bit r·4+c, row r top to bottom, column c left to right). Halves, vertical eighths, horizontal quarters and quadrants.
 */
const BLOCKS: { ch: string; mask: number }[] = (() => {
  const m = (f: (r: number, c: number) => boolean) => { let v = 0; for (let r = 0; r < PH; r++) for (let c = 0; c < PW; c++) if (f(r, c)) v |= 1 << (r * PW + c); return v >>> 0 }
  return [
    { ch: '█', mask: m(() => true) },
    { ch: '▁', mask: m(r => r >= 7) }, { ch: '▂', mask: m(r => r >= 6) }, { ch: '▃', mask: m(r => r >= 5) }, { ch: '▄', mask: m(r => r >= 4) },
    { ch: '▅', mask: m(r => r >= 3) }, { ch: '▆', mask: m(r => r >= 2) }, { ch: '▇', mask: m(r => r >= 1) },
    { ch: '▔', mask: m(r => r < 1) }, { ch: '▀', mask: m(r => r < 4) },
    { ch: '▎', mask: m((_r, c) => c < 1) }, { ch: '▌', mask: m((_r, c) => c < 2) }, { ch: '▊', mask: m((_r, c) => c < 3) }, { ch: '▐', mask: m((_r, c) => c >= 2) },
    { ch: '▘', mask: m((r, c) => r < 4 && c < 2) }, { ch: '▝', mask: m((r, c) => r < 4 && c >= 2) },
    { ch: '▖', mask: m((r, c) => r >= 4 && c < 2) }, { ch: '▗', mask: m((r, c) => r >= 4 && c >= 2) },
    { ch: '▚', mask: m((r, c) => (r < 4) === (c < 2)) }, { ch: '▞', mask: m((r, c) => (r < 4) !== (c < 2)) },
    { ch: '▛', mask: m((r, c) => !(r >= 4 && c >= 2)) }, { ch: '▜', mask: m((r, c) => !(r >= 4 && c < 2)) },
    { ch: '▙', mask: m((r, c) => !(r < 4 && c >= 2)) }, { ch: '▟', mask: m((r, c) => !(r < 4 && c < 2)) },
  ]
})()
const FULL = 0xffffffff
const TOP_HALF = BLOCKS.find(b => b.ch === '▀')!.mask
const bits = (v: number) => { v >>>= 0; let n = 0; while (v) { n += v & 1; v >>>= 1 } return n }

/** Where it's born is resolved on the first draw, once the message is on screen: `at` returns the emoji's cell, or nothing. */
interface Heart { born: number; emoji: string; at: () => { x: number; y: number } | null; x?: number; y?: number }

type Cell = [number, string]
type Lines = (Cell[] & { dirty?: boolean })[]

export class Hearts {
  private hearts: Heart[] = []
  private timer: NodeJS.Timeout | undefined
  private layer: blessed.Widgets.BoxElement

  constructor(private screen: blessed.Widgets.Screen, private over: blessed.Widgets.BoxElement, private bg: Rgb, make: typeof blessed.box) {
    // Element with no content or size: it only exists to draw in its turn, over the siblings created before it.
    this.layer = make({ parent: screen, top: 0, left: 0, width: 1, height: 1, hidden: true })
    this.layer.render = (() => { this.draw(); return undefined }) as unknown as typeof this.layer.render
  }

  /** Launches the emoji from the cell that `at` reports (its own, in the message); the glyph is rasterized meanwhile, the first time. */
  launch(emoji: string, at: () => { x: number; y: number } | null) {
    if (!glyphs.has(emoji)) void rasterize(emoji)
    this.hearts.push({ born: Date.now(), emoji, at })
    if (!this.timer) this.timer = setInterval(() => this.tick(), FRAME_MS)
  }

  private tick() {
    const now = Date.now()
    this.hearts = this.hearts.filter(h => now - h.born < LIFE_MS && glyphs.get(h.emoji) !== null)
    if (!this.hearts.length && this.timer) { clearInterval(this.timer); this.timer = undefined }
    this.screen.render()
  }

  /** For each live emoji: rises over time and grows until halfway, in the glyph's own colors. */
  private draw() {
    if (!this.hearts.length) return
    const now = Date.now()
    const lines = (this.screen as unknown as { lines: Lines }).lines
    const top = Number(this.over.atop), left = Number(this.over.aleft)
    const height = Number(this.over.height), width = Number(this.over.width)
    for (const h of this.hearts) {
      // The clock only starts once the glyph is rasterized; one that failed is dropped on the next tick.
      const g = glyphs.get(h.emoji)
      if (!g) { h.born = now; continue }
      const p = (now - h.born) / LIFE_MS
      if (p < 0) continue
      if (h.x == null || h.y == null) {
        // The message may not be drawn yet: wait up to half a second; after that it's born at the bottom, centered.
        const pos = h.at()
        if (!pos && now - h.born < 500) continue
        const { x, y } = pos ?? { x: left + Math.floor(width / 2), y: top + height - 1 }
        h.x = x; h.y = y; h.born = now
      }
      // Grows continuously until halfway, then stays at max size.
      const fullH = PH * MAX_H
      const shape = makeShape(h.emoji, g, Math.max(2, Math.min(fullH, Math.round(p * 2 * fullH))))
      // Rises from the message's line, bottom starting over the emoji, until it's entirely off the top of the
      // panel, sliding meanwhile toward the center; it never goes off the sides or the bottom. Movement is in
      // quarter-cells in both directions: the glyph packs into cells starting from any pixel.
      const H = shape.length, W = shape[0]!.length
      const travelPx = (h.y - top + MAX_H + 1) * PH
      const topPx = Math.min((top + height) * PH - H, (h.y + 1) * PH - H - Math.round(p * travelPx))
      const ease = 1 - (1 - p) * (1 - p)
      const cxPx = (h.x + 0.5 + (left + width / 2 - h.x - 0.5) * ease) * PW
      const leftPx = Math.max(left * PW, Math.min((left + width) * PW - W, Math.round(cxPx - W / 2)))
      // The color of a set of pixels: the ink's average, weighted by opacity, blended into the terminal's
      // background by what the average opacity leaves out, which softens the edges.
      const color = (qs: Px[]): number => {
        let r = 0, g = 0, b = 0, a = 0
        for (const q of qs) { r += q.r * q.a; g += q.g * q.a; b += q.b * q.a; a += q.a }
        return nearest256(mix([r / a, g / a, b / a], this.bg, 1 - a / qs.length))
      }
      const firstRow = Math.floor(topPx / PH), lastRow = Math.floor((topPx + H - 1) / PH)
      const firstCol = Math.floor(leftPx / PW), lastCol = Math.floor((leftPx + W - 1) / PW)
      for (let y = Math.max(top, firstRow); y <= Math.min(top + height - 1, lastRow); y++) {
        const row = lines[y]
        if (!row) continue
        for (let x = Math.max(left, firstCol); x <= Math.min(left + width - 1, lastCol); x++) {
          const cell = row[x]
          if (!cell) continue
          // The cell's 32 pixels and the mask of those present.
          const q: (Px | null)[] = []
          let mask = 0
          for (let r = 0; r < PH; r++) for (let c = 0; c < PW; c++) {
            const pr = y * PH + r - topPx, pc = x * PW + c - leftPx
            const v = pr >= 0 && pr < H && pc >= 0 && pc < W ? shape[pr]![pc] ?? null : null
            q.push(v); if (v) mask |= 1 << (r * PW + c)
          }
          mask >>>= 0
          if (!mask) continue
          const keep = cell[0] & 0x1ff
          const pick = (m: number) => q.filter((v, i): v is Px => !!v && !!((m >>> i) & 1))
          // Full cell: ▀ with the top half in the foreground and the bottom in the background, so the colors get half-cell resolution.
          if (mask === FULL) { cell[0] = (color(pick(TOP_HALF)) << 9) | color(pick(~TOP_HALF >>> 0)); cell[1] = '▀'; row.dirty = true; continue }
          // Otherwise the block that misses the fewest pixels. Pixels present outside the block are painted in the
          // cell's background (covering what was there); with none, the background stays. Missing pixels inside the
          // block, or outside it when the background gets painted, count as errors. Ties go to the block covering
          // more present pixels.
          let best = BLOCKS[0]!, bestErr = Infinity, bestHit = -1
          for (const b of BLOCKS) {
            const outside = (mask & ~b.mask) >>> 0
            const err = bits(b.mask & ~mask) + (outside ? bits(~mask & ~b.mask) : 0)
            const hit = bits(b.mask & mask)
            if (err < bestErr || (err === bestErr && hit > bestHit)) { best = b; bestErr = err; bestHit = hit }
          }
          const inside = pick(best.mask), outside = pick(~best.mask >>> 0)
          if (!inside.length) continue
          cell[0] = (color(inside) << 9) | (outside.length ? color(outside) : keep)
          cell[1] = best.ch
          row.dirty = true
        }
      }
    }
  }
}
