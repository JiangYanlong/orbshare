#[cfg(target_os = "macos")]
#[macro_use]
extern crate objc;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub mod videotoolbox;
#[cfg(target_os = "windows")]
pub mod windows;

use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc;

const CHANNEL_BUFFER: usize = 64;

/// Video transport mode — controls how captured frames reach the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VideoTransportMode {
    /// JPEG over WebRTC DataChannel (debug only).
    DebugJpegDataChannel,
    /// H.264 over WebRTC Video Track (production).
    WebRtcVideoTrack,
}

impl Default for VideoTransportMode {
    fn default() -> Self {
        Self::DebugJpegDataChannel
    }
}

/// Capture source mode — controls what generates video frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CaptureMode {
    /// Synthetic test pattern (no FFI, no ScreenCaptureKit).
    TestPattern,
    /// Real screen capture via ScreenCaptureKit (macOS) or WGC (Windows).
    ScreenCapture,
}

impl Default for CaptureMode {
    fn default() -> Self {
        Self::TestPattern
    }
}

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Native display/window identifier. None selects the platform primary display.
    pub source_id: Option<u32>,
    /// True for a full display, false for one desktop-independent application window.
    pub source_is_display: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_bitrate: u32,
    pub keyframe_interval_seconds: f64,
    pub capture_system_audio: bool,
    pub capture_microphone: bool,
    pub video_transport: VideoTransportMode,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            source_id: None,
            source_is_display: true,
            width: 1920,
            height: 1080,
            fps: 30,
            video_bitrate: 8_000_000,
            keyframe_interval_seconds: 1.0,
            capture_system_audio: false,
            capture_microphone: false,
            video_transport: VideoTransportMode::DebugJpegDataChannel,
        }
    }
}

#[derive(Debug)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub timestamp_us: u64,
    pub is_keyframe: bool,
    pub width: u32,
    pub height: u32,
}

/// H.264 encoded video frame — production video output.
#[derive(Debug)]
pub struct EncodedVideoFrame {
    /// H.264 Annex B NAL unit data (includes SPS/PPS on keyframes).
    pub data: Vec<u8>,
    /// Presentation timestamp in microseconds.
    pub timestamp_us: u64,
    /// True if this frame is an IDR (keyframe) with SPS/PPS prepended.
    pub is_keyframe: bool,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub struct AudioFrame {
    /// Interleaved, normalized PCM samples in the range `[-1.0, 1.0]`.
    pub samples: Vec<f32>,
    pub timestamp_us: u64,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Capture output for different transport modes.
pub enum CaptureOutput {
    /// Raw YUV420 frame (debug-jpeg-datachannel mode).
    RawVideo(EncodedFrame),
    /// H.264 encoded frame (webrtc-video-track mode).
    EncodedVideo(EncodedVideoFrame),
}

pub struct CaptureStream {
    /// Raw YUV420 frames — used in debug-jpeg-datachannel mode.
    pub video_rx: mpsc::Receiver<EncodedFrame>,
    /// H.264 encoded frames — used in webrtc-video-track mode.
    pub encoded_video_rx: Option<mpsc::Receiver<EncodedVideoFrame>>,
    /// Low-rate raw frames for the host's in-process monitor. This receiver is
    /// independent from WebRTC and never represents a connected participant.
    pub preview_video_rx: Option<mpsc::Receiver<EncodedFrame>>,
    pub audio_rx: Option<mpsc::Receiver<AudioFrame>>,
}

/// A capture source (display or window).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CaptureSource {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_display: bool,
    /// Small in-process JPEG preview used by the native source picker.
    pub thumbnail_data_url: Option<String>,
}

/// Runtime capture statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CaptureStats {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub pixel_format: String,
    pub frame_count: u64,
}

/// Encodes an EncodedFrame (YUV420 planar) into JPEG bytes.
/// When target dimensions are given, the YUV planes are sampled directly at the
/// smaller resolution — no full-resolution RGB buffer is ever allocated.
/// This keeps per-frame cost low even for Retina captures (2560×1600).
pub fn encode_frame_to_jpeg(
    frame: &EncodedFrame,
    quality: u8,
    target_width: Option<u32>,
    target_height: Option<u32>,
) -> Option<Vec<u8>> {
    let src_w = frame.width as usize;
    let src_h = frame.height as usize;
    let y_size = src_w * src_h;
    let uv_src_w = src_w / 2;
    let uv_src_h = src_h / 2;

    if frame.data.len() < y_size + uv_src_w * uv_src_h * 2 {
        return None;
    }

    let y_plane = &frame.data[..y_size];
    let u_plane = &frame.data[y_size..y_size + uv_src_w * uv_src_h];
    let v_plane = &frame.data[y_size + uv_src_w * uv_src_h..];

    let (dst_w, dst_h) = match (target_width, target_height) {
        (Some(tw), Some(th)) => {
            let max_w = (tw as usize).max(2);
            let max_h = (th as usize).max(2);
            let scale = (max_w as f32 / src_w as f32)
                .min(max_h as f32 / src_h as f32)
                .min(1.0);
            let dst_w = ((src_w as f32 * scale).round() as usize).max(2) & !1;
            let dst_h = ((src_h as f32 * scale).round() as usize).max(2) & !1;
            (dst_w.max(2), dst_h.max(2))
        }
        _ => (src_w, src_h),
    };

    let scale_x = src_w as f32 / dst_w as f32;
    let scale_y = src_h as f32 / dst_h as f32;

    // Convert YUV420 → RGB directly at the target resolution (bilinear sample)
    let mut rgb = Vec::with_capacity(dst_w * dst_h * 3);
    for row in 0..dst_h {
        let src_y_f = (row as f32 + 0.5) * scale_y - 0.5;
        let sy0 = (src_y_f.floor() as usize).min(src_h - 1);
        let sy1 = (sy0 + 1).min(src_h - 1);
        let fy = src_y_f - src_y_f.floor();

        for col in 0..dst_w {
            let src_x_f = (col as f32 + 0.5) * scale_x - 0.5;
            let sx0 = (src_x_f.floor() as usize).min(src_w - 1);
            let sx1 = (sx0 + 1).min(src_w - 1);
            let fx = src_x_f - src_x_f.floor();

            // Bilinear Y
            let y00 = y_plane[sy0 * src_w + sx0] as f32;
            let y10 = y_plane[sy0 * src_w + sx1] as f32;
            let y01 = y_plane[sy1 * src_w + sx0] as f32;
            let y11 = y_plane[sy1 * src_w + sx1] as f32;
            let y = y00 * (1.0 - fx) * (1.0 - fy)
                + y10 * fx * (1.0 - fy)
                + y01 * (1.0 - fx) * fy
                + y11 * fx * fy;

            // UV at half resolution
            let uv_row0 = (sy0 / 2).min(uv_src_h - 1);
            let uv_row1 = (sy1 / 2).min(uv_src_h - 1);
            let uv_col0 = (sx0 / 2).min(uv_src_w - 1);
            let uv_col1 = (sx1 / 2).min(uv_src_w - 1);
            let uv_fy = (fy * 0.5) as f32;
            let uv_fx = (fx * 0.5) as f32;

            let u00 = u_plane[uv_row0 * uv_src_w + uv_col0] as f32 - 128.0;
            let u10 = u_plane[uv_row0 * uv_src_w + uv_col1] as f32 - 128.0;
            let u01 = u_plane[uv_row1 * uv_src_w + uv_col0] as f32 - 128.0;
            let u11 = u_plane[uv_row1 * uv_src_w + uv_col1] as f32 - 128.0;
            let u = u00 * (1.0 - uv_fx) * (1.0 - uv_fy)
                + u10 * uv_fx * (1.0 - uv_fy)
                + u01 * (1.0 - uv_fx) * uv_fy
                + u11 * uv_fx * uv_fy;

            let v00 = v_plane[uv_row0 * uv_src_w + uv_col0] as f32 - 128.0;
            let v10 = v_plane[uv_row0 * uv_src_w + uv_col1] as f32 - 128.0;
            let v01 = v_plane[uv_row1 * uv_src_w + uv_col0] as f32 - 128.0;
            let v11 = v_plane[uv_row1 * uv_src_w + uv_col1] as f32 - 128.0;
            let v = v00 * (1.0 - uv_fx) * (1.0 - uv_fy)
                + v10 * uv_fx * (1.0 - uv_fy)
                + v01 * (1.0 - uv_fx) * uv_fy
                + v11 * uv_fx * uv_fy;

            let r = (y + 1.402 * v).clamp(0.0, 255.0) as u8;
            let g = (y - 0.344136 * u - 0.714136 * v).clamp(0.0, 255.0) as u8;
            let b = (y + 1.772 * u).clamp(0.0, 255.0) as u8;
            rgb.push(r);
            rgb.push(g);
            rgb.push(b);
        }
    }

    let img = image::RgbImage::from_raw(dst_w as u32, dst_h as u32, rgb)?;
    let mut buf = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    img.write_with_encoder(encoder).ok()?;
    Some(buf)
}

pub trait CaptureProvider: Send + Sync {
    fn request_permission(&self)
        -> Pin<Box<dyn Future<Output = Result<bool, String>> + Send + '_>>;

    fn start_capture(&mut self, config: CaptureConfig) -> Result<CaptureStream, String>;

    fn stop_capture(&mut self) -> Result<(), String>;

    /// List available capture sources (displays).
    fn list_sources(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CaptureSource>, String>> + Send + '_>>;

    /// Returns runtime capture stats if a capture is active.
    fn capture_stats(&self) -> Option<CaptureStats> {
        None
    }
}

pub fn create_capture_provider() -> Box<dyn CaptureProvider> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacOSCaptureProvider::new())
    }

    #[cfg(target_os = "windows")]
    {
        Box::new(windows::WindowsCaptureProvider::new())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        compile_error!("orb-capture is only supported on macOS and Windows");
    }
}

pub fn create_capture_provider_for_mode(mode: CaptureMode) -> Box<dyn CaptureProvider> {
    match mode {
        CaptureMode::TestPattern => {
            tracing::info!("[capture] Creating TestPattern capture provider (no FFI)");
            Box::new(TestPatternCaptureProvider::new())
        }
        CaptureMode::ScreenCapture => {
            tracing::info!("[capture] Creating ScreenCapture provider");
            create_capture_provider()
        }
    }
}

// ── TestPatternCaptureProvider ─────────────────────────────────────────────
// Generates synthetic YUV420 frames. No macOS FFI, no ScreenCaptureKit.

pub struct TestPatternCaptureProvider {
    stop_tx: Option<tokio::sync::watch::Sender<bool>>,
}

impl TestPatternCaptureProvider {
    pub fn new() -> Self {
        Self { stop_tx: None }
    }
}

impl Default for TestPatternCaptureProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureProvider for TestPatternCaptureProvider {
    fn request_permission(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<bool, String>> + Send + '_>> {
        Box::pin(async move {
            tracing::info!("[test-pattern] Permission always granted");
            Ok(true)
        })
    }

    fn start_capture(&mut self, config: CaptureConfig) -> Result<CaptureStream, String> {
        tracing::info!(
            "[test-pattern] Starting test pattern: {}x{} @ {}fps",
            config.width,
            config.height,
            config.fps
        );
        let (video_tx, video_rx) = mpsc::channel::<EncodedFrame>(CHANNEL_BUFFER);
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
        self.stop_tx = Some(stop_tx);

        let width = config.width;
        let height = config.height;
        let fps = config.fps;

        tokio::spawn(async move {
            let frame_dur = std::time::Duration::from_micros(1_000_000 / fps as u64);
            let mut frame_count: u64 = 0;
            loop {
                tokio::select! {
                    r = stop_rx.changed() => {
                        if r.is_err() || *stop_rx.borrow() {
                            tracing::info!("[test-pattern] Stopping frame generation");
                            break;
                        }
                    }
                    _ = tokio::time::sleep(frame_dur) => {
                        let frame = generate_test_frame(width, height, frame_count);
                        if video_tx.send(frame).await.is_err() {
                            break;
                        }
                        frame_count += 1;
                    }
                }
            }
        });

        tracing::info!("[test-pattern] Capture started");
        Ok(CaptureStream {
            video_rx,
            encoded_video_rx: None,
            preview_video_rx: None,
            audio_rx: None,
        })
    }

    fn stop_capture(&mut self) -> Result<(), String> {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(true);
        }
        tracing::info!("[test-pattern] Capture stopped");
        Ok(())
    }

    fn list_sources(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CaptureSource>, String>> + Send + '_>> {
        Box::pin(async move {
            Ok(vec![CaptureSource {
                id: 0,
                name: "Test Pattern".to_string(),
                width: 1920,
                height: 1080,
                is_display: true,
                thumbnail_data_url: None,
            }])
        })
    }
}

/// Generate a single YUV420 test frame with a color bar pattern and frame counter.
fn generate_test_frame(width: u32, height: u32, frame_count: u64) -> EncodedFrame {
    let w = width as usize;
    let h = height as usize;
    let y_size = w * h;
    let uv_w = w / 2;
    let uv_h = h / 2;
    let total = y_size + uv_w * uv_h * 2;
    let mut data = vec![0u8; total];

    // Color bars: cycle through 8 colors
    let colors: [(u8, u8, u8); 8] = [
        (235, 128, 128), // white
        (210, 16, 146),  // yellow
        (170, 166, 16),  // cyan
        (145, 54, 20),   // green
        (106, 202, 222), // magenta
        (81, 90, 240),   // red
        (41, 240, 110),  // blue
        (16, 128, 128),  // black
    ];

    let bar_width = w / 8;
    for row in 0..h {
        for col in 0..w {
            let bar_idx = (col / bar_width).min(7);
            let (y_val, u_val, v_val) = colors[bar_idx];

            let y_idx = row * w + col;
            data[y_idx] = y_val;

            if row % 2 == 0 && col % 2 == 0 {
                let uv_idx = (row / 2) * uv_w + (col / 2);
                data[y_size + uv_idx] = u_val;
                data[y_size + uv_w * uv_h + uv_idx] = v_val;
            }
        }
    }

    // Overlay frame counter as a simple brightness modulation in top-left area
    let counter = (frame_count % 60) as f32 / 60.0;
    for row in 0..4.min(h) {
        for col in 0..16.min(w) {
            let bar_idx = col / 2;
            let brightness = if (bar_idx as f32 / 8.0) < counter {
                235
            } else {
                16
            };
            data[row * w + col] = brightness;
        }
    }

    EncodedFrame {
        data,
        timestamp_us: frame_count * 1_000_000 / 30,
        is_keyframe: frame_count % 120 == 0,
        width,
        height,
    }
}

/// Check if screen capture permission is already granted.
pub fn check_screen_capture_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::check_screen_capture_permission_impl()
    }
    #[cfg(target_os = "windows")]
    {
        true // WGC doesn't require explicit permission
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

/// Generate one capture-source preview on demand. Source enumeration stays
/// lightweight so the picker can render before JPEG conversion begins.
pub fn capture_source_thumbnail(
    source_id: u32,
    source_is_display: bool,
) -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        macos::capture_source_thumbnail_impl(source_id, source_is_display)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (source_id, source_is_display);
        Ok(None)
    }
}

/// Open the system screen recording settings panel.
/// CGRequestScreenCaptureAccess is permanently disabled — it causes
/// foreign-exception aborts when called from non-main threads.
/// Users must grant permission via System Settings and restart OrbShare.

/// Open the system screen recording settings panel.
pub fn open_screen_recording_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
            .spawn()
            .map_err(|e| format!("Failed to open settings: {}", e))?;
    }
    Ok(())
}

#[cfg(test)]
mod pipeline_tests {
    use std::time::{Duration, Instant};

    /// Simulates the production topology: raw frames may be discarded before
    /// encoding, but encoded H.264 frames are backpressured and consumed in
    /// order. A gap after encoding would break the P-frame reference chain.
    #[tokio::test(flavor = "multi_thread")]
    async fn pipeline_topology_sustained_throughput() {
        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::channel::<crate::EncodedFrame>(2);
        let (enc_tx, mut enc_rx) = tokio::sync::mpsc::channel::<crate::EncodedVideoFrame>(2);

        // ── Producer: mimics start_coregraphics_capture loop ──
        let producer = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(33));
            // FIX under test: Burst (default) never suspends when the tick
            // handler is slower than the period, monopolizing the worker and
            // starving tasks woken into its LIFO slot.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut n: u64 = 0;
            let mut last_log = Instant::now();
            loop {
                ticker.tick().await;
                // "capture": blocking work on the worker thread
                std::thread::sleep(Duration::from_millis(88));
                let frame = crate::EncodedFrame {
                    data: vec![0u8; 1024],
                    timestamp_us: n * 83_000,
                    is_keyframe: n % 60 == 0,
                    width: 1280,
                    height: 720,
                };
                match raw_tx.try_send(frame) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        if last_log.elapsed() >= Duration::from_millis(400) {
                            eprintln!("[sim:producer] ENC_DROP n={n}");
                            last_log = Instant::now();
                        }
                    }
                    Err(_) => return,
                }
                n += 1;
            }
        });

        // ── Encoder task: mimics macos.rs encoded-mode task ──
        let encoder = tokio::spawn(async move {
            let mut encoded_n: u64 = 0;
            let mut last_frame_at = Instant::now();
            let mut max_gap_ms: u128 = 0;
            while let Some(_frame) = raw_rx.recv().await {
                let gap = last_frame_at.elapsed().as_millis();
                if gap > max_gap_ms {
                    max_gap_ms = gap;
                }
                if gap > 500 {
                    eprintln!("[sim:encoder] STALL {gap}ms before frame {encoded_n}");
                }
                last_frame_at = Instant::now();
                // "encode": ~6ms of CPU
                let t0 = Instant::now();
                while t0.elapsed() < Duration::from_millis(6) {
                    std::hint::spin_loop();
                }
                let out = crate::EncodedVideoFrame {
                    data: vec![0u8; 2048],
                    timestamp_us: encoded_n * 83_000,
                    is_keyframe: encoded_n % 60 == 0,
                    width: 1280,
                    height: 720,
                };
                if enc_tx.send(out).await.is_err() {
                    break;
                }
                encoded_n += 1;
            }
            eprintln!("[sim:encoder] DONE encoded={encoded_n} max_gap={max_gap_ms}ms");
        });

        // ── Pump: slow transport, but no post-encode frame dropping ──
        let pump = tokio::spawn(async move {
            let mut timestamps = Vec::new();
            while let Some(frame) = enc_rx.recv().await {
                // "send": blocking SRTP/packetize work
                std::thread::sleep(Duration::from_millis(200));
                timestamps.push(frame.timestamp_us);
            }
            timestamps
        });

        tokio::time::sleep(Duration::from_secs(3)).await;
        producer.abort();
        encoder.abort();
        let sent_timestamps = pump.await.expect("pump should drain and stop");
        assert!(sent_timestamps.len() >= 5);
        assert!(sent_timestamps
            .windows(2)
            .all(|pair| pair[1] - pair[0] == 83_000));
    }
}
