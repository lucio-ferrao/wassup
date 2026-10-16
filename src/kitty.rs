//! Kitty's graphics protocol as the Node version's image.ts uses it (KittyImages): each picture sent to the terminal once, by
//! an id of its own, and at each frame only placed over its cells, cut to the part in view; the oldest let go past
//! sixty. Placing is cheap, so scrolling over pictures sends no pixels.

use std::collections::HashMap;
use std::io::Write;

use base64::Engine;
use image::DynamicImage;

pub struct Kitty {
    ids: HashMap<String, (u32, u32, u32)>,
    order: Vec<String>,
    next: u32,
}

impl Kitty {
    pub fn new() -> Self {
        Self { ids: HashMap::new(), order: vec![], next: 1 }
    }

    /// Sends a picture as PNG, at most 800 pixels wide, in chunks; returns its id and the size it was sent at.
    fn transmit(&mut self, out: &mut impl Write, key: &str, img: &DynamicImage) -> (u32, u32, u32) {
        let id = self.next;
        self.next += 1;
        let img = if img.width() > 800 { img.resize(800, u32::MAX, image::imageops::FilterType::Triangle) } else { img.clone() };
        let mut png = Vec::new();
        let _ = img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let chunks: Vec<&[u8]> = b64.as_bytes().chunks(4096).collect();
        for (i, c) in chunks.iter().enumerate() {
            let more = if i + 1 < chunks.len() { 1 } else { 0 };
            let head = if i == 0 { format!("a=t,f=100,i={id},q=2,m={more}") } else { format!("m={more}") };
            let _ = write!(out, "\x1b_G{head};{}\x1b\\", std::str::from_utf8(c).unwrap_or(""));
        }
        let entry = (id, img.width(), img.height());
        self.ids.insert(key.to_string(), entry);
        self.order.push(key.to_string());
        if self.order.len() > 60 {
            let old = self.order.remove(0);
            if let Some((old_id, ..)) = self.ids.remove(&old) {
                let _ = write!(out, "\x1b_Ga=d,d=I,i={old_id},q=2\x1b\\");
            }
        }
        entry
    }

    /// Takes every placement off the screen (the pictures stay in the terminal), before a frame's are placed.
    pub fn clear(&self, out: &mut impl Write) {
        let _ = write!(out, "\x1b_Ga=d,d=a,q=2\x1b\\");
    }

    /// Places a picture from cell (x, y), 0-based, over `cols`×`rows` cells, the part from `from` to `to` (0 to 1)
    /// of its height in view.
    #[allow(clippy::too_many_arguments)]
    pub fn place(&mut self, out: &mut impl Write, key: &str, img: &DynamicImage, x: u16, y: u16, cols: u16, rows: u16, from: f32, to: f32) {
        let (id, pw, ph) = match self.ids.get(key) {
            Some(e) => *e,
            None => self.transmit(out, key, img),
        };
        let top = (from * ph as f32).floor() as u32;
        let h = (((to - from) * ph as f32).floor() as u32).max(1);
        let _ = write!(out, "\x1b7\x1b[{};{}H\x1b_Ga=p,i={id},c={cols},r={rows},x=0,y={top},w={pw},h={h},C=1,q=2\x1b\\\x1b8", y + 1, x + 1);
    }

    /// Lets every picture go, on leaving.
    pub fn dispose(&mut self, out: &mut impl Write) {
        let _ = write!(out, "\x1b_Ga=d,d=A,q=2\x1b\\");
        self.ids.clear();
        self.order.clear();
    }
}
