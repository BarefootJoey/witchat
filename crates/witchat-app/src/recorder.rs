//! Voice-note recording through Windows `MediaCapture`: AAC in an `.m4a`,
//! the format BitChat phones record and play. Capture runs on its own thread
//! so the UI never blocks on the microphone or the encoder.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// iOS caps notes at 120 s at 16 kbps; Windows' AAC encoder runs at 96 kbps
/// or more (~14 KB/s), and BLE moves roughly that much per second, so 30 s
/// keeps a note around 400 KB and well under a minute of airtime.
pub const MAX_RECORDING: Duration = Duration::from_secs(30);
/// Shorter than this is a mis-click, not a message.
const MIN_RECORDING: Duration = Duration::from_millis(700);

#[derive(Debug)]
pub enum RecorderEvent {
    Finished(PathBuf),
    Discarded,
    Failed(String),
}

#[derive(Default)]
pub struct VoiceRecorder {
    active: Option<Active>,
}

struct Active {
    started: Instant,
    stop: mpsc::Sender<bool>,
    done: mpsc::Receiver<RecorderEvent>,
}

impl VoiceRecorder {
    pub fn elapsed(&self) -> Option<Duration> {
        self.active.as_ref().map(|a| a.started.elapsed())
    }

    /// Start recording into `path` (an iOS-style `voice_<UUID>.m4a`).
    pub fn start(&mut self, path: PathBuf) {
        if self.active.is_some() {
            return;
        }
        let (stop_tx, stop_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("witchat-recorder".into()).spawn(move || {
            let _ = done_tx.send(record(&path, stop_rx));
        });
        if let Err(err) = spawned {
            tracing::warn!(%err, "voice: recorder thread did not start");
            return;
        }
        self.active = Some(Active { started: Instant::now(), stop: stop_tx, done: done_rx });
    }

    /// Stop; `keep` sends the note, otherwise it is deleted.
    pub fn finish(&mut self, keep: bool) {
        if let Some(active) = &self.active {
            let _ = active.stop.send(keep);
        }
    }

    /// The outcome once the recorder thread is done; call on the UI tick.
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub fn poll(&mut self) -> Option<RecorderEvent> {
        let event = self.active.as_ref()?.done.try_recv();
        match event {
            Ok(event) => {
                self.active = None;
                Some(event)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.active = None;
                Some(RecorderEvent::Failed("the recorder stopped unexpectedly".into()))
            }
        }
    }
}

/// `voice_<UUID>.m4a`: the name shape iOS uses, which also gives the
/// receiver a stable id for delivered/read receipts.
pub fn note_name() -> String {
    format!("voice_{}.m4a", uuid::Uuid::new_v4().to_string().to_uppercase())
}

fn record(path: &Path, stop: mpsc::Receiver<bool>) -> RecorderEvent {
    let started = Instant::now();
    let capture = match platform::begin(path) {
        Ok(capture) => capture,
        Err(err) => {
            let _ = std::fs::remove_file(path);
            return RecorderEvent::Failed(err);
        }
    };
    let keep = match stop.recv_timeout(MAX_RECORDING) {
        Ok(keep) => keep,
        Err(mpsc::RecvTimeoutError::Timeout) => true,
        Err(mpsc::RecvTimeoutError::Disconnected) => false,
    };
    let stopped = platform::end(capture);
    let too_short = started.elapsed() < MIN_RECORDING;
    if let Err(err) = stopped {
        let _ = std::fs::remove_file(path);
        return RecorderEvent::Failed(err);
    }
    if !keep || too_short {
        let _ = std::fs::remove_file(path);
        return RecorderEvent::Discarded;
    }
    RecorderEvent::Finished(path.to_path_buf())
}

#[cfg(windows)]
mod platform {
    use std::path::Path;
    use windows::core::HSTRING;
    use windows::Media::Capture::{
        MediaCapture, MediaCaptureInitializationSettings, MediaCategory, StreamingCaptureMode,
    };
    use windows::Media::MediaProperties::{AudioEncodingQuality, MediaEncodingProfile};
    use windows::Storage::{CreationCollisionOption, StorageFolder};

    pub(super) fn begin(path: &Path) -> Result<MediaCapture, String> {
        start(path).map_err(|err| {
            // E_ACCESSDENIED: Windows privacy settings block desktop apps.
            if err.code().0 as u32 == 0x8007_0005 {
                "microphone blocked: Settings > Privacy & security > Microphone > \
                 allow desktop apps to access your microphone"
                    .to_string()
            } else {
                format!("could not start recording: {err}")
            }
        })
    }

    fn start(path: &Path) -> windows::core::Result<MediaCapture> {
        // The target first: a bad path should fail before the mic turns on.
        let dir = path.parent().unwrap_or(Path::new("."));
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let folder = StorageFolder::GetFolderFromPathAsync(&HSTRING::from(dir.to_string_lossy().as_ref()))?.get()?;
        let file = folder.CreateFileAsync(&HSTRING::from(name), CreationCollisionOption::ReplaceExisting)?.get()?;
        let settings = MediaCaptureInitializationSettings::new()?;
        settings.SetStreamingCaptureMode(StreamingCaptureMode::Audio)?;
        settings.SetMediaCategory(MediaCategory::Speech)?;
        let capture = MediaCapture::new()?;
        capture.InitializeWithSettingsAsync(&settings)?.get()?;
        let profile = MediaEncodingProfile::CreateM4a(AudioEncodingQuality::Low)?;
        if let Ok(audio) = profile.Audio() {
            audio.SetChannelCount(1)?;
        }
        capture.StartRecordToStorageFileAsync(&profile, &file)?.get()?;
        Ok(capture)
    }

    pub(super) fn end(capture: MediaCapture) -> Result<(), String> {
        let result = capture.StopRecordAsync().and_then(|op| op.get());
        let _ = capture.Close();
        result.map_err(|err| format!("could not finish recording: {err}"))
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::Path;

    pub(super) struct Unsupported;

    pub(super) fn begin(_path: &Path) -> Result<Unsupported, String> {
        Err("voice recording is Windows-only for now".into())
    }

    pub(super) fn end(_capture: Unsupported) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_get_receipt_eligible_names() {
        let name = note_name();
        let a = witchat_protocol::PeerId::from_hex("0011223344556677");
        let b = witchat_protocol::PeerId::from_hex("aabbccddeeff0011");
        assert!(witchat_core::files::stable_media_id(&a, &b, Some(&name)).is_some(), "{name}");
    }

    /// Uses the real microphone: `cargo test -p witchat-app -- --ignored recorder`.
    #[test]
    #[ignore]
    fn records_a_playable_m4a_from_the_microphone() {
        let path = std::env::temp_dir().join(note_name());
        let mut recorder = VoiceRecorder::default();
        recorder.start(path.clone());
        std::thread::sleep(Duration::from_secs(2));
        recorder.finish(true);
        let event = (0..100).find_map(|_| {
            std::thread::sleep(Duration::from_millis(100));
            recorder.poll()
        });
        let Some(RecorderEvent::Finished(saved)) = event else {
            panic!("{event:?}");
        };
        let bytes = std::fs::read(&saved).unwrap();
        let _ = std::fs::remove_file(&saved);
        let ms = crate::voice::m4a_duration_ms(&bytes).expect("an mp4 movie header");
        let file = witchat_protocol::FilePacket {
            name: Some(note_name()),
            mime: Some("audio/m4a".into()),
            content: bytes.clone(),
        };
        assert!(file.validate().is_some(), "passes the BitChat MIME/magic check");
        println!("recorded {} bytes, {ms} ms", bytes.len());
        assert!((1500..3000).contains(&ms), "{ms}");
    }

    #[test]
    fn a_failed_start_reports_and_cleans_up() {
        let mut recorder = VoiceRecorder::default();
        let path = std::env::temp_dir().join("witchat-no-such-dir").join("x").join(note_name());
        recorder.start(path.clone());
        assert!(recorder.elapsed().is_some());
        let event = (0..100).find_map(|_| {
            std::thread::sleep(Duration::from_millis(100));
            recorder.poll()
        });
        assert!(matches!(event, Some(RecorderEvent::Failed(_))), "{event:?}");
        assert!(recorder.elapsed().is_none() && !path.exists());
    }
}
