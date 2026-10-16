//! Writing suggestions from a local model (llama-server, an OpenAI-compatible API), as the Node version's llm.ts, of only two
//! kinds: the word that's half-typed at the cursor, completed or corrected, and the corrections of the wrong words
//! already written in the sentence (spelling, words stuck together, a word swapped in an expression, grammar,
//! punctuation), all of them. Also a short description of an image, and each chat's state as one emoji (mood.rs).
//! The server is WA_LLM (http://127.0.0.1:8080 by default, "off" to do without) and the model WA_LLM_MODEL; with no
//! server answering, the suggestions simply don't appear.
//!
//! The requests block, for a thread of their own or `tokio::task::spawn_blocking`. Each can be given up from another
//! thread with its `Cancel`, which closes the connection at once, so the server stops working on it too, and each has
//! a time limit.

use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use log::warn;
use regex::Regex;
use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use ureq::Agent;
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport};

use crate::text::fold;

/// The language the model is asked in, and so answers in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    /// European Portuguese.
    Pt,
    En,
}

impl Lang {
    /// The user's language as wassup's i18n picks it: from WA_LANG, or else the locale (LC_ALL, LC_MESSAGES, LANG),
    /// Portuguese when it starts with "pt", English otherwise.
    pub fn from_env() -> Lang {
        let locale = ["WA_LANG", "LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|v| std::env::var(v).ok());
        let pt = locale.and_then(|l| l.get(..2).map(|p| p.eq_ignore_ascii_case("pt"))).unwrap_or(false);
        if pt { Lang::Pt } else { Lang::En }
    }
}

/// A request's way out: once cancelled, from any thread, the request gives up and closes its connection.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// A Cancel not yet cancelled.
    pub fn new() -> Cancel {
        Cancel::default()
    }

    /// Gives up the request under way with this Cancel, and any made with it from now on.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether `cancel` has been called, on this one or a clone.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Whether the two are the same Cancel, clones of one another.
    pub fn same(&self, other: &Cancel) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Why a request brought no answer. An answer with nothing usable in it isn't one of these: it comes as `Ok(None)`.
#[derive(Debug)]
pub enum Error {
    /// Given up with its `Cancel`.
    Cancelled,
    /// No answer within the time limit.
    Timeout,
    /// No server answering, or an answer that isn't JSON.
    Failed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => write!(f, "cancelled"),
            Error::Timeout => write!(f, "no answer in time"),
            Error::Failed(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// The writing suggestions for a text: the word half-typed at its end, completed or corrected, and the wrong words
/// before it, corrected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The half-typed word, when there's one and the model has a word for it.
    pub word: Option<Word>,
    /// Finished words the model flags as wrong, exactly as they are in the text, their correction, and where they are,
    /// in the text's order, never overlapping. When the model returns a whole expression ("de vem em quando" → "de vez
    /// em quando"), only the part that changes is kept ("vem" → "vez").
    pub fixes: Vec<Fix>,
}

/// The word half-typed at the end of the text (`from`, what's written) and the whole word the model proposes (`to`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub from: String,
    pub to: String,
}

/// A correction: what's written, what it should be, and where it is in the text (byte offsets, `start..end`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub from: String,
    pub to: String,
    pub start: usize,
    pub end: usize,
}

/// How long a request may take unless told otherwise: as long as wassup's, which has no limit of its own, waits
/// (Node's fetch gives up after five minutes without an answer).
pub const TIMEOUT: Duration = Duration::from_secs(300);
/// How long each address asked for the model's context may take.
const INFO_TIMEOUT: Duration = Duration::from_secs(3);
/// How often a request waiting for its answer looks at its `Cancel`.
const POLL: Duration = Duration::from_millis(100);

const SYSTEM_PT: &str = r#"Ajudas a escrever mensagens de WhatsApp em português de Portugal (ortografia europeia). Recebes a conversa recente e o texto em curso, que termina onde está o cursor.
Responde só com JSON: {"word": "...", "fixes": [{"wrong": "...", "fix": "..."}]}.
- "word": se o texto em curso acabar a meio de uma palavra, essa palavra inteira, como deve ficar escrita (completa-a; se o que está escrito tiver erro, dá a forma certa; se forem duas palavras coladas, separa-as); senão "". Escolhe pelo tom e assunto da conversa.
- "fixes": por cada erro, se alguma palavra já terminada do texto em curso tiver erro ortográfico ou acento em falta ("amanha" → "amanhã", "as 8" → "às 8", "nao" → "não"), forem duas palavras coladas sem espaço ("vamosjantar"), for uma palavra trocada por outra parecida que não faz sentido ali ("de vem em quando" → "de vez em quando"), ou houver um erro gramatical (concordância, conjugação, regência: "a gente vamos" → "a gente vai", "houveram problemas" → "houve problemas", "fazem dois anos" → "faz dois anos"), ou faltar uma vírgula a seguir a uma saudação ou antes de um vocativo ("Olá gostas de mim?" → "Olá, gostas de mim?", "obrigado Marta" → "obrigado, Marta"), em "wrong" o trecho exactamente como está escrito (o mais curto possível, só as palavras precisas) e em "fix" a sua correcção. Todos os erros, pela ordem do texto, até cinco; sem erros, []. Não mudes nomes próprios, estrangeirismos, abreviaturas correntes, a linguagem informal nem o estilo de quem escreve."#;

const SYSTEM_EN: &str = r#"You help write WhatsApp messages in English. You get the recent conversation and the text being typed, which ends where the cursor is.
Answer only with JSON: {"word": "...", "fixes": [{"wrong": "...", "fix": "..."}]}.
- "word": if the text ends in the middle of a word, that whole word as it should be written (complete it; if what is written has a typo, give the right form; if two words are stuck together, separate them); otherwise "". Choose by the tone and topic of the conversation.
- "fixes": for each error, if some finished word of the text has a spelling error ("tomorow" → "tomorrow", "recieve" → "receive"), two words are stuck together without a space ("letsgo"), a word was swapped for a similar one that makes no sense there ("could of" → "could have", "their going" → "they're going"), or there is a grammar error (agreement, tense: "he don't" → "he doesn't", "we was" → "we were"), or a comma is missing after a greeting or before a name being addressed ("Hi how are you?" → "Hi, how are you?", "thanks John" → "thanks, John"), give in "wrong" the passage exactly as written (as short as possible, only the words needed) and in "fix" its correction. All the errors, in the text's order, up to five; with none, []. Do not change proper names, slang, common abbreviations, informal language or the writer's style."#;

/// The headings of the two parts of the user's turn: the recent conversation, and the text being typed.
const LABELS_PT: (&str, &str) = ("Conversa recente:", "Texto em curso (termina no cursor, sem mais nada a seguir):");
const LABELS_EN: (&str, &str) = ("Recent conversation:", "Text being typed (ends at the cursor, nothing after it):");

/// The answer's shape for the suggestions, as written here: llama-server makes the model write the properties in this
/// order, so the word comes before the fixes and the wrong passage before its fix.
const SCHEMA: &str = r#"{"type":"object","properties":{"word":{"type":"string","maxLength":60},"fixes":{"type":"array","maxItems":5,"items":{"type":"object","properties":{"wrong":{"type":"string","maxLength":40},"fix":{"type":"string","maxLength":60}},"required":["wrong","fix"],"additionalProperties":false}}},"required":["word","fixes"],"additionalProperties":false}"#;

const DESCRIBE_PT: &str = "Esta imagem chegou numa conversa de WhatsApp e quem a recebeu só a vê em baixa resolução, num terminal. Descreve-a em uma ou duas frases curtas, em português de Portugal: o que mostra (pessoas, objectos, lugar, o que se passa) e, se tiver texto legível, o que diz (sem texto, não o menciones). Responde só com a descrição.";
const DESCRIBE_EN: &str = "This image arrived in a WhatsApp chat and whoever got it only sees it in low resolution, in a terminal. Describe it in one or two short sentences: what it shows (people, objects, place, what is going on) and, if it has legible text, what it says (with no text, don't mention it). Answer only with the description.";

const MOOD_PT: &str = r#"Recebes as mensagens mais recentes de uma conversa de WhatsApp, da mais antiga para a mais recente, cada uma com quem a escreveu. Responde só com JSON: {"emoji": "…"}, um único emoji que diga o estado da conversa agora: o tom e o que se está a passar (por exemplo, combinar um encontro, uma pergunta à espera de resposta, carinho, preocupação, brincadeira, trabalho, uma despedida)."#;
const MOOD_EN: &str = r#"You get the latest messages of a WhatsApp chat, oldest first, each with who wrote it. Answer only with JSON: {"emoji": "…"}, a single emoji that tells the state of the conversation now: its tone and what is going on (for example, arranging to meet, a question waiting for an answer, affection, worry, joking, work, a goodbye)."#;

const MOOD_SCHEMA: &str = r#"{"type":"object","properties":{"emoji":{"type":"string","maxLength":16}},"required":["emoji"],"additionalProperties":false}"#;

/// The half-typed word at the end of a text: letters, marks, apostrophes and hyphens.
static PARTIAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{M}'-]+$").unwrap());
/// A character that belongs to a word, for where a word or expression starts and ends.
static WORD_CHAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\p{L}\p{M}\p{N}'-]$").unwrap());
/// Up to four words of letters or digits, a space between each.
static WORDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{L}\p{M}\p{N}'-]+( [\p{L}\p{M}\p{N}'-]+){0,3}$").unwrap());
/// The same, each word allowed a punctuation mark after it.
static WORDS_PUNCT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\p{L}\p{M}\p{N}'-]+[,;:!?.]?( [\p{L}\p{M}\p{N}'-]+[,;:!?.]?){0,3}$").unwrap()
});
/// An emoji, with its variation selector when it has one.
static EMOJI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Extended_Pictographic}\x{FE0F}?").unwrap());

/// The local model's server, the model asked for, and the language it's asked in.
#[derive(Clone, Debug)]
pub struct Llm {
    url: String,
    model: String,
    lang: Lang,
    timeout: Duration,
}

impl Llm {
    /// The server and model from WA_LLM and WA_LLM_MODEL (http://127.0.0.1:8080 and gemma4-26b by default), or None
    /// when WA_LLM is "off" or empty.
    pub fn from_env(lang: Lang) -> Option<Llm> {
        let url = std::env::var("WA_LLM").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
        let model = std::env::var("WA_LLM_MODEL").unwrap_or_else(|_| "gemma4-26b".into());
        (url != "off" && !url.is_empty()).then(|| Llm::new(url, model, lang))
    }

    /// The server at `url` (no slash at the end), with `model`, asked in `lang`; requests limited to TIMEOUT.
    pub fn new(url: impl Into<String>, model: impl Into<String>, lang: Lang) -> Llm {
        Llm { url: url.into(), model: model.into(), lang, timeout: TIMEOUT }
    }

    /// The same, its requests given up after `timeout`.
    pub fn with_timeout(self, timeout: Duration) -> Llm {
        Llm { timeout, ..self }
    }

    /// The model's context, in tokens: for a llama-server that loads its models on demand, from the arguments it runs
    /// this one with; for a plain llama-server, the context it says it has; 8192 when neither says. 0 with no server
    /// answering. Each address asked gets three seconds.
    pub fn model_ctx(&self) -> usize {
        let agent = agent(INFO_TIMEOUT, None);
        let get = |path: &str| -> Option<Value> {
            let mut res = agent.get(format!("{}{path}", self.url)).call().ok()?;
            serde_json::from_str(&res.body_mut().read_to_string().ok()?).ok()
        };
        let Some(models) = get("/v1/models") else { return 0 };
        if let Some(ctx) = ctx_from_models(&models, &self.model) {
            return ctx;
        }
        get("/props").map_or(0, |props| ctx_from_props(&props))
    }

    /// Suggestions for `text`, the message being written, which ends at the cursor, with the chat's latest messages
    /// for context (who wrote each, and its text; oldest first). None when there's nothing to suggest, or the server
    /// said no.
    pub fn suggest(
        &self,
        context: &[(String, String)],
        text: &str,
        cancel: &Cancel,
    ) -> Result<Option<Suggestion>, Error> {
        let (system, (recent, current)) = match self.lang {
            Lang::Pt => (SYSTEM_PT, LABELS_PT),
            Lang::En => (SYSTEM_EN, LABELS_EN),
        };
        let mut user = vec![recent.to_string()];
        user.extend(context.iter().map(|(who, said)| format!("{who}: {}", clip(&squeeze(said), 200))));
        user.extend([String::new(), current.to_string(), text.to_string()]);
        let messages = json!([{ "role": "system", "content": system }, { "role": "user", "content": user.join("\n") }]);
        let Some(content) = self.ask(200, Some(("suggestion", SCHEMA)), messages, "llm", cancel)? else {
            return Ok(None);
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&content) else {
            warn!("llm: the answer is not JSON: {content}");
            return Ok(None);
        };
        Ok(read_suggestion(&parsed, text))
    }

    /// What an image shows, in a sentence or two in the user's language, from its PNG. None when the model says
    /// nothing, or the server said no.
    pub fn describe(&self, png: &[u8], cancel: &Cancel) -> Result<Option<String>, Error> {
        let prompt = match self.lang {
            Lang::Pt => DESCRIBE_PT,
            Lang::En => DESCRIBE_EN,
        };
        let url = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png));
        let messages = json!([{
            "role": "user",
            "content": [{ "type": "text", "text": prompt }, { "type": "image_url", "image_url": { "url": url } }],
        }]);
        let content = self.ask(200, None, messages, "llm: describe", cancel)?;
        Ok(content.map(|c| squeeze(&c).trim().to_string()).filter(|d| !d.is_empty()))
    }

    /// One emoji for the state of a conversation, from its latest messages (`lines`, "who: text", oldest first, as
    /// mood::mood_line writes them); None when the model gives none, or the server said no.
    pub fn mood(&self, lines: &[String], cancel: &Cancel) -> Result<Option<String>, Error> {
        let system = match self.lang {
            Lang::Pt => MOOD_PT,
            Lang::En => MOOD_EN,
        };
        let user = lines.join("\n");
        let messages = json!([{ "role": "system", "content": system }, { "role": "user", "content": user }]);
        let content = self.ask(20, Some(("mood", MOOD_SCHEMA)), messages, "llm: mood", cancel)?;
        Ok(content.and_then(|c| read_mood(&c)))
    }

    /// Asks the model, with the answer held to `schema` (its name and the schema) when given, for the text of its
    /// answer: None when there's none, or when the server says no, which is logged as `what`.
    fn ask(
        &self,
        max_tokens: u32,
        schema: Option<(&'static str, &'static str)>,
        messages: Value,
        what: &str,
        cancel: &Cancel,
    ) -> Result<Option<String>, Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let body = Request {
            model: &self.model,
            temperature: 0,
            max_tokens,
            reasoning_effort: "none",
            chat_template_kwargs: json!({ "enable_thinking": false }),
            response_format: schema.map(|(name, schema)| Format {
                kind: "json_schema",
                json_schema: Named { name, schema: serde_json::from_str(schema).expect("the schemas are JSON") },
            }),
            messages,
        };
        let body = serde_json::to_string(&body).expect("a request is JSON");
        let failed = |e: ureq::Error| match e {
            _ if cancel.is_cancelled() => Error::Cancelled,
            ureq::Error::Timeout(_) => Error::Timeout,
            e => Error::Failed(format!("{what}: {e}")),
        };
        let mut res = agent(self.timeout, Some(cancel))
            .post(format!("{}/v1/chat/completions", self.url))
            .header("Content-Type", "application/json")
            .send(body)
            .map_err(&failed)?;
        if !res.status().is_success() {
            warn!("{what}: status {}", res.status().as_u16());
            return Ok(None);
        }
        let text = res.body_mut().read_to_string().map_err(&failed)?;
        let data: Value = serde_json::from_str(&text).map_err(|e| Error::Failed(format!("{what}: {e}")))?;
        Ok(data["choices"][0]["message"]["content"].as_str().filter(|c| !c.is_empty()).map(String::from))
    }
}

/// A chat completion request, its fields in the order wassup sends them.
#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    temperature: u32,
    max_tokens: u32,
    reasoning_effort: &'static str,
    chat_template_kwargs: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<Format<'a>>,
    messages: Value,
}

#[derive(Serialize)]
struct Format<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    json_schema: Named<'a>,
}

#[derive(Serialize)]
struct Named<'a> {
    name: &'static str,
    schema: &'a RawValue,
}

/// An agent for one request: answers of any status come back as they are, the request gives up after `timeout`, and,
/// with a `cancel`, as soon as that says so.
fn agent(timeout: Duration, cancel: Option<&Cancel>) -> Agent {
    let config = Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build();
    match cancel {
        Some(cancel) => {
            let connector = DefaultConnector::new().chain(Cancellable(cancel.clone()));
            Agent::with_parts(config, connector, DefaultResolver::default())
        }
        None => Agent::new_with_config(config),
    }
}

/// The error a cancelled request ends its connection with.
fn cancelled() -> ureq::Error {
    io::Error::new(io::ErrorKind::Interrupted, "cancelled").into()
}

/// Makes each connection of a request watch the request's `Cancel` while it waits for the server.
#[derive(Debug)]
struct Cancellable(Cancel);

impl Connector<Box<dyn Transport>> for Cancellable {
    type Out = Watched;

    fn connect(
        &self,
        _: &ConnectionDetails,
        chained: Option<Box<dyn Transport>>,
    ) -> Result<Option<Watched>, ureq::Error> {
        if self.0.is_cancelled() {
            return Err(cancelled());
        }
        Ok(chained.map(|inner| Watched { inner, cancel: self.0.clone() }))
    }
}

/// A connection that, waiting for the server, looks at its `Cancel` every POLL and ends when it's set: ureq then
/// drops it, closing the socket, which tells llama-server to stop working on the request.
#[derive(Debug)]
struct Watched {
    inner: Box<dyn Transport>,
    cancel: Cancel,
}

impl Transport for Watched {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        if self.cancel.is_cancelled() {
            return Err(cancelled());
        }
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let deadline = if timeout.after.is_not_happening() { None } else { Instant::now().checked_add(*timeout.after) };
        loop {
            if self.cancel.is_cancelled() {
                return Err(cancelled());
            }
            let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if left == Some(Duration::ZERO) {
                return Err(ureq::Error::Timeout(timeout.reason));
            }
            let step = NextTimeout { after: left.map_or(POLL, |l| l.min(POLL)).into(), reason: timeout.reason };
            match self.inner.await_input(step) {
                Err(ureq::Error::Timeout(_)) => continue,
                other => return other,
            }
        }
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

/// The context from /v1/models, when the server runs the model with arguments of its own (one that loads its models
/// on demand): what follows -c or --ctx-size, 8192 without it. None for a plain server.
fn ctx_from_models(models: &Value, model: &str) -> Option<usize> {
    let m = models["data"].as_array()?.iter().find(|d| d["id"] == model)?;
    let status = m.get("status").filter(|s| s.is_object())?;
    let args = status["args"].as_array().map(Vec::as_slice).unwrap_or_default();
    let at = args.iter().position(|a| a == "-c" || a == "--ctx-size");
    let ctx = at.and_then(|i| args.get(i + 1)?.as_str()?.trim().parse::<f64>().ok());
    Some(ctx.filter(|n| n.is_finite() && *n >= 1.0).map_or(8192, |n| n as usize))
}

/// The context a plain llama-server says it has, from /props; 8192 when it says none.
fn ctx_from_props(props: &Value) -> usize {
    let ctx = props["default_generation_settings"]["n_ctx"].as_f64();
    ctx.filter(|n| *n >= 1.0).map_or(8192, |n| n as usize)
}

/// The suggestion in the model's answer, `parsed`, for `text`: only what fits the text is kept.
fn read_suggestion(parsed: &Value, text: &str) -> Option<Suggestion> {
    let partial = partial_word(text);
    let word_to = words(&parsed["word"], false);
    let word = match partial {
        Some(from) if !word_to.is_empty() && word_to != from && plausible_word(from, word_to) => {
            Some(Word { from: from.to_string(), to: word_to.to_string() })
        }
        _ => None,
    };
    // Each correction where it is in the text; the one at the very end only when no word is half-typed there. Those
    // overlapping one already taken are dropped.
    let mut fixes: Vec<Fix> = Vec::new();
    for item in parsed["fixes"].as_array().map(Vec::as_slice).unwrap_or_default().iter().take(5) {
        let (wrong, fix_to) = (words(&item["wrong"], true), words(&item["fix"], true));
        if wrong.is_empty() || fix_to.is_empty() || fix_to == wrong {
            continue;
        }
        let at_end = || if word.is_some() { None } else { fix_at_end(text, wrong, fix_to) };
        let fix = narrow_fix(text, wrong, fix_to).or_else(at_end);
        if let Some(fix) = fix.filter(|f| !fixes.iter().any(|g| f.start < g.end && g.start < f.end)) {
            fixes.push(fix);
        }
    }
    fixes.sort_by_key(|f| f.start);
    (word.is_some() || !fixes.is_empty()).then_some(Suggestion { word, fixes })
}

/// Words as the model gives them, or "" when they aren't only words made of letters or digits, up to four separated
/// by a space (words stuck together to split, expressions): the model sometimes returns quotes, punctuation or the end
/// of the text stuck on. With `punct`, for a wrong passage and its fix, each word may end in a comma or another
/// punctuation mark, as a missing comma is one of the errors asked for.
fn words(v: &Value, punct: bool) -> &str {
    let t = v.as_str().unwrap_or("").trim();
    let re: &Regex = if punct { &WORDS_PUNCT } else { &WORDS };
    if re.is_match(t) { t } else { "" }
}

/// The emoji in the model's answer for a chat's state: the first one in it, with its variation selector.
fn read_mood(content: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(content).ok()?;
    EMOJI.find(parsed["emoji"].as_str()?).map(|m| m.as_str().to_string())
}

fn word_char(c: char) -> bool {
    WORD_CHAR.is_match(c.encode_utf8(&mut [0; 4]))
}

/// Runs of white space as one space, as wassup's `replace(/\s+/g, ' ')`.
pub(crate) fn squeeze(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() || c == '\u{feff}' {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

/// At most `max` UTF-16 units of `s`, as JavaScript's `slice(0, max)` counts them, never cutting a character.
pub(crate) fn clip(s: &str, max: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max {
            return &s[..i];
        }
    }
    s
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i];
        for j in 1..=b.len() {
            cur.push((prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1])));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The word proposed for the one half-typed must continue it (ignoring accents and case) or, with three or more
/// letters already written, differ little from it: a typo, not another word ("e" never becomes "depois").
fn plausible_word(from: &str, to: &str) -> bool {
    let f: Vec<char> = fold(from).chars().collect();
    let t: Vec<char> = fold(to).chars().filter(|&c| c != ' ').collect();
    if t.starts_with(&f) {
        return t.len() > f.len() || to != from;
    }
    if f.len() < 3 {
        return false;
    }
    levenshtein(&f, &t[..f.len().min(t.len())]) <= (f.len() / 4).max(1)
}

/// The half-typed word at the end of the text, if the text ends on a letter.
fn partial_word(text: &str) -> Option<&str> {
    PARTIAL.find(text).map(|m| m.as_str())
}

/// Where, in the text, the word or expression the model says is wrong is: the last whole occurrence (between
/// characters that aren't a word's), never the one ending in the word still half-typed at the end.
fn locate_word(text: &str, phrase: &str) -> Option<(usize, usize)> {
    if phrase.is_empty() {
        return None;
    }
    let at_end = PARTIAL.is_match(text);
    let mut found = None;
    let mut from = 0;
    while let Some(at) = text[from..].find(phrase) {
        let (start, end) = (from + at, from + at + phrase.len());
        let whole = !text[..start].chars().next_back().is_some_and(word_char)
            && !text[end..].chars().next().is_some_and(word_char);
        if !whole {
            from = start + text[start..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        if at_end && end == text.len() {
            break;
        }
        found = Some((start, end));
        from = end;
    }
    found
}

/// Narrows an expression's correction down to the words that change, at its last whole occurrence in the text.
fn narrow_fix(text: &str, wrong: &str, fix: &str) -> Option<Fix> {
    narrow_at(locate_word(text, wrong)?, wrong, fix)
}

/// The model often puts the correction for the word still half-typed at the cursor into "wrong"/"fix" instead of
/// "word" ("nao" → "não", "esta bem" → "está bem"). This is accepted when the wrong passage ends right at the end of
/// the text, word for word, and each corrected word matches what was written or is plausible as its correction.
fn fix_at_end(text: &str, wrong: &str, fix: &str) -> Option<Fix> {
    if !text.ends_with(wrong) {
        return None;
    }
    let start = text.len() - wrong.len();
    if text[..start].chars().next_back().is_some_and(word_char) {
        return None;
    }
    let (a, b): (Vec<&str>, Vec<&str>) = (wrong.split(' ').collect(), fix.split(' ').collect());
    if a.len() != b.len() || !a.iter().zip(&b).all(|(w, f)| w == f || plausible_word(w, f)) {
        return None;
    }
    narrow_at((start, text.len()), wrong, fix)
}

/// The correction of `wrong`, found at `start..end`, with the words that are the same at the start and end of both
/// left out: "de vem em quando" → "de vez em quando" becomes "vem" → "vez", where "vem" is.
fn narrow_at((start, end): (usize, usize), wrong: &str, fix: &str) -> Option<Fix> {
    let (a, b): (Vec<&str>, Vec<&str>) = (wrong.split(' ').collect(), fix.split(' ').collect());
    let mut head = 0;
    while head < a.len() && head < b.len() && a[head] == b[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < a.len() - head && tail < b.len() - head && a[a.len() - 1 - tail] == b[b.len() - 1 - tail] {
        tail += 1;
    }
    let (from, to) = (a[head..a.len() - tail].join(" "), b[head..b.len() - tail].join(" "));
    if from.is_empty() || to.is_empty() || from == to {
        return None;
    }
    let start = start + a[..head].join(" ").len() + usize::from(head > 0);
    let end = end - a[a.len() - tail..].join(" ").len() - usize::from(tail > 0);
    Some(Fix { from, to, start, end })
}

/// The model the screen and the moods use, set up once from the environment (None with WA_LLM=off).
pub fn shared() -> Option<&'static Llm> {
    static SHARED: OnceLock<Option<Llm>> = OnceLock::new();
    SHARED.get_or_init(|| Llm::from_env(Lang::from_env())).as_ref()
}

/// Whether there's a model to ask.
pub fn enabled() -> bool {
    shared().is_some()
}

/// A correction's place counted in characters, as the band's cursor is, from the model's bytes.
fn in_chars(text: &str, f: Fix) -> Fix {
    let at = |b: usize| text.get(..b.min(text.len())).map(|s| s.chars().count()).unwrap_or(0);
    Fix { start: at(f.start), end: at(f.end), ..f }
}

/// The writing suggestion for `text`, its corrections' places in characters.
pub fn suggest_text(context: &[(String, String)], text: &str, cancel: &Cancel) -> Result<Option<Suggestion>, Error> {
    let Some(llm) = shared() else { return Ok(None) };
    Ok(llm.suggest(context, text, cancel)?.map(|s| Suggestion { word: s.word, fixes: s.fixes.into_iter().map(|f| in_chars(text, f)).collect() }))
}

/// What a picture shows.
pub fn describe_png(png: &[u8], cancel: &Cancel) -> Result<Option<String>, Error> {
    match shared() {
        Some(llm) => llm.describe(png, cancel),
        None => Ok(None),
    }
}

impl From<crate::spell::Fix> for Fix {
    fn from(f: crate::spell::Fix) -> Self {
        Fix { from: f.from, to: f.to, start: f.start, end: f.end }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn fix(from: &str, to: &str, start: usize, end: usize) -> Fix {
        Fix { from: from.into(), to: to.into(), start, end }
    }

    fn word(from: &str, to: &str) -> Option<Word> {
        Some(Word { from: from.into(), to: to.into() })
    }

    #[test]
    fn half_typed_word() {
        assert_eq!(partial_word("vamos ao cine"), Some("cine"));
        assert_eq!(partial_word("está bem"), Some("bem"));
        assert_eq!(partial_word("d'água"), Some("d'água"));
        assert_eq!(partial_word("ok "), None);
        assert_eq!(partial_word("às 8"), None);
    }

    #[test]
    fn plausible_words() {
        assert!(plausible_word("cine", "cinema"));
        assert!(plausible_word("amanha", "amanhã"));
        assert!(plausible_word("vamosjantar", "vamos jantar"));
        assert!(plausible_word("restaurnate", "restaurante"));
        assert!(!plausible_word("e", "depois"));
        assert!(!plausible_word("cinema", "cinema"));
        assert!(!plausible_word("casa", "praia"));
    }

    #[test]
    fn whole_word_found_last_and_not_at_the_half_typed_end() {
        assert_eq!(locate_word("nao sei se nao vou", "nao"), Some((11, 14)));
        assert_eq!(locate_word("canao nao", "nao"), None);
        assert_eq!(locate_word("nao sei, nao", "nao"), Some((0, 3)));
        assert_eq!(locate_word("vou amanha.", "amanha"), Some((4, 10)));
        assert_eq!(locate_word("às vezes", "as"), None);
    }

    #[test]
    fn expressions_narrowed_to_what_changes() {
        assert_eq!(narrow_fix("isto de vem em quando cansa", "de vem em quando", "de vez em quando"), Some(fix("vem", "vez", 8, 11)));
        assert_eq!(narrow_fix("a gente vamos lá", "a gente vamos", "a gente vai"), Some(fix("vamos", "vai", 8, 13)));
        assert_eq!(narrow_fix("Olá gostas de mim?", "Olá", "Olá,"), Some(fix("Olá", "Olá,", 0, 4)));
        assert_eq!(narrow_fix("tudo bem", "tudo bem", "tudo bem"), None);
    }

    #[test]
    fn fix_at_the_end_only_when_plausible() {
        assert_eq!(fix_at_end("eu nao", "nao", "não"), Some(fix("nao", "não", 3, 6)));
        assert_eq!(fix_at_end("ok esta bem", "esta bem", "está bem"), Some(fix("esta", "está", 3, 7)));
        assert_eq!(fix_at_end("ok e", "e", "depois"), None);
        assert_eq!(fix_at_end("canao", "nao", "não"), None);
    }

    #[test]
    fn suggestion_read_from_the_answer() {
        let text = "amanha jantamos as 8 no restau";
        let answer = json!({
            "word": "restaurante",
            "fixes": [{ "wrong": "amanha", "fix": "amanhã" }, { "wrong": "as 8", "fix": "às 8" }],
        });
        let s = read_suggestion(&answer, text).unwrap();
        assert_eq!(s.word, word("restau", "restaurante"));
        assert_eq!(s.fixes, vec![fix("amanha", "amanhã", 0, 6), fix("as", "às", 16, 18)]);
        assert_eq!(&text[s.fixes[1].start..s.fixes[1].end], "as");
    }

    #[test]
    fn suggestion_keeps_only_what_fits() {
        // A fix not in the text, one for the word half-typed at the end while there's a word for it, and one that
        // changes nothing: the word alone.
        let text = "vou ao cine";
        let answer = json!({
            "word": "cinema",
            "fixes": [{ "wrong": "vai", "fix": "vou" }, { "wrong": "cine", "fix": "cinema" }, { "wrong": "ao", "fix": "ao" }],
        });
        assert_eq!(read_suggestion(&answer, text), Some(Suggestion { word: word("cine", "cinema"), fixes: vec![] }));
        // A word that isn't one, or the half-typed word given back as it is: nothing.
        assert_eq!(read_suggestion(&json!({ "word": "cinema\"", "fixes": [] }), text), None);
        assert_eq!(read_suggestion(&json!({ "word": "cine", "fixes": [] }), text), None);
        assert_eq!(read_suggestion(&json!({ "word": 3, "fixes": "nao" }), text), None);
        // Overlapping fixes: the first one stays.
        let answer = json!({ "word": "", "fixes": [{ "wrong": "vou ao", "fix": "vou à" }, { "wrong": "ao", "fix": "à" }] });
        assert_eq!(read_suggestion(&answer, text).unwrap().fixes, vec![fix("ao", "à", 4, 6)]);
    }

    #[test]
    fn fix_at_the_end_only_without_a_word() {
        let answer = json!({ "word": "", "fixes": [{ "wrong": "nao", "fix": "não" }] });
        let s = read_suggestion(&answer, "eu nao").unwrap();
        assert_eq!((s.word, s.fixes), (None, vec![fix("nao", "não", 3, 6)]));
        let answer = json!({ "word": "não", "fixes": [{ "wrong": "nao", "fix": "não" }] });
        let s = read_suggestion(&answer, "eu nao").unwrap();
        assert_eq!((s.word, s.fixes), (word("nao", "não"), vec![]));
    }

    #[test]
    fn mood_read_from_the_answer() {
        assert_eq!(read_mood(r#"{"emoji": "🍕"}"#).as_deref(), Some("🍕"));
        assert_eq!(read_mood(r#"{"emoji": "❤️ e 😀"}"#).as_deref(), Some("❤\u{fe0f}"));
        assert_eq!(read_mood(r#"{"emoji": "  👍🏽"}"#).as_deref(), Some("👍"));
        assert_eq!(read_mood(r#"{"emoji": "ok"}"#), None);
        assert_eq!(read_mood(r#"{"emoji": 5}"#), None);
        assert_eq!(read_mood("not json"), None);
    }

    #[test]
    fn context_from_the_server() {
        let models = json!({ "data": [
            { "id": "other", "status": { "args": ["--ctx-size", "4096"] } },
            { "id": "gemma", "status": { "value": "unloaded", "args": ["llama-server", "--ctx-size", "49152", "--port", "0"] } },
        ]});
        assert_eq!(ctx_from_models(&models, "gemma"), Some(49152));
        assert_eq!(ctx_from_models(&json!({ "data": [{ "id": "gemma", "status": { "args": ["-c", "x"] } }] }), "gemma"), Some(8192));
        assert_eq!(ctx_from_models(&json!({ "data": [{ "id": "gemma" }] }), "gemma"), None);
        assert_eq!(ctx_from_models(&json!({ "data": [] }), "gemma"), None);
        assert_eq!(ctx_from_props(&json!({ "default_generation_settings": { "n_ctx": 16384 } })), 16384);
        assert_eq!(ctx_from_props(&json!({ "default_generation_settings": { "n_ctx": 0 } })), 8192);
    }

    #[test]
    fn whitespace_and_clipping() {
        assert_eq!(squeeze("a \n\t b  c"), "a b c");
        assert_eq!(squeeze(" a "), " a ");
        assert_eq!(clip("abc", 2), "ab");
        assert_eq!(clip("a😀b", 2), "a");
        assert_eq!(clip("a😀b", 3), "a😀");
        assert_eq!(clip("ação", 10), "ação");
    }

    /// A server on a free port that reads one request and answers it with `answer`, or never when None. The request
    /// comes back through the handle once the client has closed the connection.
    fn server(answer: Option<&'static str>) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0; 65536];
            loop {
                let text = String::from_utf8_lossy(&got);
                if let Some(head) = text.find("\r\n\r\n") {
                    let length = text[..head].lines().find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok())?
                    });
                    if got.len() >= head + 4 + length.unwrap_or(0) {
                        break;
                    }
                }
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got.extend_from_slice(&buf[..n]),
                }
            }
            if let Some(body) = answer {
                let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len());
                s.write_all(head.as_bytes()).unwrap();
                s.write_all(body.as_bytes()).unwrap();
            }
            while matches!(s.read(&mut buf), Ok(n) if n > 0) {}
            String::from_utf8_lossy(&got).into_owned()
        });
        (url, handle)
    }

    #[test]
    fn request_and_answer_over_http() {
        let answer = r#"{"choices":[{"message":{"role":"assistant","content":"{\"word\":\"cinema\",\"fixes\":[]}"}}]}"#;
        let (url, handle) = server(Some(answer));
        let llm = Llm::new(url, "model-x", Lang::Pt);
        let context = vec![("Rita Lemos".to_string(), "Vamos\nao cinema?".to_string())];
        let s = llm.suggest(&context, "Sim, ao cine", &Cancel::new()).unwrap().unwrap();
        assert_eq!(s.word, word("cine", "cinema"));
        let sent = handle.join().unwrap();
        let body: Value = serde_json::from_str(&sent[sent.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(body["model"], "model-x");
        assert_eq!(body["messages"][1]["content"], format!("{}\nRita Lemos: Vamos ao cinema?\n\n{}\nSim, ao cine", LABELS_PT.0, LABELS_PT.1));
        assert_eq!(body["response_format"]["json_schema"]["name"], "suggestion");
        // The schema's properties go as written: the word before the fixes.
        assert!(sent.find("\"word\":{").unwrap() < sent.find("\"fixes\":{").unwrap());
        assert!(sent.find("\"wrong\":{").unwrap() < sent.find("\"fix\":{").unwrap());
    }

    #[test]
    fn cancelled_while_waiting_closes_the_connection() {
        let (url, handle) = server(None);
        let llm = Llm::new(url, "model-x", Lang::En);
        let cancel = Cancel::new();
        let c = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            c.cancel();
        });
        let start = Instant::now();
        let got = llm.mood(&["Rita: hello".into()], &cancel);
        assert!(matches!(got, Err(Error::Cancelled)), "{got:?}");
        assert!(start.elapsed() < Duration::from_secs(1));
        // The server sees the connection closed, so it would stop working on the request.
        handle.join().unwrap();
        assert!(matches!(llm.mood(&[], &cancel), Err(Error::Cancelled)));
    }

    #[test]
    fn timeout_while_waiting() {
        let (url, _handle) = server(None);
        let llm = Llm::new(url, "model-x", Lang::En).with_timeout(Duration::from_millis(300));
        let start = Instant::now();
        assert!(matches!(llm.describe(&[0x89, b'P'], &Cancel::new()), Err(Error::Timeout)));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn no_server() {
        let llm = Llm::new("http://127.0.0.1:9", "model-x", Lang::En);
        assert_eq!(llm.model_ctx(), 0);
        assert!(matches!(llm.suggest(&[], "hello", &Cancel::new()), Err(Error::Failed(_))));
    }

    /// A small picture, a red circle on white, 32 by 32.
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAYUlEQVR42u3WuREAIAwDQZVC/9XQEeT8jGUFYIb4NgIbxfkgABaQU2ouB+i7txKM9a0Be31tgFJfGGDVZwaI9aHxGGCv90YAvwHx0E4A989O8V0rBo5iZCqGvmJtidVRAFQg45W5hMp0CQAAAABJRU5ErkJggg==";

    fn local() -> Llm {
        Llm::new("http://127.0.0.1:8080", "gemma4-26b", Lang::Pt)
    }

    #[test]
    #[ignore = "needs llama-server at 127.0.0.1:8080"]
    fn real_model_context() {
        let ctx = local().model_ctx();
        println!("ctx {ctx}");
        assert!(ctx > 0);
    }

    #[test]
    #[ignore = "needs llama-server at 127.0.0.1:8080"]
    fn real_suggestion() {
        let context = vec![("Rita Lemos".to_string(), "Então, combinamos para quando?".to_string())];
        let text = "amanha jantamos as 8 no restau";
        let s = local().suggest(&context, text, &Cancel::new()).unwrap();
        println!("{s:?}");
        let s = s.unwrap();
        assert!(s.word.is_some() || !s.fixes.is_empty());
        for f in &s.fixes {
            assert_eq!(&text[f.start..f.end], f.from);
        }
    }

    #[test]
    #[ignore = "needs llama-server at 127.0.0.1:8080"]
    fn real_description() {
        let png = base64::engine::general_purpose::STANDARD.decode(PNG).unwrap();
        let d = local().describe(&png, &Cancel::new()).unwrap();
        println!("{d:?}");
        assert!(d.is_some());
    }

    #[test]
    #[ignore = "needs llama-server at 127.0.0.1:8080"]
    fn real_mood() {
        let lines = ["Rita Lemos: Amanhã jantamos às 8?".to_string(), "eu: Combinado! Levo a sobremesa".to_string()];
        let m = local().mood(&lines, &Cancel::new()).unwrap();
        println!("{m:?}");
        assert!(m.is_some());
    }

    #[test]
    #[ignore = "needs llama-server at 127.0.0.1:8080"]
    fn real_cancel() {
        let cancel = Cancel::new();
        let c = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            c.cancel();
        });
        let start = Instant::now();
        let got = local().mood(&["Rita Lemos: Olá!".to_string()], &cancel);
        println!("{got:?} in {:?}", start.elapsed());
        assert!(matches!(got, Err(Error::Cancelled) | Ok(_)));
        assert!(start.elapsed() < Duration::from_millis(500));
    }
}
