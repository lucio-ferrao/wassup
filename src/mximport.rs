//! `wa matrix import-mine <chat> <from dd/mm/yyyy> <to dd/mm/yyyy>`: my own messages of a chat kept by wassup,
//! between two days, into a new encrypted Matrix room of mine, in my name and each at its own time.
//!
//! Matrix lets only an application service give an event its time (`ts`), and an application service has no keys
//! to encrypt with. So the messages are encrypted by wassup's own matrix-sdk session (my device, verified, its keys
//! in my backup), and its requests go through a proxy on this machine that changes only the sending: each message is
//! sent with the application service's token, as me, with the time its transaction id carries ("imp-<ms>-<n>"). The
//! application service is registered on the server beforehand, with my user in its namespace
//! (`matrix/import-as.json` keeps its token), and is meant to be removed afterwards. wassup's own process must be
//! stopped meanwhile: my session's stores are its.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use chrono::TimeZone;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use log::{info, warn};
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::encryption::{BackupDownloadStrategy, EncryptionSettings};
use matrix_sdk::ruma::api::client::room::create_room::v3::{Request as CreateRoom, RoomPreset};
use matrix_sdk::ruma::events::InitialStateEvent;
use matrix_sdk::ruma::events::room::encryption::RoomEncryptionEventContent;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::OwnedTransactionId;
use matrix_sdk::store::RoomLoadSettings;
use matrix_sdk::Client;
use serde_json::Value;

/// My session as mx.rs has it: the room's keys go to my backup as they're made.
fn settings() -> EncryptionSettings {
    EncryptionSettings { auto_enable_cross_signing: true, backup_download_strategy: BackupDownloadStrategy::AfterDecryptionFailure, auto_enable_backups: true }
}

async fn client(proxy: &str, store: &Path, session: MatrixSession) -> Result<Client> {
    let client = Client::builder().homeserver_url(proxy).sqlite_store(store, None).with_encryption_settings(settings()).build().await?;
    client.matrix_auth().restore_session(session, RoomLoadSettings::default()).await?;
    Ok(client)
}

/// The proxy: every request goes to the server as it came, but a message sent with an import's transaction id goes
/// with the application service's token, as the user whose token it came with, at the time in the id.
async fn proxy(server: String, as_token: String, users: HashMap<String, String>) -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let http = matrix_sdk::reqwest::Client::new();
    let shared = Arc::new((server, as_token, users, http));
    tokio::spawn(async move {
        while let Ok((io, _)) = listener.accept().await {
            let shared = shared.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |req| {
                    let shared = shared.clone();
                    async move {
                        Ok::<_, Infallible>(forward(&shared, req).await.unwrap_or_else(|e| {
                            warn!("import proxy: {e}");
                            Response::builder().status(StatusCode::BAD_GATEWAY).body(Full::new(Bytes::from(e.to_string()))).unwrap()
                        }))
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(io), service).await;
            });
        }
    });
    Ok(format!("http://{addr}"))
}

type Shared = (String, String, HashMap<String, String>, matrix_sdk::reqwest::Client);

async fn forward((server, as_token, users, http): &Shared, req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>> {
    let (parts, body) = req.into_parts();
    let body = body.collect().await?.to_bytes();
    let path = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let token = parts.headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    // …/rooms/{room}/send/{type}/imp-<ms>-<n>
    let at = path.contains("/send/").then(|| path.rsplit('/').next()).flatten().and_then(|txn| txn.strip_prefix("imp-")).and_then(|t| t.split('-').next()).and_then(|ms| ms.parse::<i64>().ok());
    let (url, auth) = match (at, users.get(token)) {
        (Some(ms), Some(user)) => {
            let sep = if path.contains('?') { '&' } else { '?' };
            (format!("{server}{path}{sep}user_id={}&ts={ms}", urlencode(user)), as_token.as_str())
        }
        _ => (format!("{server}{path}"), token),
    };
    let mut out = http.request(parts.method.clone(), url).body(body);
    if !auth.is_empty() {
        out = out.bearer_auth(auth);
    }
    if let Some(ct) = parts.headers.get("content-type") {
        out = out.header("content-type", ct.as_bytes());
    }
    let got = out.send().await?;
    let mut resp = Response::builder().status(got.status().as_u16());
    if let Some(ct) = got.headers().get("content-type") {
        resp = resp.header("content-type", ct.as_bytes());
    }
    Ok(resp.body(Full::new(got.bytes().await?))?)
}

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn day_start(d: &str) -> Result<i64> {
    let date = chrono::NaiveDate::parse_from_str(d, "%d/%m/%Y").map_err(|e| anyhow!("{d}: {e}"))?;
    Ok(chrono::Local.from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap_or_default()).earliest().context("date")?.timestamp_millis())
}

/// The import: a room of mine alone, named after the chat, with my messages of those days in order. Attachments go as
/// a mark of what they were, with their caption.
pub async fn import_mine(dir: &Path, chat: &str, from: &str, to: &str) -> Result<()> {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let mx = dir.join("matrix");
    let as_token: String = serde_json::from_slice::<Value>(&std::fs::read(mx.join("import-as.json")).context("matrix/import-as.json")?)?["as_token"].as_str().context("as_token")?.to_string();
    let mine: MatrixSession = serde_json::from_slice(&std::fs::read(mx.join("session.json")).context("no Matrix session: wa matrix login")?)?;
    let store = crate::store::Store::open(&dir.join("wassup.db"))?;
    let (start, end) = (day_start(from)? / 1000, day_start(to)? / 1000 + 86_400);
    let name = store.name_of(chat);
    let msgs: Vec<crate::store::Msg> = store.messages(chat, usize::MAX >> 1)?.into_iter().filter(|m| m.from_me && m.ts >= start && m.ts < end && m.kind != "deleted").collect();
    if msgs.is_empty() {
        bail!("no message of mine in {chat} between {from} and {to}");
    }
    println!("{} mensagens minhas na conversa com {name}, de {} a {}", msgs.len(), day(msgs[0].ts * 1000), day(msgs[msgs.len() - 1].ts * 1000));
    let found = Client::builder().server_name(mine.meta.user_id.server_name()).build().await?;
    let server = found.homeserver().to_string().trim_end_matches('/').to_string();
    let users = HashMap::from([(mine.tokens.access_token.clone(), mine.meta.user_id.to_string())]);
    let proxy = proxy(server, as_token, users).await?;
    let me = client(&proxy, &mx.join("store"), mine).await?;
    me.sync_once(SyncSettings::default()).await?;
    let mut create = CreateRoom::new();
    create.preset = Some(RoomPreset::PrivateChat);
    create.name = Some(format!("{name} (WhatsApp)"));
    create.initial_state = vec![InitialStateEvent::with_empty_state_key(RoomEncryptionEventContent::with_recommended_defaults()).to_raw_any()];
    let room = me.create_room(create).await?;
    me.sync_once(SyncSettings::default()).await?;
    println!("sala {}", room.room_id());
    for (n, m) in msgs.iter().enumerate() {
        let text = match m.kind.as_str() {
            "text" => m.text.clone(),
            kind if m.text.is_empty() => format!("[{kind}]"),
            kind => format!("[{kind}] {}", m.text),
        };
        // Seconds as kept; the n keeps two of one second in order.
        let txn: OwnedTransactionId = format!("imp-{}-{n}", m.ts * 1000 + (n as i64 % 1000)).into();
        room.send(RoomMessageEventContent::text_plain(&text)).with_transaction_id(txn).await.with_context(|| format!("message {n}"))?;
        if (n + 1) % 100 == 0 {
            println!("{} de {}", n + 1, msgs.len());
        }
    }
    me.sync_once(SyncSettings::default()).await?;
    info!("matrix import: {} of mine into {}", msgs.len(), room.room_id());
    println!("{} mensagens importadas para {}", msgs.len(), room.room_id());
    Ok(())
}

fn day(ms: i64) -> String {
    chrono::Local.timestamp_millis_opt(ms).single().map(|d| d.format("%d/%m/%Y").to_string()).unwrap_or_default()
}

