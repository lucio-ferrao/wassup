/**
 * :name: codes accepted in the compose line, converted on send. Names in European Portuguese (without accents,
 * so they can be typed quickly) and in English, per emoji: [emoji, pt names, en names].
 */
import { lang } from './i18n.js'

const EMOJI: [string, string[], string[]][] = [
  // faces
  ['😊', ['sorriso'], ['smile', 'blush']],
  ['😄', ['riso', 'risota'], ['grin', 'smiley']],
  ['😂', ['gargalhada', 'lol'], ['joy', 'lol']],
  ['🤣', ['chorar_a_rir'], ['rofl']],
  ['😉', ['piscadela'], ['wink']],
  ['😛', ['lingua'], ['tongue', 'stuck_out_tongue']],
  ['😘', ['beijinho'], ['kissing_heart']],
  ['😍', ['apaixonado', 'apaixonada', 'olhos_de_coracao', 'amor', 'paixao', 'coracao'], ['heart_eyes', 'in_love', 'love']],
  ['🥰', ['coracoes', 'carinho', 'apaixonado', 'apaixonada', 'amor', 'paixao', 'coracao'], ['smiling_face_with_hearts', 'three_hearts', 'in_love', 'love']],
  ['🤔', ['pensativo', 'a_pensar'], ['thinking']],
  ['😐', ['neutro', 'sem_expressao'], ['neutral', 'neutral_face']],
  ['😢', ['triste', 'lagrima'], ['cry']],
  ['😭', ['choro', 'a_chorar'], ['sob']],
  ['😠', ['zangado', 'chateado'], ['angry']],
  ['😡', ['furioso'], ['rage']],
  ['😱', ['medo', 'assustado', 'grito'], ['scream']],
  ['😮', ['espantado', 'boquiaberto'], ['open_mouth']],
  ['😎', ['oculos_de_sol', 'fixolas'], ['sunglasses', 'cool']],
  ['🤓', ['cromo', 'totó'], ['nerd']],
  ['😴', ['sono', 'a_dormir'], ['sleeping']],
  ['🤒', ['doente', 'febre'], ['sick', 'face_with_thermometer']],
  ['🥳', ['festa', 'festejar'], ['party', 'partying_face']],
  ['🤗', ['abraco'], ['hug', 'hugs']],
  ['🤫', ['chiu', 'silencio'], ['shush', 'shushing_face']],
  ['🤥', ['mentiroso', 'pinoquio'], ['lying', 'lying_face']],
  ['🤮', ['enjoado', 'vomitar'], ['vomit', 'vomiting']],
  ['😇', ['anjo', 'santinho'], ['angel', 'innocent']],
  ['😈', ['diabo', 'diabinho'], ['devil', 'smiling_imp']],
  ['🤡', ['palhaco'], ['clown']],
  ['💀', ['caveira'], ['skull']],
  ['💩', ['coco', 'trampa'], ['poop', 'hankey']],
  ['👻', ['fantasma'], ['ghost']],
  ['🤖', ['robo'], ['robot']],
  // hands
  ['👍', ['fixe', 'gosto', 'polegar', '+1'], ['thumbsup', '+1', 'like']],
  ['👎', ['nao', 'nao_gosto', '-1'], ['thumbsdown', '-1', 'dislike']],
  ['👌', ['ok', 'perfeito'], ['ok_hand', 'ok']],
  ['👏', ['palmas', 'aplausos'], ['clap']],
  ['🙏', ['rezar', 'obrigado', 'por_favor'], ['pray', 'thanks', 'please']],
  ['💪', ['forca', 'musculo'], ['muscle', 'strong']],
  ['👋', ['ola', 'adeus', 'acenar'], ['wave', 'hello', 'bye']],
  ['✊', ['punho'], ['fist']],
  ['👊', ['murro', 'soco'], ['punch', 'fist_bump']],
  ['✌️', ['paz', 'vitoria'], ['v', 'peace', 'victory']],
  ['🤘', ['metal', 'rock'], ['metal', 'rock']],
  ['🤙', ['liga_me', 'shaka'], ['call_me', 'shaka']],
  ['👉', ['aponta', 'direita_mao'], ['point_right']],
  ['👈', ['esquerda_mao'], ['point_left']],
  ['👀', ['olhos', 'a_ver'], ['eyes']],
  ['🧠', ['cerebro'], ['brain']],
  // hearts and symbols
  ['❤️', ['coracao', 'amor'], ['heart', 'love']],
  ['💋', ['beijo'], ['kiss', 'lips']],
  ['💔', ['coracao_partido', 'desgosto'], ['broken_heart']],
  ['🧡', ['coracao_laranja'], ['orange_heart']],
  ['💛', ['coracao_amarelo'], ['yellow_heart']],
  ['💚', ['coracao_verde'], ['green_heart']],
  ['💙', ['coracao_azul'], ['blue_heart']],
  ['💜', ['coracao_roxo'], ['purple_heart']],
  ['🖤', ['coracao_preto'], ['black_heart']],
  ['✨', ['brilho', 'brilhos'], ['sparkles']],
  ['⭐', ['estrela'], ['star']],
  ['🔥', ['fogo', 'lume'], ['fire']],
  ['💯', ['cem', 'cem_por_cento'], ['100']],
  ['💣', ['bomba'], ['bomb']],
  ['🎉', ['viva', 'confetes', 'parabens'], ['tada', 'party_popper', 'congrats']],
  ['🎁', ['prenda', 'presente'], ['gift']],
  ['🎂', ['bolo', 'bolo_de_anos'], ['cake', 'birthday']],
  ['🎈', ['balao'], ['balloon']],
  ['🏆', ['taca', 'trofeu'], ['trophy']],
  ['🏅', ['medalha'], ['medal']],
  ['✅', ['certo', 'visto', 'feito'], ['check', 'done', 'white_check_mark']],
  ['❌', ['errado', 'cruz'], ['x', 'cross']],
  ['⚠️', ['aviso', 'atencao', 'cuidado'], ['warning']],
  ['🚫', ['proibido'], ['no_entry', 'forbidden']],
  ['❓', ['pergunta', 'interrogacao'], ['question']],
  ['❗', ['exclamacao'], ['exclamation']],
  ['ℹ️', ['info', 'informacao'], ['info', 'information']],
  ['⏰', ['despertador', 'alarme'], ['alarm', 'alarm_clock']],
  ['🕐', ['relogio', 'hora', 'horas'], ['clock']],
  ['📅', ['calendario', 'agenda'], ['calendar']],
  ['⬆️', ['cima', 'seta_cima'], ['up', 'arrow_up']],
  ['⬇️', ['baixo', 'seta_baixo'], ['down', 'arrow_down']],
  ['⬅️', ['esquerda', 'seta_esquerda'], ['left', 'arrow_left']],
  ['➡️', ['direita', 'seta_direita'], ['right', 'arrow_right']],
  ['♻️', ['reciclar', 'reciclagem'], ['recycle']],
  ['♾️', ['infinito'], ['infinity']],
  // weather and nature
  ['☀️', ['sol'], ['sun', 'sunny']],
  ['🌙', ['lua'], ['moon']],
  ['🌧️', ['chuva'], ['rain']],
  ['☁️', ['nuvem', 'nublado'], ['cloud']],
  ['🌈', ['arco_iris'], ['rainbow']],
  ['❄️', ['neve', 'frio'], ['snow', 'snowflake']],
  ['⚡', ['raio', 'trovoada'], ['zap', 'lightning']],
  ['🌊', ['onda', 'mar'], ['wave_water', 'ocean']],
  ['🌍', ['terra', 'mundo'], ['earth', 'world']],
  ['🌳', ['arvore'], ['tree']],
  ['🌸', ['flor'], ['flower', 'blossom']],
  ['🌹', ['rosa'], ['rose']],
  ['🌻', ['girassol'], ['sunflower']],
  ['🌵', ['cacto'], ['cactus']],
  // animals
  ['🐶', ['cao', 'cachorro'], ['dog']],
  ['🐱', ['gato'], ['cat']],
  ['🐭', ['rato'], ['mouse']],
  ['🐰', ['coelho'], ['rabbit']],
  ['🐻', ['urso'], ['bear']],
  ['🐼', ['panda'], ['panda']],
  ['🦁', ['leao'], ['lion']],
  ['🐮', ['vaca'], ['cow']],
  ['🐷', ['porco'], ['pig']],
  ['🐸', ['sapo', 'ra'], ['frog']],
  ['🐵', ['macaco'], ['monkey']],
  ['🐔', ['galinha'], ['chicken']],
  ['🐓', ['galo', 'galo_de_barcelos'], ['rooster']],
  ['🐧', ['pinguim'], ['penguin']],
  ['🐦', ['passaro', 'passarinho'], ['bird']],
  ['🐟', ['peixe', 'bacalhau', 'sardinha'], ['fish']],
  ['🐙', ['polvo'], ['octopus']],
  ['🦋', ['borboleta'], ['butterfly']],
  ['🐝', ['abelha'], ['bee']],
  ['🐍', ['cobra'], ['snake']],
  ['🐴', ['cavalo'], ['horse']],
  ['🦄', ['unicornio'], ['unicorn']],
  ['🐌', ['caracol'], ['snail']],
  // food and drink
  ['🍕', ['pizza'], ['pizza']],
  ['🍔', ['hamburguer'], ['burger', 'hamburger']],
  ['🍟', ['batatas_fritas'], ['fries']],
  ['☕', ['cafe', 'bica'], ['coffee']],
  ['🍵', ['cha'], ['tea']],
  ['🍺', ['cerveja', 'imperial', 'fino'], ['beer']],
  ['🍻', ['saude', 'brinde'], ['cheers', 'beers']],
  ['🍷', ['vinho', 'copo_de_vinho'], ['wine']],
  ['🍾', ['champanhe', 'espumante'], ['champagne']],
  ['🍸', ['cocktail'], ['cocktail']],
  ['💧', ['agua', 'gota'], ['droplet', 'water']],
  ['🍎', ['maca'], ['apple']],
  ['🍌', ['banana'], ['banana']],
  ['🍊', ['laranja', 'tangerina'], ['orange', 'tangerine']],
  ['🍓', ['morango'], ['strawberry']],
  ['🍇', ['uvas'], ['grapes']],
  ['🍞', ['pao'], ['bread']],
  ['🧀', ['queijo'], ['cheese']],
  ['🥚', ['ovo'], ['egg']],
  ['🍦', ['gelado'], ['icecream', 'ice_cream']],
  ['🍫', ['chocolate'], ['chocolate']],
  // transport and places
  ['🚗', ['carro'], ['car']],
  ['🚌', ['autocarro'], ['bus']],
  ['🚆', ['comboio'], ['train']],
  ['🚋', ['eletrico'], ['tram']],
  ['✈️', ['aviao'], ['airplane', 'plane']],
  ['⛵', ['barco', 'veleiro'], ['boat', 'sailboat']],
  ['🚲', ['bicicleta', 'bike'], ['bike', 'bicycle']],
  ['🚀', ['foguetao'], ['rocket']],
  ['🏠', ['casa'], ['house', 'home']],
  ['🏢', ['escritorio', 'predio'], ['office']],
  ['🏖️', ['praia'], ['beach']],
  // objects and work
  ['📞', ['telefone'], ['phone', 'telephone']],
  ['📱', ['telemovel'], ['iphone', 'mobile']],
  ['💻', ['computador', 'portatil'], ['computer', 'laptop']],
  ['📧', ['email', 'mail'], ['email', 'e-mail']],
  ['✉️', ['envelope', 'carta'], ['envelope']],
  ['📖', ['livro'], ['book']],
  ['✏️', ['lapis'], ['pencil']],
  ['📎', ['clipe', 'anexo'], ['paperclip', 'attachment']],
  ['📁', ['pasta'], ['folder']],
  ['🔍', ['lupa', 'procurar'], ['mag', 'search']],
  ['🔑', ['chave'], ['key']],
  ['🔒', ['cadeado', 'trancado'], ['lock']],
  ['💰', ['dinheiro', 'saco_de_dinheiro'], ['moneybag', 'money']],
  ['💶', ['euro', 'euros'], ['euro']],
  ['💳', ['cartao', 'multibanco'], ['credit_card', 'card']],
  ['📈', ['grafico', 'a_subir'], ['chart', 'chart_up']],
  ['💡', ['ideia', 'lampada'], ['bulb', 'idea']],
  ['🔧', ['chave_inglesa', 'ferramenta'], ['wrench', 'tool']],
  ['🔨', ['martelo'], ['hammer']],
  ['⚙️', ['engrenagem', 'definicoes'], ['gear', 'settings']],
  ['🧱', ['tijolo'], ['brick']],
  // sports and leisure
  ['⚽', ['bola', 'futebol'], ['soccer', 'football']],
  ['🏀', ['basquetebol'], ['basketball']],
  ['🎾', ['tenis'], ['tennis']],
  ['⛳', ['golfe'], ['golf']],
  ['🎵', ['musica', 'nota'], ['music', 'note']],
  ['🎸', ['guitarra'], ['guitar']],
  ['🎤', ['microfone', 'karaoke'], ['mic', 'microphone']],
  ['🎬', ['filme', 'cinema'], ['movie', 'clapper']],
  ['🎮', ['jogo', 'consola'], ['game', 'video_game']],
  ['🎲', ['dado'], ['dice', 'game_die']],
  // people
  ['👶', ['bebe'], ['baby']],
  ['👨‍👩‍👧', ['familia'], ['family']],
  ['💑', ['casal', 'namorados'], ['couple']],
  // flags
  ['🇵🇹', ['portugal', 'pt'], ['portugal', 'pt']],
  ['🇧🇷', ['brasil', 'br'], ['brazil', 'br']],
  ['🇪🇸', ['espanha', 'es'], ['spain', 'es']],
  ['🇫🇷', ['franca', 'fr'], ['france', 'fr']],
  ['🇬🇧', ['reino_unido', 'inglaterra', 'uk'], ['uk', 'gb']],
  ['🇺🇸', ['eua', 'estados_unidos', 'us'], ['usa', 'us']],
  ['🇩🇪', ['alemanha', 'de'], ['germany', 'de']],
]

/**
 * Each :name: to its emoji. A name several share (":amor:", for the heart and the faces in love, so the list offers
 * them all) goes to the one that lists it soonest, on a tie the first in the table: ":amor:" stays the heart.
 */
const TABLE: Record<string, string> = {}
const SOONEST: Record<string, number> = {}
for (const [emoji, pt, en] of EMOJI) {
  [...pt, ...en].forEach((name, i) => {
    if (i < (SOONEST[name] ?? Infinity)) { TABLE[name] = emoji; SOONEST[name] = i }
  })
}

const CODE_RE = /(^|[^\w:]):([a-z0-9_+-]+):(?=[^\w:]|$)/gi

/** Classic smileys, only when isolated by spaces (or start/end), so as not to touch "http://" and the like. */
const EMOTICONS: [string[], string][] = [
  [[':)', ':-)', '=)'], '🙂'],
  [[':D', ':-D', '=D'], '😁'],
  [[':(', ':-(', '=('], '🙁'],
  [[";)", ';-)'], '😉'],
  [[':P', ':-P', ':p', ':-p'], '😛'],
  [[';P', ';-P', ';p'], '😜'],
  [[':*', ':-*'], '😘'],
  [[':O', ':-O', ':o', ':-o'], '😮'],
  [[":'(", ":'-("], '😢'],
  [[':/', ':-/', ':\\', ':-\\'], '😕'],
  [[':|', ':-|'], '😐'],
  [[':$', ':-$'], '😳'],
  [[':X', ':-X', ':x', ':-x'], '🤐'],
  [['>:(', '>:-('], '😠'],
  [['B)', 'B-)', '8)', '8-)'], '😎'],
  [['xD', 'XD', 'xd'], '😆'],
  [['<3'], '❤️'],
  [['</3'], '💔'],
  [[':3'], '😊'],
  [['^^', '^_^'], '😊'],
  [['-_-'], '😑'],
  [['o/'], '👋'],
  [['\\o/'], '🙌'],
]
const EMOTICON_MAP = new Map<string, string>()
for (const [faces, emoji] of EMOTICONS) for (const f of faces) EMOTICON_MAP.set(f, emoji)
const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
/** Longest first, so ">:(" is found before ":(". */
const FACES = [...EMOTICON_MAP.keys()].sort((a, b) => b.length - a.length)
const anyOf = (faces: string[]) => faces.map(escapeRe).join('|')
/** What may follow a smiley: a space, the end, or punctuation. */
const FACE_END = '(?=\\s|$|[.,!?])'
const EMOTICON_RE = new RegExp(`(^|\\s)(${anyOf(FACES)})${FACE_END}`, 'g')
/**
 * The smileys offered while typing: not those that start with a letter or a digit ("xD", "B)", "8)", "o/"), which
 * are also ordinary text being typed ("xdg-open", "(plan B)", "caro o/a").
 */
const TYPED_FACE_RE = new RegExp(`(?:^|\\s)(${anyOf(FACES.filter(f => !/^[\p{L}\p{N}]/u.test(f)))})$`)
const AFTER_FACE_RE = new RegExp(`^${FACE_END}`)

/**
 * The classic smiley `before` ends with, on its own after a space (or at the start) and with a space, the end or
 * punctuation in `after`, and the emoji it stands for.
 */
export function emoticonAt(before: string, after: string): { face: string; emoji: string } | null {
  const face = TYPED_FACE_RE.exec(before)?.[1]
  return face && AFTER_FACE_RE.test(after) ? { face, emoji: EMOTICON_MAP.get(face)! } : null
}

/** Replaces classic smileys (":)", ":-P", ":*") with the corresponding emoji; everything else stays as is. */
export function emoticonify(text: string): string {
  return text.replace(EMOTICON_RE, (_m, pre: string, face: string) => `${pre}${EMOTICON_MAP.get(face)}`)
}

/** Replaces :name: codes with the corresponding emoji; everything else, smileys included, stays as is. */
export function emojify(text: string): string {
  return text.replace(CODE_RE, (m, pre: string, code: string) => {
    const e = TABLE[code.toLowerCase()]
    return e ? `${pre}${e}` : m
  })
}

export const emojiCodes = Object.keys(TABLE)

/**
 * The list for what follows a `:` being typed (`prefix`), each emoji once with the code to display: first the classic
 * smileys that start that way (":D" for "D"; on its own, the basic ones: ":)", ":D", ":("...), in the table's order;
 * then the emojis whose name, in Portuguese or in English, starts with `prefix`, with the first matching name, one in
 * the user's language when both match. Shorter names first, so an exact match ("fixe") comes before a longer one;
 * on a tie, the emoji the name stands for when typed whole (":amor:" the heart) first, then the table order.
 */
export function completeEmoji(prefix: string): { emoji: string; code: string }[] {
  const out: { emoji: string; code: string }[] = []
  for (const [faces, emoji] of EMOTICONS) {
    const face = faces.find(f => f.startsWith(`:${prefix}`))
    if (face && !out.some(o => o.emoji === emoji)) out.push({ emoji, code: face })
  }
  if (!/^[a-z0-9_+-]+$/i.test(prefix)) return out
  const p = prefix.toLowerCase()
  const named: { emoji: string; name: string }[] = []
  for (const [emoji, pt, en] of EMOJI) {
    const [own, other] = lang === 'pt' ? [pt, en] : [en, pt]
    const name = own.find(n => n.startsWith(p)) ?? other.find(n => n.startsWith(p))
    if (name && !out.some(o => o.emoji === emoji) && !named.some(o => o.emoji === emoji)) named.push({ emoji, name })
  }
  named.sort((a, b) => a.name.length - b.name.length || Number(TABLE[b.name] === b.emoji) - Number(TABLE[a.name] === a.emoji))
  return [...out, ...named.map(o => ({ emoji: o.emoji, code: `:${o.name}:` }))]
}

