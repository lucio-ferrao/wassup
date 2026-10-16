//! Emoji by name and by smiley, as wassup has them (its emoji.ts), and its test for a message of emoji alone (from
//! hearts.ts). In the compose line, `:name:` codes, with names in European Portuguese (without accents, so they can
//! be typed quickly) and in English, become their emoji as soon as the closing ":" is typed and again on sending;
//! classic smileys (":)", "<3") become theirs where they stand on their own; and while either is being typed, a list
//! offers the emoji that fit, the user's language first.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

/// The user's language, whose emoji names the suggestions give when both languages have one that fits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lang {
    #[default]
    Pt,
    En,
}

/// An entry in the list offered while typing: the emoji, and what stands for it typed, a smiley (":D") or a
/// code (":fixe:").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub emoji: &'static str,
    pub code: String,
}

/// Each emoji with its names in Portuguese and in English. A name may be shared ("amor", for the heart and the faces
/// in love) so that the list offers them all.
const EMOJI: &[(&str, &[&str], &[&str])] = &[
    // faces
    ("😊", &["sorriso"], &["smile", "blush"]),
    ("😄", &["riso", "risota"], &["grin", "smiley"]),
    ("😂", &["gargalhada", "lol"], &["joy", "lol"]),
    ("🤣", &["chorar_a_rir"], &["rofl"]),
    ("😉", &["piscadela"], &["wink"]),
    ("😛", &["lingua"], &["tongue", "stuck_out_tongue"]),
    ("😘", &["beijinho"], &["kissing_heart"]),
    (
        "😍",
        &["apaixonado", "apaixonada", "olhos_de_coracao", "amor", "paixao", "coracao"],
        &["heart_eyes", "in_love", "love"],
    ),
    (
        "🥰",
        &["coracoes", "carinho", "apaixonado", "apaixonada", "amor", "paixao", "coracao"],
        &["smiling_face_with_hearts", "three_hearts", "in_love", "love"],
    ),
    ("🤔", &["pensativo", "a_pensar"], &["thinking"]),
    ("😐", &["neutro", "sem_expressao"], &["neutral", "neutral_face"]),
    ("😢", &["triste", "lagrima"], &["cry"]),
    ("😭", &["choro", "a_chorar"], &["sob"]),
    ("😠", &["zangado", "chateado"], &["angry"]),
    ("😡", &["furioso"], &["rage"]),
    ("😱", &["medo", "assustado", "grito"], &["scream"]),
    ("😮", &["espantado", "boquiaberto"], &["open_mouth"]),
    ("😎", &["oculos_de_sol", "fixolas"], &["sunglasses", "cool"]),
    ("🤓", &["cromo", "totó"], &["nerd"]),
    ("😴", &["sono", "a_dormir"], &["sleeping"]),
    ("🤒", &["doente", "febre"], &["sick", "face_with_thermometer"]),
    ("🥳", &["festa", "festejar"], &["party", "partying_face"]),
    ("🤗", &["abraco"], &["hug", "hugs"]),
    ("🤫", &["chiu", "silencio"], &["shush", "shushing_face"]),
    ("🤥", &["mentiroso", "pinoquio"], &["lying", "lying_face"]),
    ("🤮", &["enjoado", "vomitar"], &["vomit", "vomiting"]),
    ("😇", &["anjo", "santinho"], &["angel", "innocent"]),
    ("😈", &["diabo", "diabinho"], &["devil", "smiling_imp"]),
    ("🤡", &["palhaco"], &["clown"]),
    ("💀", &["caveira"], &["skull"]),
    ("💩", &["coco", "trampa"], &["poop", "hankey"]),
    ("👻", &["fantasma"], &["ghost"]),
    ("🤖", &["robo"], &["robot"]),
    // hands
    ("👍", &["fixe", "gosto", "polegar", "+1"], &["thumbsup", "+1", "like"]),
    ("👎", &["nao", "nao_gosto", "-1"], &["thumbsdown", "-1", "dislike"]),
    ("👌", &["ok", "perfeito"], &["ok_hand", "ok"]),
    ("👏", &["palmas", "aplausos"], &["clap"]),
    ("🙏", &["rezar", "obrigado", "por_favor"], &["pray", "thanks", "please"]),
    ("💪", &["forca", "musculo"], &["muscle", "strong"]),
    ("👋", &["ola", "adeus", "acenar"], &["wave", "hello", "bye"]),
    ("✊", &["punho"], &["fist"]),
    ("👊", &["murro", "soco"], &["punch", "fist_bump"]),
    ("✌️", &["paz", "vitoria"], &["v", "peace", "victory"]),
    ("🤘", &["metal", "rock"], &["metal", "rock"]),
    ("🤙", &["liga_me", "shaka"], &["call_me", "shaka"]),
    ("👉", &["aponta", "direita_mao"], &["point_right"]),
    ("👈", &["esquerda_mao"], &["point_left"]),
    ("👀", &["olhos", "a_ver"], &["eyes"]),
    ("🧠", &["cerebro"], &["brain"]),
    // hearts and symbols
    ("❤️", &["coracao", "amor"], &["heart", "love"]),
    ("💋", &["beijo"], &["kiss", "lips"]),
    ("💔", &["coracao_partido", "desgosto"], &["broken_heart"]),
    ("🧡", &["coracao_laranja"], &["orange_heart"]),
    ("💛", &["coracao_amarelo"], &["yellow_heart"]),
    ("💚", &["coracao_verde"], &["green_heart"]),
    ("💙", &["coracao_azul"], &["blue_heart"]),
    ("💜", &["coracao_roxo"], &["purple_heart"]),
    ("🖤", &["coracao_preto"], &["black_heart"]),
    ("✨", &["brilho", "brilhos"], &["sparkles"]),
    ("⭐", &["estrela"], &["star"]),
    ("🔥", &["fogo", "lume"], &["fire"]),
    ("💯", &["cem", "cem_por_cento"], &["100"]),
    ("💣", &["bomba"], &["bomb"]),
    ("🎉", &["viva", "confetes", "parabens"], &["tada", "party_popper", "congrats"]),
    ("🎁", &["prenda", "presente"], &["gift"]),
    ("🎂", &["bolo", "bolo_de_anos"], &["cake", "birthday"]),
    ("🎈", &["balao"], &["balloon"]),
    ("🏆", &["taca", "trofeu"], &["trophy"]),
    ("🏅", &["medalha"], &["medal"]),
    ("✅", &["certo", "visto", "feito"], &["check", "done", "white_check_mark"]),
    ("❌", &["errado", "cruz"], &["x", "cross"]),
    ("⚠️", &["aviso", "atencao", "cuidado"], &["warning"]),
    ("🚫", &["proibido"], &["no_entry", "forbidden"]),
    ("❓", &["pergunta", "interrogacao"], &["question"]),
    ("❗", &["exclamacao"], &["exclamation"]),
    ("ℹ️", &["info", "informacao"], &["info", "information"]),
    ("⏰", &["despertador", "alarme"], &["alarm", "alarm_clock"]),
    ("🕐", &["relogio", "hora", "horas"], &["clock"]),
    ("📅", &["calendario", "agenda"], &["calendar"]),
    ("⬆️", &["cima", "seta_cima"], &["up", "arrow_up"]),
    ("⬇️", &["baixo", "seta_baixo"], &["down", "arrow_down"]),
    ("⬅️", &["esquerda", "seta_esquerda"], &["left", "arrow_left"]),
    ("➡️", &["direita", "seta_direita"], &["right", "arrow_right"]),
    ("♻️", &["reciclar", "reciclagem"], &["recycle"]),
    ("♾️", &["infinito"], &["infinity"]),
    // weather and nature
    ("☀️", &["sol"], &["sun", "sunny"]),
    ("🌙", &["lua"], &["moon"]),
    ("🌧️", &["chuva"], &["rain"]),
    ("☁️", &["nuvem", "nublado"], &["cloud"]),
    ("🌈", &["arco_iris"], &["rainbow"]),
    ("❄️", &["neve", "frio"], &["snow", "snowflake"]),
    ("⚡", &["raio", "trovoada"], &["zap", "lightning"]),
    ("🌊", &["onda", "mar"], &["wave_water", "ocean"]),
    ("🌍", &["terra", "mundo"], &["earth", "world"]),
    ("🌳", &["arvore"], &["tree"]),
    ("🌸", &["flor"], &["flower", "blossom"]),
    ("🌹", &["rosa"], &["rose"]),
    ("🌻", &["girassol"], &["sunflower"]),
    ("🌵", &["cacto"], &["cactus"]),
    // animals
    ("🐶", &["cao", "cachorro"], &["dog"]),
    ("🐱", &["gato"], &["cat"]),
    ("🐭", &["rato"], &["mouse"]),
    ("🐰", &["coelho"], &["rabbit"]),
    ("🐻", &["urso"], &["bear"]),
    ("🐼", &["panda"], &["panda"]),
    ("🦁", &["leao"], &["lion"]),
    ("🐮", &["vaca"], &["cow"]),
    ("🐷", &["porco"], &["pig"]),
    ("🐸", &["sapo", "ra"], &["frog"]),
    ("🐵", &["macaco"], &["monkey"]),
    ("🐔", &["galinha"], &["chicken"]),
    ("🐓", &["galo", "galo_de_barcelos"], &["rooster"]),
    ("🐧", &["pinguim"], &["penguin"]),
    ("🐦", &["passaro", "passarinho"], &["bird"]),
    ("🐟", &["peixe", "bacalhau", "sardinha"], &["fish"]),
    ("🐙", &["polvo"], &["octopus"]),
    ("🦋", &["borboleta"], &["butterfly"]),
    ("🐝", &["abelha"], &["bee"]),
    ("🐍", &["cobra"], &["snake"]),
    ("🐴", &["cavalo"], &["horse"]),
    ("🦄", &["unicornio"], &["unicorn"]),
    ("🐌", &["caracol"], &["snail"]),
    // food and drink
    ("🍕", &["pizza"], &["pizza"]),
    ("🍔", &["hamburguer"], &["burger", "hamburger"]),
    ("🍟", &["batatas_fritas"], &["fries"]),
    ("☕", &["cafe", "bica"], &["coffee"]),
    ("🍵", &["cha"], &["tea"]),
    ("🍺", &["cerveja", "imperial", "fino"], &["beer"]),
    ("🍻", &["saude", "brinde"], &["cheers", "beers"]),
    ("🍷", &["vinho", "copo_de_vinho"], &["wine"]),
    ("🍾", &["champanhe", "espumante"], &["champagne"]),
    ("🍸", &["cocktail"], &["cocktail"]),
    ("💧", &["agua", "gota"], &["droplet", "water"]),
    ("🍎", &["maca"], &["apple"]),
    ("🍌", &["banana"], &["banana"]),
    ("🍊", &["laranja", "tangerina"], &["orange", "tangerine"]),
    ("🍓", &["morango"], &["strawberry"]),
    ("🍇", &["uvas"], &["grapes"]),
    ("🍞", &["pao"], &["bread"]),
    ("🧀", &["queijo"], &["cheese"]),
    ("🥚", &["ovo"], &["egg"]),
    ("🍦", &["gelado"], &["icecream", "ice_cream"]),
    ("🍫", &["chocolate"], &["chocolate"]),
    // transport and places
    ("🚗", &["carro"], &["car"]),
    ("🚌", &["autocarro"], &["bus"]),
    ("🚆", &["comboio"], &["train"]),
    ("🚋", &["eletrico"], &["tram"]),
    ("✈️", &["aviao"], &["airplane", "plane"]),
    ("⛵", &["barco", "veleiro"], &["boat", "sailboat"]),
    ("🚲", &["bicicleta", "bike"], &["bike", "bicycle"]),
    ("🚀", &["foguetao"], &["rocket"]),
    ("🏠", &["casa"], &["house", "home"]),
    ("🏢", &["escritorio", "predio"], &["office"]),
    ("🏖️", &["praia"], &["beach"]),
    // objects and work
    ("📞", &["telefone"], &["phone", "telephone"]),
    ("📱", &["telemovel"], &["iphone", "mobile"]),
    ("💻", &["computador", "portatil"], &["computer", "laptop"]),
    ("📧", &["email", "mail"], &["email", "e-mail"]),
    ("✉️", &["envelope", "carta"], &["envelope"]),
    ("📖", &["livro"], &["book"]),
    ("✏️", &["lapis"], &["pencil"]),
    ("📎", &["clipe", "anexo"], &["paperclip", "attachment"]),
    ("📁", &["pasta"], &["folder"]),
    ("🔍", &["lupa", "procurar"], &["mag", "search"]),
    ("🔑", &["chave"], &["key"]),
    ("🔒", &["cadeado", "trancado"], &["lock"]),
    ("💰", &["dinheiro", "saco_de_dinheiro"], &["moneybag", "money"]),
    ("💶", &["euro", "euros"], &["euro"]),
    ("💳", &["cartao", "multibanco"], &["credit_card", "card"]),
    ("📈", &["grafico", "a_subir"], &["chart", "chart_up"]),
    ("💡", &["ideia", "lampada"], &["bulb", "idea"]),
    ("🔧", &["chave_inglesa", "ferramenta"], &["wrench", "tool"]),
    ("🔨", &["martelo"], &["hammer"]),
    ("⚙️", &["engrenagem", "definicoes"], &["gear", "settings"]),
    ("🧱", &["tijolo"], &["brick"]),
    // sports and leisure
    ("⚽", &["bola", "futebol"], &["soccer", "football"]),
    ("🏀", &["basquetebol"], &["basketball"]),
    ("🎾", &["tenis"], &["tennis"]),
    ("⛳", &["golfe"], &["golf"]),
    ("🎵", &["musica", "nota"], &["music", "note"]),
    ("🎸", &["guitarra"], &["guitar"]),
    ("🎤", &["microfone", "karaoke"], &["mic", "microphone"]),
    ("🎬", &["filme", "cinema"], &["movie", "clapper"]),
    ("🎮", &["jogo", "consola"], &["game", "video_game"]),
    ("🎲", &["dado"], &["dice", "game_die"]),
    // people
    ("👶", &["bebe"], &["baby"]),
    ("👨‍👩‍👧", &["familia"], &["family"]),
    ("💑", &["casal", "namorados"], &["couple"]),
    // flags
    ("🇵🇹", &["portugal", "pt"], &["portugal", "pt"]),
    ("🇧🇷", &["brasil", "br"], &["brazil", "br"]),
    ("🇪🇸", &["espanha", "es"], &["spain", "es"]),
    ("🇫🇷", &["franca", "fr"], &["france", "fr"]),
    ("🇬🇧", &["reino_unido", "inglaterra", "uk"], &["uk", "gb"]),
    ("🇺🇸", &["eua", "estados_unidos", "us"], &["usa", "us"]),
    ("🇩🇪", &["alemanha", "de"], &["germany", "de"]),
];

/// Classic smileys, each group with the emoji it stands for.
const EMOTICONS: &[(&[&str], &str)] = &[
    (&[":)", ":-)", "=)"], "🙂"),
    (&[":D", ":-D", "=D"], "😁"),
    (&[":(", ":-(", "=("], "🙁"),
    (&[";)", ";-)"], "😉"),
    (&[":P", ":-P", ":p", ":-p"], "😛"),
    (&[";P", ";-P", ";p"], "😜"),
    (&[":*", ":-*"], "😘"),
    (&[":O", ":-O", ":o", ":-o"], "😮"),
    (&[":'(", ":'-("], "😢"),
    (&[":/", ":-/", ":\\", ":-\\"], "😕"),
    (&[":|", ":-|"], "😐"),
    (&[":$", ":-$"], "😳"),
    (&[":X", ":-X", ":x", ":-x"], "🤐"),
    (&[">:(", ">:-("], "😠"),
    (&["B)", "B-)", "8)", "8-)"], "😎"),
    (&["xD", "XD", "xd"], "😆"),
    (&["<3"], "❤️"),
    (&["</3"], "💔"),
    (&[":3"], "😊"),
    (&["^^", "^_^"], "😊"),
    (&["-_-"], "😑"),
    (&["o/"], "👋"),
    (&["\\o/"], "🙌"),
];

/// One emoji: a pictograph or a pair of regional indicators (a flag), with its presentation selector, skin tone, and
/// the pictographs joined to it (❤️‍🔥, 👨‍👩‍👧).
const ONE_EMOJI: &str = concat!(
    r"(?:\p{Extended_Pictographic}|\p{Regional_Indicator}{2})",
    r"(?:\x{FE0F}|\p{Emoji_Modifier}|\x{200D}\p{Extended_Pictographic}\x{FE0F}?)*",
);

/// The codes: each name's emoji, and the names in the order the table first gives them.
struct Codes {
    emoji: HashMap<&'static str, &'static str>,
    names: Vec<&'static str>,
}

/// Each name goes to the emoji that lists it soonest among its names (Portuguese, then English), on a tie to the
/// first in the table: ":amor:" stays the heart, which has it second, while the faces in love have it further on.
fn codes() -> &'static Codes {
    static CODES: OnceLock<Codes> = OnceLock::new();
    CODES.get_or_init(|| {
        let mut codes = Codes { emoji: HashMap::new(), names: Vec::new() };
        let mut soonest: HashMap<&str, usize> = HashMap::new();
        for &(emoji, pt, en) in EMOJI {
            for (i, &name) in pt.iter().chain(en).enumerate() {
                match soonest.get(name) {
                    Some(&s) if s <= i => continue,
                    Some(_) => {}
                    None => codes.names.push(name),
                }
                codes.emoji.insert(name, emoji);
                soonest.insert(name, i);
            }
        }
        codes
    })
}

/// Every smiley with its emoji, the longest first, so that ">:(" is found before ":(".
fn faces() -> &'static [(&'static str, &'static str)] {
    static FACES: OnceLock<Vec<(&str, &str)>> = OnceLock::new();
    FACES.get_or_init(|| {
        let mut faces: Vec<_> =
            EMOTICONS.iter().flat_map(|&(faces, emoji)| faces.iter().map(move |&f| (f, emoji))).collect();
        faces.sort_by_key(|(face, _)| std::cmp::Reverse(face.len()));
        faces
    })
}

/// Whether what comes after a smiley lets it stand on its own: a space, punctuation, or nothing.
fn face_ends(after: &str) -> bool {
    after.chars().next().is_none_or(|c| c.is_whitespace() || ".,!?".contains(c))
}

/// Whether `before` leaves what follows on its own: it's empty or ends in a space.
fn face_starts(before: &str) -> bool {
    before.chars().next_back().is_none_or(char::is_whitespace)
}

/// The smiley `before` ends with, when it stands on its own there (at the start or after a space) and `after` goes on
/// with a space, punctuation or nothing, as the suggestion for it while it's being typed. Those that start with a
/// letter or a digit ("xD", "B)", "8)", "o/") are left out: they are also ordinary text being typed ("xdg-open",
/// "(plan B)", "caro o/a").
pub fn emoticon_at(before: &str, after: &str) -> Option<Suggestion> {
    if !face_ends(after) {
        return None;
    }
    faces()
        .iter()
        .filter(|(face, _)| !face.starts_with(char::is_alphanumeric))
        .find(|(face, _)| before.strip_suffix(face).is_some_and(face_starts))
        .map(|&(face, emoji)| Suggestion { emoji, code: face.to_string() })
}

/// The text with its classic smileys (":)", ":-P", "<3") swapped for their emoji, only those on their own (at the
/// start or after a space, and followed by a space, punctuation or the end), so as not to touch "http://" and the
/// like; everything else stays as it is.
pub fn emoticonify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut free = true;
    while let Some(c) = rest.chars().next() {
        let face = faces().iter().find(|(face, _)| rest.strip_prefix(face).is_some_and(face_ends));
        if let Some(&(face, emoji)) = face.filter(|_| free) {
            out.push_str(emoji);
            rest = &rest[face.len()..];
            free = false;
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
            free = c.is_whitespace();
        }
    }
    out
}

/// The text with its `:name:` codes swapped for their emoji, the name in either language and in any case. A code
/// counts only apart from words and other codes ("a:fixe:", ":fixe:b" and ":fixe::ok:" stay); one of no emoji, the
/// smileys and everything else stay as they are. Run on the text up to the cursor when ":" is typed, it swaps the
/// code just closed.
pub fn emojify(text: &str) -> String {
    let b = text.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut i) = (0, 0);
    while i < b.len() {
        let opens = b[i] == b':' && (i == 0 || !(word(b[i - 1]) || b[i - 1] == b':'));
        let len = if opens { b[i + 1..].iter().take_while(|&&c| word(c) || c == b'+' || c == b'-').count() } else { 0 };
        let close = i + 1 + len;
        if len == 0 || b.get(close) != Some(&b':') || b.get(close + 1).is_some_and(|&c| word(c) || c == b':') {
            i += 1;
            continue;
        }
        if let Some(emoji) = codes().emoji.get(text[i + 1..close].to_ascii_lowercase().as_str()) {
            out.push_str(&text[copied..i]);
            out.push_str(emoji);
            copied = close + 1;
        }
        i = close + 1;
    }
    out.push_str(&text[copied..]);
    out
}

/// Every name a code can have, in either language, each once.
#[cfg(test)]
pub fn emoji_codes() -> &'static [&'static str] {
    &codes().names
}

/// The list for what follows a ":" being typed (`prefix`), each emoji once with the code to show for it. First the
/// smileys that start that way (":D" for "D"; for nothing yet, the basic ones, ":)", ":D", ":("...), in the order of
/// their table. Then the emoji with a name in either language that starts with `prefix`, in any case, each with the
/// first such name, one in `lang` when both languages have one: shorter names first, so an exact match ("fixe")
/// comes before a longer one, and on a tie the emoji the name stands for when typed whole (":amor:", the heart)
/// first, then the table's order.
pub fn complete_emoji(prefix: &str, lang: Lang) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Vec::new();
    for &(faces, emoji) in EMOTICONS {
        let face = faces.iter().find(|f| f.strip_prefix(':').is_some_and(|f| f.starts_with(prefix)));
        if let Some(face) = face.filter(|_| !out.iter().any(|o| o.emoji == emoji)) {
            out.push(Suggestion { emoji, code: face.to_string() });
        }
    }
    if prefix.is_empty() || !prefix.bytes().all(|c| c.is_ascii_alphanumeric() || b"_+-".contains(&c)) {
        return out;
    }
    let p = prefix.to_ascii_lowercase();
    let mut named: Vec<(&str, &str)> = Vec::new();
    for &(emoji, pt, en) in EMOJI {
        let (own, other) = if lang == Lang::Pt { (pt, en) } else { (en, pt) };
        let starts = |names: &[&'static str]| names.iter().copied().find(|n| n.starts_with(&p));
        if let Some(name) = starts(own).or_else(|| starts(other))
            && !out.iter().any(|o| o.emoji == emoji)
            && !named.iter().any(|&(e, _)| e == emoji)
        {
            named.push((emoji, name));
        }
    }
    let owns = |&(emoji, name): &(&str, &str)| codes().emoji.get(name) == Some(&emoji);
    named.sort_by(|a, b| a.1.chars().count().cmp(&b.1.chars().count()).then(owns(b).cmp(&owns(a))));
    out.extend(named.into_iter().map(|(emoji, name)| Suggestion { emoji, code: format!(":{name}:") }));
    out
}

/// A single emoji with nothing else around it but spaces, or a smiley that stands for one ("<3", ":*"): the emoji,
/// to answer with a heart rising from the message or the reaction.
pub fn reaction(text: &str) -> Option<String> {
    static ONE: OnceLock<Regex> = OnceLock::new();
    let one = ONE.get_or_init(|| Regex::new(&format!("^{ONE_EMOJI}$")).expect("emoji pattern"));
    let emoji = emoticonify(text.trim());
    one.is_match(&emoji).then_some(emoji)
}

/// Whether a text is emoji alone, any number of them, with nothing but spaces between and around them: a skin
/// tone, a flag, people joined into one (👨‍👩‍👧) and symbols shown as emoji (▶️, ℹ️) count; smileys don't, as they
/// are sent as typed.
pub fn emoji_only(text: &str) -> bool {
    static ONLY: OnceLock<Regex> = OnceLock::new();
    ONLY.get_or_init(|| Regex::new(&format!(r"^\s*(?:{ONE_EMOJI}\s*)+$")).expect("emoji pattern")).is_match(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(list: &[Suggestion]) -> Vec<String> {
        list.iter().map(|o| format!("{}{}", o.emoji, o.code)).collect()
    }

    /// Codes in either language and in any case become their emoji; a shared name goes to the emoji listing it soonest.
    #[test]
    fn codes_become_emoji() {
        assert_eq!(emojify(":fixe: e :thumbsup:"), "👍 e 👍");
        assert_eq!(emojify("Bom dia :SOL:!"), "Bom dia ☀️!");
        assert_eq!(emojify(":+1: :-1: :100:"), "👍 👎 💯");
        assert_eq!(emojify("(:ok:)"), "(👌)");
        assert_eq!(emojify(":amor: :apaixonado:"), "❤️ 😍");
    }

    /// Codes inside words or stuck to other codes, names of no emoji and codes not closed stay as typed.
    #[test]
    fn codes_only_apart() {
        for text in ["a:fixe:", ":fixe:b", ":fixe::ok:", ":nada_disto:", ":fixe", "http://exemplo.pt", "12:30:45"] {
            assert_eq!(emojify(text), text);
        }
        // A code of no emoji is passed over whole: its closing ":" doesn't open another.
        assert_eq!(emojify(":nada-:-1: e :-1:"), ":nada-:-1: e 👎");
    }

    /// Typing the closing ":" swaps the code just closed, in the text up to the cursor.
    #[test]
    fn closing_colon_swaps_the_code() {
        let before = "Vamos às 8? :fixe";
        assert_eq!(emojify(before), before);
        assert_eq!(emojify(&format!("{before}:")), "Vamos às 8? 👍");
    }

    /// Each name once, and each one that can be typed as a code turns into an emoji.
    #[test]
    fn every_code_has_its_emoji() {
        let names = emoji_codes();
        assert!(names.contains(&"fixe") && names.contains(&"thumbsup"));
        let mut seen = std::collections::HashSet::new();
        assert!(names.iter().all(|n| seen.insert(n)));
        for name in names.iter().filter(|n| n.is_ascii()) {
            assert_ne!(emojify(&format!(":{name}:")), format!(":{name}:"));
        }
    }

    /// Smileys on their own become emoji, the longest that fits; inside words or links they stay.
    #[test]
    fn smileys_on_their_own() {
        assert_eq!(emoticonify(":) :D <3"), "🙂 😁 ❤️");
        assert_eq!(emoticonify("Até amanhã :-P."), "Até amanhã 😛.");
        assert_eq!(emoticonify(">:( xD \\o/"), "😠 😆 🙌");
        for text in ["http://exemplo.pt", "ola:)", ":))", "a</3b", ":-"] {
            assert_eq!(emoticonify(text), text);
        }
    }

    /// The smiley being typed is offered when it stands on its own, but not one that starts with a letter or digit.
    #[test]
    fn smiley_being_typed() {
        let at = |before, after| emoticon_at(before, after).map(|o| format!("{}{}", o.emoji, o.code));
        assert_eq!(at("Até já :)", ""), Some("🙂:)".into()));
        assert_eq!(at(":*", " e mais"), Some("😘:*".into()));
        assert_eq!(at("ok >:(", "!"), Some("😠>:(".into()));
        assert_eq!(at("ok :)", "x"), None);
        assert_eq!(at("ok:)", ""), None);
        assert_eq!(at("plano B)", ""), None);
        assert_eq!(at("xD", ""), None);
    }

    /// Smileys come first, the basic ones alone for a bare ":"; then names, the shorter first.
    #[test]
    fn suggestions_smileys_then_shorter_names() {
        let bare = shown(&complete_emoji("", Lang::Pt));
        assert_eq!(bare.len(), 12);
        assert_eq!(bare[..3], ["🙂:)", "😁:D", "🙁:("]);
        assert_eq!(shown(&complete_emoji("D", Lang::Pt))[..5], ["😁:D", "🇩🇪:de:", "🐶:dog:", "✅:done:", "⬇️:down:"]);
        assert_eq!(shown(&complete_emoji("fix", Lang::Pt)), ["👍:fixe:", "😎:fixolas:"]);
        assert!(complete_emoji("a b", Lang::Pt).is_empty());
    }

    /// On a tie, the emoji the name stands for typed whole comes first, then the table's order; any case is taken.
    #[test]
    fn suggestions_owner_first_on_a_tie() {
        assert_eq!(shown(&complete_emoji("amor", Lang::Pt)), ["❤️:amor:", "😍:amor:", "🥰:amor:"]);
        assert_eq!(shown(&complete_emoji("COR", Lang::Pt))[..3], ["❤️:coracao:", "😍:coracao:", "🥰:coracoes:"]);
    }

    /// The name shown is in the user's language when both languages have one that fits.
    #[test]
    fn suggestions_in_the_users_language() {
        assert_eq!(shown(&complete_emoji("ok", Lang::Pt)), ["👌:ok:"]);
        assert_eq!(shown(&complete_emoji("ok", Lang::En)), ["👌:ok_hand:"]);
        assert_eq!(shown(&complete_emoji("D", Lang::Pt))[5], "🎲:dado:");
        assert_eq!(shown(&complete_emoji("D", Lang::En))[5], "🎲:dice:");
    }

    /// Any number of emoji, symbols shown as emoji, skin tones, people joined and flags; anything else spoils it.
    #[test]
    fn emoji_alone() {
        for text in ["⏰", "▶️", "↩️", "ℹ️", "©️", "‼️", "👍🏽", "👨‍👩‍👧", "❤️‍🔥", "🇵🇹", "😀😂👍🎉", " 🎂 🎉 ", "👍🏽👨‍👩‍👧🇵🇹⏰"] {
            assert!(emoji_only(text), "{text}");
        }
        for text in ["", "  ", "ok 👍", "👍!", ":)", "<3", "1", "🇵"] {
            assert!(!emoji_only(text), "{text:?}");
        }
    }

    /// A reaction is one emoji, or one smiley taken as its emoji.
    #[test]
    fn one_emoji_for_a_reaction() {
        assert_eq!(reaction(" 👍🏽 ").as_deref(), Some("👍🏽"));
        assert_eq!(reaction("👨‍👩‍👧").as_deref(), Some("👨‍👩‍👧"));
        assert_eq!(reaction("<3").as_deref(), Some("❤️"));
        assert_eq!(reaction(":*").as_deref(), Some("😘"));
        assert_eq!(reaction("👍👍"), None);
        assert_eq!(reaction("ok"), None);
    }
}
