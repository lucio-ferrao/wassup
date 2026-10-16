//! `wa --demo`: invented chats and pictures, with no WhatsApp behind, to try the screen and measure it; what's
//! asked of the connection goes straight into the store.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use image::{Rgb, RgbImage};
use tokio::sync::mpsc::UnboundedSender;

use crate::store::{self, Msg, Profile, Store};
use crate::wa::{Backend, ToUi};

/// A picture drawn here: a sky in `sky`, a sun and two hills, `w`×`h`; saved as JPEG at `path`, with a small copy
/// as the message's thumbnail.
fn picture(path: &Path, w: u32, h: u32, sky: [u8; 3]) -> Result<Vec<u8>> {
    let img = RgbImage::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let sun = ((fx - 0.72).powi(2) * (w * w) as f32 + (fy - 0.28).powi(2) * (h * h) as f32).sqrt() < 0.11 * w.min(h) as f32;
        let hill1 = fy > 1.0 - 0.6 * (1.0 - (fx - 0.33).abs() * 2.2).max(0.0);
        let hill2 = fy > 1.0 - 0.45 * (1.0 - (fx - 0.7).abs() * 2.0).max(0.0);
        if sun {
            Rgb([255, 204, 51])
        } else if hill1 {
            Rgb([34, 170, 85])
        } else if hill2 {
            Rgb([17, 136, 85])
        } else {
            Rgb(sky)
        }
    });
    img.save(path)?;
    let mut thumb = Vec::new();
    image::DynamicImage::ImageRgb8(img).thumbnail(64, 64).write_to(&mut std::io::Cursor::new(&mut thumb), image::ImageFormat::Jpeg)?;
    Ok(thumb)
}

pub fn fill(store: &Store, dir: &Path) -> Result<()> {
    let media = dir.join("media");
    std::fs::create_dir_all(&media)?;
    let now = chrono::Utc::now().timestamp();
    let rita = "351900000001@s.whatsapp.net";
    let clube = "120363000000000001@g.us";
    let me = "351900000099@s.whatsapp.net";
    store.set_state("me", me)?;
    store.set_state("meName", "Eu Mesmo")?;
    let lines = [
        "Olá! Já leste o capítulo novo?",
        "Ainda não, logo à noite.",
        "Combinamos sábado às 15h?",
        "Pode ser, levo bolo. E tu trazes o livro que me tinhas prometido há umas semanas, aquele das viagens.",
        "Ótimo 😀",
        "Vê esta página: https://exemplo.pt/livros/2026?utm_source=newsletter&utm_medium=email",
        "Não te esqueças do guarda-chuva ☔",
        "Já cheguei.",
    ];
    let landscape = picture(&media.join("demo-1.jpg"), 960, 640, [20, 68, 119])?;
    let portrait = picture(&media.join("demo-2.jpg"), 600, 1200, [113, 17, 68])?;
    picture(&media.join("demo-avatar.jpg"), 200, 200, [51, 170, 119])?;
    std::fs::write(media.join("programa-sabado.txt"), "Sábado: 15h, café e bolo; 16h, leitura.\n")?;
    let fetched = chrono::Utc::now().timestamp();
    store.set_profile(rita, &Profile { avatar: Some(media.join("demo-avatar.jpg").to_string_lossy().into_owned()), about: Some("A ler".into()), members: None, fetched })?;
    store.set_profile(clube, &Profile { avatar: None, about: None, members: Some(3), fetched })?;
    store.set_state("biz:351900000105@s.whatsapp.net", "true")?;
    store.set_state(&format!("mood:{rita}"), r#"{"emoji":"📚","upTo":0}"#)?;
    store.batch(|db| {
        Store::set_name(db, rita, "Rita Lemos", store::NAME_CONTACT)?;
        Store::touch_chat(db, clube, Some("Clube de Leitura"), 0)?;
        Store::set_name(db, "351900000002@s.whatsapp.net", "Tomás Rocha", store::NAME_CONTACT)?;
        Store::set_name(db, "351900000003@s.whatsapp.net", "Inês Fonseca", store::NAME_CONTACT)?;
        for i in 0..60i64 {
            let mine = i % 3 == 0;
            let m = Msg {
                chat: rita.into(),
                id: format!("R{i}"),
                from_me: mine,
                sender: if mine { me.into() } else { rita.into() },
                ts: now - 2 * 86400 + i * 2400,
                kind: "text".into(),
                text: format!("{} ({})", lines[i as usize % lines.len()], i + 1),
                status: if mine { 4 } else { 0 },
                ..Default::default()
            };
            Store::add_message(db, &m)?;
        }
        let img = |id: &str, file: &str, w, h, thumb: &Vec<u8>, text: &str, ts| Msg {
            chat: rita.into(),
            id: id.into(),
            sender: rita.into(),
            ts,
            kind: "image".into(),
            text: text.into(),
            thumb: Some(thumb.clone()),
            media: Some(vec![]),
            media_mime: Some("image/jpeg".into()),
            media_path: Some(media.join(file).to_string_lossy().into_owned()),
            w,
            h,
            ..Default::default()
        };
        Store::add_message(db, &img("RIMG1", "demo-1.jpg", 960, 640, &landscape, "O monte ao fim da tarde", now - 900))?;
        Store::add_message(db, &img("RIMG2", "demo-2.jpg", 600, 1200, &portrait, "", now - 600))?;
        // A little of everything wassup draws: a quote, a reaction, voice, a document, a location, one deleted, an
        // emoji on its own, markup, a pin, and mine read up to the last but one.
        let m = |id: &str, mine: bool, ts: i64, kind: &str, text: &str| Msg {
            chat: rita.into(),
            id: id.into(),
            from_me: mine,
            sender: if mine { me.into() } else { rita.into() },
            ts,
            kind: kind.into(),
            text: text.into(),
            status: if mine { 4 } else { 0 },
            ..Default::default()
        };
        Store::add_message(db, &Msg { quoted: Some(format!("{rita}\tPode ser, levo bolo.")), ..m("RQ", true, now - 560, "text", "Ainda bem, eu levo o café") })?;
        Store::set_reaction(db, rita, "RQ", rita, "❤️", 1)?;
        Store::add_message(db, &m("RM", false, now - 520, "text", "Lembra-te: *sábado* às _15h_, ~sexta~ não dá. Código da porta: `4521`"))?;
        Store::add_message(db, &Msg { media: Some(vec![]), media_mime: Some("audio/ogg".into()), ..m("RV", false, now - 400, "voice", "12s") })?;
        Store::add_message(db, &Msg {
            media: Some(vec![]),
            media_mime: Some("text/plain".into()),
            media_name: Some("programa-sabado.txt".into()),
            media_path: Some(media.join("programa-sabado.txt").to_string_lossy().into_owned()),
            ..m("RD", false, now - 350, "document", "")
        })?;
        Store::add_message(db, &m("RL", false, now - 330, "location", "Biblioteca · Rua das Flores · https://maps.google.com/?q=38.7,-9.1"))?;
        Store::add_message(db, &m("RX", false, now - 300, "deleted", ""))?;
        Store::add_message(db, &m("RP", false, now - 250, "pin", "RQ"))?;
        Store::add_message(db, &m("RE", false, now - 200, "text", "😀"))?;
        Store::set_reaction(db, rita, "RE", "me", "😂", 1)?;
        Store::add_message(db, &m("RF", false, now - 150, "text", "🎉🎂🎈🥳"))?;
        Store::add_message(db, &Msg { status: 3, ..m("RLAST", true, now - 60, "text", "Até já!") })?;
        for (k, (who, text)) in [
            ("351900000002@s.whatsapp.net", "Bom dia a todos"),
            ("351900000003@s.whatsapp.net", "Próxima leitura: um clássico"),
            ("351900000003@s.whatsapp.net", "@351900000002 trazes o livro? E @351900000777 também vem"),
        ]
        .into_iter()
        .enumerate()
        {
            Store::add_message(db, &Msg { chat: clube.into(), id: format!("C{k}"), sender: who.into(), ts: now - 3700 + 100 * k as i64, kind: "text".into(), text: text.into(), ..Default::default() })?;
        }
        Store::set_unread(db, clube, 3)?;
        let names = ["Duarte Matos", "Leonor Baptista", "Gaspar Pinto", "Beatriz Lobo", "Salvador Reis", "Matilde Seixas", "Vicente Amaral", "Carolina Teles", "Rodrigo Brito", "Alice Garcês"];
        for (i, n) in names.iter().enumerate() {
            let jid = format!("3519000001{i:02}@s.whatsapp.net");
            Store::set_name(db, &jid, n, store::NAME_CONTACT)?;
            Store::add_message(db, &Msg { chat: jid.clone(), id: format!("N{i}"), sender: jid, ts: now - 86400 * (i as i64 + 1), kind: "text".into(), text: format!("Mensagem de teste {}", i + 1), ..Default::default() })?;
        }
        Ok(())
    })
}

/// The demonstration's connection: what's asked goes into the store, and the screen is told.
pub struct Demo {
    store: Arc<Store>,
    tx: UnboundedSender<ToUi>,
}

impl Demo {
    pub fn new(store: Arc<Store>, tx: UnboundedSender<ToUi>) -> Self {
        Self { store, tx }
    }

    fn changed(&self, chat: &str) {
        let _ = self.tx.send(ToUi::Messages(chat.to_string()));
        let _ = self.tx.send(ToUi::Chats);
    }
}

impl Backend for Demo {
    fn send(&self, chat: &str, text: &str, reply_to: Option<&str>, _mentions: &[String]) {
        let me = self.store.state("me").unwrap_or_default();
        let quoted = reply_to.and_then(|id| self.store.message(chat, id)).map(|q| {
            let who = if q.from_me { me.clone() } else { q.sender.clone() };
            format!("{who}\t{}", q.text.lines().next().unwrap_or(""))
        });
        let m = Msg {
            chat: chat.into(),
            id: format!("demo{}", chrono::Utc::now().timestamp_micros()),
            from_me: true,
            sender: me,
            ts: chrono::Utc::now().timestamp(),
            kind: "text".into(),
            text: text.into(),
            status: 3,
            quoted,
            ..Default::default()
        };
        let _ = self.store.batch(|db| Store::add_message(db, &m));
        self.changed(chat);
    }

    fn react(&self, chat: &str, id: &str, emoji: &str) {
        let ts = chrono::Utc::now().timestamp_millis();
        let _ = self.store.batch(|db| Store::set_reaction(db, chat, id, "me", emoji, ts));
        self.changed(chat);
        let _ = self.tx.send(ToUi::Reaction(chat.into(), id.into(), "me".into(), emoji.into()));
    }

    fn edit(&self, chat: &str, id: &str, text: &str) {
        let _ = self.store.batch(|db| Store::edit(db, chat, id, "text", text));
        self.changed(chat);
    }

    fn delete(&self, chat: &str, id: &str, for_everyone: bool) {
        let _ = self.store.batch(|db| if for_everyone { Store::revoke(db, chat, id) } else { Store::forget(db, chat, id) });
        self.changed(chat);
    }

    fn fetch_older(&self, chat: &str) {
        let _ = self.tx.send(ToUi::Older(chat.into(), Some(0)));
    }

    fn mark_read(&self, chat: &str) {
        let _ = self.store.mark_read(chat);
        let _ = self.tx.send(ToUi::Chats);
    }

    fn ensure_media(&self, _chat: &str, _id: &str) {}
    fn ensure_preview(&self, _chat: &str, _id: &str) {}
    fn ensure_profile(&self, _jid: &str) {}
    fn ensure_name(&self, _jid: &str) {}
    fn set_composing(&self, _chat: &str, _on: bool) {}
    fn subscribe_presence(&self, chat: &str) {
        if chat == "351900000001@s.whatsapp.net" {
            let _ = self.tx.send(ToUi::Presence(chat.into(), true));
        }
    }
    fn touch_presence(&self, _pid: u32) {
        let _ = self.tx.send(ToUi::Available(true));
    }
    fn set_focus(&self, _pid: u32, _focused: bool) {}
    fn send_file(&self, _chat: &str, _path: &str, _caption: &str) {}
    fn download_all(&self, _chat: &str) {}
}
