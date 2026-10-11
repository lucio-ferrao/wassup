/**
 * Text in big letters drawn with the box-drawing characters of the app's name over the chat list (WORDMARK, ui.ts):
 * lowercase, rounded, each letter between the middle of its cells. Four rows, kept only when the text uses them:
 * one for what rises above the small letters (b, d, f, h, k, l, t, the dots of i and j, the digits, the accents),
 * two for the small letters, from the middle of the first (their top) to the middle of the second (the line they
 * sit on), and one for what drops below it (g, j, p, q, y, the cedilla).
 */
const GLYPHS: Record<string, [string, string, string, string]> = {
  a: ['   ', '╭─╮', '╰─┴', '   '],
  b: ['╷  ', '├─╮', '╰─╯', '   '],
  c: ['   ', '╭─╴', '╰─╴', '   '],
  d: ['  ╷', '╭─┤', '╰─╯', '   '],
  e: ['   ', '╭─╮', '╰─╴', '   '],
  f: [' ╭╴', '╶┼╴', ' ╵ ', '   '],
  g: ['   ', '╭─╮', '╰─┤', '╶─╯'],
  h: ['╷  ', '├─╮', '╵ ╵', '   '],
  i: ['·', '╷', '╵', ' '],
  j: [' ·', ' ╷', ' │', '╶╯'],
  k: ['╷  ', '├┬╴', '╵╰╴', '   '],
  l: ['╷ ', '│ ', '╰╴', '  '],
  m: ['     ', '╭─┬─╮', '╵ ╵ ╵', '     '],
  n: ['   ', '╭─╮', '╵ ╵', '   '],
  o: ['   ', '╭─╮', '╰─╯', '   '],
  p: ['   ', '╭─╮', '├─╯', '╵  '],
  q: ['   ', '╭─╮', '╰─┤', '  ╵'],
  r: ['   ', '╭─╴', '╵  ', '   '],
  s: ['   ', '╭─╴', '╶─╯', '   '],
  t: [' ╷ ', '╶┼╴', ' ╰╴', '   '],
  u: ['   ', '╷ ╷', '╰─╯', '   '],
  v: ['   ', '╷╭╯', '╰╯ ', '   '],
  w: ['     ', '╷ ╷ ╷', '╰─┴─╯', '     '],
  x: ['  ', '╮╭', '╯╰', '  '],
  y: ['   ', '╷ ╷', '╰─┤', '╶─╯'],
  z: ['   ', '╶─╮', '╰─╴', '   '],
  ç: ['   ', '╭─╴', '╰┬╴', ' ╯ '],
  '0': ['╭─╮', '│ │', '╰─╯', '   '],
  '1': ['╶╮ ', ' │ ', '╶┴╴', '   '],
  '2': ['╭─╮', '╭─╯', '╰─╴', '   '],
  '3': ['╶─╮', ' ─┤', '╶─╯', '   '],
  '4': ['╷ ╷', '╰─┤', '  ╵', '   '],
  '5': ['╭─╴', '╰─╮', '╶─╯', '   '],
  '6': ['╭─╴', '├─╮', '╰─╯', '   '],
  '7': ['╶─╮', '  │', '  ╵', '   '],
  '8': ['╭─╮', '├─┤', '╰─╯', '   '],
  '9': ['╭─╮', '╰─┤', '╶─╯', '   '],
}

/** The accents, simplified, over the middle of the letter (on an i, in place of its dot): the acute a slash, the grave a backslash. */
const ACCENTS: Record<string, string> = { '\u0301': '/', '\u0300': '\\', '\u0302': '^', '\u0303': '~', '\u0308': '¨' }

/** A letter's big glyph: its own, or its base letter's with the accent over it; none for what has no letter. */
function glyph(c: string): [string, string, string, string] | undefined {
  if (GLYPHS[c]) return GLYPHS[c]
  const [base, mark] = [...c.normalize('NFD')]
  const g = base ? GLYPHS[base] : undefined
  const accent = mark ? ACCENTS[mark] : undefined
  if (!g || !accent) return g
  const mid = Math.floor(g[1].length / 2)
  return [`${' '.repeat(mid)}${accent}`.padEnd(g[0].length), g[1], g[2], g[3]]
}

/** The words of `text` as their letters' big glyphs, lowercase; what has no letter (emoji, punctuation) left out. */
function bigWords(text: string): [string, string, string, string][][] {
  return text.normalize('NFC').toLowerCase().split(/\s+/)
    .map(w => [...w].map(glyph).filter((g): g is [string, string, string, string] => !!g)).filter(w => w.length)
}

/** A word's width in big letters: theirs, a column between each two. */
const wordWidth = (w: [string, string, string, string][]) => w.reduce((n, g) => n + g[0].length + 1, -1)

/**
 * `text` in big letters, as many of its words as fit in `width` columns (three between words), its rows without the
 * empty ones; null when not even the first fits, or there's nothing to draw.
 */
export function bigText(text: string, width: number): string[] | null {
  const words: [string, string, string, string][][] = []
  let used = -3
  for (const w of bigWords(text)) {
    if (used + 3 + wordWidth(w) > width) break
    words.push(w)
    used += 3 + wordWidth(w)
  }
  if (!words.length) return null
  const rows = [0, 1, 2, 3].map(r => words.map(w => w.map(g => g[r]).join(' ')).join('   ').trimEnd())
  return rows.filter((row, r) => row || r === 1 || r === 2)
}
