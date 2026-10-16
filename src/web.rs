//! `wa --web`: wassup in a browser, as the Node version's web.ts. The page (web/) runs wassup itself in each of its
//! tabs (`wa --web-tab`, webtab.rs) over a WebSocket: the same program, so the same screen, which the tab sends
//! cell by cell and the page draws in HTML. Over plain HTTP on localhost only, or, given a certificate and its key
//! (Let's Encrypt's, from certbot), over HTTPS on every interface, read again when renewed. This process keeps the
//! WhatsApp connection as a terminal does (ipc.rs), as the one with it or a client of it, so it stays up with no tab
//! open; with `--demo` there's none, and the tabs show the demonstration's invented chats, in a folder of their own.
//!
//! Entry is by passkey (WebAuthn), with no password: the device's fingerprint, face or PIN, nothing to steal or guess,
//! and useless on another site. The first one is registered through a single-use link printed on start, valid 15
//! minutes; the next ones through links made from a device already in. A session is a random token in an HttpOnly,
//! SameSite=Strict cookie (Secure over HTTPS), kept hashed in the state table and dropped after 30 days unused. The
//! passkeys, the sessions and the user's id are kept there as web.ts keeps them, under the same keys.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::net::{Ipv6Addr, SocketAddr};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, Limited, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::upgrade::Upgraded;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use log::{info, warn};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::{Child, ChildStdin};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::server::{ClientHello, ResolvesServerCert};
use tokio_rustls::rustls::sign::CertifiedKey;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};

use crate::i18n::t;
use crate::store::Store;
use crate::wa::ToUi;

/// The port when none is given, web.ts's.
const PORT: u16 = 8799;
/// Passkeys, sessions (by the token's hash) and the one user's id, in the state table.
const PASSKEYS: &str = "web:passkeys";
const SESSIONS: &str = "web:sessions";
const USER: &str = "web:user";
/// Set once the demonstration's chats are in a data folder (`--demo`), which then holds no account's.
const DEMO: &str = "web:demo";
const IDLE_MS: i64 = 30 * 86_400_000;
const INVITE_MS: i64 = 15 * 60_000;
const CHALLENGE_MS: i64 = 5 * 60_000;

/// The page's own files, built in.
const INDEX: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const APP_CSS: &str = include_str!("../web/app.css");

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
    media-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

type Body = BoxBody<Bytes, std::io::Error>;

/// `wa --web [--port N] [--invite] [--cert fullchain.pem --key privkey.pem] [--demo]`, or a browser's tab
/// (`--web-tab`, webtab.rs).
pub fn main(args: &[String]) -> Result<()> {
    if args.first().is_some_and(|a| a == "--web-tab") {
        return crate::webtab::main(args);
    }
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let port = match arg("--port") {
        Some(p) => p.parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(|| anyhow!("--port: {p} is not a port number"))?,
        None => PORT,
    };
    let tls = match (arg("--cert"), arg("--key")) {
        (Some(cert), Some(key)) => Some((PathBuf::from(cert), PathBuf::from(key))),
        (None, None) => None,
        _ => bail!("--cert and --key go together: the certificate (fullchain.pem) and its key (privkey.pem)"),
    };
    let invite = args.iter().any(|a| a == "--invite");
    let demo = args.iter().any(|a| a == "--demo");
    let dir = setup(demo)?;
    log::info!("start: pid {}, web{}", std::process::id(), if demo { ", demo" } else { "" });
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(serve(dir, demo, port, invite, tls))
}

/// What both ends do first, as main.rs does for a terminal: files for this user only, the data folder (WA_HOME, or
/// ~/.config/wassup; for the demonstration WA_HOME or a folder of its own), the log there (or WA_LOG).
pub fn setup(demo: bool) -> Result<PathBuf> {
    let dir = crate::data_dir(demo.then_some("demo-web"));
    crate::setup(&dir)?;
    Ok(dir)
}

/// The demonstration's chats, put in a data folder once; never into one with an account's.
fn fill_demo(store: &Store, dir: &Path) -> Result<()> {
    if store.state(DEMO).is_some() {
        return Ok(());
    }
    if store.state("me").is_some() {
        bail!("--demo: {} has a WhatsApp account's data; the demonstration needs a folder of its own (WA_HOME)", dir.display());
    }
    crate::demo::fill(store, dir)?;
    store.set_state(DEMO, "true")?;
    Ok(())
}

/// Serves wassup on `port`: over HTTPS on every interface with a certificate, over HTTP on localhost only without;
/// prints where, and a registration link while there's no passkey (or with `invite`). Until a signal to stop: then
/// the tabs are closed and the connection, if this process has it, with them.
async fn serve(dir: PathBuf, demo: bool, port: u16, invite: bool, tls: Option<(PathBuf, PathBuf)>) -> Result<()> {
    let store = Arc::new(Store::open(&dir.join("wassup.db"))?);
    let (wa_tx, mut wa_rx) = tokio::sync::mpsc::unbounded_channel();
    let proxy = if demo {
        fill_demo(&store, &dir)?;
        None
    } else {
        Some(crate::ipc::connect(store.clone(), &dir, wa_tx).await?)
    };
    // There's no screen here: the tabs hear the connection on their own.
    tokio::spawn(async move {
        while let Some(ev) = wa_rx.recv().await {
            if let ToUi::Connection(c, why) = ev {
                info!("web: connection {c:?} {why}");
            }
        }
    });
    let certs = match tls {
        Some((cert, key)) => Some(Arc::new(Certs::read(cert, key)?)),
        None => None,
    };
    let acceptor = certs.clone().map(tls_acceptor).transpose()?;
    // Published under the certificate's name; on localhost, not 127.0.0.1, as a passkey's relying party is a domain
    // name, never an address.
    let published = certs.as_ref().map(|c| (if port == 443 { format!("https://{}", c.name) } else { format!("https://{}:{port}", c.name) }, c.name.clone()));
    let addr: SocketAddr = if certs.is_some() { (Ipv6Addr::UNSPECIFIED, port).into() } else { ([127, 0, 0, 1], port).into() };
    let listener = TcpListener::bind(addr).await.with_context(|| format!("web: {addr}"))?;
    let web = Arc::new(Web {
        store,
        media: dir.join("media"),
        dir,
        demo,
        published,
        gate: Mutex::new(Gate::default()),
        tabs: Mutex::new(HashSet::new()),
    });
    let base = web.published.as_ref().map(|p| p.0.clone()).unwrap_or_else(|| format!("http://localhost:{port}"));
    println!("{}", t("webListening", &[&base]));
    if invite || web.state::<Vec<Passkey>>(PASSKEYS).is_empty() {
        let url = format!("{base}/?invite={}", web.new_invite(&mut web.gate.lock().unwrap()));
        println!("{}", t("webInvite", &[&url]));
        if std::io::stdout().is_terminal() {
            print!("{}", qr_text(&url));
        }
    }
    tokio::select! {
        r = accept(listener, acceptor, web.clone()) => r?,
        _ = stopped() => info!("web: stopping"),
    }
    web.close_tabs().await;
    if let Some(p) = proxy {
        p.stop().await;
    }
    Ok(())
}

/// TLS with the certificate, as it's renewed; HTTP/1.1 only, which the WebSockets go over.
fn tls_acceptor(certs: Arc<Certs>) -> Result<TlsAcceptor> {
    tokio::spawn(certs.clone().watch());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider).with_safe_default_protocol_versions()?.with_no_client_auth().with_cert_resolver(certs);
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Until SIGINT or SIGTERM.
async fn stopped() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    let term = async {
        match term.as_mut() {
            Some(t) => {
                t.recv().await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term => {}
    }
}

/// Each connection in a task of its own, its TLS handshake given 10 seconds.
async fn accept(listener: TcpListener, tls: Option<TlsAcceptor>, web: Arc<Web>) -> Result<()> {
    loop {
        let tcp = match listener.accept().await {
            Ok((tcp, _)) => tcp,
            Err(e) => {
                warn!("web: accept: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let (tls, web) = (tls.clone(), web.clone());
        tokio::spawn(async move {
            match tls {
                Some(tls) => {
                    if let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(10), tls.accept(tcp)).await {
                        connection(stream, web).await;
                    }
                }
                None => connection(tcp, web).await,
            }
        });
    }
}

/// HTTP/1.1 on one connection, upgrades (the tabs' WebSockets) included.
async fn connection<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(io: S, web: Arc<Web>) {
    let service = hyper::service::service_fn(move |req| {
        let web = web.clone();
        async move { Ok::<_, Infallible>(web.handle(req).await) }
    });
    let _ = hyper::server::conn::http1::Builder::new().timer(TokioTimer::new()).serve_connection(TokioIo::new(io), service).with_upgrades().await;
}

/// A certificate and its key, as files (certbot's fullchain.pem and privkey.pem), and the domain it's for. Let's
/// Encrypt's last 90 days and certbot renews them in place: read again when they change.
#[derive(Debug)]
struct Certs {
    cert: PathBuf,
    key: PathBuf,
    name: String,
    current: RwLock<Arc<CertifiedKey>>,
}

impl Certs {
    fn read(cert: PathBuf, key: PathBuf) -> Result<Self> {
        let (current, chain) = Self::load(&cert, &key)?;
        let leaf = chain.first().ok_or_else(|| anyhow!("web: {}: no certificate in it", cert.display()))?;
        let parsed = webpki::EndEntityCert::try_from(leaf).map_err(|e| anyhow!("web: {}: {e:?}", cert.display()))?;
        let name = parsed.valid_dns_names().find(|n| !n.starts_with('*')).map(str::to_ascii_lowercase);
        let name = name.ok_or_else(|| anyhow!("web: the certificate names no domain"))?;
        Ok(Self { cert, key, name, current: RwLock::new(Arc::new(current)) })
    }

    fn load(cert: &Path, key: &Path) -> Result<(CertifiedKey, Vec<CertificateDer<'static>>)> {
        let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
            .and_then(|certs| certs.collect::<Result<_, _>>())
            .map_err(|e| anyhow!("web: {}: {e}", cert.display()))?;
        let private = PrivateKeyDer::from_pem_file(key).map_err(|e| anyhow!("web: {}: {e}", key.display()))?;
        let signing = rustls::crypto::ring::sign::any_supported_type(&private).map_err(|e| anyhow!("web: {}: {e}", key.display()))?;
        Ok((CertifiedKey::new(chain.clone(), signing), chain))
    }

    /// Every hour, the certificate read again if its file changed.
    async fn watch(self: Arc<Self>) {
        let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let mut seen: Option<SystemTime> = modified(&self.cert);
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            let now = modified(&self.cert);
            if now == seen {
                continue;
            }
            match Self::load(&self.cert, &self.key) {
                Ok((current, _)) => {
                    *self.current.write().unwrap() = Arc::new(current);
                    seen = now;
                    info!("web: certificate read again");
                }
                Err(e) => warn!("web: certificate: {e}"),
            }
        }
    }
}

impl ResolvesServerCert for Certs {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current.read().unwrap().clone())
    }
}

/// A passkey as the state table keeps it, as web.ts does: its id and public key (COSE) in base64url.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Passkey {
    id: String,
    #[serde(rename = "publicKey")]
    public_key: String,
    counter: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transports: Option<Vec<String>>,
    created: i64,
    device: String,
}

/// A session, by its token's hash: when it was made and last used, in milliseconds.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
struct Session {
    created: i64,
    seen: i64,
}

/// A challenge handed out, by the id in its cookie, and the registration link it's for.
struct Challenge {
    challenge: String,
    expires: i64,
    invite: Option<String>,
}

/// What's kept in memory only: the registration links not used yet, the challenges handed out, the attempts at
/// signing in or registering in the last minute (past 30, refused). Its lock is also held while the passkeys and the
/// sessions are read and written back.
#[derive(Default)]
struct Gate {
    invites: HashMap<String, i64>,
    challenges: HashMap<String, Challenge>,
    attempts: Vec<i64>,
}

/// The server: the store with the state table, the folders, where it's published, and the tabs' processes.
struct Web {
    store: Arc<Store>,
    dir: PathBuf,
    media: PathBuf,
    demo: bool,
    /// Its origin and host name over HTTPS (the certificate's name and the port); none on localhost.
    published: Option<(String, String)>,
    gate: Mutex<Gate>,
    tabs: Mutex<HashSet<u32>>,
}

/// The origin the browser is on, the passkeys' relying party (its host name) and whether it's over HTTPS.
struct Origin {
    origin: String,
    rp_id: String,
    secure: bool,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// `bytes` random bytes, in base64url.
fn random(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b).expect("the system's random numbers");
    URL_SAFE_NO_PAD.encode(b)
}

fn hash(token: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, token.as_bytes()).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn header(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn cookies(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| {
            let parts: Vec<&str> = c.trim().split('=').collect();
            (parts.len() == 2).then(|| (parts[0].to_string(), parts[1].to_string()))
        })
        .collect()
}

fn cookie(name: &str, value: &str, max_age: i64, secure: bool) -> String {
    format!("{name}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{}", if secure { "; Secure" } else { "" })
}

/// A Host header's name and port; the name lowercased, an IPv6 address with its brackets.
fn split_host(host: &str) -> (String, String) {
    let host = host.to_ascii_lowercase();
    if host.starts_with('[')
        && let Some(end) = host.find(']')
    {
        return (host[..=end].to_string(), host[end + 1..].trim_start_matches(':').to_string());
    }
    match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => (name.to_string(), port.to_string()),
        _ => (host, String::new()),
    }
}

/// A short name for the device a passkey was made on, from its browser's user agent.
fn device_of(headers: &HeaderMap) -> String {
    let ua = header(headers, header::USER_AGENT).unwrap_or("");
    let has = |s: &str| ua.contains(s);
    let os = if has("iPhone") || has("iPad") {
        "iOS"
    } else if has("Android") {
        "Android"
    } else if has("Mac OS") {
        "macOS"
    } else if has("Windows") {
        "Windows"
    } else if has("Linux") {
        "Linux"
    } else {
        "?"
    };
    let browser = if has("Firefox/") {
        "Firefox"
    } else if has("Edg/") {
        "Edge"
    } else if has("Chrome/") {
        "Chrome"
    } else if has("Safari/") {
        "Safari"
    } else {
        "?"
    };
    format!("{browser} · {os}")
}

fn full(b: impl Into<Bytes>) -> Body {
    Full::new(b.into()).map_err(|never| match never {}).boxed()
}

fn empty(code: StatusCode) -> Response<Body> {
    let mut r = Response::new(full(Bytes::new()));
    *r.status_mut() = code;
    r
}

/// A JSON answer, never cached, with a cookie to set.
fn send(code: StatusCode, body: Value, cookie: Option<String>) -> Response<Body> {
    let mut r = Response::builder().status(code).header(header::CONTENT_TYPE, "application/json").header(header::CACHE_CONTROL, "no-store");
    if let Some(c) = cookie {
        r = r.header(header::SET_COOKIE, c);
    }
    r.body(full(body.to_string())).unwrap_or_else(|_| empty(StatusCode::INTERNAL_SERVER_ERROR))
}

fn error(code: StatusCode, what: &str) -> Response<Body> {
    send(code, json!({ "error": what }), None)
}

/// One of the page's own files.
fn page(text: &'static str, kind: &str, cache: &str) -> Response<Body> {
    Response::builder()
        .header(header::CONTENT_TYPE, kind)
        .header(header::CONTENT_LENGTH, text.len())
        .header(header::CACHE_CONTROL, cache)
        .body(full(Bytes::from_static(text.as_bytes())))
        .unwrap_or_else(|_| empty(StatusCode::INTERNAL_SERVER_ERROR))
}

/// A file's type by its extension, as web.ts has them.
fn type_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("ogg" | "opus") => "audio/ogg",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// `rel` against `base`, its `.` and `..` worked out on the names alone, as Node's path.resolve; an absolute `rel`
/// stands on its own.
fn resolve(base: &Path, rel: impl AsRef<Path>) -> PathBuf {
    let mut out = PathBuf::new();
    for c in base.join(rel).components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// A query's parameter, decoded as URLSearchParams decodes it.
fn query_param(query: &str, name: &str) -> Option<String> {
    let decode = |s: &str| {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            let escaped = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
            match (b[i], escaped) {
                (b'+', _) => out.push(b' '),
                (b'%', Some(v)) => {
                    out.push(v);
                    i += 2;
                }
                (c, _) => out.push(c),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(k) == name).then(|| decode(v))
    })
}

/// As JavaScript's encodeURIComponent.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// The invite's QR code for the page: black on white, a module's margin, scaled by the page.
fn qr_svg(text: &str) -> String {
    let Ok(code) = qrcode::QrCode::new(text.as_bytes()) else { return String::new() };
    let w = code.width();
    let colors = code.to_colors();
    let mut d = String::new();
    for y in 0..w {
        let mut x = 0;
        while x < w {
            if colors[y * w + x] != qrcode::Color::Dark {
                x += 1;
                continue;
            }
            let run = colors[y * w + x..(y + 1) * w].iter().take_while(|c| **c == qrcode::Color::Dark).count();
            let _ = write!(d, "M{} {}h{run}v1h-{run}z", x + 1, y + 1);
            x += run;
        }
    }
    let n = w + 2;
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n} {n}" shape-rendering="crispEdges"><path fill="#ffffff" d="M0 0h{n}v{n}H0z"/><path fill="#000000" d="{d}"/></svg>"##
    )
}

/// The invite's QR code for a terminal, two rows a line, black on white whatever its colours.
fn qr_text(text: &str) -> String {
    let Ok(code) = qrcode::QrCode::new(text.as_bytes()) else { return String::new() };
    let art = code
        .render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Dark)
        .light_color(qrcode::render::unicode::Dense1x2::Light)
        .quiet_zone(true)
        .build();
    art.lines().map(|l| format!("\x1b[30;47m{l}\x1b[0m\n")).collect()
}

/// A JSON body of at most 64 KiB: nothing is `{}`; too big or not JSON, none.
async fn read_json(body: Incoming) -> Option<Value> {
    let bytes = Limited::new(body, 64 * 1024).collect().await.ok()?.to_bytes();
    if bytes.is_empty() {
        return Some(json!({}));
    }
    serde_json::from_slice::<Value>(&bytes).ok().filter(|v| !v.is_null())
}

/// A file, read as it's sent.
async fn file(at: &Path, headers: Vec<(header::HeaderName, String)>) -> Response<Body> {
    let Ok(meta) = tokio::fs::metadata(at).await else { return empty(StatusCode::NOT_FOUND) };
    if !meta.is_file() {
        return empty(StatusCode::NOT_FOUND);
    }
    let Ok(f) = tokio::fs::File::open(at).await else { return empty(StatusCode::NOT_FOUND) };
    let chunks = futures_util::stream::unfold(Some(f), |f| async move {
        let mut f = f?;
        let mut buf = vec![0u8; 64 * 1024];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Frame::data(Bytes::from(buf))), Some(f)))
            }
            Err(e) => Some((Err(e), None)),
        }
    });
    let mut r = Response::builder().header(header::CONTENT_TYPE, type_of(at)).header(header::CONTENT_LENGTH, meta.len());
    for (k, v) in headers {
        r = r.header(k, v);
    }
    r.body(BodyExt::boxed(StreamBody::new(chunks))).unwrap_or_else(|_| empty(StatusCode::INTERNAL_SERVER_ERROR))
}

/// What the page says, checked, as the line its tab reads: the size (within bounds), keys and the like, or the
/// acknowledgement of a frame; and whether it's the size.
fn from_page(text: &str) -> Option<(String, bool)> {
    let m: Value = serde_json::from_str(text).ok()?;
    let bound = |v: &Value, default: f64, lo: f64, hi: f64| v.as_f64().unwrap_or(default).floor().clamp(lo, hi) as u16;
    let line = match m.get("t")?.as_str()? {
        "size" => json!({ "t": "size", "cols": bound(&m["cols"], 80.0, 10.0, 500.0), "rows": bound(&m["rows"], 24.0, 5.0, 300.0) }),
        "in" => json!({ "t": "in", "d": m.get("d")?.as_str()? }),
        "ack" => json!({ "t": "ack" }),
        _ => return None,
    };
    Some((format!("{line}\n"), m["t"] == "size"))
}

impl Web {
    fn state<T: DeserializeOwned + Default>(&self, key: &str) -> T {
        self.store.state(key).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    fn set_state<T: Serialize>(&self, key: &str, value: &T) {
        match serde_json::to_string(value) {
            Ok(s) => {
                if let Err(e) = self.store.set_state(key, &s) {
                    warn!("web: {key}: {e}");
                }
            }
            Err(e) => warn!("web: {key}: {e}"),
        }
    }

    /// The origin the browser is on: the one published, whatever host a request names; on localhost, plain HTTP, a
    /// secure context there too. The passkeys' relying party is its host name, so a passkey made for one name doesn't
    /// sign in on another.
    fn origin(&self, headers: &HeaderMap) -> Origin {
        if let Some((origin, host)) = &self.published {
            return Origin { origin: origin.clone(), rp_id: host.clone(), secure: true };
        }
        let (name, port) = split_host(header(headers, header::HOST).unwrap_or("localhost"));
        let origin = if port.is_empty() || port == "80" { format!("http://{name}") } else { format!("http://{name}:{port}") };
        Origin { origin, rp_id: name, secure: false }
    }

    /// The session the request carries, touched (at most once an hour, as it's written to the database), or none.
    fn session(&self, headers: &HeaderMap) -> Option<String> {
        let _gate = self.gate.lock().unwrap();
        self.session_held(headers)
    }

    /// As `session`, the gate already held.
    fn session_held(&self, headers: &HeaderMap) -> Option<String> {
        let token = cookies(headers).remove("wa_s").filter(|t| !t.is_empty())?;
        let mut all: HashMap<String, Session> = self.state(SESSIONS);
        let now = now_ms();
        let s = all.get_mut(&hash(&token))?;
        if now - s.seen > IDLE_MS {
            return None;
        }
        if now - s.seen > 3_600_000 {
            s.seen = now;
            self.set_state(SESSIONS, &all);
        }
        Some(token)
    }

    /// A new session's token, those unused for 30 days dropped.
    fn new_session(&self) -> String {
        let token = random(32);
        let now = now_ms();
        let mut all: HashMap<String, Session> = self.state(SESSIONS);
        all.retain(|_, s| now - s.seen <= IDLE_MS);
        all.insert(hash(&token), Session { created: now, seen: now });
        self.set_state(SESSIONS, &all);
        token
    }

    /// A registration link's token still valid; `spend` uses it up.
    fn invite_valid(gate: &mut Gate, token: Option<&str>, spend: bool) -> bool {
        let Some(token) = token else { return false };
        if !gate.invites.get(token).is_some_and(|e| *e >= now_ms()) {
            return false;
        }
        if spend {
            gate.invites.remove(token);
        }
        true
    }

    fn new_invite(&self, gate: &mut Gate) -> String {
        let now = now_ms();
        gate.invites.retain(|_, e| *e >= now);
        let token = random(24);
        gate.invites.insert(token.clone(), now + INVITE_MS);
        token
    }

    /// Every request: the tabs' WebSockets, the API, the page, the media; with the headers that keep the page to
    /// itself.
    async fn handle(self: Arc<Self>, req: Request<Incoming>) -> Response<Body> {
        let o = self.origin(req.headers());
        let upgrade = req.headers().contains_key(header::UPGRADE)
            && header(req.headers(), header::CONNECTION).is_some_and(|c| c.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")));
        if upgrade {
            return self.upgrade(req, &o);
        }
        let path = req.uri().path().to_string();
        let mut res = if let Some(route) = path.strip_prefix("/api/") {
            self.api(req, route, &o).await
        } else {
            match path.as_str() {
                "/" | "/index.html" => page(INDEX, "text/html; charset=utf-8", "no-store"),
                "/app.js" => page(APP_JS, "text/javascript; charset=utf-8", "no-cache"),
                "/app.css" => page(APP_CSS, "text/css; charset=utf-8", "no-cache"),
                "/media" => self.media(&req).await,
                _ => empty(StatusCode::NOT_FOUND),
            }
        };
        let h = res.headers_mut();
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
        h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
        h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
        // Once over HTTPS, the browser never comes back over plain HTTP to this name.
        if o.secure {
            h.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
        }
        res
    }

    /// An attachment a tab asked the page to open, or a picture it shows (webtab.rs): signed in, and only from the
    /// media folder.
    async fn media(&self, req: &Request<Incoming>) -> Response<Body> {
        if self.session(req.headers()).is_none() {
            return empty(StatusCode::UNAUTHORIZED);
        }
        let f = query_param(req.uri().query().unwrap_or(""), "f").unwrap_or_default();
        let at = resolve(&self.media, &f);
        if !at.starts_with(&self.media) || at == self.media {
            return empty(StatusCode::FORBIDDEN);
        }
        let name = at.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let disposition = format!("inline; filename*=UTF-8''{}", encode_uri_component(&name));
        file(&at, vec![(header::CACHE_CONTROL, "private, max-age=3600".into()), (header::CONTENT_DISPOSITION, disposition)]).await
    }

    async fn api(&self, req: Request<Incoming>, route: &str, o: &Origin) -> Response<Body> {
        let post = req.method() == Method::POST;
        // Requests that change something come from the page itself: the browser says where from.
        if post && header(req.headers(), header::ORIGIN) != Some(o.origin.as_str()) {
            return error(StatusCode::FORBIDDEN, "origin");
        }
        let headers = req.headers().clone();
        let body = if post { read_json(req.into_body()).await } else { Some(json!({})) };
        let mut gate = self.gate.lock().unwrap();
        self.api_held(&mut gate, route, post, &headers, body, o)
    }

    /// The API, once the body is read, with the gate held.
    fn api_held(&self, gate: &mut Gate, route: &str, post: bool, headers: &HeaderMap, body: Option<Value>, o: &Origin) -> Response<Body> {
        let mut passkeys: Vec<Passkey> = self.state(PASSKEYS);
        let signed_in = self.session_held(headers);
        if route == "state" {
            return send(StatusCode::OK, json!({ "in": signed_in.is_some(), "passkeys": passkeys.len() }), None);
        }
        if !post {
            return error(StatusCode::METHOD_NOT_ALLOWED, "method");
        }
        if route == "logout" {
            if let Some(token) = &signed_in {
                let mut all: HashMap<String, Session> = self.state(SESSIONS);
                all.remove(&hash(token));
                self.set_state(SESSIONS, &all);
            }
            return send(StatusCode::OK, json!({ "ok": true }), Some(cookie("wa_s", "", 0, o.secure)));
        }
        if route == "invite" {
            if signed_in.is_none() {
                return error(StatusCode::UNAUTHORIZED, "session");
            }
            let url = format!("{}/?invite={}", o.origin, self.new_invite(gate));
            return send(StatusCode::OK, json!({ "url": url, "qr": qr_svg(&url) }), None);
        }

        let now = now_ms();
        gate.attempts.retain(|a| now - a < 60_000);
        if gate.attempts.len() >= 30 {
            return error(StatusCode::TOO_MANY_REQUESTS, "busy");
        }
        gate.attempts.push(now);
        let Some(body) = body else { return error(StatusCode::BAD_REQUEST, "body") };

        let challenge_for = |gate: &mut Gate| {
            let id = cookies(headers).remove("wa_c")?;
            gate.challenges.remove(&id).filter(|c| c.expires > now_ms())
        };
        let hand_out = |gate: &mut Gate, challenge: String, invite: Option<String>| {
            let now = now_ms();
            gate.challenges.retain(|_, c| c.expires >= now);
            let id = random(16);
            gate.challenges.insert(id.clone(), Challenge { challenge, expires: now + CHALLENGE_MS, invite });
            cookie("wa_c", &id, CHALLENGE_MS / 1000, o.secure)
        };
        let sign_in = || cookie("wa_s", &self.new_session(), IDLE_MS / 1000, o.secure);

        match route {
            "register/options" => {
                let invite = body.get("invite").and_then(Value::as_str).map(String::from);
                if signed_in.is_none() && !Self::invite_valid(gate, invite.as_deref(), false) {
                    return error(StatusCode::FORBIDDEN, "invite");
                }
                let user = match self.state::<Option<String>>(USER) {
                    Some(u) => u,
                    None => {
                        let u = random(16);
                        self.set_state(USER, &u);
                        u
                    }
                };
                let challenge = random(32);
                let exclude: Vec<(String, Option<Vec<String>>)> = passkeys.iter().map(|p| (p.id.clone(), p.transports.clone())).collect();
                let options = passkey::registration_options(&o.rp_id, &user, &challenge, &exclude);
                send(StatusCode::OK, options, Some(hand_out(gate, challenge, invite)))
            }
            "register/verify" => {
                let Some(c) = challenge_for(gate) else { return error(StatusCode::BAD_REQUEST, "challenge") };
                if signed_in.is_none() && !Self::invite_valid(gate, c.invite.as_deref(), false) {
                    return error(StatusCode::FORBIDDEN, "invite");
                }
                let key = match passkey::verify_registration(&body["response"], &c.challenge, &o.origin, &o.rp_id) {
                    Ok(k) => k,
                    Err(e) => {
                        warn!("web: registration: {e}");
                        return error(StatusCode::BAD_REQUEST, "verify");
                    }
                };
                if c.invite.is_some() {
                    Self::invite_valid(gate, c.invite.as_deref(), true);
                }
                passkeys.push(Passkey {
                    id: key.id,
                    public_key: URL_SAFE_NO_PAD.encode(&key.public_key),
                    counter: key.counter,
                    transports: key.transports,
                    created: now_ms(),
                    device: device_of(headers),
                });
                self.set_state(PASSKEYS, &passkeys);
                send(StatusCode::OK, json!({ "ok": true }), Some(sign_in()))
            }
            "login/options" => {
                if passkeys.is_empty() {
                    return error(StatusCode::CONFLICT, "none");
                }
                let challenge = random(32);
                let options = passkey::authentication_options(&o.rp_id, &challenge);
                send(StatusCode::OK, options, Some(hand_out(gate, challenge, None)))
            }
            "login/verify" => {
                let c = challenge_for(gate);
                let id = body.get("id").and_then(Value::as_str);
                let key = passkeys.iter_mut().find(|p| Some(p.id.as_str()) == id);
                let (Some(c), Some(key)) = (c.as_ref(), key) else {
                    return error(StatusCode::BAD_REQUEST, if c.is_some() { "unknown" } else { "challenge" });
                };
                let public_key = URL_SAFE_NO_PAD.decode(key.public_key.trim_end_matches('=')).unwrap_or_default();
                match passkey::verify_authentication(&body, &c.challenge, &o.origin, &o.rp_id, &public_key, key.counter) {
                    Ok(counter) => key.counter = counter,
                    Err(e) => {
                        warn!("web: sign-in: {e}");
                        return error(StatusCode::BAD_REQUEST, "verify");
                    }
                }
                self.set_state(PASSKEYS, &passkeys);
                send(StatusCode::OK, json!({ "ok": true }), Some(sign_in()))
            }
            _ => error(StatusCode::NOT_FOUND, "route"),
        }
    }

    /// A tab's WebSocket: only the page itself, signed in; the browser says where from, and the cookie who.
    fn upgrade(self: &Arc<Self>, mut req: Request<Incoming>, o: &Origin) -> Response<Body> {
        let h = req.headers();
        if req.uri().path() != "/tab" || header(h, header::ORIGIN) != Some(o.origin.as_str()) || self.session(h).is_none() {
            let mut r = empty(StatusCode::UNAUTHORIZED);
            r.headers_mut().insert(header::CONNECTION, HeaderValue::from_static("close"));
            return r;
        }
        let websocket = header(h, header::UPGRADE).is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
        let version = header(h, header::SEC_WEBSOCKET_VERSION) == Some("13");
        let (true, true, Some(key)) = (websocket, version, h.get(header::SEC_WEBSOCKET_KEY)) else { return empty(StatusCode::BAD_REQUEST) };
        let accept = derive_accept_key(key.as_bytes());
        let upgraded = hyper::upgrade::on(&mut req);
        let web = self.clone();
        tokio::spawn(async move {
            match upgraded.await {
                Ok(io) => {
                    let config = WebSocketConfig::default().max_message_size(Some(1 << 20)).max_frame_size(Some(1 << 20));
                    let ws = WebSocketStream::from_raw_socket(TokioIo::new(io), Role::Server, Some(config)).await;
                    web.tab(ws).await;
                }
                Err(e) => warn!("web: upgrade: {e}"),
            }
        });
        Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header(header::SEC_WEBSOCKET_ACCEPT, accept)
            .body(full(Bytes::new()))
            .unwrap_or_else(|_| empty(StatusCode::INTERNAL_SERVER_ERROR))
    }

    /// A browser's tab: wassup itself (`--web-tab`), started once the page says its size, and its messages both
    /// ways, one per line: the page's to its stdin, checked first, and its own, from descriptor 3, as they come
    /// (webtab.rs). No Herdr nor tmux, whatever the server was started in.
    async fn tab(&self, ws: WebSocketStream<TokioIo<Upgraded>>) {
        let (mut sink, mut stream) = ws.split();
        let mut child: Option<Child> = None;
        let mut stdin: Option<ChildStdin> = None;
        let mut lines: Option<tokio::io::Lines<BufReader<tokio::net::unix::pipe::Receiver>>> = None;
        let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(30), Duration::from_secs(30));
        loop {
            tokio::select! {
                m = stream.next() => {
                    let text = match m {
                        Some(Ok(Message::Text(t))) => t.to_string(),
                        Some(Ok(Message::Binary(b))) => String::from_utf8_lossy(&b).into_owned(),
                        Some(Ok(_)) => continue,
                        _ => break,
                    };
                    let Some((line, size)) = from_page(&text) else { continue };
                    if child.is_none() && size {
                        match self.spawn_tab() {
                            Ok((c, i, l)) => (child, stdin, lines) = (Some(c), Some(i), Some(l)),
                            Err(e) => {
                                warn!("web: tab: {e}");
                                break;
                            }
                        }
                    }
                    if let Some(i) = stdin.as_mut() {
                        let _ = i.write_all(line.as_bytes()).await;
                    }
                }
                l = async {
                    match lines.as_mut() {
                        Some(l) => l.next_line().await,
                        None => std::future::pending().await,
                    }
                } => match l {
                    Ok(Some(l)) => {
                        if sink.send(Message::text(l)).await.is_err() {
                            break;
                        }
                    }
                    // Once its output is all read: the last thing it said may be to sign out.
                    _ => {
                        if let Some(c) = child.as_mut() {
                            let _ = tokio::time::timeout(Duration::from_secs(5), c.wait()).await;
                        }
                        let _ = sink.send(Message::Close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }))).await;
                        break;
                    }
                },
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                        break;
                    }
                }
            }
        }
        drop(stdin);
        if let Some(mut c) = child {
            // Its id is gone once it's reaped.
            if let Some(pid) = c.id() {
                // SAFETY: a signal to the tab's own process, which this task holds and hasn't reaped yet.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
                let _ = c.wait().await;
                self.tabs.lock().unwrap().remove(&pid);
            }
        }
        let _ = sink.close().await;
    }

    /// Starts a tab: its stdin for the page's messages, its stderr to the log, a pipe on its descriptor 3 for its own.
    fn spawn_tab(&self) -> Result<(Child, ChildStdin, tokio::io::Lines<BufReader<tokio::net::unix::pipe::Receiver>>)> {
        let exe = crate::exe();
        let (read, write) = std::io::pipe()?;
        let write = OwnedFd::from(write);
        let fd = write.as_raw_fd();
        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("--web-tab");
        if self.demo {
            cmd.arg("--demo");
        }
        cmd.env_clear()
            .envs(std::env::vars_os().filter(|(k, _)| {
                let k = k.to_string_lossy();
                !k.starts_with("HERDR_") && k != "TMUX"
            }))
            .env("TERM", "xterm-256color")
            .env("WA_HOME", &self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // SAFETY: between fork and exec only dup2 and fcntl, which are async-signal-safe; the pipe's end becomes the
        // child's descriptor 3, open across exec.
        unsafe {
            cmd.pre_exec(move || {
                let r = if fd == 3 { libc::fcntl(3, libc::F_SETFD, 0) } else { libc::dup2(fd, 3) };
                if r < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
            });
        }
        let mut child = cmd.spawn().context("--web-tab")?;
        drop(write);
        if let Some(pid) = child.id() {
            self.tabs.lock().unwrap().insert(pid);
        }
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        if let Some(err) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    warn!("web: tab: {l}");
                }
            });
        }
        let out = tokio::net::unix::pipe::Receiver::from_owned_fd(OwnedFd::from(read))?;
        Ok((child, stdin, BufReader::new(out).lines()))
    }

    /// The tabs ended before the server: each told to stop, and given a second.
    async fn close_tabs(&self) {
        let pids: Vec<u32> = self.tabs.lock().unwrap().iter().copied().collect();
        for pid in &pids {
            // SAFETY: a signal to a tab this server started.
            unsafe {
                libc::kill(*pid as libc::pid_t, libc::SIGTERM);
            }
        }
        for _ in 0..20 {
            if !pids.iter().any(|p| crate::ipc::alive(*p)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// WebAuthn as web.ts has it from simplewebauthn: the options for the browser, and the checks on what it answers.
/// Registration takes any attestation format without checking its statement: none is asked for, as what the
/// authenticator is doesn't matter, only that it keeps the key. Signing in checks the signature over the
/// authenticator's data and the client's, with the key kept at registration: ES256, EdDSA or RS256, as COSE.
mod passkey {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ring::signature::{self, UnparsedPublicKey};
    use serde_json::{Value, json};

    /// The algorithms offered, simplewebauthn's defaults: EdDSA, ES256, RS256.
    const ALGS: [i64; 3] = [-8, -7, -257];

    /// The options for navigator.credentials.create, as simplewebauthn makes them: a discoverable passkey, verified
    /// by the device, no attestation; the passkeys already registered left out, by id and how they're reached.
    pub fn registration_options(rp_id: &str, user: &str, challenge: &str, exclude: &[(String, Option<Vec<String>>)]) -> Value {
        let exclude: Vec<Value> = exclude
            .iter()
            .map(|(id, transports)| {
                let mut c = json!({ "id": id, "type": "public-key" });
                if let Some(t) = transports {
                    c["transports"] = json!(t);
                }
                c
            })
            .collect();
        json!({
            "challenge": challenge,
            "rp": { "name": "wassup", "id": rp_id },
            "user": { "id": user, "name": "wassup", "displayName": "" },
            "pubKeyCredParams": ALGS.iter().map(|a| json!({ "alg": a, "type": "public-key" })).collect::<Vec<_>>(),
            "timeout": 60000,
            "attestation": "none",
            "excludeCredentials": exclude,
            "authenticatorSelection": { "residentKey": "required", "userVerification": "required", "requireResidentKey": true },
            "extensions": { "credProps": true },
            "hints": [],
        })
    }

    /// The options for navigator.credentials.get: any passkey of this site, verified by the device.
    pub fn authentication_options(rp_id: &str, challenge: &str) -> Value {
        json!({ "rpId": rp_id, "challenge": challenge, "timeout": 60000, "userVerification": "required" })
    }

    /// A passkey just registered: its id, its public key (COSE), its counter, and how the browser says it's reached.
    pub struct Registered {
        pub id: String,
        pub public_key: Vec<u8>,
        pub counter: u32,
        pub transports: Option<Vec<String>>,
    }

    /// Checks what navigator.credentials.create gave (RegistrationResponseJSON) against the challenge handed out,
    /// the page's origin and the relying party.
    pub fn verify_registration(response: &Value, challenge: &str, origin: &str, rp_id: &str) -> Result<Registered, String> {
        let id = credential_id(response)?;
        let r = &response["response"];
        client_data(r, "webauthn.create", challenge, origin)?;
        let attestation = b64(r, "attestationObject")?;
        let object = Reader::new(&attestation).item(0)?;
        let fmt = match object.get(&Cbor::Text("fmt".into())) {
            Some(Cbor::Text(f)) => f.clone(),
            _ => return Err("attestation: no format".into()),
        };
        match object.get(&Cbor::Text("attStmt".into())) {
            Some(Cbor::Map(m)) if fmt != "none" || m.is_empty() => {}
            _ => return Err(format!("attestation: unexpected statement for {fmt:?}")),
        }
        let Some(Cbor::Bytes(auth)) = object.get(&Cbor::Text("authData".into())) else { return Err("attestation: no authenticator data".into()) };
        let data = AuthData::parse(auth, rp_id)?;
        let (cred_id, key) = data.attested.ok_or("no credential in the authenticator data")?;
        CoseKey::parse(key)?;
        if URL_SAFE_NO_PAD.encode(cred_id) != id {
            return Err("the credential's id differs from the response's".into());
        }
        let transports = r["transports"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect());
        Ok(Registered { id, public_key: key.to_vec(), counter: data.counter, transports })
    }

    /// Checks what navigator.credentials.get gave (AuthenticationResponseJSON) against the challenge handed out,
    /// the page's origin, the relying party, and the passkey's key and counter; the new counter.
    pub fn verify_authentication(response: &Value, challenge: &str, origin: &str, rp_id: &str, public_key: &[u8], counter: u32) -> Result<u32, String> {
        credential_id(response)?;
        let r = &response["response"];
        let client = client_data(r, "webauthn.get", challenge, origin)?;
        let auth = b64(r, "authenticatorData")?;
        let data = AuthData::parse(&auth, rp_id)?;
        let sig = b64(r, "signature")?;
        let mut signed = auth.clone();
        signed.extend_from_slice(ring::digest::digest(&ring::digest::SHA256, &client).as_ref());
        CoseKey::parse(public_key)?.verify(&signed, &sig)?;
        if (data.counter > 0 || counter > 0) && data.counter <= counter {
            return Err(format!("counter {} not above {counter}", data.counter));
        }
        Ok(data.counter)
    }

    /// The credential's id, the same as its raw id, of the type a passkey is.
    fn credential_id(response: &Value) -> Result<String, String> {
        let id = response["id"].as_str().filter(|i| !i.is_empty()).ok_or("no credential id")?;
        if response["rawId"].as_str() != Some(id) {
            return Err("the raw id differs from the id".into());
        }
        if response["type"] != "public-key" {
            return Err(format!("credential type {}", response["type"]));
        }
        Ok(id.to_string())
    }

    fn b64(r: &Value, field: &str) -> Result<Vec<u8>, String> {
        let s = r[field].as_str().ok_or_else(|| format!("no {field}"))?;
        URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).map_err(|e| format!("{field}: {e}"))
    }

    /// The client's data, checked: what it was for, the challenge, the origin, not from another site's frame. Its
    /// bytes, which the signature covers.
    fn client_data(r: &Value, kind: &str, challenge: &str, origin: &str) -> Result<Vec<u8>, String> {
        let raw = b64(r, "clientDataJSON")?;
        let c: Value = serde_json::from_slice(&raw).map_err(|e| format!("clientDataJSON: {e}"))?;
        if c["type"] != kind {
            return Err(format!("type {}, not {kind}", c["type"]));
        }
        if c["challenge"].as_str() != Some(challenge) {
            return Err("another challenge".into());
        }
        if c["origin"].as_str() != Some(origin) {
            return Err(format!("origin {}, not {origin}", c["origin"]));
        }
        if c["crossOrigin"] == true || !c["topOrigin"].is_null() {
            return Err("from another site's frame".into());
        }
        Ok(raw)
    }

    /// The authenticator's data: for this relying party, the user there and verified; its counter, and the
    /// credential it made (id and key) when it made one.
    struct AuthData<'a> {
        counter: u32,
        attested: Option<(&'a [u8], &'a [u8])>,
    }

    impl<'a> AuthData<'a> {
        fn parse(b: &'a [u8], rp_id: &str) -> Result<Self, String> {
            if b.len() < 37 {
                return Err("authenticator data too short".into());
            }
            if b[..32] != *ring::digest::digest(&ring::digest::SHA256, rp_id.as_bytes()).as_ref() {
                return Err("for another relying party".into());
            }
            let flags = b[32];
            if flags & 0x01 == 0 {
                return Err("user not present".into());
            }
            if flags & 0x04 == 0 {
                return Err("user not verified".into());
            }
            let counter = u32::from_be_bytes([b[33], b[34], b[35], b[36]]);
            let attested = if flags & 0x40 != 0 {
                if b.len() < 55 {
                    return Err("attested credential data too short".into());
                }
                let n = u16::from_be_bytes([b[53], b[54]]) as usize;
                let id = b.get(55..55 + n).ok_or("credential id too long")?;
                let mut reader = Reader::new(&b[55 + n..]);
                reader.item(0)?;
                Some((id, &b[55 + n..55 + n + reader.at]))
            } else {
                None
            };
            Ok(Self { counter, attested })
        }
    }

    /// A public key as COSE has it, of the kinds offered.
    enum CoseKey {
        Es256(Vec<u8>),
        Ed25519(Vec<u8>),
        Rs256 { n: Vec<u8>, e: Vec<u8> },
    }

    impl CoseKey {
        fn parse(bytes: &[u8]) -> Result<Self, String> {
            let key = Reader::new(bytes).item(0)?;
            let int = |k: i64| match key.get(&Cbor::Int(k)) {
                Some(Cbor::Int(v)) => Some(*v),
                _ => None,
            };
            let bytes = |k: i64| match key.get(&Cbor::Int(k)) {
                Some(Cbor::Bytes(v)) => Some(v.clone()),
                _ => None,
            };
            match (int(1), int(3)) {
                (Some(2), Some(-7)) => match (int(-1), bytes(-2), bytes(-3)) {
                    (Some(1), Some(x), Some(y)) if x.len() == 32 && y.len() == 32 => Ok(Self::Es256([&[4u8][..], &x, &y].concat())),
                    _ => Err("bad P-256 key".into()),
                },
                (Some(1), Some(-8)) => match (int(-1), bytes(-2)) {
                    (Some(6), Some(x)) if x.len() == 32 => Ok(Self::Ed25519(x)),
                    _ => Err("bad Ed25519 key".into()),
                },
                (Some(3), Some(-257)) => match (bytes(-1), bytes(-2)) {
                    (Some(n), Some(e)) => {
                        let strip = |v: Vec<u8>| v.iter().position(|b| *b != 0).map(|i| v[i..].to_vec()).unwrap_or_default();
                        Ok(Self::Rs256 { n: strip(n), e: strip(e) })
                    }
                    _ => Err("bad RSA key".into()),
                },
                (kty, alg) => Err(format!("unsupported key: kty {kty:?}, alg {alg:?}")),
            }
        }

        fn verify(&self, message: &[u8], sig: &[u8]) -> Result<(), String> {
            match self {
                Self::Es256(point) => UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, point).verify(message, sig),
                Self::Ed25519(x) => UnparsedPublicKey::new(&signature::ED25519, x).verify(message, sig),
                Self::Rs256 { n, e } => signature::RsaPublicKeyComponents { n, e }.verify(&signature::RSA_PKCS1_2048_8192_SHA256, message, sig),
            }
            .map_err(|_| "bad signature".to_string())
        }
    }

    /// CBOR, the little WebAuthn uses: integers, byte and text strings, arrays and maps of definite length; tags
    /// read through, floats and simple values read past.
    #[derive(Debug, PartialEq)]
    enum Cbor {
        Int(i64),
        Bytes(Vec<u8>),
        Text(String),
        Array(Vec<Cbor>),
        Map(Vec<(Cbor, Cbor)>),
        Simple,
    }

    impl Cbor {
        fn get(&self, key: &Cbor) -> Option<&Cbor> {
            match self {
                Cbor::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }
    }

    /// Reads CBOR items from a slice; `at` is how far it got.
    struct Reader<'a> {
        data: &'a [u8],
        at: usize,
    }

    impl<'a> Reader<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self { data, at: 0 }
        }

        fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
            let end = self.at.checked_add(n).filter(|e| *e <= self.data.len()).ok_or("CBOR cut short")?;
            let s = &self.data[self.at..end];
            self.at = end;
            Ok(s)
        }

        fn item(&mut self, depth: u8) -> Result<Cbor, String> {
            if depth > 16 {
                return Err("CBOR nested too deep".into());
            }
            let head = self.take(1)?[0];
            let (major, info) = (head >> 5, head & 31);
            let arg = match info {
                0..=23 => info as u64,
                24 => self.take(1)?[0] as u64,
                25 => u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as u64,
                26 => u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as u64,
                27 => u64::from_be_bytes(self.take(8)?.try_into().unwrap()),
                _ => return Err("CBOR of indefinite length".into()),
            };
            let len = |arg: u64, left: usize| usize::try_from(arg).ok().filter(|n| *n <= left).ok_or("CBOR length past its end");
            let left = self.data.len() - self.at;
            Ok(match major {
                0 => Cbor::Int(i64::try_from(arg).map_err(|_| "CBOR integer too big")?),
                1 => Cbor::Int(-1 - i64::try_from(arg).map_err(|_| "CBOR integer too big")?),
                2 => Cbor::Bytes(self.take(len(arg, left)?)?.to_vec()),
                3 => Cbor::Text(String::from_utf8(self.take(len(arg, left)?)?.to_vec()).map_err(|_| "CBOR text not UTF-8")?),
                4 => Cbor::Array((0..len(arg, left)?).map(|_| self.item(depth + 1)).collect::<Result<_, _>>()?),
                5 => Cbor::Map((0..len(arg, left)?).map(|_| Ok((self.item(depth + 1)?, self.item(depth + 1)?))).collect::<Result<_, String>>()?),
                6 => self.item(depth + 1)?,
                _ => Cbor::Simple,
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use ring::rand::SystemRandom;
        use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, Ed25519KeyPair, KeyPair};

        use super::*;

        const RP: &str = "wassup.example.org";
        const ORIGIN: &str = "https://wassup.example.org:8443";

        fn head(major: u8, n: u64, out: &mut Vec<u8>) {
            match n {
                0..=23 => out.push(major << 5 | n as u8),
                24..=255 => out.extend([major << 5 | 24, n as u8]),
                _ => {
                    out.push(major << 5 | 25);
                    out.extend((n as u16).to_be_bytes());
                }
            }
        }

        fn int(i: i64, out: &mut Vec<u8>) {
            if i >= 0 { head(0, i as u64, out) } else { head(1, (-1 - i) as u64, out) }
        }

        fn bytes(b: &[u8], out: &mut Vec<u8>) {
            head(2, b.len() as u64, out);
            out.extend(b);
        }

        fn text(s: &str, out: &mut Vec<u8>) {
            head(3, s.len() as u64, out);
            out.extend(s.as_bytes());
        }

        /// A software authenticator's key, ES256 or Ed25519, and its COSE form.
        enum Key {
            Ec(EcdsaKeyPair),
            Ed(Ed25519KeyPair),
        }

        impl Key {
            fn ec() -> Self {
                let rng = SystemRandom::new();
                let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
                Key::Ec(EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, doc.as_ref(), &rng).unwrap())
            }

            fn ed() -> Self {
                let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
                Key::Ed(Ed25519KeyPair::from_pkcs8(doc.as_ref()).unwrap())
            }

            fn cose(&self) -> Vec<u8> {
                let mut out = vec![];
                match self {
                    Key::Ec(k) => {
                        let p = k.public_key().as_ref();
                        head(5, 5, &mut out);
                        for (k, v) in [(1, 2), (3, -7), (-1, 1)] {
                            int(k, &mut out);
                            int(v, &mut out);
                        }
                        int(-2, &mut out);
                        bytes(&p[1..33], &mut out);
                        int(-3, &mut out);
                        bytes(&p[33..], &mut out);
                    }
                    Key::Ed(k) => {
                        head(5, 4, &mut out);
                        for (k, v) in [(1, 1), (3, -8), (-1, 6)] {
                            int(k, &mut out);
                            int(v, &mut out);
                        }
                        int(-2, &mut out);
                        bytes(k.public_key().as_ref(), &mut out);
                    }
                }
                out
            }

            fn sign(&self, msg: &[u8]) -> Vec<u8> {
                match self {
                    Key::Ec(k) => k.sign(&SystemRandom::new(), msg).unwrap().as_ref().to_vec(),
                    Key::Ed(k) => k.sign(msg).as_ref().to_vec(),
                }
            }
        }

        fn b64(b: &[u8]) -> String {
            URL_SAFE_NO_PAD.encode(b)
        }

        fn auth_data(rp: &str, flags: u8, counter: u32, attested: Option<(&[u8], &[u8])>) -> Vec<u8> {
            let mut d = ring::digest::digest(&ring::digest::SHA256, rp.as_bytes()).as_ref().to_vec();
            d.push(flags);
            d.extend(counter.to_be_bytes());
            if let Some((id, key)) = attested {
                d.extend([0u8; 16]);
                d.extend((id.len() as u16).to_be_bytes());
                d.extend(id);
                d.extend(key);
            }
            d
        }

        fn client(kind: &str, challenge: &str, origin: &str) -> Vec<u8> {
            json!({ "type": kind, "challenge": challenge, "origin": origin, "crossOrigin": false }).to_string().into_bytes()
        }

        fn registration(key: &Key, id: &[u8], challenge: &str, origin: &str, rp: &str, flags: u8) -> Value {
            let mut att = vec![];
            head(5, 3, &mut att);
            text("fmt", &mut att);
            text("none", &mut att);
            text("attStmt", &mut att);
            head(5, 0, &mut att);
            text("authData", &mut att);
            bytes(&auth_data(rp, flags, 0, Some((id, &key.cose()))), &mut att);
            json!({
                "id": b64(id), "rawId": b64(id), "type": "public-key",
                "response": { "clientDataJSON": b64(&client("webauthn.create", challenge, origin)), "attestationObject": b64(&att), "transports": ["internal", "hybrid"] },
            })
        }

        fn assertion(key: &Key, id: &[u8], challenge: &str, origin: &str, rp: &str, flags: u8, counter: u32) -> Value {
            let auth = auth_data(rp, flags, counter, None);
            let client = client("webauthn.get", challenge, origin);
            let mut signed = auth.clone();
            signed.extend(ring::digest::digest(&ring::digest::SHA256, &client).as_ref());
            json!({
                "id": b64(id), "rawId": b64(id), "type": "public-key",
                "response": { "clientDataJSON": b64(&client), "authenticatorData": b64(&auth), "signature": b64(&key.sign(&signed)), "userHandle": "dXNlcg" },
            })
        }

        #[test]
        fn registers_and_signs_in() {
            for key in [Key::ec(), Key::ed()] {
                let id = b"credencial-de-teste";
                let r = registration(&key, id, "Y2hhbGxlbmdl", ORIGIN, RP, 0x45);
                let reg = verify_registration(&r, "Y2hhbGxlbmdl", ORIGIN, RP).unwrap();
                assert_eq!(reg.id, b64(id));
                assert_eq!(reg.public_key, key.cose());
                assert_eq!(reg.transports, Some(vec!["internal".to_string(), "hybrid".to_string()]));
                let a = assertion(&key, id, "b3V0cm8", ORIGIN, RP, 0x05, 7);
                assert_eq!(verify_authentication(&a, "b3V0cm8", ORIGIN, RP, &reg.public_key, 0), Ok(7));
                // The same counter again, a replay or a cloned key.
                assert!(verify_authentication(&a, "b3V0cm8", ORIGIN, RP, &reg.public_key, 7).is_err());
                // Authenticators that keep no counter say 0 every time.
                let zero = assertion(&key, id, "b3V0cm8", ORIGIN, RP, 0x05, 0);
                assert_eq!(verify_authentication(&zero, "b3V0cm8", ORIGIN, RP, &reg.public_key, 0), Ok(0));
            }
        }

        #[test]
        fn refuses_what_doesnt_match() {
            let key = Key::ec();
            let id = b"credencial";
            let ok = registration(&key, id, "Y2g", ORIGIN, RP, 0x45);
            assert!(verify_registration(&ok, "b3V0cm8", ORIGIN, RP).is_err(), "another challenge");
            assert!(verify_registration(&ok, "Y2g", "https://outro.example.org", RP).is_err(), "another origin");
            assert!(verify_registration(&ok, "Y2g", ORIGIN, "outro.example.org").is_err(), "another relying party");
            assert!(verify_registration(&registration(&key, id, "Y2g", ORIGIN, RP, 0x41), "Y2g", ORIGIN, RP).is_err(), "not verified");
            let mut renamed = ok.clone();
            renamed["id"] = json!("b3V0cm8");
            renamed["rawId"] = json!("b3V0cm8");
            assert!(verify_registration(&renamed, "Y2g", ORIGIN, RP).is_err(), "another credential id");
            let cose = key.cose();
            let a = assertion(&key, id, "Y2g", ORIGIN, RP, 0x05, 1);
            assert!(verify_authentication(&a, "Y2g", ORIGIN, RP, &Key::ec().cose(), 0).is_err(), "another key");
            assert!(verify_authentication(&a, "Y2g", ORIGIN, "outro.example.org", &cose, 0).is_err(), "another relying party");
            assert!(verify_authentication(&assertion(&key, id, "Y2g", ORIGIN, RP, 0x01, 1), "Y2g", ORIGIN, RP, &cose, 0).is_err(), "not verified");
            let mut forged = a.clone();
            forged["response"]["authenticatorData"] = json!(b64(&auth_data(RP, 0x05, 9, None)));
            assert!(verify_authentication(&forged, "Y2g", ORIGIN, RP, &cose, 0).is_err(), "data changed after signing");
        }

        #[test]
        fn reads_cbor() {
            let mut b = vec![];
            head(5, 2, &mut b);
            int(-257, &mut b);
            text("ok", &mut b);
            int(3, &mut b);
            head(4, 1, &mut b);
            bytes(&[1, 2], &mut b);
            let mut r = Reader::new(&b);
            let m = r.item(0).unwrap();
            assert_eq!(r.at, b.len());
            assert_eq!(m.get(&Cbor::Int(-257)), Some(&Cbor::Text("ok".into())));
            assert_eq!(m.get(&Cbor::Int(3)), Some(&Cbor::Array(vec![Cbor::Bytes(vec![1, 2])])));
            assert!(Reader::new(&[0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).item(0).is_err(), "a length past the end");
            assert!(Reader::new(&[0x9f]).item(0).is_err(), "indefinite length");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_and_hosts() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("wa_s=abc; wa_c=def; x=a=b"));
        let c = cookies(&h);
        assert_eq!(c.get("wa_s").map(String::as_str), Some("abc"));
        assert_eq!(c.get("wa_c").map(String::as_str), Some("def"));
        assert!(!c.contains_key("x"));
        assert_eq!(split_host("LocalHost:8890"), ("localhost".into(), "8890".into()));
        assert_eq!(split_host("[::1]:8890"), ("[::1]".into(), "8890".into()));
        assert_eq!(split_host("localhost"), ("localhost".into(), String::new()));
    }

    #[test]
    fn media_stays_in_its_folder() {
        let media = Path::new("/srv/wa/media");
        assert_eq!(resolve(media, "a/b.jpg"), Path::new("/srv/wa/media/a/b.jpg"));
        assert_eq!(resolve(media, "../wassup.db"), Path::new("/srv/wa/wassup.db"));
        assert_eq!(resolve(media, "/etc/passwd"), Path::new("/etc/passwd"));
        assert_eq!(resolve(Path::new("/srv"), "wa/../wa2/./media"), Path::new("/srv/wa2/media"));
        assert_eq!(query_param("f=a%2Fb%20c.jpg&x=1", "f").as_deref(), Some("a/b c.jpg"));
        assert_eq!(query_param("x=1", "f"), None);
        assert_eq!(encode_uri_component("fotó (1).jpg"), "fot%C3%B3%20(1).jpg");
    }

    #[test]
    fn page_messages_checked() {
        let (line, size) = from_page(r#"{"t":"size","cols":1000.7,"rows":2.2}"#).unwrap();
        assert!(size && line.ends_with('\n'));
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), json!({ "t": "size", "cols": 500, "rows": 5 }));
        assert_eq!(from_page(r#"{"t":"in","d":"\u001b[A"}"#).map(|l| l.1), Some(false));
        assert_eq!(from_page(r#"{"t":"in","d":3}"#), None);
        assert_eq!(from_page(r#"{"t":"other"}"#), None);
        assert_eq!(from_page("not json"), None);
    }

    #[test]
    fn device_names() {
        let mut h = HeaderMap::new();
        h.insert(header::USER_AGENT, HeaderValue::from_static("Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0"));
        assert_eq!(device_of(&h), "Firefox · Linux");
    }
}
