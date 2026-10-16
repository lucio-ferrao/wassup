//! What the screen says, in the user's language, as wassup's i18n.ts: Portuguese of Portugal or English, from
//! WA_LANG or else the locale (LC_ALL, LC_MESSAGES, LANG); anything starting with "pt" is Portuguese, the rest
//! English. Emoji names and the writing suggestions follow the same choice.

use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Pt,
    En,
}

/// The user's language, worked out once.
pub fn lang() -> Lang {
    static LANG: OnceLock<Lang> = OnceLock::new();
    *LANG.get_or_init(|| {
        let locale = ["WA_LANG", "LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|k| std::env::var(k).ok()).unwrap_or_default();
        if locale.to_lowercase().starts_with("pt") { Lang::Pt } else { Lang::En }
    })
}

/// Each string in both languages, by key.
fn strings(key: &str) -> (&'static str, &'static str) {
    match key {
        "help" => (
            "Tab muda de tab (com texto, aceita a sugestão) · / conversas · Ctrl+F procura (na lista, em todas) · Esc fecha · PgUp/PgDn histórico, Ctrl+↓ fim · ↑ ou clique selecciona mensagem, escrever responde, : reage, Delete apaga · :fixe: emoji",
            "Tab switches tabs (with text, accepts the suggestion) · / chats · Ctrl+F searches (in the list, all of them) · Esc closes · PgUp/PgDn history, Ctrl+↓ end · ↑ or click selects a message, typing replies, : reacts, Delete deletes · :thumbsup: emoji",
        ),
        "connecting" => ("a ligar…", "connecting…"),
        "waitingQr" => ("à espera do QR", "waiting for the QR"),
        "disconnected" => ("desligado", "disconnected"),
        "qrTitle" => ("Ligar o WhatsApp", "Link WhatsApp"),
        "qrHint" => ("No telemóvel: WhatsApp › Definições › Dispositivos associados › Associar dispositivo", "On the phone: WhatsApp › Settings › Linked devices › Link a device"),
        "me" => ("eu", "me"),
        "onlyOwnText" => ("só podes corrigir mensagens de texto tuas", "you can only edit your own text messages"),
        "onlyOwnEdit" => ("só podes editar mensagens tuas", "you can only edit your own messages"),
        "onlyOwnDelete" => ("só podes apagar para todos mensagens tuas", "you can only delete your own messages for everyone"),
        "messageGone" => ("a mensagem já não está aqui", "the message is no longer here"),
        "deleteForAll" => ("Delete outra vez apaga para todos · Esc cancela", "Delete again deletes for everyone · Esc cancels"),
        "deleteForMe" => ("Delete outra vez apaga para mim · Esc cancela", "Delete again deletes for me · Esc cancels"),
        "editTooLate" => ("já passaram 15 minutos: o WhatsApp não deixa editar", "more than 15 minutes ago: WhatsApp no longer lets it be edited"),
        "unknownMessage" => ("mensagem desconhecida", "unknown message"),
        "noConnection" => ("sem ligação ao WhatsApp; espera pelo ● verde", "no WhatsApp connection; wait for the green ●"),
        "reactionRemoved" => ("reacção retirada", "reaction removed"),
        "copied" => ("copiado", "copied"),
        "error" => ("erro", "error"),
        "openFirst" => ("abre primeiro uma conversa (\"/\")", "open a chat first (\"/\")"),
        "unknownCommand" => ("comando desconhecido", "unknown command"),
        "typingWho" => ("{0} a escrever", "{0} typing"),
        "noChatWith" => ("nenhuma conversa com \"{0}\"", "no chat matching \"{0}\""),
        "deleted" => ("mensagem apagada", "message deleted"),
        "edited" => ("(editada)", "(edited)"),
        "unavailable" => ("(indisponível)", "(unavailable)"),
        "mediaUnavailable" => ("[{0} indisponível]", "[{0} unavailable]"),
        "mediaUnreadable" => ("[{0} ilegível: {1}]", "[{0} unreadable: {1}]"),
        "attachmentExpired" => ("anexo indisponível (expirou no WhatsApp)", "attachment unavailable (expired on WhatsApp)"),
        "downloading" => ("a descarregar…", "downloading…"),
        "cannotOpen" => ("não consegui abrir ({0}): {1}", "could not open ({0}): {1}"),
        "file" => ("ficheiro", "file"),
        "voiceMessage" => ("mensagem de voz", "voice message"),
        "audio" => ("áudio", "audio"),
        "video" => ("vídeo", "video"),
        "image" => ("imagem", "image"),
        "gif" => ("gif", "gif"),
        "sticker" => ("sticker", "sticker"),
        "voice" => ("voz", "voice"),
        "location" => ("localização", "location"),
        "contact" => ("contacto", "contact"),
        "poll" => ("sondagem", "poll"),
        "editHeader" => ("✎ editar: {0} · Enter envia, Esc desiste", "✎ edit: {0} · Enter sends, Esc cancels"),
        "reactHeader" => ("reagir a {0}: {1} · :código: ou emoji e Enter", "react to {0}: {1} · :code: or emoji then Enter"),
        "today" => ("hoje", "today"),
        "yesterday" => ("ontem", "yesterday"),
        "months" => ("jan fev mar abr mai jun jul ago set out nov dez", "Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec"),
        "anotherInstance" => ("outra instância do wassup ligou-se com esta conta; fecha-a e reinicia este", "another wassup instance connected with this account; close it and restart this one"),
        "loggedOut" => ("sessão terminada no telemóvel; novo QR a caminho", "session ended on the phone; a new QR is coming"),
        "closedReconnecting" => ("ligação fechada ({0}), a religar", "connection closed ({0}), reconnecting"),
        "historyProgress" => ("histórico: {0} mensagens, {1} conversas{2}", "history: {0} messages, {1} chats{2}"),
        "historyDone" => ("histórico sincronizado", "history synced"),
        "notConnectedYet" => ("ainda sem ligação", "not connected yet"),
        "serverGone" => ("o processo servidor terminou; a assumir a ligação", "the server process ended; taking over the connection"),
        "noServer" => ("não consegui ligar-me ao servidor nem ser servidor", "could not connect to the server nor become one"),
        "serverEnded" => ("servidor terminou", "server ended"),
        "serverError" => ("erro no servidor", "server error"),
        "unknownOp" => ("operação desconhecida: {0}", "unknown operation: {0}"),
        "group" => ("grupo {0}", "group {0}"),
        "loading" => ("a carregar…", "loading…"),
        "describing" => ("a descrever…", "describing…"),
        "linkPreview" => ("pré-visualização", "preview"),
        "weekdays" => ("dom seg ter qua qui sex sáb", "Sun Mon Tue Wed Thu Fri Sat"),
        "chatsCount" => ("{0} conversas", "{0} chats"),
        "unreadCount" => ("{0} por ler", "{0} unread"),
        "groupMembers" => ("grupo · {0} membros", "group · {0} members"),
        "groupOnly" => ("grupo", "group"),
        "pinned" => ("📌 fixou uma mensagem", "📌 pinned a message"),
        "unpinned" => ("📌 desafixou uma mensagem", "📌 unpinned a message"),
        "album" => ("álbum", "album"),
        "videoNote" => ("mensagem de vídeo", "video message"),
        "waitingMessage" => ("à espera desta mensagem", "waiting for this message"),
        "groupInvite" => ("convite para um grupo", "group invite"),
        "businessMessage" => ("mensagem de empresa", "business message"),
        "unsupported" => ("mensagem não suportada", "unsupported message"),
        "messageNotFound" => ("mensagem não encontrada", "message not found"),
        "loadingOlder" => ("a carregar mensagens anteriores…", "loading earlier messages…"),
        "search" => ("procurar", "search"),
        "searchHeader" => ("{0} de {1} · ↑ mais antiga, ↓ mais recente · Enter fica nela · Esc fecha", "{0} of {1} · ↑ older, ↓ newer · Enter stays on it · Esc closes"),
        "searchNone" => ("sem resultados", "no results"),
        "webListening" => ("wassup na web em {0}", "wassup on the web at {0}"),
        "webInvite" => ("Registar uma passkey (uso único, 15 minutos): {0}", "Register a passkey (single use, 15 minutes): {0}"),
        "demo" => ("demonstração", "demonstration"),
        "connected" => ("ligado", "connected"),
        "deletedLine" => ("⊘ mensagem apagada", "⊘ message deleted"),
        "message" => ("mensagem", "message"),
        "onlyMine" => ("só texto meu", "only my own text"),
        _ => ("", ""),
    }
}

/// The string for `key` in the user's language, with `{0}`, `{1}`… replaced by `args`.
pub fn t(key: &str, args: &[&dyn std::fmt::Display]) -> String {
    let (pt, en) = strings(key);
    let mut s = if lang() == Lang::Pt { pt } else { en }.to_string();
    for (i, a) in args.iter().enumerate() {
        s = s.replace(&format!("{{{i}}}"), &a.to_string());
    }
    s
}

/// The string for `key` with no arguments.
pub fn s(key: &str) -> String {
    t(key, &[])
}
