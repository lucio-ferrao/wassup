import blessed from 'blessed'

/**
 * blessed's Unicode tables date from 2015 and only know double width for CJK blocks: to it any emoji has width 1,
 * the terminal draws it with 2 cells, and from there the grid gets misaligned and garbage shows up. Here blessed's
 * `unicode` module is patched to count emoji-presentation characters as width 2, which is what terminals do
 * (wcwidth). Same for a text pictograph followed by the U+FE0F variation selector ("❤️", "✔️"): the selector asks
 * for the emoji form, which most terminals draw with 2 cells; blessed saw it as a width-0 combining character over
 * a width-1 character, and each one left a column of garbage to the right. Those that draw it with 1 (Apple's
 * Terminal, Termius), measured at startup (term.ts), keep blessed's 1.
 */
interface BlessedUnicode {
  charWidth: (str: string | number, i?: number) => number
  isCombining: (str: string, i: number) => boolean
  chars: { all: RegExp; wide: RegExp; swide: RegExp }
}

const EMOJI_WIDE = /\p{Emoji_Presentation}/u
const PICTOGRAPH = /\p{Extended_Pictographic}/u
const VS16 = '\uFE0F'

/** Whether flags are drawn as such, the terminal giving them 2 columns (term.ts), or as `[PT]` (tameEmoji). */
let realFlags = false
/** Columns the terminal gives a text pictograph with U+FE0F ("❤️"), measured at startup (term.ts); null if unknown. */
let selectorWidth: number | null = null

const isRegional = (s: string, i: number) => { const cp = s.codePointAt(i); return cp != null && cp >= 0x1F1E6 && cp <= 0x1F1FF }

/** Whether the code point at UTF-16 index `i` is the second regional indicator of a flag: one after an odd run of them. */
function flagTail(s: string, i: number): boolean {
  if (!isRegional(s, i)) return false
  let n = 0
  for (let j = i - 2; j >= 0 && isRegional(s, j); j -= 2) n++
  return n % 2 === 1
}

/**
 * `flags`: whether the terminal gives a flag 2 columns, so flags are drawn as such. `selector`: the columns it gives
 * "❤️" (null if it didn't say, taken as 2).
 */
export function patchBlessedUnicode(flags: boolean, selector: number | null) {
  const u = (blessed as unknown as { unicode: BlessedUnicode }).unicode
  if ((u as { _waPatched?: boolean })._waPatched) return
  ;(u as { _waPatched?: boolean })._waPatched = true
  realFlags = flags
  selectorWidth = selector
  const wideSelector = selector !== 1

  const orig = u.charWidth
  u.charWidth = (str, i) => {
    const at = i ?? 0
    const cp = typeof str === 'number' ? str : str.codePointAt(at)
    if (cp == null) return orig.call(u, str, i)
    const c = String.fromCodePoint(cp)
    // blessed's renderer merges the U+FE0F into the previous character's cell, so here it comes right after it.
    // Before the Latin-1 shortcut: "©️" and "®️" are pictographs below U+0100, as wide with the selector as any other.
    if (wideSelector && typeof str !== 'number' && str[at + c.length] === VS16 && PICTOGRAPH.test(c)) return 2
    if (cp <= 0xff) return orig.call(u, str, i)
    // A flag drawn as such has one cell of width 2, the second regional indicator inside the first's (patchRender):
    // the first counts 2, as an emoji, and the second nothing.
    if (realFlags && typeof str !== 'number' && flagTail(str, at)) return 0
    if (EMOJI_WIDE.test(c)) return 2
    return orig.call(u, str, i)
  }

  // chars.all is what parseContent uses to mark the second cell of each wide character. It's rebuilt in `u` mode,
  // with the CJK planes by code point and emoji by property; the U+FE0F stays inside the sequence so the marker
  // falls after it, and so does a flag's second regional indicator. With "❤️" 1 column wide only emoji wide on their
  // own take the second cell, the selector after them or not.
  const flag = flags ? '\\p{Regional_Indicator}{2}|' : ''
  const selected = wideSelector ? '\\p{Extended_Pictographic}' : '\\p{Emoji_Presentation}'
  u.chars.all = new RegExp(`(${flag}${selected}\\uFE0F|\\p{Emoji_Presentation}|[\\u{20000}-\\u{2FFFD}\\u{30000}-\\u{3FFFD}]|${u.chars.wide.source})`, 'gu')
  if (flags) patchRender(u)
  patchWrap(u)
}

/**
 * blessed fits a line to the box's width counting UTF-16 units, the second cell's marker of a wide character
 * included: an emoji outside the basic plane ("😁": two halves and the marker) or a pictograph with U+FE0F ("❤️")
 * counts three for its two cells, a flag drawn as such five, an accent written apart one for none. A line that
 * fills the width with any of them was cut short, and with wrap off its last cells were lost, the time at the end
 * of a message or of the chat list's row among them. `_wrapContent` is rebuilt from its own source so that those
 * units, which take no cell of their own, aren't counted, and those right after the last cell stay on the line.
 */
function patchWrap(u: BlessedUnicode) {
  type Wrap = (this: unknown, content: string, width: number) => unknown
  const proto = (blessed as unknown as { Element: { prototype: { _wrapContent: Wrap } } }).Element.prototype
  const src = proto._wrapContent.toString()
  // Where the count reaches the width, the units with no cell that follow (an accent written apart) stay with it.
  const marker = 'if (!line[i]) break;\n        if (++total === width) {\n          // If we\'re not wrapping the text, we have to finish up the rest of\n          // the control sequences before cutting off the line.\n          i++;'
  if (!src.includes(marker)) throw new Error('blessed: _wrapContent changed; the width patch does not apply')
  const patched = src.replace(marker, marker.replace('if (++total', 'if (uncounted(line, i)) continue;\n        if (++total') + '\n          while (i < line.length && uncounted(line, i)) i++;')
  const uncounted = (s: string, i: number) => {
    const c = s.charCodeAt(i)
    return (c >= 0xDC00 && c <= 0xDFFF) || c === 0xFE0F || (realFlags && flagTail(s, i)) || u.isCombining(s, i)
  }
  proto._wrapContent = new Function('unicode', 'uncounted', `return function ${patched.replace(/^function\s*/, '')}`)(u, uncounted) as Wrap
}

/**
 * blessed's `render` gives each character its own cell, but for combining ones, which it adds to the previous
 * cell. A flag's second regional indicator is added the same way, so the flag is written whole into one cell and
 * the terminal gets the pair together, even when only that cell is redrawn.
 */
function patchRender(u: BlessedUnicode) {
  type Render = (this: unknown) => void
  const b = blessed as unknown as { Element: { prototype: { render: Render; _render: Render } }; colors: unknown }
  const proto = b.Element.prototype
  const src = proto.render.toString()
  const marker = 'if (unicode.combining[point]) {'
  if (!src.includes(marker)) throw new Error('blessed: render changed; the flag patch does not apply')
  const patched = src.replace(marker, 'if (unicode.combining[point] || flagTail(content, ci - 1)) {')
  const render = new Function('unicode', 'colors', 'flagTail', `return ${patched}`)(u, b.colors, flagTail) as Render
  // `_render` is the same function under another name, which the widgets with a render of their own call.
  if (proto._render === proto.render) proto._render = render
  proto.render = render
}

/**
 * Sequences that terminals measure unpredictably (ZWJ, skin tones, flags) are reduced to something of known
 * width: the first emoji in the sequence, the emoji without its tone, and `[PT]` instead of the flag, unless the
 * terminal gives flags 2 columns (term.ts).
 */
export function tameEmoji(s: string): string {
  return s
    .replace(/[\u{1F3FB}-\u{1F3FF}]/gu, '')
    .replace(/(\p{Extended_Pictographic}️?)(?:‍\p{Extended_Pictographic}️?)+/gu, '$1')
    .replace(/\p{Regional_Indicator}\p{Regional_Indicator}/gu, m => realFlags ? m : `[${[...m].map(c => String.fromCodePoint(c.codePointAt(0)! - 0x1F1E6 + 65)).join('')}]`)
}

/** A text pictograph with the U+FE0F variation selector ("❤️", "✔️"): the emoji whose width terminals disagree on. */
const AMBIGUOUS = /^\p{Extended_Pictographic}️$/u

/** blessed's angle table (`screen.js`), which `draw` consults and the module doesn't export. */
const ANGLES: Record<string, boolean> = Object.fromEntries([...'┘┐┌└┼├┤┴┬│─'].map(c => [c, true]))

/**
 * When the terminal didn't say how wide "❤️" is, blessed takes 2 (see above), but some terminals give it 1: the
 * cursor ends up one cell behind where blessed thinks it is, and whatever blessed writes next on the same line, even
 * the space that pads it, lands one column to the left and covers the right half of the emoji. blessed's `draw` is
 * rewritten with a patch: right after one of those emoji the cursor moves, in absolute terms, to the cell blessed
 * assumes, so the cell next to the emoji is never touched, on terminals that measure 1 and on those that measure 2.
 * With the width measured blessed already counts what the terminal draws, and the cursor isn't moved.
 *
 * The same patch writes a space for the marker of a wide character's second cell (U+0003) when that cell comes out
 * on its own: something drawn over the first cell (the big emoji rising, hearts.ts) left the marker without its
 * character, and blessed wrote it as it was. Terminals don't move the cursor for it, so what followed on the line
 * in that frame landed a cell to the left, the time after an emoji eaten, and stayed there.
 */
export function patchBlessedDraw() {
  const Screen = (blessed as unknown as { Screen: { prototype: { draw: (start: number, end: number) => void; _waPatched?: boolean } } }).Screen
  if (Screen.prototype._waPatched) return
  Screen.prototype._waPatched = true
  const src = Screen.prototype.draw.toString()
  const marker = 'out += ch;\n      attr = data;'
  if (!src.includes(marker)) throw new Error('blessed: draw changed; the ambiguous-width emoji patch does not apply')
  const patched = src.replace(marker, 'out += ch === \'\\x03\' ? \' \' : ch;\n      if (ambiguous(ch)) out += this.tput.cup(y, x + 1);\n      attr = data;')
  const u = (blessed as unknown as { unicode: BlessedUnicode }).unicode
  Screen.prototype.draw = new Function('unicode', 'angles', 'ambiguous', `return ${patched}`)(u, ANGLES, (ch: string) => selectorWidth == null && AMBIGUOUS.test(ch))
}
