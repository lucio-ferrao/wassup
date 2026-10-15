/**
 * Colours worked out in RGB (the terminal's own, blends for notices and hearts, image pixels) and brought to the
 * 256-colour palette at the end, since blessed only takes palette indices: the nearest one in the 6×6×6 cube or
 * the grayscale ramp.
 */
export type Rgb = [number, number, number]

/** "#rrggbb" (the terminal's reply to OSC 10/11) to RGB; null if it isn't that. */
export function parseHex(s: string | null): Rgb | null {
  const m = s && /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(s)
  return m ? [parseInt(m[1]!, 16), parseInt(m[2]!, 16), parseInt(m[3]!, 16)] : null
}

/** Linear blend from `a` to `b`; `t` from 0 (only `a`) to 1 (only `b`). */
export function mix(a: Rgb, b: Rgb, t: number): Rgb {
  return [0, 1, 2].map(i => Math.round(a[i]! + (b[i]! - a[i]!) * t)) as Rgb
}

const CUBE = [0, 95, 135, 175, 215, 255]

/** The 256-palette's colour `i` in RGB: the 16 basic ones as xterm has them, then the 6×6×6 cube and the greys. */
export function rgb256(i: number): Rgb {
  const BASIC: Rgb[] = [[0, 0, 0], [205, 0, 0], [0, 205, 0], [205, 205, 0], [0, 0, 238], [205, 0, 205], [0, 205, 205], [229, 229, 229],
    [127, 127, 127], [255, 0, 0], [0, 255, 0], [255, 255, 0], [92, 92, 255], [255, 0, 255], [0, 255, 255], [255, 255, 255]]
  if (i < 16) return BASIC[i]!
  if (i < 232) { const n = i - 16; return [CUBE[Math.floor(n / 36)]!, CUBE[Math.floor(n / 6) % 6]!, CUBE[n % 6]!] }
  const g = 8 + (i - 232) * 10
  return [g, g, g]
}
const nearestCube = (c: number) => CUBE.reduce((best, v, i) => (Math.abs(v - c) < Math.abs(CUBE[best]! - c) ? i : best), 0)
const dist2 = (a: Rgb, b: Rgb) => (a[0] - b[0]) ** 2 + (a[1] - b[1]) ** 2 + (a[2] - b[2]) ** 2

/** Nearest 256-palette index to a color: the 6×6×6 cube (16..231) or the grayscale ramp (232..255). */
export function nearest256(rgb: Rgb): number {
  const [r, g, b] = rgb.map(nearestCube) as Rgb
  const cube = 16 + 36 * r + 6 * g + b
  const cubeRgb: Rgb = [CUBE[r]!, CUBE[g]!, CUBE[b]!]
  const luma = Math.round((rgb[0] + rgb[1] + rgb[2]) / 3)
  const gi = Math.max(0, Math.min(23, Math.round((luma - 8) / 10)))
  const gv = 8 + gi * 10
  return dist2(rgb, [gv, gv, gv]) < dist2(rgb, cubeRgb) ? 232 + gi : cube
}
