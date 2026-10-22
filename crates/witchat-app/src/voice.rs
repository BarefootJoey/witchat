//! In-app voice-note playback through the Windows media stack, which decodes
//! the AAC `.m4a` notes BitChat records without bundling a codec.

use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::time::Duration;
use std::time::Instant;

/// Plays one note at a time; playing another (or the same) one stops it.
#[derive(Default)]
pub struct VoicePlayer {
    playing: Option<(PathBuf, Instant)>,
    #[cfg(windows)]
    player: Option<windows::Media::Playback::MediaPlayer>,
}

impl VoicePlayer {
    pub fn playing(&self) -> Option<&Path> {
        self.playing.as_ref().map(|(p, _)| p.as_path())
    }

    /// Start `path`, or stop it if it is the note already playing.
    pub fn toggle(&mut self, path: &Path) -> Result<(), String> {
        if self.playing() == Some(path) {
            self.stop();
            return Ok(());
        }
        self.stop();
        self.start(path)?;
        self.playing = Some((path.to_path_buf(), Instant::now()));
        Ok(())
    }

    #[cfg(windows)]
    pub fn stop(&mut self) {
        if let Some(player) = &self.player {
            if let Err(err) = player.Pause() {
                tracing::debug!(%err, "voice: pause failed");
            }
        }
        self.playing = None;
    }

    #[cfg(not(windows))]
    pub fn stop(&mut self) {
        self.playing = None;
    }

    /// Forget a note once it has finished; call on the UI tick.
    #[cfg(windows)]
    pub fn poll(&mut self) {
        use windows::Media::Playback::MediaPlaybackState;
        let Some((_, started)) = &self.playing else {
            return;
        };
        // The session reports Opening/Buffering briefly before Playing.
        if started.elapsed() < Duration::from_secs(1) {
            return;
        }
        let state = self.player.as_ref().and_then(|p| p.PlaybackSession().ok()).and_then(|s| s.PlaybackState().ok());
        if !matches!(
            state,
            Some(MediaPlaybackState::Playing | MediaPlaybackState::Opening | MediaPlaybackState::Buffering)
        ) {
            self.playing = None;
        }
    }

    #[cfg(not(windows))]
    pub fn poll(&mut self) {}

    pub fn is_playing(&self) -> bool {
        self.playing.is_some()
    }

    #[cfg(windows)]
    fn start(&mut self, path: &Path) -> Result<(), String> {
        use windows::core::HSTRING;
        use windows::Foundation::Uri;
        use windows::Media::Core::MediaSource;
        use windows::Media::Playback::MediaPlayer;
        let result = (|| -> windows::core::Result<MediaPlayer> {
            let player = match &self.player {
                Some(player) => player.clone(),
                None => MediaPlayer::new()?,
            };
            let uri = Uri::CreateUri(&HSTRING::from(file_uri(path)))?;
            player.SetSource(&MediaSource::CreateFromUri(&uri)?)?;
            player.Play()?;
            Ok(player)
        })();
        match result {
            Ok(player) => {
                self.player = Some(player);
                Ok(())
            }
            Err(err) => Err(format!("could not play {}: {err}", path.display())),
        }
    }

    #[cfg(not(windows))]
    fn start(&mut self, _path: &Path) -> Result<(), String> {
        Err("in-app playback is Windows-only; click open instead".into())
    }
}

/// Length of an MP4/M4A from its movie header (`moov` > `mvhd`), as iOS
/// shows next to the play button. `None` for anything else.
pub fn m4a_duration_ms(bytes: &[u8]) -> Option<u64> {
    let moov = find_box(bytes, b"moov")?;
    let mvhd = find_box(moov, b"mvhd")?;
    let version = *mvhd.first()?;
    let (scale_at, duration_len) = if version == 1 { (20, 8) } else { (12, 4) };
    let timescale = u32::from_be_bytes(mvhd.get(scale_at..scale_at + 4)?.try_into().ok()?) as u64;
    let raw = mvhd.get(scale_at + 4..scale_at + 4 + duration_len)?;
    let duration = if duration_len == 8 {
        u64::from_be_bytes(raw.try_into().ok()?)
    } else {
        u32::from_be_bytes(raw.try_into().ok()?) as u64
    };
    (timescale > 0).then(|| duration.saturating_mul(1000) / timescale)
}

/// Body of the first top-level box of `kind` in `data`.
fn find_box<'a>(mut data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    while data.len() >= 8 {
        let size32 = u32::from_be_bytes(data[..4].try_into().ok()?) as usize;
        let (header, size) = match size32 {
            0 => (8, data.len()),
            1 => (16, u64::from_be_bytes(data.get(8..16)?.try_into().ok()?) as usize),
            n => (8, n),
        };
        if size < header || size > data.len() {
            return None;
        }
        if &data[4..8] == kind {
            return Some(&data[header..size]);
        }
        data = &data[size..];
    }
    None
}

/// `0:07`, `1:05`.
pub fn format_duration(ms: u64) -> String {
    let secs = ((ms + 500) / 1000).max(1);
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// `file:///C:/Users/a%20b/voice.m4a`, percent-encoding anything that is not
/// an unreserved URI character or a path separator.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn file_uri(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file:///");
    for byte in raw.trim_start_matches('/').bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_escape_spaces_and_unicode() {
        assert_eq!(
            file_uri(Path::new("C:\\Users\\Jane Doe\\Downloads\\witchat\\voice_1.m4a")),
            "file:///C:/Users/Jane%20Doe/Downloads/witchat/voice_1.m4a"
        );
        assert_eq!(file_uri(Path::new("C:\\é#.m4a")), "file:///C:/%C3%A9%23.m4a");
    }

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((8 + body.len()) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn durations_come_from_the_movie_header() {
        let mut mvhd = vec![0u8; 4 + 8];
        mvhd.extend_from_slice(&44_100u32.to_be_bytes());
        mvhd.extend_from_slice(&(44_100u32 * 7 + 100).to_be_bytes());
        mvhd.resize(100, 0);
        let mut file = mp4_box(b"ftyp", b"M4A isom");
        file.extend(mp4_box(b"moov", &[mp4_box(b"trak", &[1, 2]), mp4_box(b"mvhd", &mvhd)].concat()));
        file.extend(mp4_box(b"mdat", &[0; 32]));
        let ms = m4a_duration_ms(&file).unwrap();
        assert_eq!(ms, 7002);
        assert_eq!(format_duration(ms), "0:07");
        assert_eq!(format_duration(200), "0:01", "never shows 0:00");
        assert_eq!(format_duration(65_000), "1:05");
        assert!(m4a_duration_ms(b"not an mp4 at all").is_none());
        let mut truncated = file.clone();
        truncated.truncate(30);
        assert!(m4a_duration_ms(&truncated).is_none());
    }

    /// Plays half a second of silence through the real Windows media stack.
    #[cfg(windows)]
    #[test]
    fn windows_media_player_accepts_a_wav() {
        let path = std::env::temp_dir().join(format!("witchat voice test {}.wav", std::process::id()));
        std::fs::write(&path, silent_wav(8000, 4000)).unwrap();
        let mut player = VoicePlayer::default();
        player.toggle(&path).expect("playback starts");
        assert_eq!(player.playing(), Some(path.as_path()));
        player.toggle(&path).unwrap();
        assert!(player.playing().is_none(), "second click stops");
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(windows)]
    fn silent_wav(rate: u32, samples: u32) -> Vec<u8> {
        let data = samples * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data.to_le_bytes());
        wav.resize(wav.len() + data as usize, 0);
        wav
    }
}
