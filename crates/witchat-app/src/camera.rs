//! Webcam QR scanning: a `MediaCapture` frame reader on its own thread
//! hands each frame to rqrr and publishes a small mirrored preview. Frames
//! stay in memory and are dropped as soon as they are read.
//! Linux has no camera backend yet; there the frame helpers serve tests only.
#![cfg_attr(not(windows), allow(dead_code))]

use crate::qr_image::decode_qr_gray;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Shown when Windows privacy settings block the camera.
pub const DENIED: &str = "camera access denied. enable it in settings to scan QR codes.";
const PREVIEW_WIDTH: u32 = 320;
/// rqrr is quick enough below this; larger frames are halved first.
const DECODE_MAX_WIDTH: u32 = 960;

pub struct Preview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Default)]
struct Shared {
    preview: Option<Preview>,
    found: Option<String>,
    error: Option<String>,
}

/// Runs until dropped, a QR is found, or the camera fails.
pub struct CameraScanner {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
}

pub enum CameraUpdate {
    Preview(Preview),
    Found(String),
    Failed(String),
}

impl CameraScanner {
    pub fn start() -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st) = (shared.clone(), stop.clone());
        std::thread::spawn(move || {
            if let Err(err) = platform::run(&s, &st) {
                tracing::warn!(%err, "camera: scan stopped");
                if let Ok(mut shared) = s.lock() {
                    shared.error = Some(err);
                }
            }
        });
        tracing::info!("camera: scanning for a QR");
        Self { shared, stop }
    }

    /// The newest result, most decisive first.
    pub fn poll(&self) -> Option<CameraUpdate> {
        let mut shared = self.shared.lock().ok()?;
        if let Some(url) = shared.found.take() {
            return Some(CameraUpdate::Found(url));
        }
        if let Some(err) = shared.error.take() {
            return Some(CameraUpdate::Failed(err));
        }
        shared.preview.take().map(CameraUpdate::Preview)
    }
}

impl Drop for CameraScanner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// One BGRA frame: try to read a QR, and publish the preview. Returns true
/// once a QR is found.
fn handle_frame(shared: &Mutex<Shared>, width: u32, height: u32, stride: usize, bgra: &[u8]) -> bool {
    let step = if width > DECODE_MAX_WIDTH { 2 } else { 1 };
    let (gw, gh, gray) = luma(width, height, stride, bgra, step);
    let found = decode_qr_gray(gw as usize, gh as usize, &gray).ok();
    let preview = mirrored_preview(width, height, stride, bgra, PREVIEW_WIDTH);
    let Ok(mut shared) = shared.lock() else {
        return true;
    };
    shared.preview = Some(preview);
    if let Some(url) = found {
        shared.found = Some(url);
        return true;
    }
    false
}

/// Grayscale, sampling every `step`th pixel each way.
fn luma(width: u32, height: u32, stride: usize, bgra: &[u8], step: u32) -> (u32, u32, Vec<u8>) {
    let (w, h) = (width / step, height / step);
    let mut gray = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        let row = (y * step) as usize * stride;
        for x in 0..w {
            let i = row + (x * step) as usize * 4;
            let (b, g, r) = (bgra[i] as u32, bgra[i + 1] as u32, bgra[i + 2] as u32);
            gray.push(((r * 77 + g * 150 + b * 29) >> 8) as u8);
        }
    }
    (w, h, gray)
}

/// Nearest-neighbor RGBA, flipped left-right like a mirror so aiming the
/// phone at the camera feels natural.
fn mirrored_preview(width: u32, height: u32, stride: usize, bgra: &[u8], max_width: u32) -> Preview {
    let out_w = width.min(max_width).max(1);
    let out_h = (height as u64 * out_w as u64 / width.max(1) as u64).max(1) as u32;
    let mut rgba = Vec::with_capacity((out_w * out_h * 4) as usize);
    for y in 0..out_h {
        let sy = (y as u64 * height as u64 / out_h as u64) as usize;
        for x in 0..out_w {
            let sx = (width - 1) as u64 - x as u64 * width as u64 / out_w as u64;
            let i = sy * stride + sx as usize * 4;
            rgba.extend_from_slice(&[bgra[i + 2], bgra[i + 1], bgra[i], 255]);
        }
    }
    Preview { width: out_w, height: out_h, rgba }
}

#[cfg(windows)]
mod platform {
    use super::{handle_frame, Shared, DENIED};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;
    use windows::core::HRESULT;
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Capture::Frames::{MediaFrameReaderStartStatus, MediaFrameSourceKind};
    use windows::Media::Capture::{
        MediaCapture, MediaCaptureInitializationSettings, MediaCaptureMemoryPreference, MediaCaptureSharingMode,
        StreamingCaptureMode,
    };
    use windows::Storage::Streams::{Buffer, DataReader};

    const FRAME_GAP: Duration = Duration::from_millis(120);
    const E_ACCESSDENIED: HRESULT = HRESULT(0x8007_0005_u32 as i32);
    /// MF_E_NO_CAPTURE_DEVICES_AVAILABLE.
    const NO_DEVICES: HRESULT = HRESULT(0xC00D_ABE0_u32 as i32);

    fn describe(err: windows::core::Error) -> String {
        match err.code() {
            E_ACCESSDENIED => DENIED.into(),
            NO_DEVICES => "no camera found".into(),
            _ => format!("camera failed: {}", err.message()),
        }
    }

    pub(super) fn run(shared: &Mutex<Shared>, stop: &AtomicBool) -> Result<(), String> {
        let settings = MediaCaptureInitializationSettings::new().map_err(describe)?;
        settings.SetStreamingCaptureMode(StreamingCaptureMode::Video).map_err(describe)?;
        settings.SetMemoryPreference(MediaCaptureMemoryPreference::Cpu).map_err(describe)?;
        // Works while another app holds the camera.
        settings.SetSharingMode(MediaCaptureSharingMode::SharedReadOnly).map_err(describe)?;
        let capture = MediaCapture::new().map_err(describe)?;
        capture.InitializeWithSettingsAsync(&settings).and_then(|op| op.get()).map_err(describe)?;
        let result = read_frames(&capture, shared, stop);
        let _ = capture.Close();
        result
    }

    fn read_frames(capture: &MediaCapture, shared: &Mutex<Shared>, stop: &AtomicBool) -> Result<(), String> {
        let source = capture
            .FrameSources()
            .map_err(describe)?
            .into_iter()
            .filter_map(|pair| pair.Value().ok())
            .find(|s| s.Info().and_then(|i| i.SourceKind()).is_ok_and(|k| k == MediaFrameSourceKind::Color))
            .ok_or("no camera found")?;
        let reader = capture.CreateFrameReaderAsync(&source).and_then(|op| op.get()).map_err(describe)?;
        let status = reader.StartAsync().and_then(|op| op.get()).map_err(describe)?;
        if status != MediaFrameReaderStartStatus::Success {
            return Err(format!("camera unavailable ({status:?})"));
        }
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(FRAME_GAP);
            // No new frame yet comes back as an error; just wait.
            let Ok(bitmap) =
                reader.TryAcquireLatestFrame().and_then(|f| f.VideoMediaFrame()).and_then(|v| v.SoftwareBitmap())
            else {
                continue;
            };
            let Ok((w, h, bytes)) = bgra_bytes(&bitmap) else {
                continue;
            };
            let stride = bytes.len() / h.max(1) as usize;
            if stride < w as usize * 4 {
                continue;
            }
            if handle_frame(shared, w, h, stride, &bytes) {
                break;
            }
        }
        let _ = reader.StopAsync().and_then(|op| op.get());
        Ok(())
    }

    fn bgra_bytes(bitmap: &SoftwareBitmap) -> windows::core::Result<(u32, u32, Vec<u8>)> {
        let bgra = SoftwareBitmap::ConvertWithAlpha(bitmap, BitmapPixelFormat::Bgra8, BitmapAlphaMode::Ignore)?;
        let (w, h) = (bgra.PixelWidth()? as u32, bgra.PixelHeight()? as u32);
        let buffer = Buffer::Create(w * h * 4)?;
        bgra.CopyToBuffer(&buffer)?;
        let mut bytes = vec![0u8; buffer.Length()? as usize];
        DataReader::FromBuffer(&buffer)?.ReadBytes(&mut bytes)?;
        Ok((w, h, bytes))
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Shared;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    pub(super) fn run(_: &Mutex<Shared>, _: &AtomicBool) -> Result<(), String> {
        Err("camera scanning needs Windows; open a picture of the QR instead".into())
    }
}

/// Windows Settings > Privacy > Camera.
pub fn open_camera_settings() -> Option<String> {
    std::process::Command::new("explorer")
        .arg("ms-settings:privacy-webcam")
        .spawn()
        .err()
        .map(|e| format!("could not open settings: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qr_image::qr_rgba;

    /// A rendered QR as a BGRA camera frame with padded rows.
    fn frame(url: &str, pad: usize) -> (u32, u32, usize, Vec<u8>) {
        let (side, rgba) = qr_rgba(url).unwrap();
        let stride = side as usize * 4 + pad;
        let mut bgra = vec![0u8; stride * side as usize];
        for y in 0..side as usize {
            for x in 0..side as usize {
                let v = rgba[(y * side as usize + x) * 4];
                bgra[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        (side, side, stride, bgra)
    }

    #[test]
    fn a_frame_with_a_qr_is_found_despite_row_padding() {
        let url = "bitchat://verify?v=1&noise=ab&sign=cd&nick=bob&ts=1&nonce=AAAA&sig=00";
        let (w, h, stride, bgra) = frame(url, 16);
        let shared = Mutex::new(Shared::default());
        assert!(handle_frame(&shared, w, h, stride, &bgra));
        let shared = shared.into_inner().unwrap();
        assert_eq!(shared.found.as_deref(), Some(url));
        assert!(shared.preview.is_some());
    }

    /// Opens the real camera: `cargo test -p witchat-app -- --ignored camera_delivers`.
    #[test]
    #[ignore]
    fn camera_delivers_frames() {
        let scanner = CameraScanner::start();
        use std::time::Duration;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match scanner.poll() {
                Some(CameraUpdate::Preview(p)) => {
                    println!("preview {}x{}", p.width, p.height);
                    return;
                }
                Some(CameraUpdate::Failed(err)) => panic!("{err}"),
                Some(CameraUpdate::Found(url)) => {
                    println!("found {url}");
                    return;
                }
                None => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        panic!("no frame in 10 s");
    }

    #[test]
    fn preview_is_mirrored_and_scaled() {
        // Two pixels: blue on the left, red on the right.
        let bgra = [255, 0, 0, 255, 0, 0, 255, 255];
        let p = mirrored_preview(2, 1, 8, &bgra, 320);
        assert_eq!((p.width, p.height), (2, 1));
        assert_eq!(&p.rgba[..4], &[255, 0, 0, 255], "red now on the left");
        let wide = vec![0u8; 1280 * 4 * 2];
        let p = mirrored_preview(1280, 2, 1280 * 4, &wide, 320);
        assert_eq!((p.width, p.height), (320, 1));
    }
}
