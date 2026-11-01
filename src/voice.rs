//! Voice messages read: each one, once its file is here, goes through Voxtype (`voxtype transcribe`, Whisper on the
//! GPU, with the model and language of its own configuration), and what it heard is kept with the message, so it's
//! never asked again. Voxtype takes WAV at 16 kHz, mono: ffmpeg makes it, beside the attachment, and it's deleted
//! after. With no Voxtype or no ffmpeg, voice messages simply stay unread.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use tokio::process::Command;

/// What Voxtype heard in an audio file; empty when it heard nothing.
pub async fn transcribe(audio: &Path) -> Result<String> {
    let wav = audio.with_extension("transcribe.wav");
    let converted = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-i"])
        .arg(audio)
        .args(["-ar", "16000", "-ac", "1"])
        .arg(&wav)
        .status()
        .await
        .context("ffmpeg")?;
    if !converted.success() {
        let _ = std::fs::remove_file(&wav);
        return Err(anyhow!("ffmpeg: {converted}"));
    }
    let out = Command::new("voxtype").arg("-q").arg("transcribe").arg(&wav).output().await.context("voxtype");
    let _ = std::fs::remove_file(&wav);
    let out = out?;
    if !out.status.success() {
        return Err(anyhow!("voxtype: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(heard(&String::from_utf8_lossy(&out.stdout)))
}

/// The text in what `voxtype transcribe` prints: "Processing N samples…", a blank line, then what it heard.
fn heard(stdout: &str) -> String {
    let after = stdout.split_once("\n\n").map(|(_, rest)| rest).unwrap_or(stdout);
    after.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heard_is_what_follows_the_blank_line() {
        assert_eq!(heard("Processing 53760 samples (3.36s)...\n\nAmanhã levo o bolo.\n"), "Amanhã levo o bolo.");
        assert_eq!(heard("Processing 1 samples (0.00s)...\n\n\n"), "");
        assert_eq!(heard("Só isto\n"), "Só isto");
    }
}
