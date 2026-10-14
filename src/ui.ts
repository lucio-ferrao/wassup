import blessed from 'blessed'
import { spawn } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import QRCode from 'qrcode'
import { store, type ChatRow, type MessageRow, type ProfileRow, type ReactionRow } from './db.js'
import type { Mood } from './mood.js'
import { chatName, contactName, shortName, canonicalJid, thumbPath, previewPath, hasPreviewImage, mediaFile, mediaDir, jidUser, withMentions, typeLabel, pinTarget, type ConnState } from './wa.js'
import { inHerdr, reportHerdr, doneHerdr, titleHerdr, tabNameHerdr, releaseHerdr, openChatHerdr, focusHerdr, focusNextChatHerdr, paneFocusedHerdr } from './herdr.js'
import type { Backend } from './backend.js'
import { waMarkup, clipTagged, esc, colorFor, setTheme, dim, faint, italic, padding, fmtTime, fmtDay, fmtWhen, fmtWhenAt, dayKey, truncate, strWidth, wrapTagged, alignRight, visibleWidth, fold, graphemes, wrapChars } from './format.js'
import { decode, cached, decodeThumb, thumbCached, cellSize, halfBlocks, blockGrid, blockCell, detectImageMode, detectRgb, KittyImages, RgbPainter, type BlockGrid, type Decoded, type ImageMode, type RgbCell } from './image.js'
import { logger, uiLog } from './log.js'
import { linksIn, showLinks, URL_RE } from './links.js'
import { patchBlessedDraw, patchBlessedUnicode, tameEmoji } from './unicode.js'
import type { TermCaps } from './term.js'
import { emojify, emoticonify, completeEmoji, emoticonAt } from './emoji.js'
import { enableKittyKeyboard } from './kittykeys.js'
import { enableBracketedPaste } from './paste.js'
import { Hearts, reaction, emojiOnly, emojiPng } from './hearts.js'
import { SixelImages } from './sixel.js'
import { bigText } from './bigtext.js'
import { t } from './i18n.js'
import { parseHex, mix, nearest256, type Rgb } from './rainbow.js'
import { suggest, describe, llmEnabled, type Suggestion, type Fix } from './llm.js'
import { spellFixes } from './spell.js'
import { patchBlessedItalic } from './italic.js'

/** WhatsApp Web's quick reactions, in its order, plus "⋯" for typing any other. */
const QUICK = ['👍', '❤️', '😂', '😮', '😢', '🙏', '⋯']

type Focus = 'picker' | 'messages' | 'input'

/** An image in the panel: ready (with pixels) or just reserved, waiting to be downloaded and decoded once it becomes visible. */
interface ImageSlot { row: MessageRow; origLine: number; cols: number; rows: number; pad: number; path?: string; d?: Decoded; src?: string }

/** What each terminal keeps in `state`: its tabs, the process that holds them, and the last interaction. */
interface TerminalState { tabs: string[]; active: number; pid?: number; lastActive?: number; herdrTab?: string; herdrPane?: string }

/**
 * Whether a process is still alive: signal 0 only asks whether it exists (EPERM: it does, another user's). On Linux
 * a zombie, ended but not yet reaped, still answers it, and /proc says it's gone; elsewhere (macOS) there's no /proc.
 */
function pidAlive(pid: number): boolean {
  try { process.kill(pid, 0) } catch (e) { if ((e as NodeJS.ErrnoException).code !== 'EPERM') return false }
  try {
    const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8')
    return stat.charAt(stat.lastIndexOf(')') + 2) !== 'Z'
  } catch { return true }
}

/** A tab on the bar under the input: which one it is and its columns. */
interface TabSegment { x0: number; x1: number; index: number }

const num = (x: unknown): number => x as number

// Internal blessed fields the UI uses: the lines already wrapped to the panel width, the original ones (`fake`, tags
// already made SGR) and the maps between original line and drawn line (ftor: original→drawn, rtof: drawn→original).
interface ClinesBox extends blessed.Widgets.BoxElement {
  _clines: string[] & { ftor: number[][]; rtof: number[]; fake: string[] }
  childBase: number
}

/** The model's suggestion in view: the letters missing from the half-typed word, the right word, or a correction. */
type GhostView = { kind: 'suffix' | 'word'; text: string; word: { from: string; to: string } } | { kind: 'fix'; text: string; fix: Fix }
/** The app's name, over the chat list, in the rules' thin lines with round corners, like speech bubbles. */
const APP = 'wassup'
const WORDMARK = [
  '╷ ╷ ╷ ╭─╮ ╭─╴ ╭─╴ ╷ ╷ ╭─╮',
  '╰─┴─╯ ╰─┴ ╶─╯ ╶─╯ ╰─╯ ├─╯',
  '                      ╵',
]
/** Someone typing, where a chat's state is (its tab, its row) or in the window title: two emoji taking turns every 500 ms. */
/**
 * What a space does with emoji suggestions open, learnt for each kind from whether they were taken the last two times
 * (noteSuggest): in "text" it goes in and closes them, what was typed staying; in "emoji" it takes the chosen one.
 * Kept in the state table for every terminal, under the kind: the mode and the last two times, true where taken.
 */
type SuggestKind = 'smileys' | 'emojiNames'
type SuggestMode = { mode: 'text' | 'emoji'; last: boolean[] }
const typingEmoji = () => (Math.floor(Date.now() / 500) % 2 ? '💭' : '💬')
/**
 * The time of a message of mine, carrying its state in the separator: "14 06" waiting to leave, "14.06" sent (one
 * dot), "14:06" delivered or read (two). Which was read the "❮" after the time of the last one says (renderMessages).
 */
const myTime = (ts: number, status: number) => fmtTime(ts).replace(':', status >= 3 ? ':' : status >= 2 ? '.' : ' ')
/** A chat's state (moods) shows only while its last message is at most this old, in seconds: a week. */
const MOOD_AGE = 7 * 24 * 3600
/** How many of a chat's latest messages the panel draws at first, and how many more each scroll past the top adds. */
const PAGE = 300
/**
 * The country calling code a phone number starts with. The codes are prefix-free (E.164), and only these take one or
 * two digits: any other is three.
 */
const CALLING_CODE = /^(?:1|7|2[07]|3[0-469]|4[013-9]|5[1-8]|6[0-6]|8[1246]|9[0-58])/
const callingCode = (digits: string) => CALLING_CODE.exec(digits)?.[0] ?? digits.slice(0, 3)
/** Frames, 40 ms apart, of the flight of a message just sent from where it was written to its place (flyDraft). */
const FLY_FRAMES = 5
/** How long, in seconds, WhatsApp lets one of my messages be deleted for everyone: a little over two days, kept short of it. */
const DELETE_WINDOW = 2 * 24 * 3600
/** How long, in seconds, WhatsApp lets a message be edited after it was sent. */
const EDIT_WINDOW = 15 * 60
/** A message's text without the "(editada)" line an edit leaves at its end. */
const bareText = (row: MessageRow) => row.text.replace(/\n\((editada|edited)\)$/, '')
/** The wrong passages of a text between private-use marks, made the yellow underline once the text is markup. */
function markFixes(text: string, fixes: Fix[]): string {
  let out = '', at = 0
  for (const f of [...fixes].sort((a, b) => a.start - b.start)) {
    if (f.start < at || text.slice(f.start, f.end) !== f.from) continue
    out += `${text.slice(at, f.start)}\uE000${f.from}\uE001`
    at = f.end
  }
  return out + text.slice(at)
}
/** The characters of `text` (as `[...text]`) the folded `words` cover wherever they occur, accents and case aside. */
function matchedChars(text: string, words: string[]): Set<number> {
  let flat = ''
  const owner: number[] = []
  ;[...text].forEach((ch, i) => { for (const f of fold(ch)) { flat += f; owner.push(i) } })
  const marked = new Set<number>()
  for (const w of words) {
    if (w) for (let at = flat.indexOf(w); at >= 0; at = flat.indexOf(w, at + 1)) for (let k = at; k < at + w.length; k++) marked.add(owner[k]!)
  }
  return marked
}
/**
 * The search's words in a message's raw text between private-use marks, made reverse video once the text is markup;
 * an accent written apart goes with its letter, and links are left alone (shortened for showing, they'd be mangled).
 */
function markSearch(text: string, words: string[]): string {
  const chars = [...text], marked = matchedChars(text, words)
  for (const m of text.matchAll(URL_RE)) {
    const a = [...text.slice(0, m.index)].length
    for (let i = a; i < a + [...m[0]].length; i++) marked.delete(i)
  }
  let out = '', on = false
  chars.forEach((ch, i) => {
    const want = marked.has(i) || (on && fold(ch) === '')
    if (want !== on) { out += want ? '\uE002' : '\uE003'; on = want }
    out += ch
  })
  return on ? `${out}\uE003` : out
}
/** The notice for a message in another chat: time to appear, stay, and disappear, in milliseconds. */
const NOTICE = { fadeIn: 400, hold: 6000, fadeOut: 800 }

const HELP = t('help')
/** Lines of the emoji list in view; ↑/↓ scroll through the rest. */
const SUGGEST_ROWS = 5

// Terminal theme colors, never assumed: default foreground and background, and the 16 named ones, which the theme
// guarantees are readable over its background. Transient notices are discreet; only waiting for the QR code and
// connection drops stand out. Connected is not shown.
const FG = { tab: 'default', badge: 'red', warn: 'yellow', error: 'red' }

/** Gray from the 256-color ramp (232..255, from #080808 to #eeeeee in steps of 10) closest to a given luminance. */
function gray256(luma: number): number {
  return 232 + Math.max(0, Math.min(23, Math.round((luma - 8) / 10)))
}

/**
 * What's derived from the terminal's real background (OSC 11): whether the theme is dark, and the gray for the
 * selected-message highlight, moved away from its luminance toward the light side on a dark theme and toward the
 * dark side on a light one. With no response, dark is assumed and the highlight is a medium gray, readable with
 * either light or dark text.
 */
function theme(bg: string | null): { dark: boolean; selected: number } {
  const m = bg && /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(bg)
  if (!m) return { dark: true, selected: 240 }
  const luma = 0.299 * parseInt(m[1]!, 16) + 0.587 * parseInt(m[2]!, 16) + 0.114 * parseInt(m[3]!, 16)
  const dark = luma < 128
  return { dark, selected: gray256(luma + (dark ? 48 : -48)) }
}

/**
 * A key applied to a text with a cursor (in graphemes): arrows, Home/End, Backspace/Delete, Ctrl-U (everything),
 * Shift+Backspace (previous word, only on terminals with the Kitty keyboard protocol), and typed characters,
 * inserted at the cursor, with :codes: swapped for the emoji as soon as they're complete. Returns null
 * if the key isn't an editing key.
 */
function edit(value: string, cursor: number, k: string, ch: string, key: blessed.Widgets.Events.IKeyEventArg): { value: string; cursor: number } | null {
  const chars = graphemes(value)
  const at = Math.min(cursor, chars.length)
  const join = (before: string[], after: string[]) => ({ value: before.join('') + after.join(''), cursor: before.length })
  if (k === 'left') return { value, cursor: Math.max(0, at - 1) }
  if (k === 'right') return { value, cursor: Math.min(chars.length, at + 1) }
  if (k === 'home') return { value, cursor: 0 }
  if (k === 'end') return { value, cursor: chars.length }
  if (k === 'backspace') return join(chars.slice(0, Math.max(0, at - 1)), chars.slice(at))
  if (k === 'delete') return join(chars.slice(0, at), chars.slice(at + 1))
  if (k === 'C-u') return join([], [])
  if (k === 'S-backspace') return join(graphemes(chars.slice(0, at).join('').replace(/\S*\s*$/, '')), chars.slice(at))
  if (ch && !key.ctrl && !key.meta && ch >= ' ' && ch !== '\x7f') {
    // On closing a :code:, the text is swapped for the emoji right away.
    const before = chars.slice(0, at).join('') + ch
    return join(graphemes(ch === ':' ? emojify(before) : before), chars.slice(at))
  }
  return null
}



export class Ui {
  private screen: blessed.Widgets.Screen
  private tabsBar: blessed.Widgets.BoxElement
  private msgBox: ClinesBox
  /** The open chat's name over its messages (drawChatHead), and the rows it takes, which the messages start under. */
  private chatHead!: blessed.Widgets.BoxElement
  private headRows = 0
  private input: blessed.Widgets.BoxElement
  private picker: blessed.Widgets.ListElement
  /** Notice of a new message in another chat: a line with a background, sitting over its tab in the bar. */
  private toast: blessed.Widgets.BoxElement
  /**
   * Emoji suggestions for the :prefix or the smiley before the cursor: the box above the input, the options (each
   * with what it shows beside the emoji and the length of the text it replaces), and the chosen one.
   */
  private suggest: blessed.Widgets.BoxElement
  private suggestions: { emoji: string; code: string; length: number }[] = []
  private suggestIndex = 0
  /** The first option in view: the list scrolls to keep the chosen one in it. */
  private suggestTop = 0
  /** Whether the suggestions are for a smiley: Enter then sends it as typed, and only Tab or → swap it. */
  private suggestFace = false
  /**
   * Which suggestions a space may take, learnt apart: a smiley's emoji (":)"), or an emoji by its name once a
   * character follows the colon (":smi"); none for the basic smileys of a bare ":" or for mentions.
   */
  private suggestKind: SuggestKind | null = null

  /** Local model suggestion for the text `text` (continuation or correction), requested 150 ms after the last keystroke and shown for 4 s. */
  private ghost: { text: string; s: Suggestion } | undefined
  /** What floats above the input (its key: a correction's place and text, or the word), and the one whose 10 s ran out. */
  private floatKey = ''
  private floatOff = ''
  /** The correction floating right above the word it replaces. */
  private ghostBox!: blessed.Widgets.BoxElement
  /** The faint rule over the plain line (the chat list's filter, a search), with the search's count (drawBorder). */
  private ruleTop!: blessed.Widgets.BoxElement
  private ruleChar = '─'
  /**
   * The terminal draws text bigger itself (OSC 66, probed: TermCaps.textSizing): emoji on their own go twice their
   * size, as they do, in pictures, where it shows Kitty's graphics instead (emojiImages).
   */
  private textSizing = false
  /** The emoji's pictures for Kitty's graphics (emojiPng), by emoji: null where it can't be drawn, absent while drawn. */
  private emojiImages = new Map<string, Decoded | null>()
  private emojiDrawing = new Set<string>()
  /**
   * The messages of emoji on their own drawn twice their size (renderMessages): the first of their two blank lines, its
   * column, and the emoji; and those written on the terminal last, at which cell (paintBigEmoji).
   */
  private bigEmojis: { line: number; col: number; emoji: string[] }[] = []
  private bigShown: { x: number; y: number; w: number; key: string }[] = []
  /** What the rule says over a search's line: its count (drawInput). */
  private borderHeader: string | null = null
  /**
   * Where "↓", with how many messages are below, starts beside the band while they're scrolled up, and on which of the
   * input's rows: a click there goes down to the latest.
   */
  private borderBelow = Infinity
  private belowRow = -1
  /** How many messages below the view "↓" said last (0 for none): a scroll or a frame redraws the input only when it changes. */
  private belowShown = 0
  /** The rows at the bottom the layout was last set for (resizeInput). */
  private laidBottom = 0
  /** The rows at the top of the input that say what's being replied to, reacted to or edited: one, or none. */
  private headerRows = 0
  /** The message just sent, flying from where it was written to where it lands among the messages (flyDraft). */
  private flyBox!: blessed.Widgets.BoxElement
  private flight: { timer: NodeJS.Timeout; text: string; frame: number; arrived: boolean; room: number } | undefined
  private ghostTimer: NodeJS.Timeout | undefined
  /** The corrections found in my recent messages, by chat, id and text: null while being checked, or with none. */
  private sentFixes = new Map<string, Fix[] | null>()
  private sentQueue: MessageRow[] = []
  private sentChecking = false
  /** Redraws the messages when the first of my marked ones leaves the editing window, taking its marks away. */
  private sentExpiry: NodeJS.Timeout | undefined
  /** The correction shown over one of my messages (a click on its marked passage), and the box it floats in. */
  private sentFix: { row: MessageRow; fix: Fix } | undefined
  private fixBox!: blessed.Widgets.BoxElement
  /**
   * The search in the open chat (Ctrl+F): what's typed and its words, folded, the messages found (the most recent
   * first) and which of them is selected.
   */
  private search: { query: string; cursor: number; words: string[]; hits: MessageRow[]; at: number } | null = null
  /**
   * The list searching messages instead of names (Ctrl+F in it): the messages found for its text (searchAll), the row
   * each one starts on, and the rows that continue one, with the row it starts on.
   */
  private pickerSearch = false
  private pickerHits = new Map<number, MessageRow>()
  private pickerCont = new Map<number, number>()
  /**
   * The list's rows as set, each row that continues another in its parts, and whether the selection's background goes
   * over each (not over a photo's lower half); and the rows that have it now.
   */
  private pickerItems: string[] = []
  private pickerParts = new Map<number, [text: string, lit: boolean][]>()
  /**
   * Each first row's text and its time apart, so the selection's green covers the text and leaves the time out, as a
   * bubble does (selectedRow); and the columns the time takes at the right end, a cell of gap before it: just the
   * time, or the day too with a filter or messages found (pickerBubbleEnd).
   */
  private pickerHeads = new Map<number, [body: string, time: string]>()
  private pickerTimeCols = 5
  private pickerLit: number[] = []
  /**
   * My jid as the last connection gave it, kept between runs: the chat list writes numbers by its country code
   * (chatNumber) from the first frame, before the server's answer says it again (onConnection).
   */
  private myJid = store.getState<string>('me') ?? ''
  /** What WhatsApp says of each chat (Wa.ensureProfile), the chats already asked for, and each row's decoded picture. */
  private profiles = new Map<string, ProfileRow>()
  /** Each chat's state, as the emoji the server worked out from its messages (mood.ts), shown after its name. */
  private moods = this.loadMoods()
  /** The chats whose number is a WhatsApp Business account, marked 💼 after it (chatNumber). */
  private business = this.loadBusiness()
  private profileAsked = new Set<string>()
  private pickerAvatars = new Map<number, { path: string; d: Decoded }>()
  /** The photos whose small copy is being decoded (loadVisibleProfiles). */
  private thumbWaiting = new Set<string>()
  /**
   * The chats' photos in their tabs, before the names, two cells by one, placed with Kitty (placeImages) or drawn in
   * half-blocks (repainted in their exact colours by paintRgb): each one's chat, small copy's key, decoded image and
   * column on the tab bar.
   */
  private tabPhotos: { jid: string; path: string; d: Decoded; x: number }[] = []
  /** The last press on the list, and whether it was on a chat's photo (see the list's 'select'). */
  private pickerPress: { at: number; photo: boolean } | undefined
  private pickerFound: { query: string; rows: MessageRow[] } | undefined
  private ghostAbort: AbortController | undefined
  /** Right-arrow presses with the next suggestion still on the way: accepted on arrival, one per arrow, so → → → correct in a chain. */
  private acceptOnArrival = 0
  private ghostHide: NodeJS.Timeout | undefined
  /** The text as it was left after accepting a suggestion that ended mid-word: the next letter gets a space before it. */
  private accepted: string | undefined
  /** The notice for a message in another chat and the moment it started appearing; the clock animates it until it's gone. */
  private notice: { jid: string; text: string; since: number } | undefined
  private noticeTimer: NodeJS.Timeout | undefined

  private tabs: string[] = []
  private active = -1
  private segments: TabSegment[] = []
  /**
   * The tab under the pointer (-1 the chat list's own, null none), which shows a ✖ to close it in its state's place,
   * and where that ✖ is.
   */
  private tabHover: number | null = null
  private tabCloses: { x: number; index: number }[] = []
  private pickerOpen = false
  private chats: ChatRow[] = []
  private filtered: ChatRow[] = []
  /** The list's rows: a chat, or null for a day separator (only without a filter), which the selection skips. */
  private pickerSlots: (ChatRow | null)[] = []
  /** Each listed chat's last message, read when the list is built, so the live parts (typing, online) redraw cheaply. */
  private pickerLast = new Map<string, MessageRow | undefined>()
  /** The filter's words, folded, to underline them in the names. */
  private pickerWords: string[] = []
  /** The row selected before, to know which way the selection was going when it lands on a separator. */
  private pickerAt = -1
  /**
   * Whether the list's selection shows without a filter or a search: the list opens on the chat you were in, back
   * within 30 s, or once the keys move it. Otherwise no chat shows as selected until a filter or a search picks one
   * (pickerChosen), and Enter, Tab and → open nothing.
   */
  private pickerPicked = false
  private get pickerChosen(): boolean { return this.pickerPicked || !!this.filter || this.pickerSearch }
  /** The chat to select when the list opens (openPicker): the one in front, or null for the most recent. */
  private pickerFocus: string | null | undefined
  /** When the list was last closed, and the chat selected then with its row on the screen (from the list's top). */
  private pickerClosedAt = 0
  private pickerLeft: { jid: string; row: number } | undefined
  /** The row on the screen the chat to select goes back to (openPicker), when it's the one the list was left on. */
  private pickerFocusRow: number | undefined
  /** Over the list, always: the app's name, big (or on one line without UTF-8), and the counts. */
  private pickerHead!: blessed.Widgets.BoxElement
  /** WhatsApp's green, in the palette: the app's name and the unread counts in the list. */
  private green = 0
  private filter = ''
  private pickerFilterShown: string | undefined
  private focus: Focus = 'input'
  private inputValue = ''
  /** Cursor position in the input and in the chat filter, in graphemes. */
  private cursor = 0
  private filterCursor = 0
  /** Layout of the input on the last draw, used to map clicks: grapheme lines and the first visible line. */
  private inputLines: string[][] = [[]]
  private inputTop = 0
  private disableKittyKeyboard?: () => void
  private disablePaste: () => void
  private hearts: Hearts
  /** The message being dragged right to reply to it, and by how many columns. */
  private drag: { id: string; dx: number } | undefined
  /** Chats whose Herdr tab was requested recently and may not be registered yet. */
  private spawning = new Set<string>()
  private lineMap: (MessageRow | null)[] = []
  /** Content lines (indices in `lineMap`) holding a message's name and time: the only ones a drag replies from. */
  private headerLines = new Set<number>()
  /**
   * Content lines holding a message's own text (or caption), with the columns (within the panel, end exclusive)
   * the text occupies: the only cells the text selection takes. The last one also carries the time, after the text.
   */
  private textLines = new Map<number, { start: number; end: number }>()
  /** Lines carrying a group member's name, by original line: who it is and how many columns the name takes. */
  private nameLines = new Map<number, { jid: string; start: number; width: number }>()
  /** Per message line, the mentions on it (columns [start, end) and whose), which a click opens the chat of. */
  private mentionLines = new Map<number, { start: number; end: number; jid: string }[]>()
  /**
   * Text being selected with the mouse: the cell pressed (`ax`, `ay`) and the one the pointer is at (`hx`, `hy`),
   * swept in reading order within the columns of the panel it started in (`xi`..`xl`). Copied to the clipboard on
   * release and kept highlighted until the next click or key.
   */
  private textSel: { ax: number; ay: number; hx: number; hy: number; xi: number; xl: number; input: boolean } | undefined
  /** The terminal reports the pointer's movement with no button held (mode 1003): only then is there a hover. */
  private anyMotion: boolean
  /** Message under the pointer: its time line gets a "☺" that opens the quick reactions. */
  private hover: MessageRow | undefined
  /** Message whose quick-reaction bar is open, after a click on its "☺". */
  private quickFor: MessageRow | undefined
  /** Where the "☺" or the bar were last drawn, so the click can find them. */
  private quickHit: { y: number; icon?: number; items?: { x: number; w: number; emoji: string }[] } | undefined
  /** Where the pointer is (terminals that report motion): the reaction under it is drawn highlighted. */
  private pointer: { x: number; y: number } | undefined
  /** Drawn messages, in order; the selected one (click or arrows in the panel) and the one being replied to or reacted to. */
  private rows: MessageRow[] = []
  /** Per chat, how many of its latest messages the panel draws (-1: all of them); leaving the chat resets it. */
  private shown = new Map<string, number>()
  /** Chats whose older history is being asked of the phone, and those the phone has nothing older for. */
  private olderPending = new Set<string>()
  private olderDone = new Set<string>()
  /** The chat the panel last drew, so a redraw of the same one can keep the view where it was. */
  private renderedJid: string | undefined
  /** Whether that drawing had the line saying older messages are on their way. */
  private loadingDrawn = false
  private selected: MessageRow | null = null
  private replyTo: MessageRow | null = null
  /** My own message open in the input for editing (Backspace or Delete with an empty line). */
  private editing: MessageRow | null = null
  /** The selected message waiting for a second Delete (or Ctrl+D) to be deleted, and whether for everyone. */
  private deleting: { row: MessageRow; forEveryone: boolean } | null = null
  /** What's left unsent in each chat: switching tabs swaps the input, so nothing goes to the wrong person. */
  private drafts = new Map<string, { value: string; cursor: number }>()
  private reactTo: MessageRow | null = null
  /** Columns before the text on each of the input's lines: the panel's padding and the bubble's spare column, or a prompt. */
  private promptWidth = 2
  private images: ImageSlot[] = []
  /** What the model says each image shows (by chat and message), null when it gave nothing; the one on its way. */
  private descriptions = new Map<string, string | null>()
  private describing: { key: string; abort: AbortController } | undefined
  /**
   * What the popup shows (a click on it): a message's picture or a chat's photo, its file, the key its description goes
   * by; and where the picture went on the screen (0-based).
   */
  private viewing: { key: string; path: string; row?: MessageRow } | null = null
  private viewerBox: blessed.Widgets.BoxElement
  private viewerImg: { d: Decoded; path: string; x: number; y: number; cols: number; rows: number } | undefined
  private mode: ImageMode
  /** The pictures drawn in real pixels, over blank cells: Kitty's placements or sixels; none in half-blocks. */
  private pics: KittyImages | SixelImages | undefined
  /** Once the terminal stops being resized, the images are sent to it again (see the 'resize' handler). */
  private resendTimer: NodeJS.Timeout | undefined
  /** Half-block images and message bubbles repainted in 24-bit colour, when the terminal takes it. */
  private rgbPaint: RgbPainter | undefined
  private connText = t('connecting')
  private transient = ''
  private transientTimer: NodeJS.Timeout | undefined
  private atBottom = true
  private renderTimer: NodeJS.Timeout | undefined
  /** Chats where someone is typing, and the clock that turns their emoji (typingEmoji) while there's any. */
  private typing = new Set<string>()
  /** One-to-one chats whose person is online right now, for the prompt's mark. */
  private online = new Set<string>()
  /** Whether this device shows as online to the others, which the rule under the input says discreetly. */
  private available = false
  private typingTimer: NodeJS.Timeout | undefined
  private fgRgb: Rgb
  private bgRgb: Rgb
  private dirtyTabs = true
  private dirtyMessages = true
  private showingQr = false

  /** Whether the terminal background is dark (decides the strong color of the active tab and of names) and the gray for the selected message. */
  private dark: boolean
  private selectedBg: number
  /**
   * Message bubbles' backgrounds. blessed draws them in two greys of the 256-colour palette, mine never the more
   * intense of the two, which is what shows without 24-bit colour; with it, the painter repaints the cells in
   * those greys with `bubbleRgb`: WhatsApp Web's colours, mine on a dark theme toned down to the luminance of theirs
   * (its #005c4b stands out far more than their #202c33).
   */
  private bubbleBg = { mine: 0, theirs: 0 }
  private bubbleRgb: { mine: string; theirs: string } | null = null

  /**
   * `wa ema`, or any startup inside Herdr: only one chat at a time. No tabs (in Herdr, the tabs are its own) and no
   * notices or state from the others; the picker switches it.
   */
  private get fixed(): boolean { return !!this.wanted || inHerdr }
  /**
   * Input lines: two at minimum for the text being written in a chat (one for the list's filter or a search), growing
   * with the text up to half the screen.
   */
  private inputRows = 1
  /** The input's plain line, for a search in a chat, under a rule (ruleTop); otherwise the band, the list's filter too. */
  private get plainInput(): boolean { return !this.pickerOpen && (!!this.search || !this.current) }
  /** Lines occupied at the bottom: the input's, the tab bar under it, and the rule over the plain line. */
  private get bottom(): number { return this.inputRows + 1 + (this.plainInput ? 1 : 0) }
  /** Lines occupied at the top: the tab bar (1), which doesn't exist in single-chat mode. */

  constructor(private wa: Backend, caps: TermCaps, private wanted?: string) {
    this.mode = detectImageMode(caps.kittyGraphics, inHerdr, caps.sixel && !!caps.cellPx)
    ;({ dark: this.dark, selected: this.selectedBg } = theme(caps.bg))
    // WhatsApp Web's bubbles: on a dark theme #005c4b mine and #202c33 theirs, on a light one #d9fdd3 and #ffffff.
    // Mine a step quieter than theirs: darker on a dark theme, lighter on a light one; never the selection's grey,
    // which the painter would take for a bubble.
    const offSelected = (n: number) => (n === this.selectedBg ? n - 2 : n)
    this.bubbleBg = this.dark ? { mine: offSelected(235), theirs: offSelected(236) } : { mine: offSelected(255), theirs: offSelected(254) }
    this.green = nearest256(this.dark ? [0x25, 0xd3, 0x66] : [0x00, 0x80, 0x69])
    this.fgRgb = parseHex(caps.fg) ?? (this.dark ? [192, 192, 192] : [48, 48, 48])
    this.bgRgb = parseHex(caps.bg) ?? (this.dark ? [0, 0, 0] : [255, 255, 255])
    setTheme(this.dark)
    patchBlessedUnicode(caps.flagWidth === 2, caps.selectorNarrow)
    this.screen = blessed.screen({ smartCSR: true, fullUnicode: caps.utf8, title: 'wassup', warnings: false })
    // Each patch rebuilds `draw` from the source of the one before, so the italic one, which only knows blessed's own
    // variables, goes first; the wide-emoji one then adds its own on top.
    patchBlessedItalic(this.screen); patchBlessedDraw()
    // With a UTF-8 locale, frames come out in Unicode box-drawing characters (─│┌). Without this, blessed switches to
    // the DEC line-drawing set, which SSH apps on phones don't know and show as q, x, l, k.
    if (caps.utf8) (this.screen.program as unknown as { tput: { brokenACS: boolean } }).tput.brokenACS = true
    const program = this.screen.program as unknown as { _write: (s: string) => void }
    if (this.mode === 'kitty') this.pics = new KittyImages(s => program._write(s))
    // Sixels need the cells' size in pixels; forced (WA_IMAGES) where the terminal didn't say, a common one.
    else if (this.mode === 'sixel') this.pics = new SixelImages(s => program._write(s), caps.cellPx ?? { w: 10, h: 20 }, () => this.scheduleRender())
    // Focus events (DECSET 1004): a terminal that knows them says when this window or pane gains or loses the
    // focus, which decides whether activity here keeps this device online; one that doesn't simply ignores it.
    program._write('\x1b[?1004h')
    const focusChanged = (focused: boolean) => {
      this.hasFocus = focused
      // Back in front, the next key or mouse counts at once rather than up to ten seconds later.
      if (!focused) this.lastPresenceTouch = 0
      this.wa.setFocus(process.pid, focused)
      // Coming to the front is seeing the chat: what arrived meanwhile is read, and in Herdr the pane stops asking
      // for attention.
      if (focused && this.current) this.wa.markRead(this.current).catch(e => logger.warn({ e }, 'markRead'))
    }
    this.screen.program.on('focus', () => focusChanged(true))
    this.screen.program.on('blur', () => focusChanged(false))
    // In Herdr the events only come with a change, so the pane's state at start is asked of Herdr.
    paneFocusedHerdr().then(f => { if (f !== undefined && this.hasFocus === undefined) focusChanged(f) }).catch(() => {})
    if (detectRgb(caps.truecolor)) {
      this.rgbPaint = new RgbPainter(s => program._write(s))
      const lum = ([r, g, b]: Rgb) => 0.2126 * r + 0.7152 * g + 0.0722 * b
      const theirs: Rgb = this.dark ? [0x20, 0x2c, 0x33] : [0xff, 0xff, 0xff]
      const green: Rgb = this.dark ? [0x00, 0x5c, 0x4b] : [0xd9, 0xfd, 0xd3]
      const k = this.dark ? Math.min(1, lum(theirs) / lum(green)) : 1
      const mine = green.map(v => Math.round(v * k)) as Rgb
      this.bubbleRgb = { mine: mine.join(';'), theirs: theirs.join(';') }
    }
    // Only with the terminal confirming the protocol: it's what lets Shift+Backspace be distinguished, for deleting words.
    if (caps.kittyKeyboard) this.disableKittyKeyboard = enableKittyKeyboard((this.screen.program as unknown as { input: Parameters<typeof enableKittyKeyboard>[0] }).input, s => program._write(s))
    // Outside the Kitty translator: pasted text doesn't go through it.
    this.disablePaste = enableBracketedPaste((this.screen.program as unknown as { input: Parameters<typeof enableBracketedPaste>[0] }).input, s => program._write(s))
    logger.info({ caps, images: this.mode, dark: this.dark, term: process.env.TERM }, 'terminal')

    // Layout: the chat's name at the top (drawChatHead), messages at full width under it, and under them, as my next
    // message, the text being written on my bubbles' green, growing upwards with it; under that, the tab bar, with
    // the status on the right.
    this.tabsBar = blessed.box({
      parent: this.screen, top: '100%-1', left: 0, width: '100%', height: 1, tags: true, mouse: true,
    })
    this.msgBox = blessed.box({
      parent: this.screen, top: 0, left: 0, right: 0, height: `100%-${this.bottom}`, padding: { left: 1 },
      // The lines arrive wrapped to the panel's width already (blessed only wraps from the left, and measures emoji
      // one unit too long); left to itself it would still cut a line that reaches the edge at its last word.
      tags: true, wrap: false, scrollable: true, alwaysScroll: true, mouse: true,
    }) as ClinesBox
    this.chatHead = blessed.box({ parent: this.screen, top: 0, left: 0, right: 0, height: 1, padding: { left: 1 }, tags: true, wrap: false, hidden: true })
    this.input = blessed.box({
      parent: this.screen, top: `100%-${this.inputRows + 1}`, left: 0, right: 0, height: this.inputRows,
      // Each line is laid out to its width already, the bubble's to the end: blessed is left to wrap nothing.
      tags: true, mouse: true, wrap: false,
    })
    // Box-drawing only with a UTF-8 locale, like the frames; otherwise plain dashes.
    this.ruleChar = caps.utf8 ? '─' : '-'
    this.textSizing = caps.textSizing
    // No wrapping: the rule fills its width, and blessed would break it at a space.
    this.ruleTop = blessed.box({ parent: this.screen, top: `100%-${this.inputRows + 2}`, left: 0, right: 0, height: 1, tags: true, wrap: false })
    this.picker = blessed.list({
      parent: this.screen, top: 0, left: 0, right: 0, height: `100%-${this.bottom + 1}`, padding: { left: 1 }, hidden: true,
      // Its own colours kept on the selected row (blessed would force the selection's onto every cell, a photo's
      // half-blocks too, whose top halves then came out white): the selected row is given without them instead,
      // but for the photo's (selectedRow).
      tags: true, keys: true, mouse: true, invertSelected: false,
      // The selected chat is marked as the active tab is, bold and in the theme's strongest color, on my bubbles' green
      // up to its time, which stays out as a message's does (selectedRow, litRow; repainted in WhatsApp Web's colour
      // where the terminal takes 24-bit colour).
      // With no chat chosen yet (pickerChosen), the selected row looks as the others do.
      style: {
        selected: { bold: () => this.pickerChosen, fg: () => (this.pickerChosen ? (this.dark ? 'bright-white' : 'black') : undefined) },
      } as unknown as blessed.Widgets.ListElementStyle,
    })
    // Rows aren't wrapped, as the messages' aren't: a row is one line, cut at the edge should it ever run past it.
    // Set as each row is made, before blessed first lays out its content.
    const list = this.picker as unknown as { createItem: (content: string) => { wrap: boolean } }
    const createItem = list.createItem.bind(list)
    list.createItem = content => { const item = createItem(content); item.wrap = false; return item }
    // With no chat chosen, the keys' first move (↑/↓, PgUp/PgDn) shows the selection where it already is, on the most
    // recent chat; only the next ones move it. Enter opens nothing before.
    const keyed = this.picker as unknown as { move: (n: number) => void; enterSelected: () => void }
    const move = keyed.move.bind(keyed), enterSelected = keyed.enterSelected.bind(keyed)
    keyed.move = n => { if (this.pickerChosen) return move(n); this.pickerPicked = true; this.lightPickerRows(this.pickerAt) }
    keyed.enterSelected = () => { if (this.pickerChosen) enterSelected() }
    this.pickerHead = blessed.box({ parent: this.screen, top: 0, left: 0, right: 0, height: 1, padding: { left: 1 }, tags: true, wrap: false, hidden: true })
    // Floating over the messages (status at the top right); created last so it stays on top.
    this.toast = blessed.box({ parent: this.screen, top: 0, left: 0, width: 1, height: 1, tags: true, hidden: true })
    this.flyBox = blessed.box({ parent: this.screen, top: 0, left: 0, width: 1, height: 1, tags: true, hidden: true, wrap: false })
    // Emoji suggestions, above the input and over the messages, with the highlight background to stand out.
    this.suggest = blessed.box({
      parent: this.screen, top: '100%-4', left: 0, width: 1, height: 1, tags: true, hidden: true, padding: { left: 1 }, wrap: false, mouse: true,
      style: { bg: this.selectedBg } as unknown as blessed.Widgets.Types.TStyle,
    })
    // The model's correction, floating one line above the word it replaces, with the same background.
    this.ghostBox = blessed.box({
      parent: this.screen, top: 0, left: 0, width: 1, height: 1, tags: true, hidden: true, wrap: false,
      style: { bg: this.selectedBg } as unknown as blessed.Widgets.Types.TStyle,
    })
    // A click on the correction floating above a word takes it, as Tab or → do.
    this.ghostBox.on('click', () => this.acceptGhost())
    // The same over a passage of one of my messages, where a click edits the message with it.
    this.fixBox = blessed.box({
      parent: this.screen, top: 0, left: 0, width: 1, height: 1, tags: true, hidden: true, wrap: false,
      style: { bg: this.selectedBg } as unknown as blessed.Widgets.Types.TStyle,
    })
    this.fixBox.on('click', () => this.applySentFix())
    // A click on a line of the emoji list selects that one.
    this.suggest.on('click', (data: { x: number; y: number }) => {
      const row = data.y - num(this.suggest.atop)
      if (row < 0 || this.suggestTop + row >= this.suggestions.length) return
      this.suggestIndex = this.suggestTop + row
      this.acceptSuggestion()
    })

    // Over the panels, the image opened with a click: any key or click closes it.
    this.viewerBox = blessed.box({ parent: this.screen, top: 1, left: 2, right: 2, bottom: 1, border: { type: 'line' }, tags: true, wrap: false, hidden: true })
    this.viewerBox.on('click', () => this.closeViewer())
    // blessed gives the keyboard to whatever is clicked; here it goes only where setFocus puts it. The chat list's
    // arrows work only while the list holds it, and a click on the popup that closes it, on the prompt or on the bar
    // took it away from the list for good.
    for (const box of [this.tabsBar, this.msgBox, this.input, this.suggest, this.ghostBox, this.fixBox, this.viewerBox]) {
      (box as unknown as { options: { autoFocus?: boolean } }).options.autoFocus = false
    }

    // Above everything, the emoji rising when a message or reaction is a single emoji, sent or received.
    this.hearts = new Hearts(this.screen, this.msgBox, this.bgRgb, blessed.box)
    // Topmost of all: in its turn, inverts the cells of the text selection and draws the "☺" or the quick reactions
    // over the hovered message, whatever panel drew the cells.
    const overlay = blessed.box({ parent: this.screen, top: 0, left: 0, width: 1, height: 1, hidden: true })
    overlay.render = (() => { this.drawTextSel(); this.drawQuick(); return undefined }) as unknown as typeof overlay.render

    // Mouse with clicks, wheel and motion while a button is held (1000+1002) in SGR encoding (1006), instead of the
    // set blessed enables for xterm (1000/1002/1003/1005): any-motion reporting (1003) and UTF-8 encoding (1005)
    // confuse SSH apps on phones like Termius, which with 1000+1006 send taps as clicks. Any-motion reporting, which
    // the hover needs, is only asked of terminals that identified themselves (XTVERSION): desktop ones, not those
    // apps. blessed turns off whatever was enabled on exit.
    this.anyMotion = caps.version != null
    const mouse = this.screen.program as unknown as { disableMouse: () => void; setMouse: (o: Record<string, boolean>, enable: boolean) => void; _bindMouse: (s: string, buf: Buffer) => void }
    mouse.disableMouse()
    mouse.setMouse({ vt200Mouse: true, cellMotion: true, allMotion: this.anyMotion, sgrMouse: true }, true)
    // blessed only reads the first mouse sequence in each byte packet, and terminals send the button press and
    // release (or two wheel notches) in the same packet: the release was lost and there was never a click. The
    // packet is split into individual SGR sequences before blessed reads them.
    const bindMouse = mouse._bindMouse
    mouse._bindMouse = (s, buf) => {
      const parts = s.match(/\x1b\[<\d+;\d+;\d+[mM]|[^\x1b]+|\x1b(?!\[<)[\s\S]*?(?=\x1b\[<|$)/g)
      if (!parts || parts.length <= 1) return bindMouse.call(mouse, s, buf)
      for (const part of parts) bindMouse.call(mouse, part, Buffer.from(part, 'latin1'))
    }
    this.screen.program.hideCursor()
    this.bindEvents()
    this.registerTerminal()
    this.setFocus('input')
    this.drawInput()
    this.drawStatus()
    // `wa paula` opens that chat right away: the first, from most recent backward, whose name or number contains the text.
    if (this.wanted) {
      const jid = this.findChat(this.wanted)
      if (!jid) { this.quit(t('noChatWith', this.wanted)); return }
      this.openTab(jid)
    }
    this.renderNow()
    // drawStatus has already left the tab bar drawn, so the renderNow above doesn't touch the title or Herdr.
    this.updateTitle()
  }

  private get current(): string | null {
    return this.tabs[this.active] ?? null
  }

  // ---------- events ----------

  private bindEvents() {
    this.bindDiagnostics()
    this.screen.on('keypress', (ch: string, key: blessed.Widgets.Events.IKeyEventArg) => this.onKey(ch, key))
    // The picker list has its position and height calculated by hand: it's recomputed when the terminal resizes.
    this.screen.on('resize', () => {
      this.dirtyMessages = true; this.dirtyTabs = true; this.drawInput(); if (this.pickerOpen) this.refreshPicker(); this.scheduleRender()
      // A terminal may drop the images it holds when resized (on Ghostty they were gone for good), and placing them by
      // the ids it was given before then shows nothing: once the resizing stops, they're all let go and sent again.
      if (!this.pics) return
      clearTimeout(this.resendTimer)
      this.resendTimer = setTimeout(() => { this.pics?.dispose(); this.scheduleRender() }, 200)
    })
    // The painter looks at blessed's buffers just before it draws, when it's known which rows it will redraw.
    const screen = this.screen as unknown as { draw: (start: number, end: number) => void }
    const draw = screen.draw.bind(screen)
    screen.draw = (start, end) => {
      this.rgbPaint?.snapshot(this.screenRows('olines'), this.screenRows('lines'))
      if (this.pics instanceof SixelImages) this.pics.snapshot(this.screenRows('olines'), this.screenRows('lines'))
      draw(start, end)
    }
    this.screen.on('render', () => { this.loadVisibleImages(); this.loadVisibleProfiles(); this.placeImages(); this.paintRgb(); this.paintBigEmoji(); this.flushSixels() })

    // The mouse wheel scrolls one line per notch (by default blessed jumps half the panel, or two list entries).
    this.msgBox.removeAllListeners('wheeldown')
    this.msgBox.removeAllListeners('wheelup')
    this.msgBox.on('wheeldown', () => { this.msgBox.scroll(1); this.screen.render() })
    // Reaching the top with the wheel or PgUp brings older messages (loadOlder); ↑ on the first message does the same.
    this.msgBox.on('wheelup', () => { this.msgBox.scroll(-1); if (this.msgBox.childBase === 0) this.loadOlder(); this.screen.render() })
    this.picker.removeAllListeners('element wheeldown')
    this.picker.removeAllListeners('element wheelup')
    this.picker.on('element wheeldown', () => { this.picker.scroll(1, true); this.screen.render() })
    this.picker.on('element wheelup', () => { this.picker.scroll(-1, true); this.screen.render() })

    // A click on a row already selected comes as 'select' too, before 'element click': one on its photo opens the photo.
    this.picker.on('select', (_item, index) => {
      if (this.pickerPress && this.pickerPress.photo && Date.now() - this.pickerPress.at < 1000) return
      this.pickChat(index)
    })
    this.picker.on('element mousedown', (el: blessed.Widgets.BlessedElement, data: { x: number }) => {
      const i = this.picker.getItemIndex(el), at = this.pickerCont.get(i) ?? i, c = this.pickerSlots[at]
      const col = data.x - num(this.picker.aleft) - num(this.picker.ileft)
      this.pickerPress = { at: Date.now(), photo: !!c && this.onPhoto(col) && !this.pickerHits.has(at) && !!this.profiles.get(c.jid)?.avatar }
    })
    // The click lands on the item (a child of the list) and arrives as 'element click', after blessed has already
    // moved the selection; on a day separator it opens nothing. On a chat's photo (its four columns, on either row)
    // it opens the photo, large, in the popup.
    this.picker.on('element click', (el: blessed.Widgets.BlessedElement, data: { x: number }) => {
      const i = this.picker.getItemIndex(el), at = this.pickerCont.get(i) ?? i, c = this.pickerSlots[at]
      if (!c) return
      const col = data.x - num(this.picker.aleft) - num(this.picker.ileft)
      if (this.onPhoto(col) && !this.pickerHits.has(at) && this.profiles.get(c.jid)?.avatar) return this.openPhoto(c.jid)
      if (this.pickerOpen) this.pickChat(at)
    })
    // The selection never rests on a day separator: it goes on past it the way it was going, or back when there's
    // nothing further.
    this.picker.on('select item', (_item, i: number) => {
      if (this.pickerSlots[i] === null) {
        const dir = i < this.pickerAt ? -1 : 1
        const next = (d: number) => { for (let j = i + d; j >= 0 && j < this.pickerSlots.length; j += d) if (this.pickerSlots[j]) return j; return -1 }
        const j = next(dir) >= 0 ? next(dir) : next(-dir)
        if (j >= 0) return this.picker.select(j)
      }
      this.pickerAt = i
      this.lightPickerRows(i)
    })

    // A click on another tab goes to that chat (on the active one, with the chat list open, back to it); on a tab's
    // photo, the photo opens large; on the ✖ a tab shows under the pointer, it closes.
    this.tabsBar.on('click', (data: { x: number; y: number }) => {
      const x = data.x - num(this.tabsBar.aleft)
      const seg = this.segments.find(s => x >= s.x0 && x < s.x1)
      uiLog.info({ x, seg }, 'tab bar click')
      if (!seg) return
      const close = this.tabCloses.find(c => x >= c.x && x <= c.x + 1)
      if (close) {
        this.tabHover = null
        // The list's own, as Esc there: back to the chat in front, or with no tab left, out.
        if (close.index < 0) return this.tabs.length ? this.closePicker() : this.quit()
        return this.closeTab(close.index)
      }
      const tp = this.tabPhotos.find(p => x >= p.x && x < p.x + 2)
      if (tp) return this.openPhoto(tp.jid)
      if (!this.fixed && (this.pickerOpen || seg.index !== this.active)) this.activateTab(seg.index)
    })
    // Clicking a message's "☺" opens its quick reactions, clicking an attachment opens it; outside messages a click
    // returns focus to the input. Selecting is for the keyboard. Dragging a message's name and time line to the right (press and release on
    // that line, 4 or more columns ahead) starts a reply to it, like on WhatsApp mobile; dragging over any other
    // line selects text (below).
    // While the button is held the message's lines slide right with the pointer, like WhatsApp Web; letting go
    // 4 or more columns to the right starts the reply, less snaps back. In SGR the motion reports carry bit 32 of
    // the button byte, which blessed hands over as repeated 'mousedown's: the raw byte tells them apart.
    // `header`: the message's name or time line, where a drag replies and the "☺" sits; on the line where the time
    // follows the text, only outside the text's own columns, which select text instead.
    const lineAt = (y: number, x = -1) => {
      const line = this.msgBox.childBase + (y - num(this.msgBox.atop) - num(this.msgBox.itop))
      const orig = this.msgBox._clines?.rtof?.[line]
      if (orig == null) return null
      const cols = this.textLines.get(orig), col = x - num(this.msgBox.aleft) - num(this.msgBox.ileft)
      const onText = cols != null && col >= cols.start && col < cols.end
      const name = this.nameLines.get(orig)
      const mention = this.mentionLines.get(orig)?.find(m => col >= m.start && col < m.end)
      return { row: this.lineMap[orig] ?? null, header: this.headerLines.has(orig) && !onText, name: name && col >= name.start && col < name.start + name.width ? name.jid : mention?.jid ?? null }
    }
    const rowAt = (y: number) => lineAt(y)?.row ?? null
    let pressed: { x: number; y: number; row: MessageRow | null; header: boolean } | undefined
    this.msgBox.on('mouse', (data: { action: string; x: number; y: number; raw?: number[] }) => {
      const motion = !!((data.raw?.[0] ?? 0) & 32)
      if (data.action === 'mousedown' && !motion) { const l = lineAt(data.y, data.x); pressed = { x: data.x, y: data.y, row: l?.row ?? null, header: !!l?.header }; return }
      if (!motion || !pressed?.row || !pressed.header) return
      const dx = pressed.y === data.y ? Math.max(0, Math.min(8, data.x - pressed.x)) : 0
      if (this.drag?.id === pressed.row.id && this.drag.dx === dx) return
      this.drag = { id: pressed.row.id, dx }
      this.dirtyMessages = true
      this.renderNow()
    })
    // A double click (two clicks on the same message within 400 ms) also starts the reply, closing the quick
    // reactions the first click opened.
    let lastClick: { y: number; at: number; id: string } | undefined
    this.msgBox.on('click', (data: { x: number; y: number }) => {
      const row = rowAt(data.y)
      const dragged = pressed?.header && pressed.y === data.y && data.x - pressed.x >= 4
      pressed = undefined
      if (this.drag) { this.drag = undefined; this.dirtyMessages = true }
      if (this.textSelected()) return
      const now = Date.now()
      const double = !!row && lastClick?.id === row.id && now - lastClick.at < 400
      lastClick = row ? { y: data.y, at: now, id: row.id } : undefined
      // With the quick reactions open, one of them reacts, "⋯" goes to the keyboard flow; any other click closes
      // the bar and does nothing else, like on WhatsApp Web, unless it's the second of a double click (below).
      const hit = this.quickHit
      if (this.quickFor) {
        const target = this.quickFor
        const item = hit && data.y === hit.y ? hit.items?.find(i => data.x >= i.x && data.x < i.x + i.w) : undefined
        if (item) {
          this.quickFor = undefined
          if (item.emoji === '⋯') { this.reactTo = target; this.replyTo = null; this.setFocus('input'); this.drawInput() }
          else void this.react(target, item.emoji, true)
          return this.renderNow()
        }
        if (!double) { this.quickFor = undefined; return this.renderNow() }
      }
      if (row && (dragged || double)) {
        this.quickFor = undefined
        this.replyTo = row; this.reactTo = null
        this.setFocus('input')
        this.drawInput()
        return this.renderNow()
      }
      this.hideSentFix()
      // A click on a link copies it, whole; on a group member's name or a mention, it opens the chat with that person;
      // on a passage of mine marked wrong, its correction floats above it, to be clicked in turn.
      if (!dragged) {
        const url = this.linkAt(data.x, data.y); if (url) return this.copyToClipboard(url)
        if (row) { const hit = this.fixAt(data.x, data.y, row); if (hit) return this.showSentFix(row, hit.fix, hit.x, data.y) }
        const who = lineAt(data.y, data.x)?.name; if (who) return this.openChat(canonicalJid(who))
      }
      if (!row) { this.setFocus('input'); return this.renderNow() }
      // The "☺" opens the message's quick reactions; an attachment opens, an image only when the click lands on the
      // image itself; a pin or unpin goes to the message it's about. A click anywhere else on a message does nothing.
      const onIcon = hit?.icon != null && data.y === hit.y && Math.abs(data.x - hit.icon) <= 1
      const hasImage = this.images.some(i => i.row.id === row.id)
      const picture = this.imageAt(data.x, data.y)
      if (onIcon) this.quickFor = row
      else if (row.type === 'pinInChat') return this.jumpTo(pinTarget(row))
      else if (picture?.src && picture.row.id === row.id) return this.openViewer(row)
      else if (row.media_mime && (!hasImage || this.imageAt(data.x, data.y)?.row.id === row.id)) {
        if (hasImage && this.mode !== 'none' && (row.type === 'image' || row.type === 'sticker')) return this.openViewer(row)
        this.openMedia(row)
      }
      this.renderNow()
    })
    this.msgBox.on('scroll', () => { this.updateAtBottom(); this.hideSentFix(); if (this.textSel) { this.textSel = undefined; this.screen.render() } })
    // Clicking the input places the cursor at the clicked position (or at the end of the line, if the click lands past the text).
    this.input.on('click', (data: { x: number; y: number }) => {
      if (this.textSelected()) return
      // On "↓" beside the band, scrolled up, the messages go down to the latest, as Ctrl+↓ does.
      const cx = data.x - num(this.input.aleft) - num(this.input.ileft), cy = data.y - num(this.input.atop) - num(this.input.itop)
      if (!this.plainInput && cy === this.belowRow && cx >= this.borderBelow) return this.toLatest()
      if (!this.pickerOpen) this.setFocus('input')
      {
        const x = data.x - num(this.input.aleft) - num(this.input.ileft) - this.promptWidth
        const row = this.inputTop + data.y - num(this.input.atop) - num(this.input.itop) - this.headerRows
        let pos = 0
        for (let r = 0; r < Math.min(row, this.inputLines.length); r++) pos += this.inputLines[r]!.length
        const line = this.inputLines[row]
        if (line) {
          let col = 0
          for (const ch of line) { const w = visibleWidth(esc(ch)); if (col + w / 2 > x) break; col += w; pos++ }
        }
        if (this.pickerOpen) this.filterCursor = pos
        // The emoji list follows the cursor: it's for what's before the new position, if anything.
        else { this.cursor = pos; this.updateSuggestions() }
        this.drawInput()
      }
      this.screen.render()
    })
    // Text selection, like in a terminal: pressing on text (any line of the messages but a name and time one, or
    // the input) and dragging highlights the cells swept, in reading order, within that panel; letting go copies
    // the text to the clipboard (OSC 52) and leaves it highlighted until the next click or key. The screen gets the
    // events after the panels, so the panels' click handlers already see the selection and stay out of its way.
    const inside = (box: blessed.Widgets.BoxElement, x: number, y: number) => {
      const xi = num(box.aleft) + num(box.ileft), xl = num(box.aleft) + num(box.width) - (num(box.iwidth) - num(box.ileft))
      const yi = num(box.atop) + num(box.itop), yl = num(box.atop) + num(box.height) - (num(box.iheight) - num(box.itop))
      return x >= xi && x < xl && y >= yi && y < yl ? { xi, xl, yi, yl } : null
    }
    let selPress: { x: number; y: number; xi: number; xl: number; yi: number; yl: number; input: boolean } | undefined
    this.screen.on('mouse', (d: { action: string; x: number; y: number; raw?: number[] }) => {
      if (d.action === 'mousemove') {
        // On the tab bar, the chat's tab under the pointer shows a ✖ to close it (drawTabs).
        const bx = d.x - num(this.tabsBar.aleft)
        const seg = d.y === num(this.tabsBar.atop) ? this.segments.find(s => bx >= s.x0 && bx < s.x1) : undefined
        const tab = seg ? seg.index : null
        if (tab !== this.tabHover) { this.tabHover = tab; this.dirtyTabs = true; this.scheduleRender() }
        const row = !this.pickerOpen && inside(this.msgBox, d.x, d.y) ? lineAt(d.y)?.row ?? undefined : undefined
        // A redraw when the message under the pointer changes, and along the line with the "☺" or the reactions,
        // where the one under the pointer is highlighted.
        const onBar = (y: number) => this.quickHit != null && y === this.quickHit.y
        const changed = row?.id !== this.hover?.id || onBar(d.y) || (this.pointer != null && onBar(this.pointer.y))
        this.pointer = { x: d.x, y: d.y }
        this.hover = row
        if (changed) this.screen.render()
        return
      }
      const motion = !!((d.raw?.[0] ?? 0) & 32)
      if (d.action === 'mousedown' && !motion) {
        if (this.textSel) { this.textSel = undefined; this.screen.render() }
        const box = (this.pickerOpen || lineAt(d.y, d.x)?.header ? null : inside(this.msgBox, d.x, d.y)) ?? inside(this.input, d.x, d.y)
        selPress = box ? { x: d.x, y: d.y, ...box, input: !inside(this.msgBox, d.x, d.y) } : undefined
        return
      }
      if (motion && selPress) {
        const { xi, xl, yi, yl } = selPress
        const hx = Math.max(xi, Math.min(xl - 1, d.x)), hy = Math.max(yi, Math.min(yl - 1, d.y))
        if (this.textSel?.hx === hx && this.textSel.hy === hy) return
        this.textSel = { ax: selPress.x, ay: selPress.y, hx, hy, xi, xl, input: selPress.input }
        return this.screen.render()
      }
      if (d.action === 'mouseup' && selPress) {
        selPress = undefined
        if (!this.textSel) return
        if (!this.textSelected()) { this.textSel = undefined; return this.screen.render() }
        this.copyTextSel()
      }
    })

    this.wa.on('connection', (state, detail) => this.onConnection(state, detail))
    this.wa.on('chats', () => { this.dirtyTabs = true; if (this.pickerOpen) this.refreshPicker(); this.scheduleRender() })
    this.wa.on('profile', jid => {
      const p = store.getProfile(jid)
      if (p) this.profiles.set(jid, p)
      if (store.getState<boolean>(`biz:${jid}`)) this.business.add(jid); else this.business.delete(jid)
      if (this.pickerOpen) { this.redrawPickerRows([jid]); this.scheduleRender() }
    })
    this.wa.on('mood', jid => {
      this.moods.set(jid, store.getState<Mood>(`mood:${jid}`)?.emoji ?? '')
      this.dirtyTabs = true
      if (this.pickerOpen) this.redrawPickerRows([jid])
      if (jid === this.current) this.drawInput()
      this.scheduleRender()
    })
    this.wa.on('typing', (jid, who) => this.onTyping(jid, who.length > 0))
    this.wa.on('available', on => { this.available = on })
    this.wa.on('presence', (jid, on) => {
      if (on) this.online.add(jid); else this.online.delete(jid)
      if (this.pickerOpen) { this.redrawPickerRows([jid]); this.screen.render() }
      if (jid === this.current) { this.drawInput(); this.screen.render() }
    })
    this.wa.on('messages', jid => {
      if (jid === '*' || jid === this.current) this.dirtyMessages = true
      this.dirtyTabs = true
      // The open list shows each chat's last message: a change to it (its state, an edit) redraws that row.
      if (this.pickerOpen) {
        if (jid === '*') this.refreshPicker()
        else if (this.pickerLast.has(jid)) { this.pickerLast.set(jid, store.lastMessage(jid)); this.redrawPickerRows([jid]) }
      }
      this.scheduleRender()
    })
    this.wa.on('notify', (jid, row) => {
      if (jid === this.current) {
        // Arriving in the chat in front of you, it's read only while you show as online (writing here lately): a
        // focused pane or window with no one at it would take it as seen. Otherwise it waits, and the phone notifies,
        // until you write, open the chat or come back to the terminal.
        if (this.available) this.markReadIfSeen(jid)
        if (row.type === 'text' && reaction(row.text)) this.heartFor(r => r.id === row.id, row.text)
        return
      }
      // An archived chat stays quiet: no tab or pane opens for it, no notice, no bell.
      if (store.getChat(jid)?.archived) return
      // In Herdr the new chat opens in a pane or tab of its own, in the background, by the same rule as a new tab: only
      // the most recently used terminal, and never if it's already open in another. Until the new one registers, the
      // request is remembered.
      if (this.fixed) {
        if (inHerdr && !this.openElsewhere(jid) && this.isMostRecentTerminal() && !this.spawning.has(jid)) {
          this.spawning.add(jid)
          setTimeout(() => this.spawning.delete(jid), 15000)
          openChatHerdr(jid, false).catch(e => logger.warn({ e }, 'herdr: open chat'))
        }
        return
      }
      // The notice's text with its mentions by first name, as the message shows them (withMentions).
      const said = row.text ? withMentions(row.text) : `[${row.type}]`
      if (this.tabs.includes(jid)) { this.screen.program.bell(); this.notify(jid, said); return }
      // Chat without a tab: with several terminals, only the most recently used one opens the tab, and never if the
      // chat already has a tab in another live terminal.
      if (this.openElsewhere(jid) || !this.isMostRecentTerminal()) return
      this.openTab(jid, false)
      this.screen.program.bell()
      this.notify(jid, said)
    })
    this.wa.on('status', text => this.flash(text))
    // A reaction, mine or someone else's, animates from the spot where it appears in the message.
    this.wa.on('reaction', (jid, msgId, _sender, emoji) => { if (jid === this.current && reaction(emoji)) this.heartFor(r => r.id === msgId, emoji) })
  }

  /** Logs to wa.log everything that arrives from the terminal and what the UI does with it. */
  private bindDiagnostics() {
    const program = this.screen.program as unknown as { input: NodeJS.ReadStream }
    program.input.on('data', (b: Buffer) => {
      // Only escape sequences (mouse, special keys), never the typed text.
      if (b[0] === 0x1b) uiLog.info({ raw: JSON.stringify(b.toString('latin1')) }, 'bytes')
    })
    this.screen.on('mouse', (d: { action: string; button?: string; x: number; y: number; shift?: boolean; ctrl?: boolean }) => {
      // blessed hands the focus events over as mouse events too: gaining or losing the focus isn't activity.
      if (d.action === 'focus' || d.action === 'blur') return
      this.touchActivity()
      // Pointer movement with no button, when the terminal reports it, is one event per cell: not logged.
      if (d.action !== 'mousemove') uiLog.info({ action: d.action, button: d.button, x: d.x, y: d.y, shift: d.shift, ctrl: d.ctrl }, 'mouse')
    })
    const named: [string, blessed.Widgets.BlessedElement][] = [['tabs', this.tabsBar], ['messages', this.msgBox], ['input', this.input], ['picker', this.picker]]
    for (const [name, w] of named) {
      ;(w as unknown as { on: (ev: string, fn: (el: blessed.Widgets.BlessedElement, d: { action: string; x: number; y: number }) => void) => void })
        .on('element mouse', (el, d) => { if (d.action !== 'mousemove') uiLog.info({ panel: name, child: el !== w ? el.type : undefined, action: d.action, x: d.x, y: d.y }, 'mouse in panel') })
    }
    this.screen.on('keypress', (_ch: string, key: blessed.Widgets.Events.IKeyEventArg) => uiLog.info({ key: key.full, focus: this.focus }, 'key'))
    uiLog.info({ modes: `mouse 1000+1002${this.anyMotion ? '+1003' : ''}+1006`, term: process.env.TERM, program: process.env.TERM_PROGRAM, cols: this.screen.width, rows: this.screen.height }, 'startup')
  }

  private onConnection(state: ConnState, detail?: string) {
    if (this.wa.me && this.wa.me !== this.myJid) { this.myJid = this.wa.me; store.setState('me', this.myJid) }
    // Any change of connection, the switch to another server process included, voids what was said about who's
    // typing and who's online: the new connection doesn't know, and would never send the "stopped" or "offline"
    // that clears it. Typing ends as if they'd stopped; online comes back once open, from the presence
    // subscriptions renewed below.
    for (const jid of [...this.typing]) this.onTyping(jid, false)
    if (this.online.size) { this.online.clear(); this.drawInput() }
    if (state === 'qr' && this.wa.qr) {
      QRCode.toString(this.wa.qr, { type: 'terminal', small: true }, (err, qr) => {
        if (err) { logger.error({ err }, 'qr'); return }
        this.showingQr = true
        // The QR goes in the message panel, which the chat list hides.
        if (this.pickerOpen) this.closePicker(false)
        this.msgBox.setContent(['', `  {bold}${esc(t('qrTitle'))}{/bold}`, '', `  ${esc(t('qrHint'))}`, '', qr].join('\n'))
        this.lineMap = []; this.images = []; this.rows = []; this.selected = null
        this.screen.render()
      })
      this.connText = `{${FG.warn}-fg}● ${esc(t('waitingQr'))}{/${FG.warn}-fg}`
    } else if (state === 'open') {
      this.connText = ''
      this.showingQr = false
      this.dirtyMessages = true
      for (const jid of this.tabs) { this.wa.subscribePresence(jid); this.wa.ensureName(jid) }
      if (this.lastWrite && this.hasFocus !== false && Date.now() - this.lastWrite < 120000) { this.lastPresenceTouch = Date.now(); this.wa.touchPresence(process.pid) }
      if (this.hasFocus !== undefined) this.wa.setFocus(process.pid, this.hasFocus)
      this.scheduleRender()
    } else if (state === 'closed') {
      this.connText = `{${FG.error}-fg}● ${esc(detail ?? t('disconnected'))}{/${FG.error}-fg}`
    } else {
      this.connText = `{${FG.warn}-fg}● ${esc(t('connecting'))}{/${FG.warn}-fg}`
    }
    this.drawStatus()
    this.screen.render()
  }

  /**
   * Someone started or stopped typing: two emoji take turns where the chat's state's emoji is, in its tab and its row
   * of the list, and before its name in the window title (in Herdr, the agent's name).
   */
  private onTyping(jid: string, active: boolean) {
    if (active) this.typing.add(jid); else this.typing.delete(jid)
    this.syncTypingTimer()
    this.drawTabs()
    this.redrawPickerRows([jid])
    if (jid === this.current) this.drawInput()
    this.updateTitle()
    this.screen.render()
  }

  /** The typing emoji's clock, a look every 100 ms for their turn every 500: it runs while anyone is typing. */
  private syncTypingTimer() {
    const on = this.typing.size > 0
    if (on && !this.typingTimer) {
      this.typingTimer = setInterval(() => {
        this.drawTabs()
        this.redrawPickerRows(this.typing)
        this.updateTitle()
        this.screen.render()
      }, 100)
    } else if (!on && this.typingTimer) { clearInterval(this.typingTimer); this.typingTimer = undefined }
  }

  /** Pasted text goes in whole where the cursor is; in the input it keeps the lines, in the picker filter it collapses to one. */
  private paste(text: string) {
    if (this.focus === 'picker') {
      const chars = graphemes(this.filter), at = Math.min(this.filterCursor, chars.length)
      const ins = graphemes(text.replace(/\n/g, ' '))
      this.filter = [...chars.slice(0, at), ...ins, ...chars.slice(at)].join(''); this.filterCursor = at + ins.length
      this.refreshPicker()
      return this.screen.render()
    }
    if (this.focus !== 'input' || !text) return
    const chars = graphemes(this.inputValue), at = Math.min(this.cursor, chars.length)
    const ins = graphemes(text)
    this.inputValue = [...chars.slice(0, at), ...ins, ...chars.slice(at)].join(''); this.cursor = at + ins.length
    this.accepted = undefined
    this.promoteActive()
    this.noteComposing()
    this.noteWriting()
    this.updateSuggestions()
    this.drawInput()
    this.screen.render()
  }

  private onKey(ch: string, key: blessed.Widgets.Events.IKeyEventArg) {
    this.touchActivity()
    if (this.textSel) { this.textSel = undefined; this.screen.render() }
    const k = key.full
    if (k !== 'right') this.acceptOnArrival = 0
    // blessed emits each Enter twice: a synthetic "enter" and right after the real "return". Only the second counts;
    // otherwise, with suggestions open, the first would accept the emoji and the second would send the message.
    if (key.name === 'enter' && key.sequence === '\r') return
    if (k === 'C-c') return this.quit()
    // Ctrl+R redraws the whole screen, for whatever left marks on it (a glyph wider than its cell, a message
    // written straight to the terminal): blessed forgets what it believes is there and paints everything again.
    if (k === 'C-r') return this.redraw()
    if (this.viewing) return this.closeViewer()
    if (this.sentFix) this.hideSentFix()
    if (this.search) return this.searchKey(k, ch, key)
    if (k === 'C-f' && this.current && !this.pickerOpen) return this.openSearch()
    // The list's tab says which: "wassup" for the names, "procurar" for the messages.
    if (k === 'C-f' && this.pickerOpen) { this.pickerSearch = !this.pickerSearch; this.dirtyTabs = true; this.refreshPicker(); return this.renderNow() }
    if (k === 'paste') return this.paste(ch)
    // ESC closes, in order: the reply or reaction in progress, the selection, the picker filter, the picker, the
    // active tab, the program.
    if (k === 'escape') {
      if (this.quickFor) { this.quickFor = undefined; return this.screen.render() }
      if (this.suggestions.length) {
        if (this.suggestKind) this.noteSuggest(this.suggestKind, false)
        this.suggestions = []; this.drawSuggestions(); this.drawInput(); return this.screen.render()
      }
      if (this.replyTo || this.reactTo) { this.replyTo = this.reactTo = null; this.drawInput(); return this.screen.render() }
      if (this.deleting) { this.deleting = null; this.drawInput(); return this.screen.render() }
      if (this.editing) { this.editing = null; this.inputValue = ''; this.cursor = 0; this.updateSuggestions(); this.drawInput(); return this.screen.render() }
      if (this.focus === 'messages') { this.setFocus('input'); return this.renderNow() }
      if (this.pickerOpen) {
        if (this.pickerSearch) { this.pickerSearch = false; this.dirtyTabs = true; this.filter = ''; this.filterCursor = 0; this.refreshPicker(); return this.renderNow() }
        if (this.filter) { this.filter = ''; this.filterCursor = 0; this.refreshPicker(); return this.screen.render() }
        // With no tabs there's nowhere to go back to: the picker is the only panel, and closing it means quitting.
        return this.tabs.length ? this.closePicker() : this.quit()
      }
      if (this.current) return this.closeTab(this.active)
      return this.quit()
    }
    // With the chat list open, PgUp and PgDn move its selection a page, the rows in view less one, past the day
    // separators like the arrows.
    if ((k === 'pageup' || k === 'pagedown') && this.pickerOpen) {
      const page = Math.max(1, num(this.picker.height) - num(this.picker.iheight) - 1)
      ;(this.picker as unknown as { move: (n: number) => void }).move(k === 'pageup' ? -page : page)
      return this.screen.render()
    }
    if (k === 'pageup') { this.msgBox.scroll(-(this.innerHeight() - 1)); if (this.msgBox.childBase === 0) this.loadOlder(); return this.screen.render() }
    if (k === 'pagedown') { this.msgBox.scroll(this.innerHeight() - 1); return this.screen.render() }
    // Ctrl+↓ or Ctrl+PgDn: straight to the latest message, leaving any selection.
    if ((k === 'C-down' || k === 'C-pagedown') && !this.pickerOpen && this.current) return this.toLatest()
    // Tab cycles through the open tabs (in Herdr, the panes or tabs of the other conversations); with the picker
    // open it goes back to the active tab. New chats open with "/".
    // With text in the input, Tab accepts the suggestion in view: the emoji list, or the model's; with no text, it
    // switches tabs. The right arrow, with the cursor already at the end, does the same as Tab; mid-text it keeps
    // moving the cursor.
    if ((k === 'tab' || (k === 'right' && this.cursorAtEnd() && (this.suggestions.length || this.ghostShown()))) && this.focus === 'input' && !this.pickerOpen && this.inputValue) {
      if (this.suggestions.length) return this.acceptSuggestion()
      if (this.ghostShown()) this.acceptGhost()
      return
    }
    // → at the end, with no suggestion in view but one requested: the acceptance is flagged for when it arrives.
    if (k === 'right' && this.focus === 'input' && !this.pickerOpen && this.inputValue && this.cursorAtEnd() && (this.ghostTimer || this.ghostAbort)) {
      this.acceptOnArrival++
      return
    }
    // In the chat list: Enter (below, from the list) opens the chat; → (at the end of the filter) and Tab do the
    // same, except in Herdr, where Enter puts it in this pane, → opens it in a new pane and Tab in a new tab.
    if (this.pickerOpen && (k === 'tab' || (k === 'right' && this.filterCursor >= graphemes(this.filter).length))) {
      if (!this.pickerChosen) return
      return this.pickChat((this.picker as unknown as { selected: number }).selected, !inHerdr ? 'here' : k === 'tab' ? 'tab' : 'pane')
    }
    if (k === 'tab') {
      // In Herdr each conversation is a pane or tab of its own: Tab moves to the next one, in Herdr's order. With no
      // other conversation open to go to, there or here, it opens the chat list.
      if (inHerdr && !this.pickerOpen) {
        return void focusNextChatHerdr().then(moved => { if (!moved) this.openPicker() }).catch(e => logger.warn({ e: String(e) }, 'herdr: next chat'))
      }
      if (!this.pickerOpen && this.tabs.length < 2) return this.openPicker()
      if (!this.tabs.length) return
      return this.activateTab(this.pickerOpen ? this.active : (this.active + 1) % this.tabs.length)
    }

    if (this.focus === 'picker') {
      // Typing with the picker open filters the chats; arrows and Enter belong to the list.
      const e = edit(this.filter, this.filterCursor, k, ch, key)
      if (!e) return
      this.filterCursor = e.cursor
      if (e.value !== this.filter) { this.filter = e.value; this.refreshPicker() }
      else this.drawInput()
      return this.screen.render()
    }
    if (this.focus === 'input') {
      // "/" with the input empty opens the chat list right away; whatever's typed next filters the list.
      if (ch === '/' && !this.inputValue) return this.openPicker()
      // Shift+Enter (only with the Kitty protocol, which distinguishes it) or Ctrl+J start a new line in the message.
      if (k === 'S-return' || k === 'linefeed') return this.paste('\n')
      // With emoji suggestions open, ↑/↓ choose (with a single one they go on to the messages) and Enter or Tab
      // accept (Enter not on a whole smiley's own emoji, where it sends the message as typed); everything else keeps
      // typing and refines them.
      if (this.suggestions.length) {
        if ((k === 'up' || k === 'down') && this.suggestions.length > 1) {
          this.suggestIndex = (this.suggestIndex + (k === 'up' ? -1 : 1) + this.suggestions.length) % this.suggestions.length
          this.drawSuggestions()
          return this.screen.render()
        }
        if ((k === 'enter' || k === 'return') && !(this.suggestFace && this.suggestIndex === 0)) return this.acceptSuggestion()
        // A space takes the chosen emoji or goes in, as learnt (noteSuggest); after a smiley, Enter sends it as typed.
        if (this.suggestKind && ch === ' ') {
          if (store.getState<SuggestMode>(this.suggestKind)?.mode === 'emoji') return this.acceptSuggestion()
          this.noteSuggest(this.suggestKind, false)
        }
        if (this.suggestFace && (k === 'enter' || k === 'return')) this.noteSuggest('smileys', false)
      }
      if (k === 'enter' || k === 'return') {
        const v = this.inputValue, draft = this.draftShown()
        if (v) this.noteWriting()
        this.inputValue = ''; this.cursor = 0; this.stopComposing(); this.updateSuggestions(); this.drawInput(); this.screen.render()
        return void this.submit(v, draft)
      }
      // Right after accepting a suggestion that ended mid-word, a letter or digit starts a new word: it goes in
      // with a space before it. Space and punctuation follow directly.
      if (this.accepted === this.inputValue && this.cursorAtEnd() && ch && /^[\p{L}\p{N}]$/u.test(ch) && !key.ctrl && !key.meta) {
        this.inputValue += ' '
        this.cursor++
      }
      const e = edit(this.inputValue, this.cursor, k, ch, key)
      // Erasing a reply's or a reaction's text down to nothing, or Backspace with nothing left, drops the reply or the
      // reaction with it.
      if ((this.replyTo || this.reactTo) && (this.inputValue ? !!e && !e.value : k === 'backspace')) {
        this.replyTo = this.reactTo = null
        this.inputValue = ''
        this.cursor = 0
        this.updateSuggestions()
        this.drawInput()
        return this.screen.render()
      }
      if (!e) { if (k === 'up') this.moveSelection(-1); return }
      // Only the text changing spends the promised space; moving the cursor (→ at the end, with no suggestion) leaves it unspent.
      if (e.value !== this.inputValue) { this.accepted = undefined; this.promoteActive(); this.noteWriting() }
      this.inputValue = e.value
      this.cursor = e.cursor
      this.noteComposing()
      this.updateSuggestions()
      this.drawInput()
      return this.screen.render()
    }
    if (this.focus === 'messages') {
      // Delete or Ctrl+D over the selected message deletes it, once confirmed by a second one: mine for everyone
      // while WhatsApp allows it (DELETE_WINDOW), any other for me only. Any other key gives up.
      if ((k === 'delete' || k === 'C-d') && this.selected && this.selected.type !== 'deleted') {
        if (this.deleting?.row.id === this.selected.id) return void this.deleteMessage(this.deleting)
        const row = this.selected
        this.deleting = { row, forEveryone: row.from_me === 1 && Date.now() / 1000 - row.ts < DELETE_WINDOW }
        this.drawInput()
        return this.screen.render()
      }
      if (this.deleting) { this.deleting = null; this.drawInput() }
      if (k === 'up' || k === 'down') return this.moveSelection(k === 'up' ? -1 : 1)
      // → over the selected message replies to it. It's also what Termius sends on a right swipe: a burst of
      // arrows, with no position; the following ones land on the empty input and do nothing.
      if (k === 'right' && this.selected) {
        this.replyTo = this.selected; this.reactTo = null
        this.setFocus('input')
        this.drawInput()
        return this.screen.render()
      }
      // Backspace over my own text message opens it in the input to correct it; Enter sends the edit, Esc gives up.
      if (k === 'backspace' && this.selected) return this.editMessage(this.selected)
      // Typing over the selected message starts a reply right away, with what was typed; ":" starts a reaction, and
      // stays typed so it can continue with the emoji's :code:. The input's header says which message.
      if (this.selected && ch && !key.ctrl && !key.meta && ch >= ' ' && ch !== '\x7f') {
        if (ch === ':') { this.reactTo = this.selected; this.replyTo = null } else { this.replyTo = this.selected; this.reactTo = null }
        this.inputValue = ch
        this.cursor = 1
        this.promoteActive()
        this.setFocus('input')
        return this.renderNow()
      }
    }
  }

  // ---------- text selection ----------

  /** Whether the mouse selection covers more than the cell it started on. */
  private textSelected(): boolean {
    const s = this.textSel
    return !!s && (s.ax !== s.hx || s.ay !== s.hy)
  }

  /** The selection's first and last cells, in reading order. */
  private textSelRange(): { x0: number; y0: number; x1: number; y1: number } | null {
    const s = this.textSel
    if (!s) return null
    const back = s.hy < s.ay || (s.hy === s.ay && s.hx < s.ax)
    return back ? { x0: s.hx, y0: s.hy, x1: s.ax, y1: s.ay } : { x0: s.ax, y0: s.ay, x1: s.hx, y1: s.hy }
  }

  /**
   * The cells of screen row `y` the selection takes, or nothing: only written text counts, never the name and
   * time, day separators, quotes, reactions or media notes. In the messages that's the rows showing a message's
   * own text; in the input, every row, from after the prompt.
   */
  private selCells(y: number): { from: number; to: number } | null {
    const s = this.textSel, r = this.textSelRange()
    if (!s || !r) return null
    let from = s.xi, to = s.xl - 1
    if (s.input) {
      from += this.promptWidth
    } else {
      const real = this.msgBox.childBase + (y - num(this.msgBox.atop) - num(this.msgBox.itop))
      const orig = this.msgBox._clines?.rtof?.[real]
      const cols = orig != null ? this.textLines.get(orig) : undefined
      if (!cols) return null
      from = s.xi + cols.start; to = s.xi + cols.end - 1
    }
    if (y === r.y0) from = Math.max(from, r.x0)
    if (y === r.y1) to = Math.min(to, r.x1)
    return from <= to ? { from, to } : null
  }

  /** Inverts the selected cells in the screen buffer, right before blessed writes it out. */
  private drawTextSel() {
    const r = this.textSelRange()
    if (!r) return
    const lines = (this.screen as unknown as { lines: ([number, string][] & { dirty?: boolean })[] }).lines
    for (let y = r.y0; y <= r.y1; y++) {
      const line = lines[y], cells = this.selCells(y)
      if (!line || !cells) continue
      for (let x = cells.from; x <= cells.to; x++) {
        const cell = line[x]
        if (cell) cell[0] ^= 8 << 18
      }
      line.dirty = true
    }
  }

  /** Copies the selected cells' text to the clipboard, one line per screen row, without the spaces around each. */
  private copyTextSel() {
    const r = this.textSelRange()
    if (!r) return
    const lines = (this.screen as unknown as { lines: [number, string][][] }).lines
    const out: string[] = []
    for (let y = r.y0; y <= r.y1; y++) {
      const cells = this.selCells(y)
      if (!cells) continue
      let text = ''
      for (let x = cells.from; x <= cells.to; x++) {
        const ch = lines[y]?.[x]?.[1]
        // The cell after a wide character holds blessed's marker, not text.
        if (ch && ch !== '\x03') text += ch
      }
      out.push(text.trim())
    }
    if (out.length) this.copyToClipboard(out.join('\n'))
  }

  /** Puts `text` in the terminal's clipboard (OSC 52) and says so in the status. */
  private copyToClipboard(text: string) {
    ;(this.screen.program as unknown as { _write: (s: string) => void })._write(`\x1b]52;c;${Buffer.from(text).toString('base64')}\x1b\\`)
    this.flash(t('copied'))
  }

  /**
   * The URL under screen cell (`x`, `y`), whole, if the cell is on a message's text and the run of non-blank
   * cells around it is part of one of the message's URLs; a URL wrapped over two lines is found from either piece.
   */
  private linkAt(x: number, y: number): string | null {
    const real = this.msgBox.childBase + (y - num(this.msgBox.atop) - num(this.msgBox.itop))
    const orig = this.msgBox._clines?.rtof?.[real]
    const cols = orig != null ? this.textLines.get(orig) : undefined
    const row = orig != null ? this.lineMap[orig] : null
    if (!cols || !row?.text) return null
    const xi = num(this.msgBox.aleft) + num(this.msgBox.ileft)
    const line = (this.screen as unknown as { lines: [number, string][][] }).lines[y]
    if (!line) return null
    const ch = (cx: number) => { const c = line[cx]?.[1]; return c && c !== '\x03' && c !== ' ' ? c : '' }
    if (x < xi + cols.start || x >= xi + cols.end || !ch(x)) return null
    let a = x, b = x
    while (a - 1 >= xi + cols.start && (ch(a - 1) || line[a - 1]?.[1] === '\x03')) a--
    while (b + 1 < xi + cols.end && (ch(b + 1) || line[b + 1]?.[1] === '\x03')) b++
    let run = ''
    for (let cx = a; cx <= b; cx++) run += ch(cx)
    // Punctuation stuck to the link ("(https://…)") is in the run but not in the link, and vice versa. The run is
    // the link as shown (clean and short); what's copied is the whole clean link.
    const links = linksIn(row.text)
    const hit = links.find(l => l.shown.includes(run) || run.includes(l.shown))
    const same = hit ? links.filter(l => l.shown === hit.shown) : []
    if (same.length < 2) return hit?.url ?? null
    // The same text for more than one link (their identifiers shown as "…", see shortUrl): the one clicked is the
    // one after as many of them as come before it in the message's text, its earlier lines and this one up to the run.
    const fake = this.msgBox._clines.fake
    let before = ''
    for (let i = orig! - 1; i >= 0 && this.lineMap[i]?.id === row.id; i--) if (this.textLines.has(i)) before = (fake[i] ?? '').replace(/\x1b\[[\d;]*m/g, '').trim() + before
    for (let cx = xi + cols.start; cx < a; cx++) before += ch(cx)
    return same[Math.min(same.length - 1, before.split(hit!.shown).length - 1)]!.url
  }

  // ---------- reactions ----------

  /** Sends `emoji` as my reaction to `row` (empty removes it); the same emoji again, from the mouse, removes it too. */
  private async react(row: MessageRow, emoji: string, toggle = false) {
    if (this.wa.state !== 'open') return this.flash(t('noConnection'))
    const mine = store.listReactions(row.chat_jid).find(r => r.msg_id === row.id && r.sender_jid === this.wa.me)
    const send = toggle && mine?.emoji === emoji ? '' : emoji
    try {
      await this.wa.react(row.chat_jid, row.id, send)
      if (!send) this.flash(t('reactionRemoved'))
    } catch (e) {
      logger.error({ e }, 'react')
      this.flash(`${t('error')}: ${(e as Error).message}`, 10000)
    }
  }

  /**
   * Over the hovered message's time line: a gray "☺" to its right (to its left in mine, which sit flush right),
   * or, once clicked, the quick reactions in its place. Drawn straight into the screen buffer, so the panel isn't
   * rebuilt at every pointer move; where it landed is kept for the click.
   */
  private drawQuick() {
    this.quickHit = undefined
    const row = this.quickFor ?? this.hover
    if (!row || this.showingQr || this.viewing) return
    // On the line with its time, the last of its header lines (in a group, the sender's name opens the message).
    let idx = -1
    for (let i = 0; i < this.lineMap.length; i++) if (this.lineMap[i]?.id === row.id && this.headerLines.has(i)) idx = i
    if (idx < 0) return
    const real = this.msgBox._clines.ftor[idx]?.[0]
    if (real == null) return
    const y = real - this.msgBox.childBase
    if (y < 0 || y >= this.innerHeight()) return
    const sy = num(this.msgBox.atop) + num(this.msgBox.itop) + y
    const xi = num(this.msgBox.aleft) + num(this.msgBox.ileft), xl = xi + num(this.msgBox.width) - num(this.msgBox.iwidth)
    const line = (this.screen as unknown as { lines: ([number, string][] & { dirty?: boolean })[] }).lines[sy]
    if (!line) return
    const mine = row.from_me === 1
    const blank = (x: number) => { const ch = line[x]?.[1]; return ch === ' ' || ch === '' }
    // Where the time line's text starts (mine) or ends (others).
    let edge = mine ? xi : xl - 1
    if (mine) while (edge < xl && blank(edge)) edge++
    else while (edge >= xi && blank(edge)) edge--
    // The pointer over an item's click area (its cells and one on each side) makes it stand out: the "☺" goes
    // from gray to bold in the text color, a reaction gets the selected message's background over the whole area.
    const over = (x: number, w: number) => this.pointer != null && this.pointer.y === sy && this.pointer.x >= x - 1 && this.pointer.x <= x + w
    const put = (x: number, ch: string, w: number, hot: boolean) => {
      for (let i = hot && w > 1 ? -1 : 0; i < (hot && w > 1 ? w + 1 : w); i++) {
        const cell = line[x + i]
        if (!cell) continue
        // The cell's own background (the selected message's, say) stays; the text goes gray.
        if (w > 1) cell[0] = hot ? (cell[0] & ~0x1ff) | this.selectedBg : cell[0]
        else cell[0] = hot ? (cell[0] & ~(0x1ff << 9)) | (0x1ff << 9) | (1 << 18) : (cell[0] & ~(0x1ff << 9)) | (244 << 9)
        if (i >= 0) cell[1] = i ? ' ' : ch
      }
    }
    // Two cells between the reactions: each one's click area is its own cells plus one on each side, so a click
    // that lands next to the emoji still counts, and no cell belongs to two of them.
    const items = this.quickFor ? QUICK.map(emoji => ({ emoji, w: emoji === '⋯' ? 1 : 2 })) : [{ emoji: '☺', w: 1 }]
    const gap = this.quickFor ? 2 : 1
    const total = items.reduce((n, i) => n + i.w, 0) + (items.length - 1) * gap
    let x = mine ? edge - 2 - total : edge + 2
    x = Math.max(xi, Math.min(xl - total, x))
    const hit: { y: number; icon?: number; items?: { x: number; w: number; emoji: string }[] } = { y: sy }
    if (this.quickFor) hit.items = []
    for (const item of items) {
      put(x, item.emoji, item.w, over(x, item.w))
      if (hit.items) hit.items.push({ x: x - 1, w: item.w + 2, emoji: item.emoji }); else hit.icon = x
      x += item.w + gap
    }
    line.dirty = true
    this.quickHit = hit
  }

  // ---------- message selection ----------

  private select(row: MessageRow | null) {
    this.selected = row
    this.dirtyMessages = true
    if (row) { if (this.focus !== 'messages') this.setFocus('messages') }
    else if (this.focus === 'messages') this.setFocus('input')
  }

  /** Moves the selection to the previous (-1) or next (+1) message; with no selection, ↑ picks the last one; ↓ from the last one goes back to the input. */
  private moveSelection(dir: -1 | 1) {
    if (!this.current || !this.rows.length) return
    const i = this.selected ? this.rows.findIndex(r => r.id === this.selected!.id) : this.rows.length
    // ↑ on the first message drawn: nothing above yet, so older ones are brought in; the selection stays put.
    if (dir === -1 && i === 0) { this.loadOlder(); return void this.screen.render() }
    const next = i + dir
    this.select(next >= this.rows.length ? null : this.rows[Math.max(0, next)]!)
    this.renderNow()
    if (this.selected) this.scrollToSelected()
    this.screen.render()
  }

  /**
   * Selects a message of the open chat and scrolls to it, drawing the stored messages back as far as it when it's
   * older than those in the panel; one that isn't stored (from before what the phone handed over) is only flashed.
   */
  private jumpTo(id: string | null, focus = true) {
    const jid = this.current
    const target = jid && id ? store.getMessage(jid, id) : undefined
    if (!jid || !target) return this.flash(t('messageNotFound'))
    const limit = this.shown.get(jid) ?? PAGE
    const need = store.countMessagesSince(jid, target.ts)
    if (limit !== -1 && need > limit) this.shown.set(jid, need)
    // Without `focus` the selection moves but the keys stay where they were (the search, typing on).
    if (focus) this.select(target)
    else { this.selected = target; this.dirtyMessages = true }
    this.renderNow()
    this.scrollToSelected()
    this.screen.render()
  }

  /** Opens the search in the chat in front: the input becomes its line, with "procurar ❯" as the prompt. */
  private openSearch() {
    this.search = { query: '', cursor: 0, words: [], hits: [], at: 0 }
    this.suggestions = []
    this.drawSuggestions()
    this.ghostBox.hide()
    if (this.selected) { this.selected = null; this.dirtyMessages = true }
    this.setFocus('input')
    this.drawInput()
    this.renderNow()
  }

  /**
   * Keys while searching: typing narrows it, every change finding again in the chat's whole stored history; ↑ goes
   * to an older match and ↓ to a newer one, the message selected and in view, older history drawn as it needs;
   * Enter closes it on the match, left selected; Esc closes it.
   */
  private searchKey(k: string, ch: string, key: blessed.Widgets.Events.IKeyEventArg) {
    const s = this.search!
    if (k === 'escape') return this.closeSearch(false)
    if (k === 'enter' || k === 'return') return this.closeSearch(true)
    if (k === 'up' || k === 'down') {
      const at = s.at + (k === 'up' ? 1 : -1)
      if (at >= 0 && at < s.hits.length) { s.at = at; this.showSearchHit() }
      return
    }
    const chars = graphemes(s.query)
    const e = k === 'paste'
      ? { value: [...chars.slice(0, s.cursor), ...graphemes(ch.replace(/\s+/g, ' ')), ...chars.slice(s.cursor)].join(''), cursor: s.cursor + graphemes(ch.replace(/\s+/g, ' ')).length }
      : edit(s.query, s.cursor, k, ch, key)
    if (!e) return
    s.cursor = e.cursor
    if (e.value !== s.query) {
      s.query = e.value
      s.words = fold(e.value).split(/\s+/).filter(Boolean)
      s.hits = store.searchMessages(e.value, this.current!, 1000)
      s.at = 0
      this.dirtyMessages = true
      if (s.hits.length) return this.showSearchHit()
      this.selected = null
    }
    this.drawInput()
    this.renderNow()
  }

  private showSearchHit() {
    const s = this.search!
    this.jumpTo(s.hits[s.at]!.id, false)
    this.drawInput()
    this.screen.render()
  }

  /** Closes the search: with `stay`, on the match in view, which stays selected for the keys that act on a message. */
  private closeSearch(stay: boolean) {
    const s = this.search!
    this.search = null
    this.dirtyMessages = true
    const hit = stay ? s.hits[s.at] : undefined
    if (hit && this.selected?.id === hit.id) this.setFocus('messages')
    else { this.selected = null; this.setFocus('input') }
    this.drawInput()
    this.renderNow()
  }

  /** Scrolls the panel just enough for the selected message to become fully visible. */
  private scrollToSelected() {
    const id = this.selected?.id
    const first = this.lineMap.findIndex(r => r?.id === id)
    if (first < 0) return
    let last = first
    while (last + 1 < this.lineMap.length && this.lineMap[last + 1]?.id === id) last++
    const ftor = this.msgBox._clines?.ftor
    const top = ftor?.[first]?.[0], bottom = ftor?.[last]?.at(-1)
    if (top == null || bottom == null) return
    const base = this.msgBox.childBase, h = this.innerHeight()
    if (top < base) this.msgBox.scrollTo(top)
    else if (bottom >= base + h) this.msgBox.scrollTo(bottom - h + 1)
  }

  /** Deletes the message the confirmation was for (see the Delete key): for everyone, or for me only. */
  private async deleteMessage(d: { row: MessageRow; forEveryone: boolean }) {
    this.deleting = null
    this.select(null)
    this.drawInput()
    this.renderNow()
    if (this.wa.state !== 'open') return this.flash(t('noConnection'))
    try {
      await this.wa.deleteMessage(d.row.chat_jid, d.row.id, d.forEveryone)
    } catch (e) {
      logger.error({ e }, 'delete')
      this.flash(`${t('error')}: ${(e as Error).message}`, 10000)
    }
  }

  /** Puts one of my own text messages in the input, to correct and resend it as an edit. */
  private editMessage(row: MessageRow) {
    if (!row.from_me || row.type !== 'text') return this.flash(t('onlyOwnText'))
    this.editing = row
    this.replyTo = this.reactTo = null
    this.setFocus('input')
    this.inputValue = row.text.replace(/\n\((editada|edited)\)$/, '')
    this.cursor = graphemes(this.inputValue).length
    this.updateSuggestions()
    this.drawInput()
    this.renderNow()
  }

  private who(row: MessageRow): string {
    return row.from_me ? t('me') : row.chat_jid.endsWith('@g.us') ? contactName(row.sender_jid) : chatName(row.chat_jid)
  }

  private snippet(row: MessageRow): string {
    return row.text.split('\n')[0] || `[${row.type}]`
  }

  private async submit(v: string, draft?: { lines: string[]; top: number }) {
    const text = emojify(v.trim())
    // Reaction in progress: what was typed is the emoji (empty removes the reaction), and it goes to the chosen message.
    const reactTo = this.reactTo
    if (reactTo) {
      this.reactTo = null
      this.drawInput()
      this.screen.render()
      // The ":" the reaction starts with, alone, counts the same as nothing: it removes the reaction. A smiley
      // (":)", "<3") goes as its emoji.
      return this.react(reactTo, text === ':' ? '' : emoticonify(text))
    }
    // Edit in progress: the text replaces the open message's; empty sends nothing and the edit stays open.
    const editing = this.editing
    if (editing) {
      if (!text) { this.drawInput(); return this.screen.render() }
      this.editing = null
      this.drawInput()
      this.screen.render()
      if (this.wa.state !== 'open') return this.flash(t('noConnection'))
      try {
        await this.wa.edit(editing.chat_jid, editing.id, text)
      } catch (e) {
        logger.error({ e }, 'edit')
        this.flash(`${t('error')}: ${(e as Error).message}`, 10000)
      }
      return
    }
    if (!text) return
    if (text.startsWith('/')) return this.openPicker(text.slice(1).trim())
    if (!this.current) return this.flash(t('openFirst'))
    if (this.wa.state !== 'open') return this.flash(t('noConnection'))
    const jid = this.current
    // On sending, the panel jumps to the bottom to show the new message, even if it was looking at history.
    this.atBottom = true
    try {
      // A message that starts with a smiley (":)", ":D") isn't a ":" command.
      if (emoticonify(text).startsWith(':')) return this.flash(`${t('unknownCommand')}: ${text.split(' ')[0]}. ${HELP}`, 10000)
      const replyTo = this.replyTo?.chat_jid === jid ? this.replyTo : null
      this.replyTo = null
      this.drawInput()
      this.screen.render()
      const { text: out, mentions } = this.withMentionIds(jid, text)
      if (draft && !emojiOnly(text)) this.flyDraft(out, draft)
      await this.wa.send(jid, out, replyTo?.id, mentions)
      // My own message only appears once the server echoes it back; the most recent one of mine with the heart is then searched for.
      if (reaction(text)) this.heartFor(r => r.from_me === 1 && r.text === text && Date.now() - r.ts * 1000 < 30000, text)
    } catch (e) {
      logger.error({ e }, 'submit')
      this.flash(`${t('error')}: ${(e as Error).message}`, 10000)
    }
  }

  // ---------- tabs ----------

  /**
   * Each terminal has its own tabs, stored in `state` under the terminal's device (/dev/pts/N). The record also
   * carries the pid and the time of the last interaction: that's how the various processes know, from the database
   * alone, which tabs are open in other live terminals and which terminal was used most recently.
   */
  /** This terminal's record in the database: it only serves to coordinate terminals open at the same time. */
  private tabsKey(): string {
    return `tabs:pid${process.pid}`
  }

  private lastActive = Date.now()
  /**
   * When something was last written in a chat here (typed, deleted, pasted or sent): only writing makes this device
   * show as online, not opening wassup, the mouse, other keys or gaining the focus. 0 while nothing was.
   */
  private lastWrite = 0
  /**
   * Whether this terminal has the focus, from the terminal's focus events (DECSET 1004) or, in Herdr, its pane's
   * state at start; undefined while neither has said, which counts as having it, so a terminal without the events
   * behaves as before. Without the focus, activity here doesn't keep this device online.
   */
  private hasFocus: boolean | undefined
  private lastActiveSaved = 0

  /**
   * Marks the chat as read only while it's in front of you: in Herdr, with its pane focused (a pane it opened in the
   * background for a new message stays unread until you go to it, see focusChanged); elsewhere, unless the terminal
   * said it lost the focus. A read receipt from here tells the phone the chat was seen, and it doesn't notify.
   */
  private markReadIfSeen(jid: string) {
    if (inHerdr ? this.hasFocus !== true : this.hasFocus === false) return
    this.wa.markRead(jid).catch(e => logger.warn({ e }, 'markRead'))
  }
  private lastPresenceTouch = 0
  /** The chat we told "typing" to, when we told it, and the deadline to say we stopped. */
  private composingJid: string | null = null
  private composingSentAt = 0
  private composingTimer: NodeJS.Timeout | undefined

  /** Always starts with no tabs (nothing is restored from previous runs) and clears the records of terminals already dead. */
  private registerTerminal() {
    for (const r of store.listState<TerminalState>('tabs:')) if (!r.value.pid || !pidAlive(r.value.pid)) store.deleteState(r.key)
    this.saveTabs()
  }

  private saveTabs() {
    this.lastActiveSaved = this.lastActive
    store.setState(this.tabsKey(), { tabs: this.tabs, active: this.active, pid: process.pid, lastActive: this.lastActive, herdrTab: process.env.HERDR_TAB_ID, herdrPane: process.env.HERDR_PANE_ID } satisfies TerminalState)
  }

  /**
   * The input changed: the active chat gets told we're typing, repeated every 5 seconds while we keep going, and
   * that we stopped after 5 seconds idle, on sending, on clearing everything, or on switching tabs.
   */
  private noteComposing() {
    const jid = this.current
    if (!jid || !this.inputValue || this.pickerOpen) return this.stopComposing()
    const now = Date.now()
    if (jid !== this.composingJid || now - this.composingSentAt > 5000) {
      if (this.composingJid && jid !== this.composingJid) this.wa.setComposing(this.composingJid, false)
      this.wa.setComposing(jid, true)
      this.composingJid = jid
      this.composingSentAt = now
    }
    if (this.composingTimer) clearTimeout(this.composingTimer)
    this.composingTimer = setTimeout(() => this.stopComposing(), 5000)
  }

  private stopComposing() {
    if (this.composingTimer) { clearTimeout(this.composingTimer); this.composingTimer = undefined }
    if (!this.composingJid) return
    this.wa.setComposing(this.composingJid, false)
    this.composingJid = null
  }

  /** Marks this terminal as the most recently used; saves at most every two seconds. */
  private touchActivity() {
    this.lastActive = Date.now()
    if (this.lastActive - this.lastActiveSaved > 2000) this.saveTabs()
  }

  /** Something was written in a chat: keeps this device "available" while it has the focus; every 10 seconds is enough. */
  private noteWriting() {
    this.lastWrite = Date.now()
    // Writing in a chat is having seen it: what arrived while you weren't online is read then, as often as the presence.
    if (this.hasFocus !== false && this.lastWrite - this.lastPresenceTouch > 10000) {
      this.lastPresenceTouch = this.lastWrite
      this.wa.touchPresence(process.pid)
      if (this.current) this.markReadIfSeen(this.current)
    }
  }

  /** Records of the other terminals whose process is still alive. */
  private otherTerminals(): TerminalState[] {
    const mine = this.tabsKey()
    return store.listState<TerminalState>('tabs:').filter(r => r.key !== mine && r.value.pid && pidAlive(r.value.pid)).map(r => r.value)
  }

  private openElsewhere(jid: string): boolean {
    return this.otherTerminals().some(t => t.tabs.includes(jid))
  }

  private isMostRecentTerminal(): boolean {
    return this.otherTerminals().every(t => (t.lastActive ?? 0) <= this.lastActive)
  }

  /** Opens (or finds) the chat's tab; with `activate` it becomes the active one and the chat is marked as read. */
  private openTab(jid: string, activate = true) {
    let i = this.tabs.indexOf(jid)
    if (i < 0) { this.tabs.push(jid); i = this.tabs.length - 1 }
    uiLog.info({ jid, index: i, activate }, 'open tab')
    this.dirtyTabs = true
    if (activate) this.activateTab(i)
    else { this.saveTabs(); this.scheduleRender() }
  }

  /** Activates the tab without touching the bar's order; it's typing that brings it to the front (promoteActive). */
  private activateTab(i: number) {
    const jid = this.tabs[i]
    if (!jid) return
    if (i !== this.active) {
      this.search = null
      this.stopComposing()
      const prev = this.current
      if (prev) this.shown.delete(prev)
      this.active = i; this.atBottom = true; this.dirtyMessages = true; this.selected = this.replyTo = this.reactTo = null; this.quickFor = undefined
      // A message correction isn't a draft: it's dropped. Everything else stays saved in the chat being left.
      if (this.editing) { this.editing = null; this.inputValue = ''; this.cursor = 0 }
      this.switchDraft(prev, jid)
    }
    if (this.notice?.jid === jid) this.notice = undefined
    this.dirtyTabs = true
    this.saveTabs()
    this.wa.subscribePresence(jid)
    this.wa.ensureName(jid)
    if (this.pickerOpen) this.closePicker(false)
    this.setFocus('input')
    this.renderNow()
    this.markReadIfSeen(jid)
  }

  /** Moves the active tab to the first position, next to the input, when typing starts in it. */
  private promoteActive() {
    if (this.active <= 0 || !this.tabs[this.active]) return
    const [jid] = this.tabs.splice(this.active, 1)
    this.tabs.unshift(jid!)
    this.active = 0
    this.dirtyTabs = true
    this.saveTabs()
    this.drawTabs()
  }

  /** Saves the input as a draft of the chat being left and puts the draft of the chat being entered on the line. */
  private switchDraft(from: string | null, to: string | null) {
    if (from) {
      if (this.inputValue) this.drafts.set(from, { value: this.inputValue, cursor: this.cursor })
      else this.drafts.delete(from)
    }
    const d = to ? this.drafts.get(to) : undefined
    this.inputValue = d?.value ?? ''
    this.cursor = d?.cursor ?? 0
    this.updateSuggestions()
  }

  /** Closes the tab; if it was the active one, moves to the one on the right, or the one on the left, or to "chats". */
  private closeTab(i: number) {
    const closing = this.tabs[i]
    if (!closing) return
    uiLog.info({ jid: closing, index: i }, 'close tab')
    const wasActive = this.active === i
    if (wasActive) this.search = null
    this.tabs.splice(i, 1)
    if (this.active > i) this.active--
    else if (wasActive) { this.active = Math.min(i, this.tabs.length - 1); this.atBottom = true }
    // The draft goes with the tab; if it was the active one, the input becomes that of the remaining chat.
    this.drafts.delete(closing)
    if (wasActive) { this.editing = null; this.switchDraft(null, this.current) }
    this.dirtyTabs = true
    this.dirtyMessages = true
    this.lineMap = []; this.images = []; this.rows = []; this.selected = null
    this.saveTabs()
    // Closing the last tab goes back to the chat list, on the chat closed, where Esc, with no tab left, quits. In
    // single-chat mode (Herdr's panes, each a chat, and `wa <name>`) closing the chat quits, its pane with it.
    if (!this.tabs.length) return this.fixed ? this.quit() : this.openPicker('', closing)
    // The prompt takes the name of the chat now in front.
    this.drawInput()
    this.renderNow()
    const jid = this.current
    if (jid) this.markReadIfSeen(jid)
  }

  /**
   * The tab bar, under the input: one tab per open chat (in single-chat mode, that one), its photo (tabPhotos), its
   * name and its state's emoji (moodOf), or in its place two emoji taking turns while someone types there, or 🔔
   * with messages unread. The active one on my bubbles' green, as the band it's the label of, its name in its colour,
   * bold; the others on the other side's grey, quieter. The one under the pointer shows a faint ✖ in its state's
   * place, which closes it, as Esc closes the active one. With the chat list open, its own tab comes first and is the
   * active one, "wassup" (or "procurar", searching them all), its ✖ closing the list. Names
   * shorten for the tabs to fit, down to 6 characters. Against the right edge, a passing message or the connection's
   * state when it needs attention.
   */
  private drawTabs() {
    const width = num(this.tabsBar.width)
    const list = this.fixed ? (this.current ? [this.current] : []) : this.tabs
    const activeAt = this.pickerOpen ? -1 : this.fixed ? 0 : this.active
    // While someone types there, the two emoji taking turns (typingEmoji) stand where the state's is; with no state
    // (yet), its place is kept blank, so the tab doesn't grow when one comes.
    const tabs = list.map((jid, i) => {
      const chat = store.getChat(jid)
      // With messages unread, 🔔 in the state's place (while someone types there, the typing emoji still win).
      const mood = this.typing.has(jid) ? ` ${typingEmoji()}` : chat?.unread ? ' 🔔' : this.moodOf(jid, chat?.last_ts) || '   '
      // Under the pointer, a faint ✖ to close it where the state's emoji is, as wide (tabHover).
      const closing = i === this.tabHover && !this.fixed
      return { jid, i, name: chatName(jid), mood: closing ? ' ✖'.padEnd(Math.max(2, strWidth(mood))) : mood, closing }
    })
    // With the chat list open, its own tab comes first, active: the band is its filter's. After its name, the place
    // of an emoji: the two taking turns while someone types in any chat (typingEmoji); otherwise 🔔 with messages
    // unread in any, as a tab has it; and under the pointer a ✖ that closes the list, as Esc does. With nothing to
    // show, the app's own, 👾, the tab keeping its width.
    const own = this.pickerOpen ? (this.pickerSearch ? t('search') : APP) : ''
    const ownClose = !!own && this.tabHover === -1
    const ownW = own ? strWidth(own) + 3 + 3 : 0
    // Each tab: a cell either side of its text, and a cell between tabs; the first starts where the band does.
    // Each one's photo, before its name, once it has one (its cells kept while it's decoded, so nothing moves).
    const photoOf = (jid: string) => {
      const avatar = this.mode === 'none' ? undefined : (this.profiles.get(jid) ?? store.getProfile(jid))?.avatar
      return avatar ? path.join(mediaDir(jid), avatar) : null
    }
    const photos = new Map(tabs.map(t => [t.i, photoOf(t.jid)]))
    this.tabPhotos = []
    const overhead = (t: (typeof tabs)[number]) => 2 + strWidth(t.mood) + 1 + (photos.get(t.i) ? 3 : 0)
    let nameW = Math.max(...tabs.map(t => strWidth(t.name)), 0)
    const fits = (w: number) => 1 + ownW + tabs.reduce((sum, t) => sum + Math.min(strWidth(t.name), w) + overhead(t), 0) <= width
    while (nameW > 6 && !fits(nameW)) nameW--
    let out = '', x = 0
    this.segments = []
    this.tabCloses = []
    if (own) {
      const bg = this.bubbleBg.mine
      const strong = this.dark ? 'bright-white' : 'black'
      const unread = store.listChats().some(c => c.unread > 0 && !c.archived)
      const close = ownClose ? ` ${dim('✖')} ` : this.typing.size ? ` ${typingEmoji()}` : unread ? ' 🔔' : ' 👾'
      out += ` {${bg}-bg} {${strong}-fg}{bold}${esc(own)}{/bold}{/${strong}-fg}${close} {/${bg}-bg}`
      this.segments.push({ x0: 1, x1: ownW, index: -1 })
      if (ownClose) this.tabCloses.push({ x: 2 + strWidth(own) + 1, index: -1 })
      x = ownW
    }
    for (const t of tabs) {
      const name = truncate(t.name, nameW)
      const active = t.i === activeAt
      const bg = active ? this.bubbleBg.mine : this.bubbleBg.theirs
      const color = active ? colorFor(t.jid) : FG.tab
      const lead = ` {${bg}-bg} `
      const label = active ? `{bold}${esc(name)}{/bold}` : esc(name)
      // The photo, two cells and one of gap after the tab's spare cell: blank where Kitty places it, half-blocks
      // otherwise (their full reset at the end, the tab's background opened again after it).
      let photo = ''
      const photoFile = photos.get(t.i)
      if (photoFile) {
        const d = thumbCached(photoFile)
        if (d && !(d instanceof Error)) {
          this.tabPhotos.push({ jid: t.jid, path: `${photoFile}#small`, d, x: x + 2 })
          photo = `${this.mode === 'kitty' ? '  ' : halfBlocks(d, 2, 1)[0]!}\x1b[48;5;${bg}m `
        } else {
          photo = '   '
          if (!d && !this.thumbWaiting.has(photoFile)) {
            this.thumbWaiting.add(photoFile)
            decodeThumb(photoFile).then(() => { this.thumbWaiting.delete(photoFile); this.dirtyTabs = true; this.scheduleRender() })
          }
        }
      }
      const w = 2 + (photo ? 3 : 0) + strWidth(name) + strWidth(t.mood)
      this.segments.push({ x0: x + 1, x1: x + 1 + w, index: t.i })
      if (t.closing) this.tabCloses.push({ x: x + 2 + (photo ? 3 : 0) + strWidth(name) + 1, index: t.i })
      const mood = t.closing ? t.mood.replace('✖', dim('✖')) : t.mood
      out += `${lead}${photo}{${color}-fg}${label}{/${color}-fg}${mood} {/${bg}-bg}`
      x += 1 + w
    }
    // Status flush right: the transient message (yellow) or the connection; truncated if it doesn't fit.
    // Connected isn't announced: only transient notices and states that need attention (QR, connection dropped).
    const avail = width - x - 2
    const text = this.transient ? dim(esc(truncate(this.transient, Math.max(1, avail)))) : this.connText
    if (avail >= 6 && text) out += ' '.repeat(Math.max(1, width - x - visibleWidth(text) - 1)) + text
    this.tabsBar.setContent(out)
    this.drawNotice(width)
  }

  /**
   * Lays the notice over the chat's tab (or flush right if the tab isn't in view), without its name and without a
   * background: the text emerges from the background up to a tone a bit below normal text, stays, then merges back
   * into the background. The color at each instant is the background→text blend at the moment's opacity, quantized
   * to 256 colors.
   */
  private drawNotice(width: number) {
    const n = this.notice
    if (!n) return this.toast.hide()
    const text = truncate(n.text, Math.max(1, width - 2))
    const w = strWidth(text) + 2
    const seg = this.segments.find(s => this.tabs[s.index] === n.jid)
    const left = Math.max(0, Math.min(seg && seg.x0 < width ? seg.x0 : width, width - w))
    this.toast.top = '100%-1'
    this.toast.left = left
    this.toast.width = Math.min(w, width)
    const c = nearest256(mix(this.bgRgb, this.fgRgb, 0.85 * this.noticeOpacity(n.since)))
    this.toast.setContent(`{${c}-fg} ${esc(text)} {/${c}-fg}`)
    this.toast.show()
  }

  /** Notice opacity (0..1) since it started: rises, holds, falls; smooth curve at both ends. */
  private noticeOpacity(since: number): number {
    const t = Date.now() - since
    const ease = (x: number) => x * x * (3 - 2 * x)
    if (t < NOTICE.fadeIn) return ease(t / NOTICE.fadeIn)
    if (t < NOTICE.fadeIn + NOTICE.hold) return 1
    return ease(Math.max(0, 1 - (t - NOTICE.fadeIn - NOTICE.hold) / NOTICE.fadeOut))
  }

  private notify(jid: string, text: string) {
    // A notice on top of another already visible one doesn't fade in again: it stays opaque with the new text.
    const since = this.notice && this.noticeOpacity(this.notice.since) >= 1 ? Date.now() - NOTICE.fadeIn : Date.now()
    this.notice = { jid, text, since }
    if (!this.noticeTimer) {
      this.noticeTimer = setInterval(() => {
        if (this.notice && Date.now() - this.notice.since >= NOTICE.fadeIn + NOTICE.hold + NOTICE.fadeOut) this.notice = undefined
        if (!this.notice && this.noticeTimer) { clearInterval(this.noticeTimer); this.noticeTimer = undefined }
        this.drawStatus(); this.screen.render()
      }, 40)
    }
    this.drawStatus()
    this.screen.render()
  }

  // ---------- picker ----------

  /** The chat list, with `filter` typed in it; `from`, the chat you come from, if not the one in front (closeTab). */
  private openPicker(filter = '', from = this.current) {
    this.pickerSearch = false
    this.filter = filter
    this.filterCursor = graphemes(filter).length
    this.pickerOpen = true
    this.dirtyTabs = true
    this.picker.show()
    this.pickerHead.show()
    this.msgBox.hide()
    // Back within 30 s of leaving it, the list opens on the chat you come from, selected and in view however far up it
    // is, on the same row of the screen when it's the one it was left on; later, on the most recent ones, at the
    // bottom, none selected till the keys move the selection (pickerChosen).
    const back = Date.now() - this.pickerClosedAt < 30000
    this.pickerPicked = back && !!from
    this.pickerFocus = back ? from : null
    this.pickerFocusRow = back && from && this.pickerLeft?.jid === from ? this.pickerLeft.row : undefined
    this.refreshPicker()
    this.setFocus('picker')
    this.renderNow()
  }

  private closePicker(render = true) {
    this.pickerOpen = false
    this.pickerClosedAt = Date.now()
    const list = this.picker as unknown as { selected: number; childBase: number }
    const left = this.pickerSlots[list.selected]?.jid
    this.pickerLeft = left ? { jid: left, row: list.selected - list.childBase } : undefined
    this.dirtyTabs = true
    this.filter = ''
    this.picker.hide()
    this.pickerHead.hide()
    this.msgBox.show()
    this.setFocus('input')
    if (render) this.renderNow()
  }

  private findChat(text: string): string | null {
    if (store.getChat(text)) return text
    const f = fold(text)
    return store.listChats().find(c => fold(chatName(c.jid)).includes(f) || jidUser(c.jid).includes(f))?.jid ?? null
  }

  /**
   * The chat chosen in the list. `how` only matters in Herdr: 'here' (Enter) replaces this pane's chat, 'pane' (→)
   * and 'tab' (Tab) open it in a new pane or tab; all three move the focus to where it already is, if it is.
   */
  private pickChat(index: number, how: 'here' | 'pane' | 'tab' = 'here') {
    const jid = this.pickerSlots[index]?.jid
    uiLog.info({ index, jid, how }, 'pick chat')
    // A message found opens its chat here, on that message, selected.
    const hit = this.pickerHits.get(index)
    if (hit) { this.openChat(hit.chat_jid); if (this.current === hit.chat_jid) this.jumpTo(hit.id); return }
    if (jid) this.openChat(jid, how)
  }

  /** The messages in every chat with each of the filter's words, the most recent first; found again only when it changes. */
  private searchAll(filter: string): MessageRow[] {
    const query = filter.trim()
    if (this.pickerFound?.query !== query) this.pickerFound = { query, rows: store.searchMessages(query, null, 100).reverse() }
    return this.pickerFound.rows
  }

  /**
   * A message found, as rows of the list: its chat's name in its colour, who wrote it and, at the right edge, when;
   * then up to two lines of its text, indented, from a little before the first match, the matches in reverse video.
   */
  private pickerHitRows(m: MessageRow, c: ChatRow, width: number, slot: number): [string, [string, boolean][][]] {
    const color = colorFor(c.jid)
    const who = m.from_me ? t('me') : c.is_group ? contactName(m.sender_jid) : ''
    const right = faint(esc(fmtWhen(m.ts)))
    const end = this.pickerBubbleEnd(width)
    const head = `{${color}-fg}${esc(truncate(chatName(c.jid), Math.max(8, end - 4)))}{/${color}-fg}${who ? dim(` · ${esc(truncate(who, 24))}`) : ''}`
    const indent = '    ', room = Math.max(8, end - indent.length)
    let text = showLinks(withMentions(m.text)).replace(/\s+/g, ' ')
    // The first match stays in view: past a third of the first line, the text starts a little before it, after "…".
    const first = Math.min(...matchedChars(text, this.pickerWords))
    if (Number.isFinite(first) && first > room / 3) text = `…${[...text].slice(first - Math.floor(room / 3)).join('')}`
    const lines = wrapChars(graphemes(text), room).map(l => l.join('').trim()).filter(Boolean)
    if (lines.length > 2) lines[1] = truncate(`${lines[1]} ${lines[2]}`, room)
    const marked = (line: string) => {
      const at = matchedChars(line, this.pickerWords)
      let out = '', on = false
      ;[...line].forEach((ch, i) => { if (at.has(i) !== on) { out += on ? '{/inverse}' : '{inverse}'; on = !on }; out += esc(ch) })
      return on ? `${out}{/inverse}` : out
    }
    const body = `${head}${' '.repeat(Math.max(1, end - visibleWidth(head)))}`
    const time = `${' '.repeat(Math.max(1, width - end - visibleWidth(right)))}${right}`
    this.pickerHeads.set(slot, [body, time])
    return [`${body}${time}`, lines.slice(0, 2).map(l => [[indent, true], [marked(l), true]] as [string, boolean][])]
  }

  /** Opens the chat the way the list does: in this pane or tab ('here'), or, in Herdr, in a new pane or tab. */
  private openChat(jid: string, how: 'here' | 'pane' | 'tab' = 'here') {
    if (inHerdr && jid !== this.current) {
      const other = this.otherTerminals().find(t => t.herdrTab && t.tabs.includes(jid))
      if (other?.herdrTab) { this.closePicker(); return focusHerdr(other.herdrTab, other.herdrPane) }
      if (how !== 'here') { this.closePicker(); return void openChatHerdr(jid, true, how).catch(e => logger.warn({ e }, 'herdr: open chat')) }
    }
    // In single-chat mode the picker switches the chat instead of adding a tab; the previous one's draft stays saved.
    const prev = this.fixed ? this.current : null
    this.openTab(jid)
    if (prev && prev !== jid) {
      this.tabs = [jid]; this.active = 0
      this.dirtyTabs = true
      this.saveTabs()
      this.renderNow()
    }
  }

  private refreshPicker() {
    // Keep the selection on the same chat: WhatsApp events redraw the list all the time and used to reset it to the top.
    // On a message found, on that message.
    const selectedAt = (this.picker as unknown as { selected: number }).selected
    const selectedHit = this.pickerHits.get(selectedAt)?.id
    const selectedJid = selectedHit ? undefined : this.pickerSlots[selectedAt]?.jid
    const sameFilter = this.pickerFilterShown === this.filter
    // Made anew while open with the same filter (a message arriving, the history coming in), the list stays where
    // it's being looked at: at the bottom if it was there, otherwise with its first chat in view on the same row,
    // rather than scrolled back to the selection, which the wheel may have left out of view.
    const list = this.picker as unknown as { childBase: number; childOffset: number }
    const keepView = sameFilter && this.pickerFocus === undefined && this.pickerSlots.length > 0
    const atEnd = list.childBase + num(this.picker.height) - num(this.picker.iheight) >= this.pickerSlots.length
    let anchor: { jid: string; row: number } | undefined
    for (let i = list.childBase; keepView && !anchor && i < this.pickerSlots.length; i++) {
      const c = this.pickerSlots[this.pickerCont.get(i) ?? i]
      if (c && !this.pickerHits.has(i)) anchor = { jid: c.jid, row: (this.pickerCont.get(i) ?? i) - list.childBase }
    }
    this.pickerFilterShown = this.filter
    // Most recent at the bottom, like the messages; the default selection is the last one (the most recent).
    this.chats = store.listChats().filter(c => !c.archived).reverse()
    const f = fold(this.filter)
    // The filter is taken word by word, every one of them somewhere in the name (or the number), in any order:
    // "irene viegas" finds Irene Teles Viegas. The best matches go to the bottom, next to the prompt and the default
    // selection: names that start with the first word and have each other one starting a word ("ana": Ana Viegas);
    // above them, names where every word starts one ("viegas": Ana Viegas, after a space, hyphen or any non-letter);
    // on top, the rest (Mariana, a number). Each group keeps its order, most recent at the bottom.
    const words = f.split(/\s+/).filter(Boolean)
    const matches = this.chats.filter(c => {
      const n = fold(chatName(c.jid)), u = jidUser(c.jid)
      return words.every(w => n.includes(w) || u.includes(w))
    })
    const starts = words.map(w => new RegExp(`(?:^|[^\\p{L}\\p{N}])${w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`, 'u'))
    const rank = (c: ChatRow) => {
      const n = fold(chatName(c.jid))
      if (!starts.every(re => re.test(n))) return 0
      return n.startsWith(words[0]!) ? 2 : 1
    }
    this.filtered = words.length ? [0, 1, 2].flatMap(r => matches.filter(c => rank(c) === r)) : this.chats
    this.pickerWords = words
    const width = num(this.picker.width) - num(this.picker.iwidth) - 1
    // Without a filter the chats go under a separator for each day, as the messages do ("hoje", "ontem", "12 set"),
    // the most recent at the bottom, each with just its time. A filter orders them by how well they match, which no
    // separator would follow.
    const bucket = (ts: number) => fmtDay(ts)
    const slots: (ChatRow | null)[] = [], items: string[] = []
    // Searching messages (Ctrl+F), from two characters on, the list holds only the messages found in every chat, the
    // most recent at the bottom, each on its first row (selectable) and the rows of text after it (skipped over).
    this.pickerHits.clear()
    this.pickerCont.clear()
    this.pickerParts.clear()
    this.pickerHeads.clear()
    this.pickerAvatars.clear()
    this.pickerLit = []
    this.profiles = new Map(store.listProfiles().map(p => [p.jid, p]))
    this.moods = this.loadMoods()
    this.business = this.loadBusiness()
    const hits = this.pickerSearch && this.filter.trim().length >= 2 ? this.searchAll(this.filter) : []
    this.pickerTimeCols = words.length || hits.length ? 12 : 5
    for (const m of hits) {
      const chat = store.getChat(m.chat_jid)
      if (!chat) continue
      const start = slots.length, rows = this.pickerHitRows(m, chat, width, start)
      this.pickerHits.set(start, m)
      slots.push(chat); items.push(rows[0])
      for (const part of rows[1]) { this.pickerCont.set(slots.length, start); this.pickerParts.set(slots.length, part); slots.push(null); items.push(part.map(([text]) => text).join('')) }
    }
    let lastBucket = ''
    this.pickerLast.clear()
    // A blank row between chats, but for the first one and the one right under a day's separator.
    let fresh = true
    for (const c of this.pickerSearch ? [] : this.filtered) {
      const b = bucket(c.last_ts)
      if (!words.length && b !== lastBucket) {
        lastBucket = b
        slots.push(null)
        // Centred, as the days in the chat are.
        const label = `${this.ruleChar.repeat(2)} ${b} ${this.ruleChar.repeat(2)}`
        items.push(dim(esc(`${' '.repeat(Math.max(0, Math.floor((width - strWidth(label)) / 2)))}${label}`)))
        fresh = true
      }
      if (!fresh) { slots.push(null); items.push('') }
      fresh = false
      this.pickerLast.set(c.jid, store.lastMessage(c.jid))
      // Two rows each: the first is the one selected, the second is skipped over.
      this.pickerCont.set(slots.length + 1, slots.length)
      slots.push(c, null)
      items.push('', '')
    }
    this.pickerSlots = slots
    slots.forEach((c, i) => {
      if (!c || this.pickerHits.has(i)) return
      const [first, second] = this.pickerRows(c, width, i)
      items[i] = first; items[i + 1] = second.map(([text]) => text).join(''); this.pickerParts.set(i + 1, second)
    })
    this.pickerItems = items
    this.picker.setItems(items as unknown as string[])
    // List flush to the bottom when it's shorter than the panel, with a blank line separating it from the prompt.
    // Never shorter than one line: blessed skips an element of zero height altogether, leaving what was drawn there
    // and the list's scroll state stale until the next refresh. The app's name heads it always, however small the
    // terminal: big (WORDMARK) with a UTF-8 locale, on one line otherwise; the list takes what's left under it.
    const panel = num(this.screen.height) - this.bottom - 1
    const rows = Math.max(1, items.length)
    const big = this.ruleChar === '─'
    const head = big ? WORDMARK.length : 1
    const room = Math.max(1, panel - head)
    const gap = Math.max(0, room - rows)
    this.picker.top = head + gap
    this.picker.height = Math.max(1, room - gap)
    this.drawPickerHead(big)
    const focusing = this.pickerFocus !== undefined
    const want = focusing ? this.pickerFocus : sameFilter ? selectedJid : null
    this.pickerFocus = undefined
    const keep = selectedHit && sameFilter ? [...this.pickerHits].find(([, m]) => m.id === selectedHit)?.[0] ?? -1
      : want ? slots.findIndex((c, i) => c?.jid === want && !this.pickerHits.has(i)) : -1
    // The chat you came back from, should it not be in the list, leaves the most recent unselected.
    if (focusing && keep < 0) this.pickerPicked = false
    // By default the last row that can be selected: a message found ends in rows of its text.
    let last = slots.length - 1
    while (last > 0 && !slots[last]) last--
    this.pickerAt = keep >= 0 ? keep : Math.max(0, last)
    // Lit here too: blessed says nothing when the selection stays on the same row, the rows anew all the same.
    this.picker.select(this.pickerAt)
    this.lightPickerRows(this.pickerAt)
    // The chat goes back to the row of the screen it was on, as far as the list's ends allow.
    const visible = num(this.picker.height) - num(this.picker.iheight)
    if (this.pickerFocusRow !== undefined && keep >= 0) {
      const base = Math.max(0, Math.min(keep - this.pickerFocusRow, slots.length - visible))
      list.childBase = base
      list.childOffset = keep - base
    } else if (keepView) {
      const at = anchor ? slots.findIndex((c, i) => c?.jid === anchor.jid && !this.pickerHits.has(i)) : -1
      const base = atEnd || at < 0 ? Math.max(0, slots.length - visible) : Math.max(0, Math.min(at - anchor!.row, slots.length - visible))
      list.childBase = base
      list.childOffset = this.pickerAt - base
    }
    this.pickerFocusRow = undefined
    this.drawInput()
  }

  /**
   * A chat's two rows in the list, the photo (avatarCells) in the first four cells of both. On the first, its name in
   * the colour it has as a sender in groups (bold with unread messages, the filter's words underlined), its state's
   * emoji (moodOf), or in its place, while someone types there, the two emoji taking turns (typingEmoji), the unread
   * count in red, 👀 while the person is online, "·" when it has a tab, and at the right edge
   * the time of the last message, its day being its separator's (with a filter, the day too, fmtWhenAt). On the
   * second, under the name, the person's number or the group's size (chatNumber). The second row comes in its parts
   * (see pickerParts).
   */
  private pickerRows(c: ChatRow, width: number, slot: number): [string, [string, boolean][]] {
    const [top, bottom] = this.avatarCells(c, slot)
    const color = colorFor(c.jid)
    const eyes = !c.is_group && this.online.has(c.jid) && this.ruleChar === '─' ? ' 👀' : ''
    const open = this.tabs.includes(c.jid) ? ' ·' : ''
    const ts = this.pickerLast.get(c.jid)?.ts ?? c.last_ts
    // Just the time under its day's separator, as in a chat; with a filter, which has no separators, the day too.
    const when = !ts ? '' : faint(esc(!this.pickerWords.length && dayKey(ts) === dayKey(c.last_ts) ? fmtTime(ts) : fmtWhenAt(ts)))
    const mood = this.typing.has(c.jid) ? ` ${typingEmoji()}` : this.moodOf(c.jid, c.last_ts)
    const badge = c.unread > 0 ? ` (${c.unread})` : ''
    // The time flush right, in the column my messages' times end in, outside the selection's green, a cell of gap
    // before it, as a message's is.
    const end = this.pickerBubbleEnd(width)
    const name = truncate(chatName(c.jid), Math.max(4, end - 4 - 1 - 1 - strWidth(mood) - strWidth(badge) - strWidth(eyes) - strWidth(open)))
    const named = `{${color}-fg}${this.underlineMatches(name)}{/${color}-fg}`
    const unread = badge ? `{${FG.badge}-fg}{bold}${badge}{/bold}{/${FG.badge}-fg}` : ''
    const left = `${top} ${c.unread > 0 ? `{bold}${named}{/bold}` : named}${mood}${unread}${eyes}${dim(open)}`
    const body = `${left}${' '.repeat(Math.max(1, end - visibleWidth(left)))}`
    const time = `${' '.repeat(Math.max(1, width - end - visibleWidth(when)))}${when}`
    this.pickerHeads.set(slot, [body, time])
    const first = `${body}${time}`
    const number = this.chatNumber(c)
    return [first, [[bottom, false], [` ${number ? dim(esc(number)) : ''}`, true]]]
  }

  /**
   * A chat's picture as two rows of four cells: its picture from WhatsApp in half-blocks, or blank cells where Kitty
   * places it (placeImages), remembered by row; without one, its initials on the chat's colour.
   */
  private avatarCells(c: ChatRow, slot: number): [string, string] {
    const avatar = this.profiles.get(c.jid)?.avatar
    const file = avatar ? path.join(mediaDir(c.jid), avatar) : null
    const d = file ? thumbCached(file) : undefined
    if (file && d && !(d instanceof Error) && this.mode !== 'none') {
      // Kitty keeps images by this key: the small copy's own, apart from the big one the popup shows.
      this.pickerAvatars.set(slot, { path: `${file}#small`, d })
      if (this.pics) return ['    ', '    ']
      const [top, bottom] = halfBlocks(d, 4, 2)
      return [top!, bottom!]
    }
    const words = chatName(c.jid).split(/\s+/).filter(w => /^\p{L}/u.test(w) && !/^(de|da|do|das|dos|e)$/i.test(w))
    const initials = (words.length ? words.slice(0, 2).map(w => [...w][0]!.toUpperCase()).join('') : '#').padEnd(2)
    const color = colorFor(c.jid)
    return [`{${color}-bg}{bold} ${esc(initials)} {/bold}{/${color}-bg}`, `{${color}-bg}    {/${color}-bg}`]
  }

  /** The chats whose number the server found to be a WhatsApp Business account. */
  private loadBusiness(): Set<string> {
    return new Set(store.listState<boolean>('biz:').filter(r => r.value).map(r => r.key.slice('biz:'.length)))
  }

  /** The chats' states the server keeps (mood.ts), by chat. */
  private loadMoods(): Map<string, string> {
    return new Map(store.listState<Mood>('mood:').map(r => [r.key.slice('mood:'.length), r.value.emoji]))
  }

  /** A chat's state, " " and its emoji, while its last message (at `lastTs`) is no older than a week; otherwise none. */
  private moodOf(jid: string, lastTs: number | undefined): string {
    const emoji = this.moods.get(jid)
    return emoji && lastTs && Date.now() / 1000 - lastTs <= MOOD_AGE ? ` ${emoji}` : ''
  }

  /**
   * What the list says of a chat under its name: for a person, their number (by the number a lid stands for when
   * known; nothing with only a lid), without the country code when it's the same as mine, and 💼 after it when it's a
   * WhatsApp Business account; for a group, how many members it has.
   */
  private chatNumber(c: ChatRow): string {
    if (c.is_group) { const p = this.profiles.get(c.jid); return p?.members != null ? t('groupMembers', p.members) : t('groupOnly') }
    const pn = c.jid.endsWith('@lid') ? store.getPn(c.jid) : c.jid
    const digits = pn ? jidUser(pn) : ''
    if (!digits) return ''
    const code = callingCode(digits), national = digits.slice(code.length)
    const pt = code === '351' && national.length === 9 ? `${national.slice(0, 3)} ${national.slice(3, 6)} ${national.slice(6)}` : ''
    const number = code === callingCode(jidUser(this.myJid)) ? pt || national : pt ? `+351 ${pt}` : `+${digits}`
    return this.business.has(c.jid) ? `${number} 💼` : number
  }

  /** Whether a column of the list's rows (from the list's text start) is on a chat's photo: its first four. */
  private onPhoto(col: number): boolean {
    return col >= 0 && col < 4
  }

  /**
   * A row that continues the selected one, with the selection's background over its text up to the row's last cell,
   * as far as the list paints the selected row itself (a cell past the width the rows' text is laid out to).
   */
  private litRow(i: number): string {
    const parts = this.pickerParts.get(i) ?? [[this.pickerItems[i] ?? '', true]]
    const width = this.pickerBubbleEnd(num(this.picker.width) - num(this.picker.iwidth) - 1)
    const bg = this.bubbleBg.mine
    const on = (s: string) => `{${bg}-bg}${s}{/${bg}-bg}`
    const pad = ' '.repeat(padding(parts.map(([text]) => text).join(''), width))
    return parts.map(([text, lit]) => (lit ? on(text) : text)).join('') + (pad ? on(pad) : '')
  }

  /**
   * The selected row as the list shows it: without the colours its text was given in tags (a chat's name in its own),
   * so the selection's, the theme's strongest, takes them; a photo's colours, written as SGR, stay.
   */
  private selectedRow(i: number): string {
    const strip = (s: string) => s.replace(/\{\/?\d+-fg\}/g, '')
    const head = this.pickerHeads.get(i)
    if (!head) return strip(this.pickerItems[i] ?? '')
    // The green opened again after the photo, whose half-blocks end in a full reset and whose initials' colour ends in
    // the default background, and closed before the time.
    const on = `\x1b[48;5;${this.bubbleBg.mine}m`
    return `${on}${strip(head[0]).replace(/\x1b\[0m|\{\/\d+-bg\}/g, m => `${m}${on}`)}\x1b[49m${head[1]}`
  }

  /**
   * Where the selection's green ends in a row laid out to `width`, short of the time's columns and a cell of gap; the
   * time ends a column short of the list's edge, in the column my messages' times end in, their mark's cell after.
   */
  private pickerBubbleEnd(width: number): number {
    return width - this.pickerTimeCols - 1
  }

  /**
   * The rows that continue the one selected get its background; those that had it go back to their own look. With no
   * chat chosen yet (pickerChosen), none has it.
   */
  private lightPickerRows(i: number) {
    for (const j of this.pickerLit) this.picker.setItem(j as unknown as blessed.Widgets.BlessedElement, this.pickerItems[j] ?? '')
    // A list with nothing in it (a search yet to find anything) has no row to light.
    if (!this.pickerSlots[i]) { this.pickerLit = []; return }
    const own = [i]
    for (let j = i + 1; this.pickerCont.get(j) === i; j++) own.push(j)
    this.pickerLit = this.pickerChosen ? own : []
    for (const j of this.pickerLit) this.picker.setItem(j as unknown as blessed.Widgets.BlessedElement, j === i ? this.selectedRow(i) : this.litRow(j))
    // Its rows stay in view with it: blessed only keeps the selected one, which at the bottom left them out.
    const list = this.picker as unknown as { childBase: number; childOffset: number }
    const rows = num(this.picker.height) - num(this.picker.iheight), last = own.at(-1)!
    if (last >= list.childBase + rows) { list.childBase = last - rows + 1; list.childOffset = i - list.childBase }
  }

  /**
   * After each frame with the list open: the profiles of the chats in view are asked for (once each per session; the
   * server asks WhatsApp at most once a day), and their photos' small copies decoded (decodeThumb, kept for the
   * session), the rows drawn again once they are; the most recent chats first, from the bottom of the list up.
   */
  private loadVisibleProfiles() {
    if (!this.pickerOpen || this.pickerSearch) return
    const base = (this.picker as unknown as { childBase: number }).childBase
    const rows = num(this.picker.height) - num(this.picker.iheight)
    for (let i = Math.min(this.pickerSlots.length, base + rows) - 1; i >= base; i--) {
      const c = this.pickerSlots[i]
      if (!c || this.pickerHits.has(i)) continue
      // Only once there's a connection to ask through: asked before, at startup, the request was lost and the chat
      // taken as asked, and only the chats scrolled to later got their photos.
      if (!this.profileAsked.has(c.jid) && this.wa.state === 'open') { this.profileAsked.add(c.jid); this.wa.ensureProfile(c.jid) }
      const avatar = this.profiles.get(c.jid)?.avatar
      const file = avatar ? path.join(mediaDir(c.jid), avatar) : null
      // Each photo is waited for once: asked again at every frame while it decoded, each frame added a redraw for when
      // it was done, all drawn at once then, each adding more to the photos still decoding, a storm that kept the
      // client busy while the list was open. The redraw is a scheduled one, not a frame drawn on the spot.
      if (file && !thumbCached(file) && !this.thumbWaiting.has(file)) {
        this.thumbWaiting.add(file)
        // The tabs too: a tab waiting for the same photo didn't ask for it again.
        decodeThumb(file).then(() => { this.thumbWaiting.delete(file); this.dirtyTabs = true; if (this.pickerOpen) this.redrawPickerRows([c.jid]); this.scheduleRender() })
      }
    }
  }

  /** A chat's name with the filter's words underlined wherever they match, accents and case aside. */
  private underlineMatches(name: string): string {
    const chars = [...name]
    if (!this.pickerWords.length) return esc(name)
    // The folded name, with the character each of its letters came from.
    let flat = ''
    const owner: number[] = []
    chars.forEach((ch, i) => { for (const f of fold(ch)) { flat += f; owner.push(i) } })
    const marked = new Set<number>()
    for (const w of this.pickerWords) {
      for (let at = flat.indexOf(w); at >= 0; at = flat.indexOf(w, at + 1)) for (let k = at; k < at + w.length; k++) marked.add(owner[k]!)
    }
    let out = ''
    chars.forEach((ch, i) => {
      const on = marked.has(i), before = marked.has(i - 1)
      if (on && !before) out += '{underline}'
      if (!on && before) out += '{/underline}'
      out += esc(ch)
    })
    return marked.has(chars.length - 1) ? `${out}{/underline}` : out
  }

  /**
   * Over the list: the app's name in WhatsApp's green, big (WORDMARK), with its 👾 at the right of its first row and
   * the counts at the right of its second, or of its third (where only the p's tail is) when the terminal is too
   * narrow for them there, cut to fit at worst; without UTF-8 on one line, the counts after it.
   */
  private drawPickerHead(big: boolean) {
    const unread = this.chats.filter(c => c.unread > 0).length
    const plain = [t('chatsCount', this.chats.length), ...(unread ? [t('unreadCount', unread)] : [])].join(' · ')
    const counts = dim(esc(plain))
    const green = (s: string) => `{${this.green}-fg}${esc(s)}{/${this.green}-fg}`
    this.pickerHead.top = 0
    if (big) {
      // The counts end where the rows' times end, a column short of the edge.
      const w = num(this.pickerHead.width) - num(this.pickerHead.iwidth) - 1
      const after = (row: number) => w - strWidth(WORDMARK[row]!) - strWidth(plain)
      const row = after(1) >= 2 ? 1 : 2
      const room = w - strWidth(WORDMARK[row]!) - 2
      const shown = after(row) >= 2 ? counts : room > 3 ? dim(esc(truncate(plain, room))) : ''
      const fill = w - strWidth(WORDMARK[row]!) - visibleWidth(shown)
      this.pickerHead.height = WORDMARK.length
      this.pickerHead.setContent(WORDMARK.map((l, i) => green(l) + (i === 0 ? '  👾' : '') + (i === row && shown ? ' '.repeat(fill) + shown : '')).join('\n'))
    } else {
      this.pickerHead.height = 1
      this.pickerHead.setContent(`{bold}${green(APP)}{/bold}  ${counts}`)
    }
  }

  /** Redraws the list rows of these chats from what was read when the list was built: the live parts change. */
  private redrawPickerRows(jids: Iterable<string>) {
    if (!this.pickerOpen) return
    const want = new Set(jids)
    const width = num(this.picker.width) - num(this.picker.iwidth) - 1
    this.pickerSlots.forEach((c, i) => {
      if (!c || !want.has(c.jid) || this.pickerHits.has(i)) return
      const [first, second] = this.pickerRows(c, width, i)
      this.pickerItems[i] = first; this.pickerItems[i + 1] = second.map(([text]) => text).join(''); this.pickerParts.set(i + 1, second)
      this.picker.setItem(i as unknown as blessed.Widgets.BlessedElement, this.pickerLit.includes(i) ? this.selectedRow(i) : first)
      this.picker.setItem((i + 1) as unknown as blessed.Widgets.BlessedElement, this.pickerLit.includes(i + 1) ? this.litRow(i + 1) : this.pickerItems[i + 1]!)
    })
  }

  // ---------- state ----------

  private setFocus(f: Focus) {
    uiLog.info({ from: this.focus, to: f }, 'focus')
    this.focus = f
    if (f !== 'messages' && this.selected) { this.selected = null; this.dirtyMessages = true }
    if (f !== 'input' && this.suggestions.length) { this.suggestions = []; this.drawSuggestions() }
    const w = f === 'picker' ? this.picker : f === 'messages' ? this.msgBox : this.input
    w.focus()
    this.drawInput()
    this.drawStatus()
  }

  private flash(text: string, ms = 6000) {
    this.transient = text
    if (this.transientTimer) clearTimeout(this.transientTimer)
    this.transientTimer = setTimeout(() => { this.transient = ''; this.drawStatus(); this.screen.render() }, ms)
    this.drawStatus()
    this.screen.render()
  }

  private scheduleRender() {
    if (this.renderTimer) return
    this.renderTimer = setTimeout(() => { this.renderTimer = undefined; this.renderNow() }, 40)
  }

  private renderNow() {
    this.drawChatHead()
    if (this.dirtyTabs) { this.dirtyTabs = false; this.drawTabs(); this.updateTitle() }
    if (this.dirtyMessages && !this.showingQr) { this.dirtyMessages = false; if (this.current) this.renderMessages() }
    // With no chat open (startup without one asked for, or chats arriving for the first time, after the QR) the
    // chat list opens, to pick from.
    if (!this.current && !this.pickerOpen && !this.showingQr && store.listChats().some(c => !c.archived)) return this.openPicker()
    if (this.viewing) this.drawViewer()
    this.screen.render()
  }

  /**
   * The open chat's name at the top of the screen, over its messages, big (bigText) in the colour it has in groups,
   * as the app's name is over the chat list; on one line, bold, where not even its first word fits, without a UTF-8
   * locale, or on a screen under 16 rows. None with the chat list open or the QR in view. The messages start under it.
   */
  private drawChatHead() {
    const jid = this.pickerOpen || this.showingQr ? undefined : this.current
    let rows = 0
    if (jid) {
      const name = chatName(jid), color = colorFor(jid), width = num(this.screen.width) - 2
      const big = this.ruleChar === '─' && num(this.screen.height) >= 16 ? bigText(name, width) : null
      const lines = big ? big.map(l => `{${color}-fg}${esc(l)}{/${color}-fg}`) : [`{${color}-fg}{bold}${esc(truncate(name, width))}{/bold}{/${color}-fg}`]
      this.chatHead.setContent(lines.join('\n'))
      rows = lines.length
    }
    if (rows) this.chatHead.show(); else this.chatHead.hide()
    if (rows === this.headRows) return
    this.headRows = rows
    this.chatHead.height = Math.max(1, rows)
    this.msgBox.top = rows
    this.msgBox.height = `100%-${this.bottom + rows}`
    this.dirtyMessages = true
  }

  // Window title: the active chat, with two emoji taking turns in front while someone types (typingEmoji), or else a
  // dot while there are unread messages in any chat. In Herdr, where the title is the agent's name, the name alone:
  // the agent's status says the rest.
  private titleShown = ''
  /** How many unread messages the title last counted, so a new one is told apart (doneHerdr). */
  private unreadShown = 0
  private updateTitle() {
    const unread = store.listChats().filter(c => c.unread > 0 && (this.fixed ? c.jid === this.current : !c.archived))
    const typing = [...this.typing].filter(jid => !this.fixed || jid === this.current).map(jid => chatName(jid))
    const mark = inHerdr ? '' : typing.length ? `${typingEmoji()} ` : unread.length ? '● ' : ''
    const title = `${mark}${this.current ? chatName(this.current) : 'wassup'}`
    if (title !== this.titleShown) { this.titleShown = title; this.screen.title = title; titleHerdr(title) }
    // In Herdr, alone in its tab, the tab takes the chat's first name, with no state.
    if (inHerdr) tabNameHerdr(this.current ? shortName(this.current) : null)
    // In Herdr the agent's status: "working" while someone types; "done", in blue, from a new unread message until you
    // look at the pane (doneHerdr; Herdr gives it too when the typing stops out of view); idle otherwise. Never
    // "blocked".
    const summary = unread.map(c => `${chatName(c.jid)} (${c.unread})`).join(', ')
    const total = unread.reduce((n, c) => n + c.unread, 0)
    if (typing.length) reportHerdr('working', t('typingWho', typing.join(', ')))
    else if (total > this.unreadShown) doneHerdr(summary)
    else reportHerdr('idle', summary || undefined)
    this.unreadShown = total
  }

  quit(reason?: string) {
    this.pics?.dispose()
    if (this.pics instanceof SixelImages) this.pics.end()
    this.disableKittyKeyboard?.()
    this.disablePaste()
    ;(this.screen.program as unknown as { _write: (s: string) => void })._write('\x1b[?1004l')
    const released = releaseHerdr()
    this.screen.destroy()
    // Nothing the connection reports while closing reaches the destroyed screen.
    this.wa.removeAllListeners()
    if (reason) process.stderr.write(`${reason}\n`)
    // The connection is closed before the process ends, so the "unavailable" and the socket's close get out.
    const stopped = this.wa.stop().catch(() => {})
    store.deleteState(this.tabsKey())
    void Promise.all([released, stopped]).then(() => { store.close(); process.exit(0) })
  }

  private innerHeight(): number {
    return num(this.msgBox.height) - num(this.msgBox.iheight)
  }

  /** Straight to the latest message, leaving any selection. */
  private toLatest() {
    if (this.selected) this.select(null)
    this.atBottom = true
    this.dirtyMessages = true
    this.renderNow()
  }

  private updateAtBottom() {
    const total = this.msgBox._clines?.length ?? 0
    const at = this.msgBox.childBase + this.innerHeight() >= total
    const was = this.atBottom
    this.atBottom = at
    // "↓" beside the band follows: it comes and goes, and says how many are below.
    if (!this.plainInput && (at !== was || this.belowCount() !== this.belowShown)) this.drawInput()
  }

  // ---------- drawing ----------

  private drawStatus() {
    this.dirtyTabs = true
    this.drawTabs()
    this.dirtyTabs = false
  }

  // ---------- emoji suggestions ----------

  /**
   * A `:` right before the cursor opens the list with the basic smileys (":)", ":D"...); what's typed after it
   * narrows it to the smileys and the emojis whose name starts that way. A whole smiley before the cursor, on its
   * own after a space or at the start, puts its emoji first, those that don't start with ":" too ("<3", ";)").
   * Only with the focus on the input: a draft put back with it elsewhere (closing a tab) opens nothing.
   */
  /**
   * Who can be mentioned in a group: those who wrote in it, most recent first, with a known name. Each goes by its
   * first name, or its whole name when another one shares the first; the mention goes to their lid when it's known,
   * the id groups use now, otherwise to the id they wrote with.
   */
  private mentionables(group: string): { jid: string; name: string; token: string }[] {
    const people = store.recentSenders(group, 500).map(sender => {
      const name = contactName(sender)
      const jid = sender.endsWith('@lid') ? sender : store.getLid(sender) ?? sender
      return { jid, name, first: name.split(' ')[0]! }
    }).filter(p => !p.name.startsWith('lid:') && !p.name.startsWith('+') && p.jid !== this.wa.me)
    return people.map(p => ({ jid: p.jid, name: p.name, token: people.some(o => o !== p && o.first === p.first) ? p.name : p.first }))
  }

  /** In a group, each "@" and a member's name in the text becomes "@" and their number, and they the ones mentioned. */
  private withMentionIds(chatJid: string, text: string): { text: string; mentions: string[] } {
    if (!chatJid.endsWith('@g.us') || !text.includes('@')) return { text, mentions: [] }
    const mentions: string[] = []
    // The longer names first, so "@Ana Silva" isn't taken as "@Ana".
    for (const p of this.mentionables(chatJid).sort((a, b) => b.token.length - a.token.length)) {
      const re = new RegExp(`(?<![\\p{L}\\p{N}_])@${p.token.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?![\\p{L}\\p{N}])`, 'gu')
      const out = text.replace(re, `@${jidUser(p.jid)}`)
      if (out !== text && !mentions.includes(p.jid)) mentions.push(p.jid)
      text = out
    }
    return { text, mentions }
  }

  private updateSuggestions(ghostDelay = 150) {
    const chars = graphemes(this.inputValue)
    const at = Math.min(this.cursor, chars.length)
    const before = chars.slice(0, at).join('')
    const writing = this.focus === 'input'
    const face = writing ? emoticonAt(before, chars.slice(at).join('')) : null
    const m = writing ? /(^|[^\w:]):([^\s:]*)$/.exec(before) : null
    const typed = m ? graphemes(`:${m[2]}`).length : 0
    // In a group, "@" and the start of any of a member's names lists them, as "@" and the name the mention goes by.
    const mention = writing && this.current?.endsWith('@g.us') ? /(^|[^\p{L}\p{N}_@])@([\p{L}\p{M}]*)$/u.exec(before) : null
    const who = mention ? this.mentionables(this.current!).filter(p => fold(p.name).split(/\s+/).some(w => w.startsWith(fold(mention[2]!)))) : []
    const options = [
      ...face ? [{ emoji: face.emoji, code: face.face, length: graphemes(face.face).length }] : [],
      ...(m ? completeEmoji(m[2]!) : []).map(o => ({ ...o, length: typed })),
      ...who.map(p => ({ emoji: `@${p.token}`, code: p.name, length: graphemes(`@${mention![2]}`).length })),
    ].filter((o, i, all) => all.findIndex(p => p.emoji === o.emoji) === i)
    const same = options.length === this.suggestions.length && options.every((o, i) => o.emoji === this.suggestions[i]!.emoji)
    this.suggestions = options
    this.suggestFace = !!face
    this.suggestKind = face ? 'smileys' : m && typed > 1 && options.length ? 'emojiNames' : null
    if (!same) this.suggestIndex = 0
    this.drawSuggestions()
    this.scheduleGhost(ghostDelay)
  }

  // ---------- local model suggestions ----------

  private cursorAtEnd(): boolean {
    return this.cursor >= graphemes(this.inputValue).length
  }

  /**
   * The model's corrections that still apply: all of them for the text they were asked for; with more typed since,
   * those whose text up to the word's end is untouched and the word not extended into another. Their offsets hold,
   * as everything before their end is the same.
   */
  private liveFixes(): Fix[] {
    const g = this.ghost
    if (!g) return []
    if (g.text === this.inputValue) return g.s.fixes
    return g.s.fixes.filter(f => {
      const next = this.inputValue[f.end]
      return this.inputValue.startsWith(g.text.slice(0, f.end)) && (next == null || !/[\p{L}\p{M}\p{N}'-]/u.test(next))
    })
  }

  /** Whether anything of the stored suggestion still applies to the text. */
  private ghostValid(): boolean {
    const g = this.ghost
    return !!g && ((!!g.s.word && g.text === this.inputValue) || this.liveFixes().length > 0)
  }

  /** The correction the cursor is on (from its first letter to just past its last), or, at the end of the text, the last one. */
  private currentFix(): Fix | null {
    const fixes = this.liveFixes()
    const at = graphemes(this.inputValue).slice(0, this.cursor).join('').length
    return fixes.find(f => at >= f.start && at <= f.end) ?? (this.cursorAtEnd() ? fixes.at(-1) ?? null : null)
  }

  /**
   * The suggestion in view, the one Tab or → accepts: the word half-typed at the end, with the cursor there (the
   * letters missing, or the right word), which comes first; otherwise the correction the cursor is on (currentFix).
   * None while the emoji list is open: it's the list Tab takes, and it sits where a correction would float.
   */
  private ghostShown(): GhostView | null {
    const g = this.ghost
    if (!g || this.pickerOpen || this.suggestions.length) return null
    if (g.s.word && g.text === this.inputValue && this.cursorAtEnd()) {
      const { from, to } = g.s.word
      if (to.toLowerCase().startsWith(from.toLowerCase()) && to.length > from.length) return { kind: 'suffix', text: to.slice(from.length), word: g.s.word }
      return { kind: 'word', text: to, word: g.s.word }
    }
    const fix = this.currentFix()
    return fix ? { kind: 'fix', text: fix.to, fix } : null
  }

  /**
   * Asks the model for a suggestion for the current text, `delay` ms after the last keystroke (150 while typing; 0
   * right after accepting one, which is when it's idle waiting for the next one), and only with the cursor at the
   * end, with no reaction in progress nor emoji suggestions open but for a whole smiley's. A new request cancels the
   * previous one; the response is only used if the text is still the same when it arrives. Its corrections stay
   * underlined while they apply (`liveFixes`); what floats above goes after 10 s (drawInput). It all goes when a new
   * one arrives.
   */
  private scheduleGhost(delay = 150) {
    if (this.ghost && !this.ghostValid()) this.clearGhost()
    if (this.ghostTimer) { clearTimeout(this.ghostTimer); this.ghostTimer = undefined }
    this.ghostAbort?.abort()
    this.ghostAbort = undefined
    const jid = this.current
    if (!jid || this.focus !== 'input' || this.pickerOpen || this.reactTo || (this.suggestions.length && !this.suggestFace)) return
    if (!this.cursorAtEnd() || this.inputValue.trim().length < 3 || this.ghost?.text === this.inputValue) return
    const text = this.inputValue
    this.ghostTimer = setTimeout(() => {
      this.ghostTimer = undefined
      if (text !== this.inputValue) return
      const abort = new AbortController()
      this.ghostAbort = abort
      // The model when there's one, and while it answers; otherwise the local spell checker, words only (spell.ts).
      // A model that doesn't answer (no server) is left alone for a minute, the spell checker standing in.
      const local = () => spellFixes(text).then(fixes => (fixes.length ? { word: null, fixes } : null))
      const viaModel = llmEnabled && Date.now() >= this.llmDownUntil
      const context = viaModel ? store.listMessages(jid, 6).filter(r => r.text && r.type !== 'deleted').map(r => ({ who: this.who(r), text: r.text })) : []
      const request = !viaModel ? local() : suggest(context, text, abort.signal).catch(e => {
        if (abort.signal.aborted) throw e
        logger.debug({ e: String(e) }, 'llm: no answer, spell checker instead')
        this.llmDownUntil = Date.now() + 60000
        return local()
      })
      request.then(s => {
        if (abort.signal.aborted || text !== this.inputValue) return
        if (this.ghostAbort === abort) this.ghostAbort = undefined
        if (!s) { this.acceptOnArrival = 0; return }
        this.ghost = { text, s }
        if (this.acceptOnArrival > 0) { this.acceptOnArrival--; return void this.acceptGhost() }
        // A new suggestion floats again for its 10 s (drawInput).
        this.floatKey = this.floatOff = ''
        this.drawInput()
        this.screen.render()
      }, e => { this.acceptOnArrival = 0; if (!abort.signal.aborted) logger.debug({ e: String(e) }, 'suggestion') })
    }, delay)
  }

  /** Until when the model is left alone after it didn't answer (scheduleGhost). */
  private llmDownUntil = 0

  /**
   * The corrections for one of my text messages while it can still be edited (EDIT_WINDOW), or null: asked for once
   * per text, one message at a time (checkSent), when the message first shows them (my last, or an earlier one
   * selected), and remembered for the session.
   */
  private sentFixesFor(row: MessageRow): Fix[] | null {
    if (!row.from_me || row.type !== 'text' || Date.now() / 1000 - row.ts >= EDIT_WINDOW) return null
    const text = bareText(row)
    if (text.trim().length < 3) return null
    const key = `${row.chat_jid} ${row.id} ${text}`
    const known = this.sentFixes.get(key)
    if (known === undefined) { this.sentFixes.set(key, null); this.sentQueue.push(row); this.checkSent() }
    return known ?? null
  }

  /**
   * Checks the next of my messages waiting (sentFixesFor) as the writing suggestions check the input: the model, with
   * the chat's latest messages as context, or the local spell checker without it. The message is whole, so a fix
   * the model gives for its last word as the word half-typed counts too, unless it only completes it.
   */
  private checkSent() {
    if (this.sentChecking) return
    const row = this.sentQueue.shift()
    if (!row) return
    this.sentChecking = true
    const text = bareText(row), key = `${row.chat_jid} ${row.id} ${text}`
    const local = () => spellFixes(text)
    const viaModel = llmEnabled && Date.now() >= this.llmDownUntil
    const context = store.listMessages(row.chat_jid, 8).filter(r => r.id !== row.id && r.ts <= row.ts && r.text && r.type !== 'deleted').slice(-6).map(r => ({ who: this.who(r), text: r.text }))
    const request = !viaModel ? local() : suggest(context, text, new AbortController().signal).then(s => {
      if (!s) return []
      const { word } = s
      const last = word && !word.to.toLowerCase().startsWith(word.from.toLowerCase()) ? { from: word.from, to: word.to, start: text.length - word.from.length, end: text.length } : null
      return last && !s.fixes.some(f => f.end > last.start) ? [...s.fixes, last] : s.fixes
    }, e => {
      logger.debug({ e: String(e) }, 'llm: no answer, spell checker instead')
      this.llmDownUntil = Date.now() + 60000
      return local()
    })
    request.then(fixes => {
      if (fixes.length) { this.sentFixes.set(key, fixes); if (row.chat_jid === this.current) { this.dirtyMessages = true; this.scheduleRender() } }
    }, e => logger.debug({ e: String(e) }, 'sent check')).finally(() => { this.sentChecking = false; this.checkSent() })
  }

  /**
   * The correction for the passage marked wrong under screen cell (`x`, `y`) of one of my messages, and the column
   * where that passage starts on the line: the cells underlined in yellow around the click, matched by their text.
   */
  private fixAt(x: number, y: number, row: MessageRow): { fix: Fix; x: number } | null {
    const line = this.screenRows('lines')[y]
    if (!line) return null
    const marked = (cx: number) => { const a = line[cx]?.[0]; return a != null && ((a >> 18) & 2) !== 0 && ((a >> 9) & 0x1ff) === 3 }
    // Only a passage marked: a click on a message whose marks aren't shown doesn't get it checked.
    const fixes = marked(x) ? this.sentFixesFor(row) : null
    if (!fixes) return null
    let a = x, b = x
    while (a > 0 && marked(a - 1)) a--
    while (marked(b + 1)) b++
    const run = line.slice(a, b + 1).map(c => (c[1] === '\x03' ? '' : c[1])).join('').trim()
    const fix = fixes.find(f => f.from === run) ?? fixes.find(f => f.from.includes(run))
    return fix ? { fix, x: a } : null
  }

  /** Floats a correction over the passage of my message it fixes, starting on its column, the line above it if there's one. */
  private showSentFix(row: MessageRow, fix: Fix, x: number, y: number) {
    this.sentFix = { row, fix }
    const top = num(this.msgBox.atop) + num(this.msgBox.itop)
    this.fixBox.top = y > top ? y - 1 : y + 1
    this.fixBox.left = Math.max(0, x - 1)
    this.fixBox.width = strWidth(fix.to) + 2
    this.fixBox.setContent(` ${dim(italic(esc(fix.to)))}`)
    this.fixBox.show()
    this.fixBox.setFront()
    this.screen.render()
  }

  private hideSentFix() {
    if (!this.sentFix) return
    this.sentFix = undefined
    this.fixBox.hide()
  }

  /**
   * Edits my message with the correction shown over it. The others found in it stay marked, moved to their place in
   * the new text, which isn't checked again.
   */
  private applySentFix() {
    const shown = this.sentFix
    this.hideSentFix()
    if (!shown) return this.screen.render()
    const { row, fix } = shown
    if (Date.now() / 1000 - row.ts >= EDIT_WINDOW) return this.flash(t('editTooLate'))
    const text = bareText(row)
    if (text.slice(fix.start, fix.end) !== fix.from) return this.screen.render()
    const value = text.slice(0, fix.start) + fix.to + text.slice(fix.end), delta = fix.to.length - (fix.end - fix.start)
    const rest = (this.sentFixes.get(`${row.chat_jid} ${row.id} ${text}`) ?? []).filter(f => f !== fix).map(f => (f.start >= fix.end ? { ...f, start: f.start + delta, end: f.end + delta } : f))
    this.sentFixes.set(`${row.chat_jid} ${row.id} ${value}`, rest.length ? rest : null)
    this.wa.edit(row.chat_jid, row.id, value).catch(e => this.flash(`${t('error')}: ${(e as Error).message}`))
    this.screen.render()
  }

  private clearGhost() {
    this.ghost = undefined
    this.ghostBox.hide()
    if (this.ghostHide) { clearTimeout(this.ghostHide); this.ghostHide = undefined }
    this.floatKey = this.floatOff = ''
  }

  /**
   * Takes the suggestion in view (ghostShown). The other corrections stay underlined, those after the one taken
   * moved along by its change in length; with none left, the next suggestion is asked for at once.
   */
  private acceptGhost() {
    const v = this.ghostShown()
    if (!v) return
    const atEnd = this.cursorAtEnd()
    let value: string, rest: Fix[]
    if (v.kind === 'fix') {
      const { start, end, to } = v.fix, delta = to.length - (end - start)
      value = this.inputValue.slice(0, start) + to + this.inputValue.slice(end)
      rest = this.liveFixes().filter(f => f !== v.fix).map(f => (f.start >= end ? { ...f, start: f.start + delta, end: f.end + delta } : f))
      this.cursor = atEnd ? graphemes(value).length : graphemes(value.slice(0, start + to.length)).length
    } else {
      value = this.inputValue.slice(0, this.inputValue.length - v.word.from.length) + v.word.to
      rest = this.liveFixes()
      this.cursor = graphemes(value).length
    }
    this.inputValue = value
    this.accepted = /[\p{L}\p{M}\p{N}'-]$/u.test(this.inputValue) ? this.inputValue : undefined
    this.clearGhost()
    if (rest.length) this.ghost = { text: value, s: { word: null, fixes: rest } }
    this.promoteActive()
    this.updateSuggestions(0)
    this.drawInput()
    this.screen.render()
  }

  private drawSuggestions() {
    if (!this.suggestions.length) { this.suggest.hide(); return }
    const lines = this.suggestions.map((o, i) => i === this.suggestIndex
      ? `{bold}› ${esc(o.emoji)}  ${esc(o.code)}{/bold}`
      : `  ${esc(o.emoji)}  ${esc(o.code)}`)
    // SUGGEST_ROWS at a time, scrolled just enough to show the chosen one.
    if (this.suggestIndex < this.suggestTop) this.suggestTop = this.suggestIndex
    else if (this.suggestIndex >= this.suggestTop + SUGGEST_ROWS) this.suggestTop = this.suggestIndex - SUGGEST_ROWS + 1
    const shown = lines.slice(this.suggestTop, this.suggestTop + SUGGEST_ROWS)
    // As wide as the widest option, in view or not, so the box doesn't change width while scrolling. One column of
    // margin on the right, which also serves as padding: blessed wraps the line if a closing tag lands on the last column.
    this.suggest.width = Math.max(...lines.map(visibleWidth)) + 2
    this.suggest.height = shown.length
    // Right above the band where the text is written, over the messages, so it never covers what's being typed.
    this.suggest.top = `100%-${this.bottom + shown.length}`
    this.suggest.setContent(shown.join('\n'))
    this.suggest.show()
  }

  /** The `:prefix`, the smiley or the `@name` gives way to the chosen emoji or mention, followed by a space. */
  private acceptSuggestion() {
    if (this.suggestKind) this.noteSuggest(this.suggestKind, true)
    const o = this.suggestions[this.suggestIndex]!
    const chars = graphemes(this.inputValue)
    const at = Math.min(this.cursor, chars.length)
    const before = [...chars.slice(0, at - o.length), o.emoji, ' '].join('')
    this.inputValue = before + chars.slice(at).join('')
    // Counted in graphemes: a mention ("@Ana") is several.
    this.cursor = graphemes(before).length
    this.suggestions = []
    this.drawSuggestions()
    this.drawInput()
    return this.screen.render()
  }

  /**
   * Suggestions of this kind taken (Tab, →, Enter on a name's, a space in "emoji") or not (a space in "text", Esc,
   * Enter after a smiley): with the last two times alike, the mode follows them (SuggestKind); unlike, it stays.
   */
  private noteSuggest(kind: SuggestKind, taken: boolean) {
    const kept = store.getState<SuggestMode>(kind)
    const last = [...(kept?.last ?? []), taken].slice(-2)
    const mode = last.length === 2 && last[0] === last[1] ? (taken ? 'emoji' : 'text') : kept?.mode ?? 'text'
    store.setState(kind, { mode, last } satisfies SuggestMode)
  }

  private drawInput() {
    // In a chat the text is written as my next message, on my bubbles' green, in a band as wide as my bubbles can be,
    // two rows at least, growing upwards with the text, wrapped by word (never mid-word), the chat's tab, with its
    // name, right under it (drawTabs); what's being replied to, reacted to or edited on a row of its own at its top.
    // The chat list's filter is written in the same band, its tab "wassup" (or "procurar", searching them all).
    // When the text has more lines than fit, the ones around the cursor are shown, with the cursor on the bottom one
    // whenever possible. While the messages are scrolled up, "↓" and how many are below, beside the band where the
    // time goes. Searching the chat (Ctrl+F), the line is the search's, plain, after "procurar ❯", under a faint rule
    // (drawBorder).
    const W = num(this.input.width)
    // Searching the chat (Ctrl+F), the line is the search's, the rule above saying how it goes.
    const searching = !!this.search && !this.pickerOpen
    const plain = this.plainInput
    const who = searching ? t('search') : ''
    // My bubbles' widest: they stop short of the time and its mark, with a cell of gap (see renderMessages).
    const stampW = visibleWidth(myTime(Date.now() / 1000, 0)) + 1
    const bandW = W - 4 - stampW
    const pw = this.promptWidth = plain ? 1 + (who ? strWidth(who) + 1 : 0) + 2 : 2
    const target = this.pickerOpen || searching ? null : this.replyTo ?? this.reactTo ?? this.editing
    const found = searching && this.search!.query ? (this.search!.hits.length ? t('searchHeader', this.search!.at + 1, this.search!.hits.length) : t('searchNone')) : null
    const header = searching ? (found ? `${t('search')} · ${found}` : t('search'))
      : this.deleting ? t(this.deleting.forEveryone ? 'deleteForAll' : 'deleteForMe')
      : (!target ? null : this.editing
      ? t('editHeader', this.snippet(target))
      : this.replyTo
        ? `↩ ${target.chat_jid.endsWith('@g.us') && !target.from_me ? `${this.who(target)}: ` : ''}${this.snippet(target)}`
        : t('reactHeader', this.who(target), this.snippet(target)))
    // Model suggestion, discreet, in gray italic: the letters missing from the word mid-typing, attached to the cursor
    // (which sits on the first one); a correction, whether of the mid-typed word or of a wrong word further back,
    // floating on the line above the word, starting on its column. Tab accepts. Every wrong passage the model found
    // is underlined in yellow while it applies; what floats is the cursor's one (ghostShown).
    const view = searching || this.pickerOpen ? null : this.ghostShown()
    const width = Math.max(4, plain ? W - pw - 1 : bandW)
    const chars = graphemes(this.pickerOpen ? this.filter : searching ? this.search!.query : this.inputValue)
    const cursor = Math.min(this.pickerOpen ? this.filterCursor : searching ? this.search!.cursor : this.cursor, chars.length)
    const lines = wrapChars(chars, width)
    // Cursor's line and column: at the end of the text it sits after the last grapheme, and moves to a new line if it doesn't fit.
    let row = 0, start = 0
    while (row < lines.length - 1 && cursor >= start + lines[row]!.length) start += lines[row++]!.length
    let col = cursor - start
    if (col >= lines[row]!.length && visibleWidth(esc(lines[row]!.join(''))) >= width) { lines.push([]); row++; col = 0 }
    // The "\n" or the space that closes a line stay in it, so the cursor counts them, but aren't drawn: one space
    // past the width would make blessed wrap the line. A trailing space that fits is drawn, so the cursor advances with it.
    const text = (l: string[]) => { const t = l.filter(c => c !== '\n').join(''); return strWidth(t) > width ? t.replace(/\s+$/, '') : t }
    // The missing letters go right at the cursor when they fit on its line. Any other suggestion is the whole word
    // as it should be, floating right above the word it replaces, from the same column (pulled left when it would run
    // past the edge); Tab, → or a click on it take it.
    const cursorLine = lines[row]!
    const avail = width - visibleWidth(esc(text(cursorLine))) - 1
    let ghostNext = '', ghostAbove = '', ghostLine = -1, ghostCol = 0
    // What floats stays 10 s, then goes until another takes its place (a new suggestion, the cursor on another word).
    const floatKey = !view ? '' : view.kind === 'fix' ? `${view.fix.start}:${view.fix.to}` : `word:${view.word.to}`
    if (floatKey !== this.floatKey) {
      this.floatKey = floatKey
      if (this.ghostHide) clearTimeout(this.ghostHide)
      this.ghostHide = floatKey ? setTimeout(() => { this.floatOff = floatKey; this.drawInput(); this.screen.render() }, 10000) : undefined
    }
    if (view && floatKey !== this.floatOff && (view.kind === 'fix' || col >= cursorLine.length)) {
      if (view.kind === 'suffix' && strWidth(view.text) <= avail + 1) ghostNext = view.text
      else {
        const word = view.kind === 'fix' ? view.fix.to : view.word.to
        const startUnit = view.kind === 'fix' ? view.fix.start : this.inputValue.length - view.word.from.length
        let at = graphemes(this.inputValue.slice(0, startUnit)).length, wl = 0
        while (wl < lines.length - 1 && at >= lines[wl]!.length) at -= lines[wl++]!.length
        ghostAbove = truncate(word, width)
        ghostLine = wl
        ghostCol = Math.max(0, Math.min(strWidth(lines[wl]!.slice(0, at).join('')), width - strWidth(ghostAbove)))
      }
    }
    // The band takes two rows at least, and grows with the text, up to half the screen, a row more for the header at
    // its top; the plain line, one, a row more for the rule over it.
    this.headerRows = header && !plain ? 1 : 0
    const rows = Math.max(plain ? 1 : 2 + this.headerRows, Math.min(lines.length + this.headerRows, Math.floor(num(this.screen.height) / 2)))
    if (rows !== this.inputRows || this.laidBottom !== rows + 1 + (plain ? 1 : 0)) this.resizeInput(rows)
    const rowsAvail = Math.max(1, rows - this.headerRows)
    this.inputLines = lines
    this.inputTop = Math.max(0, Math.min(row - (rowsAvail - 1), lines.length - rowsAvail))
    const showCursor = this.focus === 'input' || this.focus === 'picker'
    // The graphemes of the passages the model found wrong, by their place in the whole text, and where each line starts.
    const marked = new Set<number>()
    if (!this.pickerOpen && !searching) {
      for (const f of this.liveFixes()) {
        const a = graphemes(this.inputValue.slice(0, f.start)).length, b = graphemes(this.inputValue.slice(0, f.end)).length
        for (let i = a; i < b; i++) marked.add(i)
      }
    }
    const lineStart: number[] = []
    lines.reduce((at, l) => { lineStart.push(at); return at + l.length }, 0)
    // As `text`, a line's graphemes as drawn (no "\n", no trailing space past the width), the wrong ones underlined.
    const paint = (gs: string[], from: number) => {
      const drawn = strWidth(gs.filter(c => c !== '\n').join('')) > width ? gs.slice(0, gs.length - (/\s*$/.exec(gs.join(''))?.[0].length ?? 0)) : gs
      let out = '', on = false
      drawn.forEach((g, k) => {
        if (g === '\n') return
        const m = marked.has(from + k)
        if (m !== on) { out += m ? '{underline}{yellow-fg}' : '{/yellow-fg}{/underline}'; on = m }
        out += esc(g)
      })
      return on ? `${out}{/yellow-fg}{/underline}` : out
    }
    const render = (line: string[], r: number) => {
      const from = lineStart[r] ?? 0
      if (!showCursor || r !== row) return paint(line, from)
      const before = paint(line.slice(0, col), from)
      if (ghostNext) {
        // The cursor sits on the suggestion's first letter, with no empty cell in between; the rest follows in italic,
        // opened again after the cursor, as closing the inverse (ESC[27m) clears every flag in blessed.
        const g = graphemes(ghostNext)
        return before + dim('{inverse}' + italic(esc(g[0]!)) + '{/inverse}' + italic(esc(g.slice(1).join(''))))
      }
      const under = line[col] == null || line[col] === '\n' ? ' ' : line[col]!
      return before + '{inverse}' + esc(under) + '{/inverse}' + paint(line.slice(col + 1), from + col + 1)
    }
    const visible = lines.slice(this.inputTop, this.inputTop + rowsAvail)
    let out: string[]
    if (plain) {
      const prompt = ` ${who ? `{${this.green}-fg}${esc(who)}{/${this.green}-fg} ` : ''}❯ `
      out = visible.map((l, i) => (this.inputTop + i === 0 ? prompt : ' '.repeat(pw)) + render(l, this.inputTop + i))
    } else {
      // Each row a stretch of the band, its spare column on either side; the background opened again after the
      // cursor, whose inverse closes it, and every tag closed at the band's end ({/}), or blessed would carry the
      // background on to the row's end.
      const bg = this.bubbleBg.mine
      const band = (s: string) => ` {${bg}-bg} ${s.replace(/\{\/inverse\}/g, `{/inverse}{${bg}-bg}`)}${' '.repeat(Math.max(0, bandW - visibleWidth(s)))} {/}`
      out = visible.map((l, i) => band(render(l, this.inputTop + i)))
      while (out.length < rowsAvail) out.push(band(''))
      // The confirmation a deletion waits for, in the colour of errors; what's being replied to or edited, faint.
      const said = esc(truncate(header ?? '', bandW))
      if (this.headerRows) out.unshift(band(this.deleting ? `{${FG.error}-fg}${said}{/${FG.error}-fg}` : dim(said)))
      // "↓" goes beside the band's first text row, in the time's column.
      const first = this.headerRows
      this.belowRow = first
      const below = this.belowShown = this.belowCount()
      this.borderBelow = Infinity
      if (!this.atBottom && first >= 0 && !this.pickerOpen) {
        out[first] += ` ${dim(esc(below ? `↓ ${below}` : '↓'))}`
        this.borderBelow = bandW + 3
      }
    }
    this.input.setContent(out.join('\n'))
    this.borderHeader = plain ? header : null
    this.drawBorder()
    // The correction floats one line above its word's line, when that line is in view: the input takes the last
    // `rows` lines, the prompt its first columns, after the padding.
    if (ghostAbove && ghostLine >= this.inputTop && ghostLine < this.inputTop + rowsAvail) {
      this.ghostBox.top = num(this.screen.height) - 1 - rows + this.headerRows + (ghostLine - this.inputTop) - 1
      // A cell of its background on either side, the word itself on the column of the one it replaces.
      this.ghostBox.left = num(this.input.aleft) + num(this.input.ileft) + pw + ghostCol - 1
      this.ghostBox.width = strWidth(ghostAbove) + 2
      this.ghostBox.setContent(` ${dim(italic(esc(ghostAbove)))}`)
      this.ghostBox.show()
    } else this.ghostBox.hide()
  }

  /** The text being written as the band shows it, line by line, and the screen row of its first line; none outside a chat. */
  private draftShown(): { lines: string[]; top: number } | undefined {
    if (!this.current || this.pickerOpen || this.search || !this.inputValue.trim()) return undefined
    const rows = Math.max(1, this.inputRows - this.headerRows)
    const lines = this.inputLines.slice(this.inputTop, this.inputTop + rows).map(l => l.filter(c => c !== '\n').join('').replace(/\s+$/, ''))
    return { lines, top: num(this.input.atop) + this.headerRows }
  }

  /**
   * The message just sent goes from where it was written to where it lands among the messages. First the
   * conversation moves up, a row at a time, to make room for it (renderMessages), the text still where it was written;
   * then, on my bubbles' green, the band narrows to the bubble's width, against its right edge, and rises into that
   * room (FLY_FRAMES frames, eased). It stays there until the message itself is drawn, two seconds at most.
   */
  private flyDraft(text: string, from: { lines: string[]; top: number }) {
    this.landed()
    const W = num(this.screen.width)
    const right = W - 1 - (visibleWidth(myTime(Date.now() / 1000, 0)) + 1)
    const inner = Math.max(1, ...from.lines.map(l => strWidth(l)))
    const n = from.lines.length, bg = this.bubbleBg.mine
    const left0 = 1, left1 = Math.max(1, right - inner - 2)
    const flight = { timer: undefined as unknown as NodeJS.Timeout, text, frame: 0, arrived: false, room: 0 }
    const draw = (left: number, top: number) => {
      const w = right - left
      this.flyBox.left = left
      this.flyBox.top = top
      this.flyBox.width = w
      this.flyBox.height = n
      this.flyBox.setContent(from.lines.map(l => `{${bg}-bg} ${esc(l)}${' '.repeat(Math.max(0, w - 2 - strWidth(l)))} {/${bg}-bg}`).join('\n'))
      this.flyBox.show()
      this.flyBox.setFront()
    }
    // Where it lands: the message's first row, once drawn, or the room made for it, the n + 1 last rows.
    const landing = () => {
      let at = this.lineMap.findIndex(r => r?.from_me === 1 && r.text === text)
      if (at < 0) at = this.lineMap.length - (n + 1)
      const real = this.msgBox._clines?.ftor?.[at]?.[0] ?? at
      return num(this.msgBox.atop) + num(this.msgBox.itop) + real - this.msgBox.childBase
    }
    this.flight = flight
    draw(left0, from.top)
    this.screen.render()
    flight.timer = setInterval(() => {
      if (flight.room < n + 1 && !flight.arrived) { flight.room++; this.dirtyMessages = true; return this.renderNow() }
      clearInterval(flight.timer)
      const top1 = landing()
      flight.timer = setInterval(() => {
        const p = ++flight.frame / FLY_FRAMES, e = p * p * (3 - 2 * p)
        draw(Math.round(left0 + (left1 - left0) * e), Math.round(from.top + (top1 - from.top) * e))
        if (flight.frame >= FLY_FRAMES) { clearInterval(flight.timer); if (flight.arrived) return this.landed() }
        this.screen.render()
      }, 40)
    }, 30)
    setTimeout(() => { if (this.flight === flight) this.landed() }, 2000)
  }

  /** The flight is over: the message drawn in its place, or, the time for it run out, its room given back. */
  private landed(render = true) {
    const f = this.flight
    if (!f) return
    clearInterval(f.timer)
    this.flight = undefined
    this.flyBox.hide()
    this.dirtyMessages = true
    if (render) this.renderNow()
  }

  /**
   * Launches the animated emoji from its place in the message `pick` identifies. The position is looked up at draw
   * time, once the message (or its reactions line) is already in the panel: its last line in `lineMap`, converted
   * to the real line by blessed's map and to the screen by the scroll; the column is the emoji's in that line,
   * without the color codes. No emoji on the line yet drawn: nothing is returned and the animation asks again.
   */
  private heartFor(pick: (r: MessageRow) => boolean, text: string) {
    const emoji = reaction(text)
    if (!emoji) return
    this.hearts.launch(emoji, () => {
      // Drawn twice its size, it's born over the middle of its second line.
      const big = this.bigEmojis.find(b => { const r = this.lineMap[b.line]; return !!r && pick(r) && reaction(r.text) === emoji })
      const top = big ? this.msgBox._clines.ftor[big.line]?.[0] : undefined
      if (big && top != null && top >= this.msgBox.childBase) {
        return { x: num(this.msgBox.aleft) + num(this.msgBox.ileft) + big.col + 1, y: num(this.msgBox.atop) + num(this.msgBox.itop) + top - this.msgBox.childBase + 1 }
      }
      let idx = -1
      for (let i = this.lineMap.length - 1; i >= 0; i--) { const r = this.lineMap[i]; if (r && pick(r)) { idx = i; break } }
      if (idx < 0) return null
      const real = this.msgBox._clines.ftor[idx]?.[0]
      if (real == null) return null
      const y = real - this.msgBox.childBase
      if (y < 0 || y >= this.innerHeight()) return null
      const line = (this.msgBox._clines[real] ?? '').replace(/\x1b\[[\d;]*m/g, '')
      // The emoji as shown (without its skin tone, the first of a joined sequence, a flag as letters: tameEmoji), or
      // the text as received (a smiley like "<3" that stands for it).
      const at = [emoji, tameEmoji(emoji), text.trim()].map(s => line.indexOf(s)).find(i => i >= 0)
      if (at == null) return null
      return { x: num(this.msgBox.aleft) + num(this.msgBox.ileft) + strWidth(line.slice(0, at)), y: num(this.msgBox.atop) + num(this.msgBox.itop) + y }
    })
  }

  /** Changes the input's height and shifts whatever depends on it: messages, floating bar, suggestions, and picker. */
  private resizeInput(rows: number) {
    this.inputRows = rows
    this.input.height = rows
    this.laidBottom = this.bottom
    this.input.top = `100%-${rows + 1}`
    this.ruleTop.top = `100%-${rows + 2}`
    this.msgBox.height = `100%-${this.bottom + this.headRows}`
    this.picker.height = `100%-${this.bottom + 1}`
    this.dirtyMessages = true
    this.dirtyTabs = true
    if (this.suggestions.length) this.drawSuggestions()
    if (this.pickerOpen) this.refreshPicker()
    this.scheduleRender()
  }

  private imagePathFor(row: MessageRow): string | null {
    const file = mediaFile(row)
    if (file && /^image\//.test(row.media_mime ?? '') && fs.existsSync(file)) return file
    const t = thumbPath(row.chat_jid, row.id)
    return fs.existsSync(t) ? t : null
  }

  private renderMessages() {
    const jid = this.current
    if (!jid) return
    // Scrolled up in the same chat, the message at the top of the view stays where it is when lines come or go
    // above it (older history, a reaction, an image's real height); at the bottom, the view follows the bottom.
    const pin = !this.atBottom && this.renderedJid === jid ? this.topAnchor() : null
    const wasAtTop = this.msgBox.childBase === 0
    // While the phone is asked for older messages, a line at the very top says so.
    const loading = this.olderPending.has(jid)
    const loadingNew = loading && !(this.loadingDrawn && this.renderedJid === jid)
    const isGroup = jid.endsWith('@g.us')
    const width = num(this.msgBox.width) - num(this.msgBox.iwidth)
    const lines: string[] = []
    const map: (MessageRow | null)[] = []
    const images: ImageSlot[] = []
    const bigs: typeof this.bigEmojis = []
    const selectedId = this.selected?.id
    // The selected message gets the background at full width, whoever it's from: the lines arrive here already
    // wrapped to the panel's width, and get padded with spaces up to the edge.
    const decorate = (line: string, row: MessageRow | null) => {
      if (row && row.id === this.drag?.id && this.drag.dx) line = clipTagged(' '.repeat(this.drag.dx) + line, width)
      if (row && row.id === selectedId) line = `{${this.selectedBg}-bg}${line}${' '.repeat(padding(line, width))}{/${this.selectedBg}-bg}`
      return line
    }
    // Lines go in as they are; a message's are decorated once it's complete (time and reactions included), so its
    // bubble and its time come out the same whether it's selected or not.
    const push = (line: string, row: MessageRow | null) => {
      lines.push(line)
      map.push(row)
    }
    const reactions = new Map<string, ReactionRow[]>()
    for (const r of store.listReactions(jid)) reactions.set(r.msg_id, [...(reactions.get(r.msg_id) ?? []), r])
    // Encrypted content for another message, stored before the client knew to drop it, isn't a message to show.
    const rows = store.listMessages(jid, this.shown.get(jid) ?? PAGE).filter(r => r.type !== 'secretEncrypted')
    // The last of mine the other side has read (or played), which gets the prompt's mark turned round, "❮", right after its time.
    const lastRead = [...rows].reverse().find(r => r.from_me === 1 && (r.status ?? 0) >= 4)?.id
    // The last of mine, whose passages found wrong are marked; an earlier one's only while it's selected.
    const lastMine = [...rows].reverse().find(r => r.from_me === 1)?.id
    this.rows = rows
    this.selected = rows.find(r => r.id === selectedId) ?? null
    this.dropDescribing()
    const headers = new Set<number>(), texts = new Map<number, { start: number; end: number }>(), names = new Map<number, { jid: string; start: number; width: number }>()
    const mentions = new Map<number, { start: number; end: number; jid: string }[]>()
    // The text under the highlight is about to change, and so may the place of a correction floating over it.
    this.textSel = undefined
    this.hideSentFix()
    let expires = Infinity
    let lastDay = ''
    if (loading) {
      const label = t('loadingOlder')
      push(dim(`${' '.repeat(Math.max(0, Math.floor((width - strWidth(label)) / 2)))}${esc(label)}`), null)
    }

    for (const row of rows) {
      const day = dayKey(row.ts)
      if (day !== lastDay) {
        lastDay = day
        const label = `── ${fmtDay(row.ts)} ──`
        push(dim(`${' '.repeat(Math.max(0, Math.floor((width - strWidth(label)) / 2)))}${label}`), null)
        // A blank line under it, so the day stands apart from its first message as from the last one before.
        push('', null)
      }
      const bubbleFrom = lines.length
      // My own messages stay flush right: I wrap the lines myself (blessed only wraps from the left) and push each
      // one to the edge; other people's stay on the left, wrapped the same way.
      const mine = row.from_me === 1
      // Mine carry their state in the time's separator (myTime), the same width whatever the state, and a cell after
      // it, "❮" on the last read and blank on the others, so all their times line up.
      const stamp = faint(mine ? `${myTime(row.ts, row.status ?? 0)}${row.id === lastRead ? '❮' : ' '}` : fmtTime(row.ts))
      // Messages go in a bubble (see bubble), with the time outside it, except emoji on their own, which stand bare
      // with the time beside them. The pictures of images, stickers, videos and GIFs stay out of it too (`pictures`,
      // their rows), while what comes with them (the sender's name, a quote, the caption, the video's note) goes in.
      // Theirs in a bubble start a column in, for its spare column on the left: the panel's own left column is
      // padding, which takes no background.
      const bare = row.type === 'text' && emojiOnly(row.text)
      const pictures = new Set<number>()
      const indent = mine || bare ? 0 : 1, ind = ' '.repeat(indent)
      // Lines go up to the panel's last column: with wrap off, blessed neither wraps nor cuts a line that fills it,
      // closing tags included. Mine stop short of the columns the time and its mark take, with two cells of gap,
      // so no line of theirs (text, quote, reactions) runs into them; the line that carries the time is the only
      // one reaching the edge.
      const textWidth = mine ? Math.max(1, width - 2 - visibleWidth(stamp)) : width - indent
      // In a bubble the text wraps a column short of that, so its spare column on the far side always has room.
      const wrapAt = bare ? textWidth : Math.max(1, textWidth - 1)
      // The last line `out` wrote for this message, as given, so the time can be appended to it afterwards.
      let last: { at: number; line: string } | null = null
      const out = (line: string, r: MessageRow | null) => {
        for (const l of wrapTagged(line, wrapAt)) { last = { at: map.length, line: l }; push(mine ? alignRight(l, textWidth) : ind + l, r) }
      }
      // No names: mine are on the right, the other side's on the left, both in the default color. Only in groups
      // does the sender's name open the message, in their color. The time closes it (below), so the text lines of
      // consecutive messages read straight down; mine carries its state in it. Both lines are the message's
      // "header" for the drag-to-reply and the "☺".
      const header = (line: string) => { const at = map.length; out(line, row); for (let i = at; i < map.length; i++) headers.add(i) }
      if (isGroup && !mine) {
        const who = contactName(row.sender_jid)
        names.set(map.length, { jid: row.sender_jid, start: indent, width: strWidth(who) })
        header(`{${colorFor(row.sender_jid)}-fg}${esc(who)}{/${colorFor(row.sender_jid)}-fg}`)
      }
      // A message deleted shows nothing of what it was, the message it answered included.
      if (row.quoted && row.type !== 'deleted') {
        const [who, text] = row.quoted.split('\t')
        // Who it was from only matters in groups; one-on-one the other person is obvious, and mine don't carry a name either.
        const author = who === this.wa.me || !row.chat_jid.endsWith('@g.us') ? '' : `${esc(contactName(who ?? ''))}: `
        out(dim(`│ ${author}${esc(truncate(showLinks(withMentions(text ?? '')), width - 6))}`), row)
      }

      const type = row.type
      // The note after an attachment: that it's gone, when it is; while it isn't on disk yet, "⤓" in green, and a
      // click on the message fetches it into the app's media folder (openMedia), where it stays; the mark goes with
      // the redraw that follows. Images and stickers fetch themselves as they come into view.
      const fetchMark = this.ruleChar === '─' ? '⤓' : 'v'
      const mediaHint = row.media_err && !row.media_path ? ` ${dim(t('unavailable'))}`
        : row.media_mime && !row.media_path ? ` {${this.green}-fg}${fetchMark}{/${this.green}-fg}` : ''
      let stamped = false
      if (type === 'deleted') out(dim(`⊘ ${t('deleted')}`), row)
      else if (type === 'image' || type === 'sticker' || type === 'gif' || type === 'video') {
        // An image or sticker with no caption carries the time to the right of its last row. Mine end where the text
        // would, short of the time's columns.
        const beside = (type === 'image' || type === 'sticker') && !row.text
        const from = lines.length
        if (this.pushImage(row, push, images, lines, width, mine, beside ? stamp : undefined, undefined, textWidth)) { headers.add(map.length - 1); stamped = true }
        for (let i = from; i < lines.length; i++) pictures.add(i)
        last = null
        // The selected one's description beside it, or under it when it doesn't fit there.
        const aside = row.id === selectedId ? this.imageAside(row) : undefined
        if (aside && !this.placeAside(lines, images.at(-1), row, aside, width, mine, stamped)) out(aside, row)
        if (type === 'video' || type === 'gif') out(`{magenta-fg}▶ ${type === 'gif' ? t('gif') : t('video')}{/magenta-fg}${mediaHint}`, row)
      } else if (type === 'document') {
        out(`{yellow-fg}📎 ${esc(row.media_name ?? t('file'))}{/yellow-fg}${mediaHint}`, row)
      } else if (type === 'audio' || type === 'voice') {
        out(`{yellow-fg}${type === 'voice' ? '🎤' : '🎵'} ${type === 'voice' ? t('voiceMessage') : t('audio')} ${esc(row.text)}{/yellow-fg}${mediaHint}`, row)
      } else if (type === 'location') out(`{yellow-fg}📍 ${waMarkup(row.text)}{/yellow-fg}`, row)
      else if (type === 'contact') out(`{yellow-fg}👤 ${esc(row.text)}{/yellow-fg}`, row)
      else if (type === 'poll') for (const l of row.text.split('\n')) out(`{yellow-fg}${esc(l)}{/yellow-fg}`, row)
      // Kinds without a drawing of their own are stored as "[kind]": they're named instead (typeLabel).
      else if (type !== 'text') out(dim(esc(row.text && row.text !== `[${type}]` ? row.text : typeLabel(row))), row)

      // A link's preview image, inside the bubble above the text, as WhatsApp Web shows it.
      if (type === 'text' && hasPreviewImage(row) && !fs.existsSync(`${previewPath(row.chat_jid, row.id)}.none`)) {
        const from = lines.length
        this.pushImage(row, push, images, lines, width, mine, undefined, { src: previewPath(row.chat_jid, row.id), maxCols: 40, maxRows: 12, indent }, textWidth)
        // Beside it, the lines stay out of the bubble's measure, so the time keeps its place after the text.
        const aside = row.id === selectedId ? this.imageAside(row) : undefined
        if (aside && this.placeAside(lines, images.at(-1), row, aside, width, mine, false)) for (let i = from; i < lines.length; i++) pictures.add(i)
        else if (aside) out(aside, row)
      }
      // Bare, the time goes at the end of the message's last line when it fits there with two cells of gap; beside an
      // image's last row it went in above. Otherwise it gets its own line.
      if (row.text && (type === 'text' || type === 'image' || type === 'video' || type === 'gif' || type === 'document')) {
        // Mentions by first name, in the colour the person's name has in groups; where each lands is kept for a click.
        const marks = new Map<string, { jid: string; width: number }>()
        // Mine still within the editing window, the last or the one selected: the passages the check found wrong,
        // underlined in yellow (sentFixesFor); while searching, the search's words instead, in reverse video.
        const words = this.search?.words.length ? this.search.words : null
        const fixes = !words && mine && type === 'text' && (row.id === lastMine || row.id === selectedId) ? this.sentFixesFor(row) : null
        if (fixes) expires = Math.min(expires, row.ts + EDIT_WINDOW)
        const shown = withMentions(waMarkup(words ? markSearch(row.text, words) : fixes ? markFixes(row.text, fixes) : row.text), (jid, first) => {
          const color = colorFor(jid), token = `{${color}-fg}@${esc(first)}{/${color}-fg}`
          marks.set(token, { jid, width: strWidth(`@${first}`) })
          return token
        }).replace(/\uE000/g, '{underline}{yellow-fg}').replace(/\uE001/g, '{/yellow-fg}{/underline}')
          .replace(/\uE002/g, '{inverse}').replace(/\uE003/g, '{/inverse}')
        // Emoji on their own, one to three, twice their size where the terminal draws text bigger, or shows Kitty's
        // graphics: two lines of blank cells, four columns for each, the emoji written over them after each frame
        // (paintBigEmoji) or placed there in pictures (placeImages), and the time beside the second. One that can't be
        // drawn in a picture keeps its message as it was.
        const big = (this.textSizing || this.pics) && bare ? graphemes(row.text.trim()).filter(g => g.trim()) : []
        if (!this.textSizing && big.some(e => this.emojiImage(e) === null)) big.length = 0
        const bigW = big.length * 4, stampW = visibleWidth(stamp)
        const bigged = big.length > 0 && big.length <= 3 && bigW + 2 + stampW <= width
        if (bigged) {
          const top = ' '.repeat(bigW + 2 + stampW), bottom = `${' '.repeat(bigW)}  ${stamp}`
          bigs.push({ line: map.length, col: mine ? width - visibleWidth(bottom) : 0, emoji: big })
          push(mine ? alignRight(top, width) : top, row)
          headers.add(map.length)
          push(mine ? alignRight(bottom, width) : bottom, row)
          stamped = true
        }
        const wrapped = bigged ? [] : shown.split('\n').flatMap(l => wrapTagged(l, wrapAt))
        wrapped.forEach((l, i) => {
          const tw = visibleWidth(l)
          const withStamp = bare && i === wrapped.length - 1 && tw + 2 + visibleWidth(stamp) <= width
          const line = withStamp ? `${l}  ${stamp}` : l
          const edge = withStamp ? width : textWidth
          const at = map.length
          push(mine ? alignRight(line, edge) : ind + line, row)
          const start = mine ? padding(line, edge) : indent
          texts.set(at, { start, end: start + tw })
          const spans: { start: number; end: number; jid: string }[] = []
          for (const [token, m] of marks) {
            for (let k = l.indexOf(token); k >= 0; k = l.indexOf(token, k + 1)) {
              const col = start + visibleWidth(l.slice(0, k))
              spans.push({ start: col, end: col + m.width, jid: m.jid })
            }
          }
          if (spans.length) mentions.set(at, spans)
          if (withStamp) { headers.add(at); stamped = true }
        })
      }
      const note = last as { at: number; line: string } | null
      if (bare && !stamped && note && visibleWidth(note.line) + 2 + visibleWidth(stamp) <= width) {
        const line = `${note.line}  ${stamp}`
        lines[note.at] = mine ? alignRight(line, width) : line
        headers.add(note.at); stamped = true
      }
      if (!bare) {
        const rect = this.bubble(lines, bubbleFrom, lines.length, mine, width, row.id !== selectedId && row.id !== this.drag?.id, pictures)
        // In a bubble, the time goes outside it on its last line: right after it for theirs, against the edge for mine.
        const at = lines.length - 1, line = lines[at] ?? ''
        const gap = rect ? (mine ? width - visibleWidth(stamp) : rect.end + 1) - visibleWidth(line) : 0
        const tailed = `${line}${' '.repeat(Math.max(0, gap))}${stamp}`
        if (!stamped && rect && !pictures.has(at) && gap >= 1 && visibleWidth(tailed) <= width) { lines[at] = tailed; headers.add(at); stamped = true }
      }
      // On its own line the time goes straight in, flush right for mine, without passing through the wrapping.
      if (!stamped) { headers.add(map.length); push(mine ? alignRight(stamp, width) : stamp, row) }
      // Reactions underneath, outside the bubble: each emoji, with how many when more than one person reacted with it.
      const rs = reactions.get(row.id)
      if (rs?.length) {
        const byEmoji = new Map<string, number>()
        for (const r of rs) byEmoji.set(r.emoji, (byEmoji.get(r.emoji) ?? 0) + 1)
        const parts = [...byEmoji].map(([emoji, n]) => (n > 1 ? `${emoji} ${n}` : emoji))
        out(dim(esc(parts.join('  '))), row)
      }
      for (let i = bubbleFrom; i < lines.length; i++) lines[i] = decorate(lines[i]!, row)
      push('', null)
    }
    // Room for the message just sent while it's on its way (flyDraft), made a row at a time; once it's here, its
    // rows stay blank until it lands in them, so it isn't seen twice.
    const f = this.flight
    if (f) {
      f.arrived ||= rows.some(r => r.from_me === 1 && r.text === f.text && Date.now() - r.ts * 1000 < 60000)
      if (!f.arrived) for (let i = 0; i < f.room; i++) push('', null)
      else if (f.frame < FLY_FRAMES) map.forEach((r, i) => { if (r?.from_me === 1 && r.text === f.text) lines[i] = '' })
    }

    // The marks go when the first marked message can no longer be edited.
    if (this.sentExpiry) clearTimeout(this.sentExpiry)
    this.sentExpiry = expires < Infinity ? setTimeout(() => { this.dirtyMessages = true; this.scheduleRender() }, Math.max(0, expires * 1000 - Date.now()) + 1000) : undefined
    this.lineMap = map
    this.headerLines = headers
    this.textLines = texts
    this.nameLines = names
    this.mentionLines = mentions
    this.images = images
    this.bigEmojis = bigs
    this.msgBox.setContent(lines.join('\n'))
    if (this.atBottom) this.msgBox.setScrollPerc(100)
    // The waiting line has just appeared above the oldest message in view: the view goes up the line or two to show it.
    else if (loadingNew && (wasAtTop || pin?.id === rows[0]?.id)) this.msgBox.scrollTo(0)
    else if (pin) { const first = this.firstLineOf(pin.id); if (first != null) this.msgBox.scrollTo(first - pin.offset) }
    this.renderedJid = jid
    this.loadingDrawn = loading
    // What's below the view, said beside the band, follows the messages.
    if (!this.plainInput && this.belowCount() !== this.belowShown) this.drawInput()
    if (f?.arrived && f.frame >= FLY_FRAMES) this.landed(false)
  }

  /** The message at the top of the view, and how far its first line is from the top (negative when it starts above). */
  private topAnchor(): { id: string; offset: number } | null {
    const rtof = this.msgBox._clines?.rtof
    if (!rtof) return null
    const base = this.msgBox.childBase
    for (let y = base; y < base + this.innerHeight(); y++) {
      const orig = rtof[y]
      if (orig == null) break
      const row = this.lineMap[orig]
      if (!row) continue
      const first = this.firstLineOf(row.id)
      return first == null ? null : { id: row.id, offset: first - base }
    }
    return null
  }

  /** The drawn line where a message starts. */
  private firstLineOf(id: string): number | undefined {
    const orig = this.lineMap.findIndex(r => r?.id === id)
    return orig < 0 ? undefined : this.msgBox._clines?.ftor?.[orig]?.[0]
  }

  /**
   * Scrolling past the top of a chat: first the stored messages the panel doesn't draw yet, another page each time;
   * once all of them are drawn, up to 50 more asked of the phone, until it has nothing older. The view stays on the
   * message that was at the top (renderMessages), so what arrives lands above it.
   */
  private loadOlder() {
    const jid = this.current
    if (!jid) return
    const limit = this.shown.get(jid) ?? PAGE
    if (limit !== -1 && store.countMessages(jid) > limit) {
      this.shown.set(jid, limit + PAGE)
      this.dirtyMessages = true
      return this.renderNow()
    }
    this.shown.set(jid, -1)
    if (this.olderPending.has(jid) || this.olderDone.has(jid)) return
    this.olderPending.add(jid)
    this.dirtyMessages = true
    this.renderNow()
    this.wa.fetchOlder(jid)
      .then(n => { if (!n) this.olderDone.add(jid) })
      .catch(e => logger.warn({ e: String(e), jid }, 'fetchOlder'))
      .finally(() => {
        this.olderPending.delete(jid)
        // The waiting line goes, with or without anything new, and what arrived lands above the message at the top.
        if (jid === this.current) { this.dirtyMessages = true; this.scheduleRender() }
      })
  }

  /**
   * Reserves the image's space and draws it if already decoded. The download and decoding only happen once the
   * image becomes visible in the panel (loadVisibleImages), never for all 300 messages at once.
   */
  /**
   * Pushes the message's image (or its placeholder while it loads). With `stamp`, the time goes two cells to the
   * right of the image's last row: mine stop short of the time's columns, like the text does, so the row reaches the
   * edge; the other side's are flush left and the time follows. Returns whether the stamp was placed.
   */
  private pushImage(row: MessageRow, push: (l: string, r: MessageRow | null) => void, images: ImageSlot[], lines: string[], width: number, mine = false, stamp?: string, preview?: { src: string; maxCols: number; maxRows: number; indent: number }, textEdge?: number): boolean {
    if (this.mode === 'none') { if (!preview) push(dim(`[${row.type}]`), row); return false }
    if (!preview && row.media_err && !this.imagePathFor(row)) { push(dim(t('mediaUnavailable', row.type)), row); return false }
    // A link preview has its own file (fetched by ensurePreview when it comes into view) and a smaller frame.
    const path = preview ? (fs.existsSync(preview.src) ? preview.src : null) : this.imagePathFor(row)
    const d = path ? cached(path) : undefined
    if (d instanceof Error) { if (!preview) push(dim(t('mediaUnreadable', row.type, esc(d.message))), row); return false }
    // Size: from the pixels if we already have them, otherwise from the dimensions the message carries, otherwise a default rectangle.
    const w = d?.w ?? (preview ? 4 : row.media_w ?? 4), h = d?.h ?? (preview ? 3 : row.media_h ?? 3)
    // In block mode the image takes up to 40 columns: each cell is a color pair the terminal (and a multiplexer
    // in between) has to paint, and a chat full of photos scrolls at the cost of those cells. In Kitty, with
    // real pixels, its natural size up to 60 columns is enough. The height never exceeds the panel.
    const maxRows = preview ? preview.maxRows : row.type === 'sticker' ? 12 : Math.max(4, this.innerHeight() - 2)
    const limit = Math.min(mine && stamp ? Math.max(1, width - 2 - visibleWidth(stamp)) : width, preview?.maxCols ?? width)
    // Mine end where the text ends (`textEdge`), short of the message's time, beside the image or not.
    const edge = textEdge ?? (mine && stamp ? Math.max(1, width - 2 - visibleWidth(stamp)) : width)
    const { cols, rows: fullRows } = this.pics
      ? cellSize(w, h, Math.min(limit, 60), Math.min(maxRows, 18))
      : cellSize(w, h, Math.min(limit, 40), maxRows, true)
    // A preview still on its way takes one line, not the frame of an image whose size isn't known yet.
    const rows = preview && !d ? 1 : fullRows
    const pad = mine ? Math.max(0, edge - cols) : preview?.indent ?? 0
    const tail = stamp && pad + cols + 2 + visibleWidth(stamp) <= width ? `  ${stamp}` : ''
    // The image's rows, the last one followed by the time; a blank row is only spaces up to where the time starts.
    const blank = tail ? ' '.repeat(pad + cols) : ''
    if (!d) {
      images.push({ row, origLine: lines.length, cols, rows, pad, src: preview?.src })
      const what = preview ? t('linkPreview') : row.type
      const label = `${' '.repeat(pad)}${dim(`[${what}${path ? ` ${t('loading')}` : !preview && row.media_path ? '' : ` ${t('downloading')}`}]`)}`
      push(rows === 1 ? label + tail : label, row)
      for (let i = 1; i < rows; i++) push(i === rows - 1 ? blank + tail : '', row)
      return !!tail
    }
    if (this.pics) {
      images.push({ row, origLine: lines.length, cols, rows, d, pad, path: path!, src: preview?.src })
      for (let i = 0; i < rows; i++) push(i === rows - 1 ? blank + tail : '', row)
    } else {
      // The slot is kept for the click, which has to land on the image's own cells; without a path, placeImages skips it.
      images.push({ row, origLine: lines.length, cols, rows, d, pad, src: preview?.src })
      halfBlocks(d, cols, rows).forEach((l, i, all) => push(' '.repeat(pad) + l + (i === all.length - 1 ? tail : ''), row))
    }
    return !!tail
  }

  /**
   * The message's lines [from, to) get a bubble background, like WhatsApp Web's: the rectangle around their text,
   * with a spare column on each side. Only tags and spaces are added around each line's own content, never inside
   * it, so no column moves (clicks, selection and images keep their places). The background is opened in three runs,
   * before, over and after the content: an image line ends in a full reset, which would leave the spare cells after
   * it bare. The lines in `skip` (a picture's rows) are left as they are, out of the rectangle and its measure.
   * Returns the rectangle's columns [start, end), also with `paint` off (the selected message, whose own background
   * spans the whole width, or one being dragged), so the time goes in the same place.
   */
  private bubble(lines: string[], from: number, to: number, mine: boolean, width: number, paint: boolean, skip = new Set<number>()): { start: number; end: number } | null {
    if (to <= from) return null
    const spans = lines.slice(from, to).map(l => {
      const start = l.length - l.trimStart().length
      return { start, end: Math.max(start, visibleWidth(l)) }
    })
    const used = spans.filter((sp, i) => sp.end > sp.start && !skip.has(from + i))
    if (!used.length) return null
    const minStart = Math.min(...used.map(sp => sp.start)), maxEnd = Math.max(...used.map(sp => sp.end))
    const bStart = Math.max(0, minStart - 1)
    const bEnd = Math.min(width, maxEnd + 1)
    if (!paint) return { start: bStart, end: bEnd }
    const bg = mine ? this.bubbleBg.mine : this.bubbleBg.theirs
    const on = (s: string) => (s ? `{${bg}-bg}${s}{/${bg}-bg}` : '')
    for (let i = from; i < to; i++) {
      if (skip.has(i)) continue
      const sp = spans[i - from]!, line = lines[i]!
      const blank = sp.end <= sp.start
      const start = blank ? bStart : Math.max(sp.start, bStart), end = blank ? bStart : sp.end
      lines[i] = ' '.repeat(bStart) + on(' '.repeat(start - bStart)) + on(blank ? '' : line.slice(sp.start)) + on(' '.repeat(Math.max(0, bEnd - end)))
    }
    return { start: bStart, end: bEnd }
  }

  /** The selected message's picture description, beside it, when images are drawn in half-blocks (which lose its detail). */
  private imageAside(row: MessageRow): string | undefined {
    const path = this.mode === 'blocks' ? this.pictureOf(row) : null
    return path ? this.describedAs(`${row.chat_jid} ${row.id}`, path) : undefined
  }

  /**
   * The message's picture to describe or show in the popup: an image or sticker's full file once it's here (the
   * thumbnail, when the file expired), or a link preview's image.
   */
  private pictureOf(row: MessageRow): string | null {
    if (row.type === 'image' || row.type === 'sticker') {
      const path = this.imagePathFor(row)
      return path && (path === mediaFile(row) || row.media_err) ? path : null
    }
    const preview = previewPath(row.chat_jid, row.id)
    return row.type === 'text' && hasPreviewImage(row) && fs.existsSync(preview) ? preview : null
  }

  /**
   * What a picture (`path`: a message's, see pictureOf, or a chat's photo) shows, from the model, ready for the screen,
   * by `key` (the message's chat and id, or the photo's chat): "a descrever…"
   * while it answers, nothing when it can't. Asked for once the picture is decoded, kept for the session, and given
   * up if neither the selection nor the popup is on it any more (dropDescribing). A model that doesn't answer is left
   * alone for a minute, as for the writing suggestions.
   */
  private describedAs(key: string, path: string): string | undefined {
    if (!llmEnabled) return undefined
    if (this.descriptions.has(key)) { const text = this.descriptions.get(key); return text ? dim(italic(esc(text))) : undefined }
    const d = cached(path)
    if (!d || d instanceof Error) return undefined
    if (this.describing?.key !== key && Date.now() >= this.llmDownUntil) {
      const abort = new AbortController()
      this.describing = { key, abort }
      const done = () => {
        this.describing = undefined
        this.dirtyMessages = true
        this.scheduleRender()
      }
      describe(d.png, abort.signal).then(text => {
        if (abort.signal.aborted) return
        this.descriptions.set(key, text)
        done()
      }, e => {
        if (abort.signal.aborted) return
        logger.debug({ e: String(e) }, 'llm: no description')
        this.llmDownUntil = Date.now() + 60000
        done()
      })
    }
    return this.describing?.key === key ? dim(t('describing')) : undefined
  }

  /** A description on its way for an image neither selected nor in the popup is given up. */
  private dropDescribing() {
    const keep = [this.selected ? `${this.selected.chat_jid} ${this.selected.id}` : '', this.viewing?.key ?? '']
    if (this.describing && !keep.includes(this.describing.key)) {
      this.describing.abort.abort()
      this.describing = undefined
    }
  }

  /**
   * Opens the message's picture in the popup: a link preview's once it's here, an image or sticker's once its full
   * file is (fetched first otherwise, like openMedia).
   */
  private openViewer(row: MessageRow) {
    const path = this.pictureOf(row), media = row.type === 'image' || row.type === 'sticker'
    if (!path || (media && path !== mediaFile(row))) {
      if (!media) return
      if (row.media_err) return this.flash(t('attachmentExpired'))
      this.wa.ensureMedia(row)
      return this.flash(t('downloading'))
    }
    this.viewing = { key: `${row.chat_jid} ${row.id}`, path, row }
    this.quickFor = undefined
    this.viewerBox.show()
    this.renderNow()
  }

  /** Opens a chat's photo in the popup, as large as it fits, with its description: a click on it in the list. */
  private openPhoto(jid: string) {
    const avatar = (this.profiles.get(jid) ?? store.getProfile(jid))?.avatar
    if (!avatar) return
    this.viewing = { key: `photo ${jid}`, path: path.join(mediaDir(jid), avatar) }
    this.viewerBox.show()
    this.renderNow()
  }

  private closeViewer() {
    this.viewing = null
    this.viewerImg = undefined
    this.viewerBox.hide()
    this.dropDescribing()
    this.renderNow()
  }

  /**
   * The popup's content: the picture as large as the box allows, centred, with its description under it (up to 80
   * columns wide, for reading), the two together centred in height. The description's room (four lines and a blank
   * one above) is kept from the start, so the picture doesn't shrink when it arrives; only a longer one takes more.
   * In half-blocks the picture is in the lines; with Kitty, placeImages puts it there.
   */
  private drawViewer() {
    const view = this.viewing!, box = this.viewerBox, path = view.path
    const iw = Math.max(1, num(this.screen.width) - 4 - 2), ih = Math.max(1, num(this.screen.height) - 2 - 2)
    const d = cached(path)
    this.viewerImg = undefined
    if (!d) {
      decode(path).then(() => { if (this.viewing === view) this.scheduleRender() })
      return box.setContent(dim(t('loading')))
    }
    if (d instanceof Error) return box.setContent(dim(t('mediaUnreadable', view.row?.type ?? t('image'), esc(d.message))))
    const about = this.describedAs(view.key, path)
    const text = about ? wrapTagged(about, Math.min(iw, 80)) : []
    const below = Math.max(llmEnabled ? 5 : 0, text.length ? text.length + 1 : 0)
    const { cols, rows } = cellSize(d.w, d.h, iw, Math.max(1, ih - below), true)
    const top = Math.max(0, Math.floor((ih - rows - below) / 2))
    const left = Math.floor((iw - cols) / 2)
    const block = Math.max(0, ...text.map(visibleWidth)), indent = ' '.repeat(Math.max(0, Math.floor((iw - block) / 2)))
    const picture = this.pics ? Array.from({ length: rows }, () => '') : halfBlocks(d, cols, rows).map(l => ' '.repeat(left) + l)
    box.setContent([...Array.from({ length: top }, () => ''), ...picture, ...(text.length ? ['', ...text.map(l => indent + l)] : [])].join('\n'))
    this.viewerImg = { d, path, x: 2 + 1 + left, y: 1 + 1 + top, cols, rows }
  }

  /**
   * Puts `text` beside the message's picture (its image or its link preview), just drawn in half-blocks, from its
   * top row: to its right for theirs, short of the time on its last row; to its left for mine, the block of lines
   * ending two cells before it. Returns false, touching nothing, when there's less than 16 columns there or the text
   * needs more rows than the picture has.
   */
  private placeAside(lines: string[], img: ImageSlot | undefined, row: MessageRow, text: string, width: number, mine: boolean, stamped: boolean): boolean {
    if (!img?.d || img.row !== row) return false
    const room = mine ? img.pad - 2 : width - img.pad - img.cols - 2
    if (room < 16) return false
    const wrapped = wrapTagged(text, room)
    if (wrapped.length > img.rows - (stamped && !mine ? 1 : 0)) return false
    const block = Math.max(...wrapped.map(visibleWidth))
    wrapped.forEach((l, i) => {
      const at = img.origLine + i, line = lines[at]!
      lines[at] = mine ? `${' '.repeat(img.pad - 2 - block)}${l}${' '.repeat(block - visibleWidth(l) + 2)}${line.slice(img.pad)}` : `${line}  ${l}`
    })
    return true
  }

  /** Drawn lines [start, end) of an image, and the panel's visible window. */
  private imageSpan(img: ImageSlot): { top: number; bottom: number } | null {
    const top = this.msgBox._clines?.ftor?.[img.origLine]?.[0]
    return top == null ? null : { top, bottom: top + img.rows }
  }

  /** The image (or its placeholder) whose cells hold the screen position, if any. */
  private imageAt(x: number, y: number): ImageSlot | null {
    const line = this.msgBox.childBase + (y - num(this.msgBox.atop) - num(this.msgBox.itop))
    const col = x - num(this.msgBox.aleft) - num(this.msgBox.ileft)
    for (const img of this.images) {
      const span = this.imageSpan(img)
      if (span && line >= span.top && line < span.bottom && col >= img.pad && col < img.pad + img.cols) return img
    }
    return null
  }

  /** After each frame: downloads and decodes only the images that are in view. */
  private loadVisibleImages() {
    if (!this.current || this.pickerOpen) return
    const base = this.msgBox.childBase, innerH = this.innerHeight()
    for (const img of this.images) {
      if (img.d) continue
      const span = this.imageSpan(img)
      if (!span || span.bottom <= base || span.top >= base + innerH) continue
      const row = store.getMessage(img.row.chat_jid, img.row.id) ?? img.row
      // A link preview: its own file, fetched by the server when it isn't there yet.
      if (img.src) {
        if (!fs.existsSync(img.src)) { this.wa.ensurePreview(row); continue }
        if (!cached(img.src)) decode(img.src).then(() => { if (this.current === row.chat_jid) { this.dirtyMessages = true; this.scheduleRender() } })
        continue
      }
      const path = this.imagePathFor(row)
      if (path && !cached(path)) {
        uiLog.info({ id: row.id, path }, 'decode visible image')
        decode(path).then(() => { if (this.current === row.chat_jid) { this.dirtyMessages = true; this.scheduleRender() } })
      }
      // Thumbnail by hand while the full file hasn't arrived; videos and gifs stay with just the thumbnail.
      if (!row.media_path && !row.media_err && row.type !== 'video' && row.type !== 'gif') this.wa.ensureMedia(row)
    }
  }

  /** After each blessed frame: re-places the visible Kitty images in the messages panel. */
  private placeImages() {
    if (!this.pics) return
    this.pics.clear()
    const v = this.viewerImg
    if (v) return this.pics.place(v.path, v.d, v.x + 1, v.y + 1, v.cols, v.rows)
    // The chats' photos in their tabs, before the names; with sixels in half-blocks, as on the screen's last row a
    // picture would scroll it.
    if (this.pics instanceof KittyImages) for (const tp of this.tabPhotos) this.pics.place(tp.path, tp.d, num(this.tabsBar.aleft) + tp.x + 1, num(this.tabsBar.atop) + 1, 2, 1)
    // With the list open, the pictures of the chats whose two rows are in view.
    if (this.pickerOpen) {
      for (const a of this.visibleAvatars()) this.pics.place(a.path, a.d, a.x + 1, a.y + 1, 4, 2)
      return
    }
    if (!this.current || this.pickerOpen) return
    const clines = this.msgBox._clines
    if (!clines?.ftor) return
    const base = this.msgBox.childBase
    const innerH = this.innerHeight()
    const col = num(this.msgBox.aleft) + num(this.msgBox.ileft) + 1
    for (const img of this.images) {
      if (!img.d || !img.path) continue
      const top = clines.ftor[img.origLine]?.[0]
      if (top == null) continue
      const bottom = top + img.rows
      const visTop = Math.max(top, base), visBottom = Math.min(bottom, base + innerH)
      if (visTop >= visBottom) continue
      const row = num(this.msgBox.atop) + num(this.msgBox.itop) + (visTop - base) + 1
      this.pics.place(img.path, img.d, col + img.pad, row, img.cols, visBottom - visTop, (visTop - top) / img.rows, (visBottom - top) / img.rows)
    }
    // Emoji on their own twice their size, where the terminal doesn't draw text bigger itself: each in its picture,
    // four cells by two, once both their lines are in view.
    if (this.textSizing) return
    for (const b of this.bigEmojis) {
      const top = clines.ftor[b.line]?.[0]
      if (top == null || top < base || top + 1 >= base + innerH) continue
      const row = num(this.msgBox.atop) + num(this.msgBox.itop) + (top - base) + 1
      b.emoji.forEach((e, k) => { const d = this.emojiImage(e); if (d) this.pics!.place(`emoji:${e}`, d, col + b.col + 4 * k, row, 4, 2) })
    }
  }

  /**
   * An emoji's picture for Kitty's graphics, drawn (emojiPng) the first time it's asked for, and the frame drawn again
   * once it is; undefined meanwhile. One that can't be drawn makes the messages be laid out again, without it big.
   */
  private emojiImage(emoji: string): Decoded | null | undefined {
    if (this.emojiImages.has(emoji)) return this.emojiImages.get(emoji)
    if (this.emojiDrawing.has(emoji)) return undefined
    this.emojiDrawing.add(emoji)
    void emojiPng(emoji).then(p => {
      this.emojiDrawing.delete(emoji)
      this.emojiImages.set(emoji, p && { w: p.size, h: p.size, png: p.png, rgba: Buffer.alloc(0), rw: 0, rh: 0 })
      if (!p) this.dirtyMessages = true
      this.scheduleRender()
    })
    return undefined
  }

  /** The chat list's pictures whose two rows are both in view, and the screen cell (0-based) of their top left. */
  private visibleAvatars(): { path: string; d: Decoded; x: number; y: number }[] {
    const base = (this.picker as unknown as { childBase: number }).childBase
    const rows = num(this.picker.height) - num(this.picker.iheight)
    const x = num(this.picker.aleft) + num(this.picker.ileft), y = num(this.picker.atop) + num(this.picker.itop)
    return [...this.pickerAvatars].filter(([i]) => i >= base && i + 1 < base + rows).map(([i, a]) => ({ ...a, x, y: y + i - base }))
  }

  /**
   * After each frame, the emoji of the messages drawn twice their size (bigEmojis), written on their blank cells by
   * the terminal itself (OSC 66, two cells wide each before doubling, so a skin tone or a flag takes no more), on the
   * terminal's own background: those with both lines in view and their cells still blank in blessed's buffer, so
   * whatever is drawn over them (a notice, the rising emoji) keeps its place. Written again every frame, as blessed
   * may have written over any of them, which takes the whole emoji away; those drawn last that went elsewhere or
   * away are cleared first, where blessed believes its blank cells are already on the screen.
   */
  private paintBigEmoji() {
    if (!this.textSizing) return
    const lines = this.screenRows('lines')
    const blank = (x: number, y: number, w: number) => [y, y + 1].every(r => {
      const row = lines[r]
      if (!row) return false
      for (let c = x; c < x + w; c++) if (row[c]?.[1] !== ' ') return false
      return true
    })
    const now: typeof this.bigShown = []
    let out = ''
    if (this.current && !this.pickerOpen && !this.showingQr && !this.viewing) {
      const clines = this.msgBox._clines, base = this.msgBox.childBase, innerH = this.innerHeight()
      const x0 = num(this.msgBox.aleft) + num(this.msgBox.ileft), y0 = num(this.msgBox.atop) + num(this.msgBox.itop)
      for (const b of this.bigEmojis) {
        const top = clines?.ftor?.[b.line]?.[0]
        if (top == null || top < base || top + 1 >= base + innerH) continue
        const x = x0 + b.col, y = y0 + top - base, w = b.emoji.length * 4
        if (!blank(x, y, w)) continue
        now.push({ x, y, w, key: b.emoji.join('') })
        out += `\x1b[${y + 1};${x + 1}H${b.emoji.map(e => `\x1b]66;s=2:w=2;${e}\x1b\\`).join('')}`
      }
    }
    const codeAttr = (this.screen as unknown as { codeAttr: (attr: number) => string }).codeAttr.bind(this.screen)
    let clear = ''
    for (const o of this.bigShown) {
      if (now.some(n => n.x === o.x && n.y === o.y && n.key === o.key) || !blank(o.x, o.y, o.w)) continue
      for (const r of [o.y, o.y + 1]) {
        clear += `\x1b[${r + 1};${o.x + 1}H`
        for (let c = o.x; c < o.x + o.w; c++) clear += `${codeAttr(lines[r]![c]![0])} `
      }
    }
    this.bigShown = now
    if (clear || out) (this.screen.program as unknown as { _write: (s: string) => void })._write(`\x1b7${clear}\x1b[0m${out}\x1b[0m\x1b8`)
  }

  /** After each frame, with sixels: the pictures placed (placeImages) drawn over their cells, last of all (sixel.ts). */
  private flushSixels() {
    if (!(this.pics instanceof SixelImages)) return
    const codeAttr = (this.screen as unknown as { codeAttr: (attr: number) => string }).codeAttr.bind(this.screen)
    this.pics.flush(this.screenRows('lines'), this.screenRows('olines'), codeAttr)
  }

  /** blessed's frame buffers: `lines` is the frame being built, `olines` the one last drawn to the terminal. */
  private screenRows(which: 'lines' | 'olines'): ([number, string][] | undefined)[] {
    return (this.screen as unknown as Record<string, ([number, string][] | undefined)[]>)[which] ?? []
  }

  /**
   * After each frame, the half-block images in view get their exact colours, and the message bubbles theirs
   * (RgbPainter). An image cell is only taken when blessed's buffer still holds the half-block it was given there,
   * so whatever is drawn over an image (a notice, the reply header, the quick reactions, a dragged message) keeps
   * its place; a bubble cell is any cell of the panel (or, with the chat list open, of the list, where only the
   * selected row has that background) in one of the bubbles' greys, repainted with its own character, colour and
   * attributes over the exact background.
   */
  private paintRgb() {
    if (!this.rgbPaint) return
    const olines = this.screenRows('olines')
    const cells: RgbCell[] = []
    const clines = this.msgBox._clines
    const lines = this.screenRows('lines')
    const rgbAt = (rgb: Uint8Array, i: number) => `${rgb[i * 3]};${rgb[i * 3 + 1]};${rgb[i * 3 + 2]}`
    // The cells a picture takes: one of its dark pixels can come out, in the 256 colours, in a bubble's grey, and
    // painted by both the bubble lost its colour to the bubble's in the first frame and got it back at the next.
    const pictures = new Set<number>()
    // Row `r` of a picture `cols` wide, drawn from screen cell (left, y).
    const pictureRow = (grid: BlockGrid, cols: number, r: number, left: number, y: number) => {
      const row = lines[y]
      if (!row) return
      for (let c = 0; c < cols; c++) {
        const cell = blockCell(grid.idx, cols, r, c)
        if (!cell) continue
        const x = left + c, held = row[x]
        const attr = held?.[0] ?? -1
        // Bold is the only flag a picture's cell may carry: the selected row of the chat list has it.
        if (held?.[1] !== cell.ch || (attr >> 18) & ~1 || ((attr >> 9) & 0x1ff) !== (cell.fg < 0 ? 0x1ff : cell.fg) || (attr & 0x1ff) !== (cell.bg < 0 ? 0x1ff : cell.bg)) continue
        cells.push({ x, y, ch: cell.ch, w: 1, sgr: `0;38;2;${rgbAt(grid.rgb, cell.fgAt)}${cell.bgAt < 0 ? '' : `;48;2;${rgbAt(grid.rgb, cell.bgAt)}`}` })
        pictures.add(y * 65536 + x)
      }
    }
    if (this.current && !this.pickerOpen && !this.showingQr && clines?.ftor) {
      const base = this.msgBox.childBase, innerH = this.innerHeight()
      const x0 = num(this.msgBox.aleft) + num(this.msgBox.ileft), y0 = num(this.msgBox.atop) + num(this.msgBox.itop)
      for (const img of this.mode === 'blocks' ? this.images : []) {
        if (!img.d) continue
        const top = clines.ftor[img.origLine]?.[0]
        if (top == null) continue
        const visTop = Math.max(top, base), visBottom = Math.min(top + img.rows, base + innerH)
        if (visTop >= visBottom) continue
        const grid = blockGrid(img.d, img.cols, img.rows)
        for (let ln = visTop; ln < visBottom; ln++) pictureRow(grid, img.cols, ln - top, x0 + img.pad, y0 + ln - base)
      }
    }
    // The pictures in the chat list.
    if (this.pickerOpen && this.mode === 'blocks') {
      for (const a of this.visibleAvatars()) {
        const grid = blockGrid(a.d, 4, 2)
        pictureRow(grid, 4, 0, a.x, a.y)
        pictureRow(grid, 4, 1, a.x, a.y + 1)
      }
    }
    // The tabs' photos, in half-blocks.
    for (const tp of this.mode === 'blocks' || this.mode === 'sixel' ? this.tabPhotos : []) pictureRow(blockGrid(tp.d, 2, 1), 2, 0, num(this.tabsBar.aleft) + tp.x, num(this.tabsBar.atop))
    // The popup's picture, over the panel.
    const v = this.mode === 'blocks' ? this.viewerImg : undefined
    if (v) {
      const grid = blockGrid(v.d, v.cols, v.rows)
      for (let r = 0; r < v.rows; r++) pictureRow(grid, v.cols, r, v.x, v.y + r)
    }
    // The bubbles' cells: the message panel's and the rows under it (the text being written, the one just sent flying,
    // the tabs), or, with the chat list open, its rows (the selected chat's) and the same rows under it, where the
    // filter is written over its tab.
    const W = num(this.screen.width), H = num(this.screen.height)
    const p = this.picker
    const regions = this.pickerOpen
      ? [{ x0: num(p.aleft) + num(p.ileft), y0: num(p.atop) + num(p.itop), x1: num(p.aleft) + num(p.width) - num(p.iwidth) + num(p.ileft), y1: num(p.atop) + num(p.height) - num(p.iheight) + num(p.itop) },
        { x0: 0, y0: num(this.input.atop), x1: W, y1: H }]
      : this.current && !this.showingQr ? [{ x0: 0, y0: num(this.msgBox.atop) + num(this.msgBox.itop), x1: W, y1: H }] : []
    if (this.bubbleRgb && regions.length) {
      const lines = this.screenRows('lines')
      const bubbles = { [this.bubbleBg.mine]: this.bubbleRgb.mine, [this.bubbleBg.theirs]: this.bubbleRgb.theirs } as Record<number, string>
      // blessed's attribute flags as SGR codes: bold, underline, blink, inverse, invisible, and italic (italic.ts).
      const FLAGS: [number, number][] = [[1, 1], [2, 4], [4, 5], [8, 7], [16, 8], [32, 3]]
      for (const { x0, y0, x1, y1 } of regions) for (let y = y0; y < y1; y++) {
        const row = lines[y]
        if (!row) continue
        for (let x = x0; x < x1; x++) {
          const held = row[x]
          if (!held || pictures.has(y * 65536 + x)) continue
          const rgb = bubbles[held[0] & 0x1ff]
          // The second cell of a wide character holds blessed's marker: the character itself covers it.
          if (!rgb || held[1] === '\u0003') continue
          const flags = held[0] >> 18, fg = (held[0] >> 9) & 0x1ff
          const codes = ['0', ...FLAGS.filter(([bit]) => flags & bit).map(([, code]) => String(code)), fg === 0x1ff ? '39' : `38;5;${fg}`, `48;2;${rgb}`]
          cells.push({ x, y, ch: held[1] || ' ', w: Math.max(1, strWidth(held[1] || ' ')), sgr: codes.join(';') })
        }
      }
      cells.sort((a, b) => a.y - b.y || a.x - b.x)
    }
    this.rgbPaint.paint(cells, olines)
  }

  /** The faint rule over the plain line, with the search's count (borderHeader); none over the band, in a chat. */
  private drawBorder() {
    if (!this.plainInput) return void this.ruleTop.hide()
    const width = num(this.screen.width), h = this.ruleChar
    const header = this.borderHeader ? ` ${truncate(this.borderHeader, Math.max(1, width - 6))} ` : ''
    this.ruleTop.setContent(`${faint(esc(h.repeat(2)))}${dim(esc(header))}${faint(esc(h.repeat(Math.max(0, width - 2 - strWidth(header)))))}`)
    this.ruleTop.show()
  }

  /** How many messages are below the view, all of their rows out of it; 0 at the bottom. */
  private belowCount(): number {
    if (this.atBottom) return 0
    const base = this.msgBox.childBase + this.innerHeight()
    return new Set(this.lineMap.slice(base).filter((r): r is MessageRow => !!r).map(r => r.id)).size
  }

  private redraw() {
    this.screen.program.clear()
    this.screen.realloc()
    this.dirtyMessages = true
    this.dirtyTabs = true
    this.drawInput()
    this.renderNow()
  }

  private openMedia(row: MessageRow) {
    const file = mediaFile(row)
    if (!file) {
      if (row.media_err) return this.flash(t('attachmentExpired'))
      this.wa.ensureMedia(row)
      return this.flash(t('downloading'))
    }
    // The desktop's own opener: `open` on macOS, `xdg-open` elsewhere.
    const opener = process.platform === 'darwin' ? 'open' : 'xdg-open'
    const child = spawn(opener, [file], { detached: true, stdio: 'ignore' })
    child.on('error', e => this.flash(t('cannotOpen', opener, e.message)))
    child.unref()
  }
}
