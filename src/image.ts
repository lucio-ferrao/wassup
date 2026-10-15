import { createJimp } from '@jimp/core'
import { defaultFormats, defaultPlugins } from 'jimp'
import webp from '@jimp/wasm-webp'
import { nearest256 } from './rainbow.js'

const Jimp = createJimp({ formats: [...defaultFormats, webp], plugins: defaultPlugins })

export interface Decoded {
  w: number
  h: number
  png: Buffer
  /** RGBA pixels of the already-downscaled image (max. 400 px wide), for the half-blocks */
  rgba: Buffer
  rw: number
  rh: number
}

const cache = new Map<string, Decoded | Error>()
const pending = new Map<string, Promise<Decoded | Error>>()
// Each decoded image takes up close to 1 MB (PNG for Kitty plus the pixels for the blocks): the cache is limited
// to the most recently used.
const CACHE_MAX = 40

export function cached(path: string): Decoded | Error | undefined {
  const hit = cache.get(path)
  if (hit) { cache.delete(path); cache.set(path, hit) }
  return hit
}

function remember(path: string, d: Decoded | Error) {
  cache.set(path, d)
  if (cache.size > CACHE_MAX) cache.delete(cache.keys().next().value!)
}

/**
 * Decodes with sharp (native, off the main thread: a 9 Mpx photo in tens of ms), falling back to jimp (pure
 * JavaScript, hundreds of ms blocking the UI) only when sharp isn't available. Never more than two at once.
 */
type SharpFn = typeof import('sharp').default
let sharpMod: SharpFn | null | undefined
/** sharp, loaded on first use, or null when it isn't installed. */
export async function loadSharp() {
  if (sharpMod !== undefined) return sharpMod
  try { sharpMod = (await import('sharp')).default } catch { sharpMod = null }
  return sharpMod
}

async function decodeWithSharp(sharp: SharpFn, path: string): Promise<Decoded> {
  const img = sharp(path, { animated: false }).rotate()
  const meta = await img.metadata()
  const w = meta.width ?? 0, h = meta.height ?? 0
  const png = await img.clone().resize({ width: 800, withoutEnlargement: true }).png().toBuffer()
  const { data, info } = await img.clone().resize({ width: 400, withoutEnlargement: true }).ensureAlpha().raw().toBuffer({ resolveWithObject: true })
  return { w, h, png, rgba: Buffer.from(data), rw: info.width, rh: info.height }
}

async function decodeWithJimp(path: string): Promise<Decoded> {
  const img = await Jimp.read(path)
  const w = img.width, h = img.height
  if (w > 800) img.resize({ w: 800 })
  const png = Buffer.from(await img.getBuffer('image/png'))
  const small = img.clone()
  if (small.width > 400) small.resize({ w: 400 })
  return { w, h, png, rgba: Buffer.from(small.bitmap.data), rw: small.width, rh: small.height }
}

const DECODE_CONCURRENCY = 2
let decoding = 0
const waiting: (() => void)[] = []
const acquire = () => new Promise<void>(r => { if (decoding < DECODE_CONCURRENCY) { decoding++; r() } else waiting.push(() => { decoding++; r() }) })
const release = () => { decoding--; waiting.shift()?.() }

export function decode(path: string): Promise<Decoded | Error> {
  const hit = cached(path)
  if (hit) return Promise.resolve(hit)
  const p = pending.get(path)
  if (p) return p
  const job = (async (): Promise<Decoded | Error> => {
    await acquire()
    try {
      const sharp = await loadSharp()
      return sharp ? await decodeWithSharp(sharp, path) : await decodeWithJimp(path)
    } catch (e) {
      return e instanceof Error ? e : new Error(String(e))
    } finally {
      release()
    }
  })()
  pending.set(path, job)
  job.then(r => { remember(path, r); pending.delete(path) })
  return job
}

/**
 * Small copies of pictures, `THUMB_PX` square at most, for the chat list's photos: decoded once each and kept apart
 * from the big pictures' cache (CACHE_MAX), where dozens of photos would push each other and the messages' images
 * out and be decoded again at every scroll. Their `w`×`h` is the copy's own, which is what Kitty is given.
 */
const THUMB_PX = 64
const thumbs = new Map<string, Decoded | Error>()
const thumbsPending = new Map<string, Promise<Decoded | Error>>()

export function thumbCached(path: string): Decoded | Error | undefined {
  return thumbs.get(path)
}

export function decodeThumb(path: string): Promise<Decoded | Error> {
  const hit = thumbs.get(path)
  if (hit) return Promise.resolve(hit)
  const p = thumbsPending.get(path)
  if (p) return p
  const job = (async (): Promise<Decoded | Error> => {
    await acquire()
    try {
      const sharp = await loadSharp()
      if (sharp) {
        const img = sharp(path, { animated: false }).rotate().resize({ width: THUMB_PX, height: THUMB_PX, fit: 'cover' })
        const png = await img.clone().png().toBuffer()
        const { data, info } = await img.clone().ensureAlpha().raw().toBuffer({ resolveWithObject: true })
        return { w: info.width, h: info.height, png, rgba: Buffer.from(data), rw: info.width, rh: info.height }
      }
      const img = (await Jimp.read(path)).cover({ w: THUMB_PX, h: THUMB_PX })
      return { w: img.width, h: img.height, png: Buffer.from(await img.getBuffer('image/png')), rgba: Buffer.from(img.bitmap.data), rw: img.width, rh: img.height }
    } catch (e) {
      return e instanceof Error ? e : new Error(String(e))
    } finally {
      release()
    }
  })()
  thumbsPending.set(path, job)
  job.then(r => { thumbs.set(path, r); thumbsPending.delete(path) })
  return job
}

/**
 * Size in cells for a w×h px image, assuming cells twice as tall as they are wide. With `fill` it occupies the
 * whole available width (shrunk only if the height doesn't fit); without `fill` it never grows past its natural
 * size.
 */
export function cellSize(w: number, h: number, maxCols: number, maxRows: number, fill = false): { cols: number; rows: number } {
  let cols = Math.max(1, fill ? maxCols : Math.min(maxCols, Math.round(w / 8)))
  let rows = Math.max(1, Math.round((cols * h) / w / 2))
  if (rows > maxRows) {
    rows = maxRows
    cols = Math.max(1, Math.round((rows * 2 * w) / h))
  }
  return { cols, rows }
}

/**
 * The palette index of each half-cell of an image drawn `cols` wide and `rows` tall in half-blocks (`cols` ×
 * `rows * 2` of them, row by row), or -1 where it's transparent. Each half-cell gets the average of all the pixels
 * it covers, weighted by their opacity, instead of one pixel picked from them, which broke edges and text into
 * stray dots. No dithering: with half-cells this big and a palette of six levels per channel, Floyd–Steinberg's
 * carried error showed up as saturated dots that weren't in the picture.
 */
export function blockColors(d: Decoded, cols: number, rows: number): { idx: Int16Array; rgb: Uint8Array } {
  const W = cols, H = rows * 2
  const sx = d.rw / W, sy = d.rh / H
  const out = new Int16Array(W * H).fill(-1)
  // The average itself, three bytes per half-cell, for terminals that take 24-bit colour (RgbBlocks).
  const rgb = new Uint8Array(W * H * 3)
  for (let y = 0; y < H; y++) {
    const y0 = Math.min(d.rh - 1, Math.floor(y * sy)), y1 = Math.max(y0 + 1, Math.min(d.rh, Math.floor((y + 1) * sy)))
    for (let x = 0; x < W; x++) {
      const x0 = Math.min(d.rw - 1, Math.floor(x * sx)), x1 = Math.max(x0 + 1, Math.min(d.rw, Math.floor((x + 1) * sx)))
      let sa = 0, sr = 0, sg = 0, sb = 0
      for (let yy = y0; yy < y1; yy++) {
        for (let xx = x0; xx < x1; xx++) {
          const o = (yy * d.rw + xx) * 4, a = d.rgba[o + 3]!
          sa += a; sr += d.rgba[o]! * a; sg += d.rgba[o + 1]! * a; sb += d.rgba[o + 2]! * a
        }
      }
      // Transparent below a quarter of opacity, the same threshold as one pixel had.
      if (sa / ((y1 - y0) * (x1 - x0)) < 64) continue
      const i = y * W + x, avg: [number, number, number] = [sr / sa, sg / sa, sb / sa]
      out[i] = nearest256(avg)
      rgb[i * 3] = Math.round(avg[0]); rgb[i * 3 + 1] = Math.round(avg[1]); rgb[i * 3 + 2] = Math.round(avg[2])
    }
  }
  return { idx: out, rgb }
}

/** An image's half-cells at one size: palette indices, 24-bit averages, and the lines halfBlocks draws from them. */
export interface BlockGrid { idx: Int16Array; rgb: Uint8Array; lines: string[] }

/** Grids per image and size: averaging reads every pixel, too much to redo on each redraw. */
const blockCache = new WeakMap<Decoded, Map<string, BlockGrid>>()

export function blockGrid(d: Decoded, cols: number, rows: number): BlockGrid {
  const key = `${cols}x${rows}`
  const sizes = blockCache.get(d) ?? new Map<string, BlockGrid>()
  blockCache.set(d, sizes)
  const hit = sizes.get(key)
  if (hit) return hit
  const { idx, rgb } = blockColors(d, cols, rows)
  const grid = { idx, rgb, lines: blockLines(idx, cols, rows) }
  sizes.set(key, grid)
  return grid
}

/**
 * Rows of ▀ half-blocks, two pixel rows per line, coloured by blockColors in the 256-colour palette blessed keeps
 * anyway (handing it 24-bit colours made it match each cell against the palette while parsing), and with a colour
 * only written where it changes from the cell before: a line costs blessed and the terminal a fraction of what one
 * SGR pair per cell did.
 */
export function halfBlocks(d: Decoded, cols: number, rows: number): string[] {
  return blockGrid(d, cols, rows).lines
}

function blockLines(px: Int16Array, cols: number, rows: number): string[] {
  const out: string[] = []
  for (let r = 0; r < rows; r++) {
    let line = '', fg = -1, bg = -1
    for (let c = 0; c < cols; c++) {
      const top = px[r * 2 * cols + c]!, bottom = px[(r * 2 + 1) * cols + c]!
      // Which color goes in the foreground (the block's glyph) and which in the background; -1 is the terminal's own.
      const [f, b, ch] = top < 0 && bottom < 0 ? [-1, -1, ' '] : bottom < 0 ? [top, -1, '▀'] : top < 0 ? [bottom, -1, '▄'] : [top, bottom, '▀']
      if (b < 0 && bg >= 0) { line += '\x1b[0m'; fg = -1 }
      else if (b >= 0 && b !== bg) line += `\x1b[48;5;${b}m`
      if (f >= 0 && f !== fg) line += `\x1b[38;5;${f}m`
      fg = f; bg = b
      line += ch
    }
    out.push(line + '\x1b[0m')
  }
  return out
}

/**
 * The cell blockLines puts at column `c` of row `r`: its glyph, its palette colours (-1 is the terminal's own) and
 * the half-cells they come from (-1 for none), or null where both halves are transparent and nothing is drawn.
 */
export function blockCell(idx: Int16Array, cols: number, r: number, c: number): { ch: string; fg: number; bg: number; fgAt: number; bgAt: number } | null {
  const ti = r * 2 * cols + c, bi = (r * 2 + 1) * cols + c
  const top = idx[ti]!, bottom = idx[bi]!
  if (top < 0 && bottom < 0) return null
  if (bottom < 0) return { ch: '▀', fg: top, bg: -1, fgAt: ti, bgAt: -1 }
  if (top < 0) return { ch: '▄', fg: bottom, bg: -1, fgAt: bi, bgAt: -1 }
  return { ch: '▀', fg: top, bg: bottom, fgAt: ti, bgAt: bi }
}

/**
 * One cell to paint in 24-bit colour, at its screen position (0-based): its character, the SGR parameters that
 * draw it (from a reset, so nothing carries over) and how many cells the character takes (2 for a wide one).
 */
export interface RgbCell { x: number; y: number; ch: string; sgr: string; w: number }

/** blessed's frame buffer: per row, per cell, the attribute code and the character. */
type CellRows = ([number, string][] | undefined)[]

/**
 * 24-bit colour, on terminals that confirmed it (or with WA_COLORS=truecolor), for half-block images and message
 * bubbles. blessed keeps colours to the 256-colour palette, so it still draws them in it; after each frame this
 * repaints their cells straight on the terminal with the exact colours, cursor and attributes saved and restored
 * around it, as the Kitty placements are. Images and bubbles go through the one painter, so they never fight
 * over a cell.
 *
 * Only what changed is written, or the typing animation, at 25 frames a second, would send megabytes: a cell is
 * repainted when blessed has just rewritten it (its content in the previous frame buffer differs, snapshot taken
 * on 'prerender') or when the colour due there isn't the one painted last. A row blessed scrolled with the
 * terminal's own line insert and delete moves the painting with it; the previous buffer's row is then another
 * array, which counts as rewritten too.
 */
export class RgbPainter {
  /** What was painted at each screen cell (y * 65536 + x), and the previous buffer's rows that held it. */
  private painted = new Map<number, string>()
  private rows = new Map<number, unknown>()
  /** Before blessed draws: the previous buffer's content of each painted cell still in place. */
  private before = new Map<number, string>()

  constructor(private write: (s: string) => void) {}

  /**
   * `lines` is the frame about to be drawn: blessed writes a wide character again whenever its row is redrawn, even
   * unchanged (it leaves "\0" in its copy for that), so in those rows it no longer counts as still painted.
   */
  snapshot(olines: CellRows, lines: CellRows) {
    this.before.clear()
    for (const key of this.painted.keys()) {
      const y = Math.floor(key / 65536), row = olines[y]
      if (!row || row !== this.rows.get(y)) continue
      const cell = row[key % 65536]
      if (!cell || (cell[1] === '\0' && (lines[y] as { dirty?: boolean } | undefined)?.dirty)) continue
      this.before.set(key, `${cell[0]}|${cell[1]}`)
    }
  }

  /** After blessed draws: `cells` are the cells now in view to paint, with blessed's own copy matching; `olines` its buffer as drawn. */
  paint(cells: RgbCell[], olines: CellRows) {
    const now = new Map<number, string>()
    let out = '', lastY = -1, nextX = -1, sgr = ''
    for (const c of cells) {
      const key = c.y * 65536 + c.x, sig = `${c.ch}\u0000${c.sgr}`
      now.set(key, sig)
      const cell = olines[c.y]?.[c.x]
      if (this.painted.get(key) === sig && cell && this.before.get(key) === `${cell[0]}|${cell[1]}`) continue
      if (c.y !== lastY || c.x !== nextX) out += `\x1b[${c.y + 1};${c.x + 1}H`
      if (c.sgr !== sgr) { out += `\x1b[${c.sgr}m`; sgr = c.sgr }
      out += c.ch
      // A wide character moves the terminal's cursor two cells on.
      lastY = c.y; nextX = c.x + c.w
    }
    this.painted = now
    this.rows.clear()
    for (const key of now.keys()) { const y = Math.floor(key / 65536); this.rows.set(y, olines[y]) }
    if (out) this.write(`\x1b7${out}\x1b[0m\x1b8`)
  }
}

/** 24-bit colour for images and bubbles: WA_COLORS=truecolor|256 forces it; otherwise what the terminal confirmed when probed. */
export function detectRgb(confirmed: boolean): boolean {
  const forced = process.env.WA_COLORS
  return forced === 'truecolor' ? true : forced === '256' ? false : confirmed
}

/** 'web': in a browser's tab, the page draws them (webtab.ts). */
export type ImageMode = 'kitty' | 'sixel' | 'blocks' | 'none' | 'web'

/**
 * `WA_IMAGES` forces the mode; otherwise what the terminal answered during probing is used: Kitty's graphics, or
 * else sixels (sixel.ts) where the terminal takes them, said its cells' size and sharp is there to encode them.
 * Inside Herdr the probe reaches the terminal underneath, which says yes to Kitty graphics, but the multiplexer
 * doesn't relay the placements and the images come out as empty space: there it's half-blocks, which show something.
 */
export function detectImageMode(kittyGraphics: boolean, inHerdr = false, sixel = false): ImageMode {
  const forced = process.env.WA_IMAGES
  if (forced === 'kitty' || forced === 'sixel' || forced === 'blocks' || forced === 'none') return forced
  return inHerdr ? 'blocks' : kittyGraphics ? 'kitty' : sixel && sharpMod ? 'sixel' : 'blocks'
}

/**
 * Kitty graphics protocol (Ghostty, Kitty, WezTerm): the image is transmitted once per id and then placed in
 * screen cells on every frame. With q=2 the terminal doesn't reply, so as not to mix bytes into blessed's stdin.
 */
export class KittyImages {
  private ids = new Map<string, number>()
  private nextId = 1
  private order: string[] = []
  constructor(private write: (s: string) => void, private maxImages = 60) {}

  private transmit(path: string, png: Buffer): number {
    const id = this.nextId++
    const b64 = png.toString('base64')
    const chunk = 4096
    for (let i = 0; i < b64.length; i += chunk) {
      const last = i + chunk >= b64.length
      const head = i === 0 ? `a=t,f=100,i=${id},q=2,m=${last ? 0 : 1}` : `m=${last ? 0 : 1}`
      this.write(`\x1b_G${head};${b64.slice(i, i + chunk)}\x1b\\`)
    }
    this.ids.set(path, id)
    this.order.push(path)
    if (this.order.length > this.maxImages) {
      const old = this.order.shift()!
      const oldId = this.ids.get(old)
      this.ids.delete(old)
      if (oldId) this.write(`\x1b_Ga=d,d=I,i=${oldId},q=2\x1b\\`)
    }
    return id
  }

  /** Clears every placement on screen (the data stays in the terminal). Call at the start of each frame. */
  clear() {
    this.write('\x1b_Ga=d,d=a,q=2\x1b\\')
  }

  /**
   * Places the image with its top-left corner at cell (col,row), 1-based, occupying cols×rows cells.
   * `crop` is the visible vertical fraction [top,bottom) from 0 to 1, for images partly outside the panel.
   */
  place(path: string, d: Decoded, col: number, row: number, cols: number, rows: number, cropTop = 0, cropBottom = 1) {
    const id = this.ids.get(path) ?? this.transmit(path, d.png)
    const pw = Math.min(d.w, 800), ph = Math.round((d.h * pw) / d.w)
    const y = Math.floor(cropTop * ph), h = Math.max(1, Math.floor((cropBottom - cropTop) * ph))
    this.write(`\x1b7\x1b[${row};${col}H\x1b_Ga=p,i=${id},c=${cols},r=${rows},x=0,y=${y},w=${pw},h=${h},C=1,q=2\x1b\\\x1b8`)
  }

  /** Releases everything, on exit. */
  dispose() {
    this.write('\x1b_Ga=d,d=A,q=2\x1b\\')
    this.ids.clear()
    this.order = []
  }
}
