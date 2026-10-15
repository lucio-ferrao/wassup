# wassup

A WhatsApp client for the terminal, inspired by [wechit](https://github.com/LingDong-/wechit). It connects as a
"linked device" through the [baileys](https://github.com/WhiskeySockets/Baileys) library, keeps everything in a local
SQLite database and draws the interface with panels, mouse and images right in the terminal.

## Running

```sh
npm install -g github:lucio-ferrao/wassup --allow-git=all --install-links
wa              # from anywhere: opens on the chat list
wa emma         # opens straight into the chat whose name or number contains "emma"
```

`--install-links` makes npm put a real copy in place: without it npm 11 links the package to a temporary clone it then
deletes, and the install fails ("spawn sh ENOENT" in `node_modules/baileys`) or ends up empty. `--allow-git=all` lets
npm 12 install from GitHub, which it refuses by default (npm 11 needs `all`, as it turns `root` down for a global
install; npm 10 ignores it). npm 12 then warns that it skipped three dependencies' install scripts; they aren't
needed. Without installing, `npx --allow-git=all --install-links github:lucio-ferrao/wassup` runs it from npm's cache
(the first time downloads the dependencies, about 100 MB). Or, from a clone: `npm install`, then `./wa` (or `npm
start`).

The first time, a QR code appears: on the phone, WhatsApp › Settings › Linked devices › Link a device. The session is
saved and later runs connect directly.

While you write in a chat the client announces itself "available" to WhatsApp, so the phone does not notify, just as
with WhatsApp Web open. Only writing counts (typing, deleting, pasting or sending): not opening it, the mouse, other
keys or getting the focus. After 2 minutes without writing it goes back to "unavailable", and at once when its window
or pane loses the focus, in terminals that report it (Herdr does). A message arriving in the chat you have open counts
as read only while you show as online and, in Herdr, its pane has the focus; otherwise it stays unread, and the phone
notifies, until you write, open the chat or come back to the terminal.

Requirements: Node 22.13 or newer (it uses the SQLite built into Node).

Language: Portuguese or English, from `WA_LANG` or the locale (`LC_ALL`, `LC_MESSAGES`, `LANG`). It sets the interface
texts, the emoji names and the language of the writing suggestions.

## Interface

The chat's photo at the top left, square and as tall as its name (real pixels with Kitty or sixels, half-blocks
otherwise; a click on it opens it large), and the name after it, big, drawn in the rounded lines of the `wassup` over
the chat list, in lowercase and in the colour the chat has in groups (only the words that fit; on one line, bold, with
no room or no UTF-8 locale); a click on the name goes back to the chat list, the tab left open. Messages across the full
width under it, and under them, as your next message, the text being written on your bubbles' green, in a band as wide
as your bubbles can be, growing upwards with the text; while the messages are scrolled up, `↓` and how many are below
sit beside the band, where the time goes, and a click on them goes down to the latest. When it's sent, the conversation
first moves up to make room for it, and then the band narrows to the bubble and rises into that room. Under the band,
the tab bar, with the connection state on the right. Beyond the bubbles, no backgrounds of its own: the colours are the
terminal theme's, and on startup the terminal is asked for its real background colour to pick light or dark shades.

- **Tabs**: one per open chat, under the band where you write: its name and its state's emoji (see Writing suggestions;
  its place kept blank till there's one), or 🔔 in its place with messages unread; the active one on the band's green, as
  its label, its name in the colour it has in groups, the others on a quieter grey. A click on another tab goes to that
  chat; Esc closes the active one, and the faint ✖ a tab shows in its state's place while the pointer is over it closes
  that one (on the list's own tab, it closes the list, as Esc does there). Tab cycles through them, or, with only one
  open, opens the chat list; whatever is left unsent stays with each chat. While someone is typing in a chat, two emoji
  take turns in its tab where its state's emoji is (💬, 💭). New messages in a chat without a tab open one without
  activating it, with a passing notice over it and the bell; an archived chat stays quiet.
- **Chats**: `wa` starts on the list, and `/` opens it again, under the app's name with its 👾, most recent at the
  bottom, under a separator for each day, as the messages are (`hoje`, `ontem`, `12 set`), a blank row between chats.
  Each takes two rows, its photo from WhatsApp on the left of both (four cells by two, real pixels with Kitty or sixels,
  half-blocks otherwise; its initials on its colour without one). On the first row, the name in its colour, its state's
  emoji (or, while someone types there, 💬 and 💭 taking turns in its place), the unread count in red, 👀 while the person
  is online, and at the right edge the time of the last message, its day being its separator's (with a filter, which has
  no separators, the day too: `ontem 21:35`, `12 set 21:35`), in the column your messages' times end in. On the second,
  under the name, the person's number (without the country code when it's the same as mine, and 💼 after it for a
  WhatsApp Business account, known from its messages or its business profile) or a group's number of members. The
  selected chat is on the green of the band where you write, as the active tab is, up to its time, which stays out flush
  right, as a message's does. Back within 30 seconds, the list opens on the chat you were in, selected; otherwise none
  is, till the first ↑ or ↓ selects the most recent, or a filter its best match, and till then Enter, Tab and → open
  nothing. Your chat with yourself goes by your profile name and "(eu)", where WhatsApp gives only a masked number.
  Photos, "abouts" and group sizes are asked of WhatsApp for the chats in view, at most once a day, and kept; a chat
  opened whose person has no name here (only the number, or the masked one WhatsApp gives for someone not saved) has its
  latest messages asked of the phone again, at most once a day, as its copy may carry the profile name; a click on a
  photo opens it large, with its description, as images do. What's typed in the band, with the list's own tab, `wassup`,
  active under it (after its name, 💬 and 💭 taking turns while someone types in any chat, or else 🔔 with messages unread
  in any, and 👾 with neither), filters it word by word, ignoring accents and case, with the matches underlined; Enter,
  Tab, → or a click opens. Ctrl+F there searches the messages of every chat instead (the tab says `procurar`; again or
  Esc back to the names): from two characters on, the list holds those that have all the words, the most recent at the
  bottom, each as its chat, who wrote it and when, and two lines of its text from a little before the match, the matches
  in reverse video; Enter or a click opens the chat on that message.
- **Messages**: yours on the right, each in a bubble with WhatsApp Web's colours where the terminal takes 24-bit colour
  (yours, on a dark theme, toned down to the brightness of theirs) and two discreet greys otherwise, with the time
  outside it, yours with their state in its separator (`14 06` not sent yet, `14.06` sent, `14:06` delivered; `14:06❮`
  the last of yours that was read) and a link's preview image inside it above the text; the pictures of images,
  stickers, videos and GIFs stay out of it, with their caption in it, and emoji on their own go bare (one to three of
  them twice their size, two lines tall and on the terminal's background: drawn by the terminal itself where it draws
  text bigger, as Kitty does, asked on startup, or else in pictures where it shows Kitty's graphics, as Ghostty does, or
  sixels); each day starts with a separator and a blank line. Mouse wheel or PgUp/PgDn; scrolling past the top brings
  older messages, first the stored ones and then from the phone, and Ctrl+↓ or Ctrl+PgDn goes back to the latest. An
  attachment not downloaded yet shows `⤓`: a click fetches it into the app's media folder, where it stays, and the mark
  goes; once there, a click opens it with `xdg-open` (`open` on macOS), except images and stickers, which open in a
  popup in the client, as large as it fits, with the local model's description under them, as do link previews; any key
  or click closes it. ↑ selects a message: typing replies to it, `:` reacts (erasing what was typed, down to nothing,
  drops the reply or the reaction), Backspace opens one of yours for editing, and Delete or Ctrl+D deletes it once
  pressed again (the band says how): yours for everyone, as the phone does, for two days after it was sent, and any
  other only for you. Mentions show the person's first name in their colour (`@Ana`); a click on one, or in a group on a
  member's name, opens the chat with them; a click on a pin (`📌 pinned a message`) goes to the message it's about. The
  `☺` on the line with the time of a message under the pointer, after the time (before the bubble in yours), opens its
  quick reactions; a double click, dragging it to the right, or → with it selected, starts a reply. Ctrl+F searches the
  open chat's whole history, accents and case aside: the input becomes `procurar ❯`, the matches are shown in reverse
  video and the most recent one selected, ↑ goes to an older one and ↓ to a newer one, with a count on the rule above
  the input; Enter stays on it, Esc closes.
- **Input**: grows with the text up to half the screen; what's being replied to, reacted to or edited shows on the
  band's first row; Enter sends, Shift+Enter or Ctrl+J start a new line, and pasting several lines keeps them. Ctrl-U
  clears, Shift-Backspace deletes a word (with the Kitty keyboard protocol). `:` and a letter open the emoji list;
  ↑/↓, Enter, Tab, → or a click pick one. In a group, `@` lists those who wrote in it, most recent first, narrowed by
  the start of any of their names (`@lem` finds Rita Lemos); the one picked goes in as `@Rita` (the whole name when
  two share the first), and the message goes out mentioning them, as the phone does.
- Esc closes, in order: the filter, the list, the active tab. Closing the last tab goes back to the list, and Esc there,
  with no tab left, quits; started for a chat (`wa <name>`, as the panes wa opens in Herdr for new messages are) closing
  it quits, the pane with it. Ctrl-C quits at once; Ctrl-R redraws the screen.
- **Several terminals**: the first process is the server with the WhatsApp connection; the next ones connect to it
  through a socket and are interface only, each with its own tabs. If the server ends, another one takes over.
- **Single chat**: `wa <name>` opens only that chat, its tab alone under the band, without notices from others; the
  `/` list swaps it. An emoji on its own, as a message or a reaction, sends a big copy of it floating up the panel,
  drawn in block characters from the system's emoji font (the same glyph and colours the terminal shows), or, where
  the system has no colour one, from the Noto Color Emoji that comes with wassup; needs `sharp`.

### Herdr

Inside [Herdr](https://herdr.dev) wassup starts in single-chat mode and each chat is a Herdr tab or pane: in the list,
Enter puts the chat in this pane, → opens it in a new pane beside it and Tab in a new tab (all three focus the one
that already has it), and a message from a chat without one (and not archived) opens it in the background, in a pane
when its tab is already split and in a tab otherwise, still unread (so the phone notifies) until you go to it. Tab,
with nothing typed, moves to the next conversation's pane or tab, in the order Herdr shows them, or, with no other
one, opens the chat list. wassup shows up in Herdr's agent list under the chat's name alone: `working` while the other
person types, `done` (blue) from a new message (or when they stop typing) until you look at its pane, `idle`
otherwise; alone in its tab, the tab takes the contact's first name. Outside Herdr the terminal window title carries
the name, with `●` for unread messages or the same two emoji taking turns while they type.

### Formatting and emoji

WhatsApp markup is shown with terminal attributes: `*bold*`, `_italic_`, `~strikethrough~`, `` `code` ``, `> quote`.
When sending, write the markup as on the phone. `:name:` codes are replaced by the emoji as soon as the second `:` is
typed, with names in Portuguese and in English (`:thumbsup:` 👍, `:kissing_heart:` 😘, `:coffee:` ☕ …; the list is in
`src/emoji.ts`). The suggestion list (`:` and a letter) finds them by either language's names too, showing the user's
language's when both match; `:` on its own lists the basic smileys. Classic smileys (`:)`, `;)`, `<3` …), on their own
after a space, get their emoji offered in the list, which Tab or → take; a space after one either goes in, the smiley
sent as typed, or takes the emoji too, as learnt from what you did the last two times: taking the emoji both times (Tab,
→, or the space itself) makes the space take it, leaving it both times (a space, Esc, or Enter, which sends the smiley
as typed) makes it go in; kept for every terminal. Emoji by their names (`:smi`) learn the same, apart, from the first
character after the colon; a bare `:` never takes one. Links are shown without what only tracks who shared them
(`fbclid`, `utm_…`, `igsh`, `si` and the many others in [ClearURLs](https://github.com/ClearURLs/Rules)' rules,
redirections through a site undone) and shortened (a long path cut in the middle); from social networks and short links
(Facebook, Instagram, YouTube, Spotify, TikTok, X, `maps.app.goo.gl` …) only the readable parts stay, the opaque
identifiers made `…` and the parameters left out (`instagram.com/reel/…`, `instagram.com/stories/rita.lemos/…`,
`youtube.com/watch`). A click copies the whole clean link.

### Images

On startup the client asks the terminal what it can do, assuming nothing from `TERM`. In terminals with the Kitty
graphics protocol (Ghostty, Kitty, WezTerm, Konsole, iTerm2 3.7) images, stickers and thumbnails are shown for real
inside the panel; in those that take sixels instead (foot, among others), announced in their identification, their
cells' size in pixels asked and `sharp` there to encode them, too, but for the tabs' photos, in half-blocks, as on the
screen's last row a picture would scroll it; elsewhere, and inside Herdr (which doesn't pass the placements through),
they are drawn with coloured half-blocks, each the average colour of the area it covers: in 24-bit colour when the
terminal confirms it (XTGETTCAP or DECRQSS; Herdr does), in the 256-colour palette otherwise.
`WA_IMAGES=kitty|sixel|blocks|none` forces the mode, and `WA_COLORS=truecolor|256` the colours. In half-blocks, the
selected image, sticker or link preview gets a sentence or two from the local model (see below) on what it shows, beside
it, or under it when there's no room.

## Writing suggestions

With a local `llama-server` at `http://127.0.0.1:8080` (or `WA_LLM`), model `gemma4-26b` (or `WA_LLM_MODEL`), the
input asks for a suggestion shortly after the last key, with the latest messages as context: the letters missing from
the word being typed, right at the cursor, and every wrong passage already written (spelling, accents, grammar, a
missing comma) underlined in yellow, with the right word right above the one the cursor is on, or the last. Tab, →
or a click on it accept it; the others stay underlined. The prompt is in the user's language. `WA_LLM=off` turns the
model off. Without it (`WA_LLM=off`, or no server answering, then for a minute before asking again) a small local
spell checker stands in, words only, in the same way: Hunspell in WebAssembly with LibreOffice's Portuguese (Portugal)
and English dictionaries, a word being right in any of them; words you wrote yourself more than once and the names of
contacts and chats count as known, unless all they lack is an accent.

Your last text message is checked the same way once sent, and an earlier one when you select it, while WhatsApp still
lets them be edited (15 minutes): the wrong passages are underlined in yellow in the bubble; a click on one floats its
correction above it, and a click on that edits the message with it. The marks go when the 15 minutes are up.

Each chat also gets a state, one emoji the model picks for the conversation now (its tone and what's going on), from
its latest messages, as many as fit in the model's context; it shows after the chat's name, over the input and in the
chat list, while the chat's last message is no more than a week old. The server works them out calmly, as the model
takes one request at a time: only for chats with new messages, quiet for two minutes, open in a terminal or active in
the last day, from the most recent to the oldest; one at a time, a look every 30 seconds; never while you're writing
in a terminal (a request under way gives way); and waking the model when it's asleep, its requests keeping it awake
while the queue goes through.

## Data

Everything lives in `~/.config/wa` (or `WA_HOME`), readable by this user only (mode 700, umask 077 for whatever is
written):

| Path | Contents |
|---|---|
| `auth/` | Session credentials (delete to link again) |
| `wa.db` | SQLite with chats, contacts, messages and reactions |
| `media/<chat>/` | Downloaded attachments and thumbnails |
| `wa.log` | Log (level with `WA_LOG=info|debug`) |

History starts with what WhatsApp sends to new devices. `WA_FULL_HISTORY=1` asks for the full history when linking.

## Layout

| File | Role |
|---|---|
| `src/wa.ts` | WhatsApp connection: QR, reconnection, messages, reactions, sending, attachments |
| `src/ipc.ts` | Server and client over a Unix socket, so several processes share one connection |
| `src/db.ts` | SQLite schema and queries (`node:sqlite`) |
| `src/ui.ts` | blessed interface: panels, keyboard, mouse, message rendering |
| `src/format.ts` | WhatsApp markup, dates, colours, line wrapping |
| `src/links.ts`, `src/clearurls/` | Links cleaned of tracking and shortened; ClearURLs' rules (LGPL-3.0, from [ClearURLs/Rules](https://github.com/ClearURLs/Rules) at 11086f4, 2026-03-25) |
| `src/fonts/` | Noto Color Emoji (COLRv1, OFL-1.1, from [googlefonts/noto-emoji](https://github.com/googlefonts/noto-emoji) v2.051), for the big emoji where the system has no colour emoji font |
| `src/image.ts` | Decoding, half-blocks (24-bit or 256 colours), Kitty graphics protocol |
| `src/sixel.ts` | Sixel graphics: encoding with a palette, and drawing the pictures again where blessed wrote over them |
| `src/bigtext.ts` | The open chat's name in big letters, in the rounded lines of the wordmark |
| `src/term.ts` | Probing the terminal's capabilities |
| `src/kittykeys.ts`, `src/paste.ts` | Kitty keyboard protocol and bracketed paste, read before blessed |
| `src/herdr.ts` | Agent state, titles and tabs in Herdr |
| `src/hearts.ts` | Animated emoji rising from a single-emoji message or reaction |
| `src/i18n.ts` | Interface strings in Portuguese and English |
| `src/llm.ts` | Writing suggestions, image descriptions and chats' states from the local `llama-server` |
| `src/mood.ts` | Each chat's state as an emoji, worked out calmly by the server |
| `src/spell.ts` | Local spell checker (Hunspell, pt-PT and English) when there's no model |
| `src/emoji.ts`, `src/italic.ts`, `src/rainbow.ts` | `:name:` table, italics in blessed, colours |
