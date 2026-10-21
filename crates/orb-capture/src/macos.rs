#[cfg(target_os = "macos")]
use crate::videotoolbox::VideoToolboxEncoder;
use crate::{
    AudioFrame, CaptureConfig, CaptureProvider, CaptureSource, CaptureStats, CaptureStream,
    EncodedFrame, EncodedVideoFrame, VideoTransportMode,
};
use base64::Engine;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::display::CGDisplay;
use core_graphics::window::{
    copy_window_info, kCGNullWindowID, kCGWindowBounds, kCGWindowLayer,
    kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly, kCGWindowName,
    kCGWindowNumber, kCGWindowOwnerName, kCGWindowOwnerPID,
};
use image::codecs::jpeg::JpegEncoder;
use image::ExtendedColorType;
use std::collections::HashMap;
use std::future::Future;
use std::os::raw::c_void;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tracing::{error, info};

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Protocol, Sel};

const CHANNEL_BUFFER: usize = 64;
const VIDEO_FRAME_BUFFER: usize = 2;
const HOST_PREVIEW_RAW_INTERVAL: Duration = Duration::from_secs(1);
const BGRA_PIXEL_FORMAT: u32 = 0x42475241; // 'BGRA'

// ── CoreGraphics FFI ─────────────────────────────────────────────────────────

type CGDirectDisplayID = u32;
type CGImageRef = *mut c_void;
type CGColorSpaceRef = *mut c_void;
type CGContextRef = *mut c_void;
type CGDisplayModeRef = *mut c_void;

extern "C" {
    fn CGMainDisplayID() -> CGDirectDisplayID;
    fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> CGDisplayModeRef;
    fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayCreateImage(display: CGDirectDisplayID) -> CGImageRef;
    fn CGWindowListCreateImage(
        screen_bounds: CGRect,
        list_option: u32,
        window_id: u32,
        image_option: u32,
    ) -> CGImageRef;
    fn CGImageGetWidth(image: CGImageRef) -> usize;
    fn CGImageGetHeight(image: CGImageRef) -> usize;
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    fn CGContextDrawImage(context: CGContextRef, rect: CGRect, image: CGImageRef);
    fn CGContextRelease(context: CGContextRef);
    fn CGColorSpaceRelease(space: CGColorSpaceRef);
    fn CGImageRelease(image: CGImageRef);
    fn CFRelease(cf: *mut c_void);
    fn CGRectMakeWithDictionaryRepresentation(dictionary: *const c_void, rect: *mut CGRect)
        -> bool;
    static CGRectNull: CGRect;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

const K_CG_IMAGE_ALPHA_PREMULTIPLIED_FIRST: u32 = 2;
const K_CG_BITMAP_BYTE_ORDER_32_LITTLE: u32 = 2 << 12;
const K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW: u32 = 1 << 3;
const K_CG_WINDOW_IMAGE_BOUNDS_IGNORE_FRAMING: u32 = 1 << 0;
const K_CG_WINDOW_IMAGE_BEST_RESOLUTION: u32 = 1 << 3;

// A successful screen image or ScreenCaptureKit source enumeration is stronger
// evidence than a later CoreGraphics probe. On macOS that probe can block and
// transiently return null while ScreenCaptureKit is busy, which previously made
// the UI revoke an already granted permission after every source-picker click.
// Revoking the system permission requires restarting the app before capture can
// work again, so keeping the positive result for this process is safe.
static SCREEN_CAPTURE_PERMISSION_GRANTED: AtomicBool = AtomicBool::new(false);

// ── CoreMedia FFI ────────────────────────────────────────────────────────────

type CMSampleBufferRef = *mut c_void;
type CMFormatDescriptionRef = *mut c_void;
type CMBlockBufferRef = *mut c_void;
type CVPixelBufferRef = *mut c_void;
type CVReturn = i32;
type OSStatus = i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioStreamBasicDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioBuffer {
    number_channels: u32,
    data_byte_size: u32,
    data: *mut c_void,
}

#[repr(C)]
struct AudioBufferList2 {
    number_buffers: u32,
    buffers: [AudioBuffer; 2],
}

const K_CV_RETURN_SUCCESS: CVReturn = 0;

extern "C" {
    fn CMSampleBufferGetImageBuffer(buffer: CMSampleBufferRef) -> CVPixelBufferRef;
    fn CMSampleBufferGetNumSamples(buffer: CMSampleBufferRef) -> isize;
    fn CMSampleBufferGetFormatDescription(buffer: CMSampleBufferRef) -> CMFormatDescriptionRef;
    fn CMSampleBufferGetPresentationTimeStamp(buffer: CMSampleBufferRef) -> CMTime;
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        buffer: CMSampleBufferRef,
        buffer_list_size_needed: *mut usize,
        buffer_list: *mut AudioBufferList2,
        buffer_list_size: usize,
        buffer_list_allocator: *const c_void,
        block_buffer_allocator: *const c_void,
        flags: u32,
        block_buffer: *mut CMBlockBufferRef,
    ) -> OSStatus;
    fn CMAudioFormatDescriptionGetStreamBasicDescription(
        description: CMFormatDescriptionRef,
    ) -> *const AudioStreamBasicDescription;
    fn CVPixelBufferLockBaseAddress(pixel_buffer: CVPixelBufferRef, lock_flags: u64) -> CVReturn;
    fn CVPixelBufferUnlockBaseAddress(
        pixel_buffer: CVPixelBufferRef,
        unlock_flags: u64,
    ) -> CVReturn;
    fn CVPixelBufferGetBaseAddress(pixel_buffer: CVPixelBufferRef) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRow(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetPlaneCount(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetBaseAddressOfPlane(
        pixel_buffer: CVPixelBufferRef,
        plane_index: usize,
    ) -> *mut u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(
        pixel_buffer: CVPixelBufferRef,
        plane_index: usize,
    ) -> usize;
    fn CVPixelBufferGetWidth(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetPixelFormatType(pixel_buffer: CVPixelBufferRef) -> u32;
}

// ── GCD FFI ──────────────────────────────────────────────────────────────────

extern "C" {
    fn dispatch_queue_create(
        label: *const std::os::raw::c_char,
        attr: *const c_void,
    ) -> *mut c_void;
}

// ── ScreenCaptureKit constants ───────────────────────────────────────────────

const SC_STREAM_OUTPUT_TYPE_SCREEN: isize = 0;
const SC_STREAM_OUTPUT_TYPE_AUDIO: isize = 1;
const NV12_VIDEO_RANGE_PIXEL_FORMAT: u32 = 0x3432_3076; // '420v'
const K_AUDIO_FORMAT_LINEAR_PCM: u32 = 0x6c70_636d; // 'lpcm'
const K_AUDIO_FORMAT_FLAG_IS_FLOAT: u32 = 1 << 0;
const K_AUDIO_FORMAT_FLAG_IS_BIG_ENDIAN: u32 = 1 << 1;
const K_AUDIO_FORMAT_FLAG_IS_SIGNED_INTEGER: u32 = 1 << 2;
const K_AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED: u32 = 1 << 5;

// ── Send-safe wrapper for raw pointer ────────────────────────────────────────

struct SendPtr(*const CaptureState);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

// ── Global state registry ────────────────────────────────────────────────────

fn capture_state_registry() -> &'static Mutex<HashMap<usize, SendPtr>> {
    static MAP: OnceLock<Mutex<HashMap<usize, SendPtr>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

// ── CaptureState ─────────────────────────────────────────────────────────────

struct CaptureState {
    tx: mpsc::Sender<EncodedFrame>,
    audio_tx: Option<mpsc::Sender<AudioFrame>>,
    width: AtomicU32,
    height: AtomicU32,
    fps: AtomicU32,
    frame_count: AtomicU64,
    audio_frame_count: AtomicU64,
    audio_drop_count: AtomicU64,
    frame_gate: Mutex<FrameRateGate>,
}

struct FrameRateGate {
    interval: Duration,
    last_emitted_at: Option<Instant>,
}

impl FrameRateGate {
    fn new(fps: u32) -> Self {
        Self {
            interval: Duration::from_secs_f64(1.0 / fps.max(1) as f64),
            last_emitted_at: None,
        }
    }

    fn should_emit(&mut self, now: Instant) -> bool {
        if self
            .last_emitted_at
            .is_some_and(|last| now.duration_since(last) < self.interval)
        {
            return false;
        }
        self.last_emitted_at = Some(now);
        true
    }
}

// ── ObjC class registration ─────────────────────────────────────────────────

unsafe fn register_stream_output_class() -> &'static Class {
    static STREAM_OUTPUT_CLASS: OnceLock<&'static Class> = OnceLock::new();
    if let Some(cls) = STREAM_OUTPUT_CLASS.get() {
        return cls;
    }
    if let Some(cls) = Class::get("OrbStreamOutput") {
        let _ = STREAM_OUTPUT_CLASS.set(cls);
        return cls;
    }

    let superclass = class!(NSObject);
    let mut decl = ClassDecl::new("OrbStreamOutput", superclass)
        .expect("Failed to declare OrbStreamOutput class");

    if let Some(proto) = Protocol::get("SCStreamOutput") {
        decl.add_protocol(proto);
    } else {
        error!("SCStreamOutput protocol not found — requires macOS 12.3+");
    }

    extern "C" fn init_fn(this: &mut Object, _cmd: Sel) -> *mut Object {
        this as *mut Object
    }

    extern "C" fn did_output_fn(
        this: &Object,
        _cmd: Sel,
        _stream: *mut Object,
        sample_buffer: CMSampleBufferRef,
        of_type: isize,
    ) {
        let output_addr = this as *const Object as usize;
        match of_type {
            SC_STREAM_OUTPUT_TYPE_SCREEN => on_video_frame(output_addr, sample_buffer),
            SC_STREAM_OUTPUT_TYPE_AUDIO => on_audio_frame(output_addr, sample_buffer),
            _ => {}
        }
    }

    unsafe {
        decl.add_method(
            sel!(init),
            init_fn as extern "C" fn(&mut Object, Sel) -> *mut Object,
        );
        decl.add_method(
            sel!(stream:didOutputSampleBuffer:ofType:),
            did_output_fn as extern "C" fn(&Object, Sel, *mut Object, CMSampleBufferRef, isize),
        );
    }

    let cls = decl.register();
    let _ = STREAM_OUTPUT_CLASS.set(cls);
    cls
}

// ── ObjC callback ────────────────────────────────────────────────────────────

fn on_video_frame(output_addr: usize, sample_buffer: CMSampleBufferRef) {
    let state = {
        let map = capture_state_registry().lock().unwrap();
        match map.get(&output_addr) {
            Some(sp) => unsafe { &*sp.0 },
            None => return,
        }
    };

    // ScreenCaptureKit can keep delivering at the display refresh rate even
    // when VideoToolbox advertises a lower ExpectedFrameRate. Gate before the
    // costly pixel conversion so a 20fps tier really encodes and sends 20fps.
    // Previously the nominal 8Mbps budget was spread across ~57fps, which made
    // shared text visibly soft despite the correct encoder bitrate setting.
    if !state.frame_gate.lock().unwrap().should_emit(Instant::now()) {
        return;
    }

    let pixel_buffer = unsafe { CMSampleBufferGetImageBuffer(sample_buffer) };
    if pixel_buffer.is_null() {
        return;
    }

    if unsafe { CVPixelBufferLockBaseAddress(pixel_buffer, 1) } != K_CV_RETURN_SUCCESS {
        return;
    }

    let pw = unsafe { CVPixelBufferGetWidth(pixel_buffer) };
    let ph = unsafe { CVPixelBufferGetHeight(pixel_buffer) };
    let fmt = unsafe { CVPixelBufferGetPixelFormatType(pixel_buffer) };

    if pw == 0 || ph == 0 {
        unsafe {
            CVPixelBufferUnlockBaseAddress(pixel_buffer, 1);
        }
        return;
    }

    let yuv = if fmt == NV12_VIDEO_RANGE_PIXEL_FORMAT {
        copy_nv12_pixel_buffer(pixel_buffer, pw, ph)
    } else if fmt == BGRA_PIXEL_FORMAT {
        let base = unsafe { CVPixelBufferGetBaseAddress(pixel_buffer) };
        let bytes_per_row = unsafe { CVPixelBufferGetBytesPerRow(pixel_buffer) };
        if base.is_null() {
            None
        } else {
            let src = unsafe { std::slice::from_raw_parts(base as *const u8, bytes_per_row * ph) };
            convert_bgra_to_yuv420_top_down(src, pw, ph, bytes_per_row)
        }
    } else {
        tracing::warn!("[macOS] Unsupported ScreenCaptureKit pixel format: 0x{fmt:08x}");
        None
    };

    if let Some(yuv) = yuv {
        state.width.store(pw as u32, Ordering::Relaxed);
        state.height.store(ph as u32, Ordering::Relaxed);

        let frame_count = state.frame_count.fetch_add(1, Ordering::Relaxed);
        let fps = state.fps.load(Ordering::Relaxed).max(1);
        let timestamp_us = frame_count * 1_000_000 / fps as u64;

        let frame = EncodedFrame {
            data: yuv,
            timestamp_us,
            is_keyframe: frame_count % 120 == 0,
            width: pw as u32,
            height: ph as u32,
        };
        let _ = state.tx.try_send(frame);
    }

    unsafe {
        CVPixelBufferUnlockBaseAddress(pixel_buffer, 1);
    }
}

fn on_audio_frame(output_addr: usize, sample_buffer: CMSampleBufferRef) {
    if sample_buffer.is_null() {
        return;
    }
    let state = {
        let map = capture_state_registry().lock().unwrap();
        match map.get(&output_addr) {
            Some(sp) => unsafe { &*sp.0 },
            None => return,
        }
    };
    let Some(audio_tx) = state.audio_tx.as_ref() else {
        return;
    };

    let result = unsafe { copy_audio_samples(sample_buffer) };
    let (samples, sample_rate, channels, timestamp_us) = match result {
        Ok(frame) => frame,
        Err(error) => {
            let dropped = state.audio_drop_count.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped == 1 || dropped % 100 == 0 {
                tracing::warn!(
                    "[orbshare:system-audio] dropped invalid PCM buffer #{dropped}: {error}"
                );
            }
            return;
        }
    };
    if samples.is_empty() {
        return;
    }

    let frame = AudioFrame {
        samples,
        timestamp_us,
        sample_rate,
        channels,
    };
    match audio_tx.try_send(frame) {
        Ok(()) => {
            state.audio_frame_count.fetch_add(1, Ordering::Relaxed);
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            let dropped = state.audio_drop_count.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped == 1 || dropped % 100 == 0 {
                tracing::warn!(
                    "[orbshare:system-audio] PCM channel full; dropped frame #{dropped}"
                );
            }
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {}
    }
}

unsafe fn copy_audio_samples(
    sample_buffer: CMSampleBufferRef,
) -> Result<(Vec<f32>, u32, u16, u64), String> {
    let description = CMSampleBufferGetFormatDescription(sample_buffer);
    if description.is_null() {
        return Err("missing audio format description".to_string());
    }
    let asbd_ptr = CMAudioFormatDescriptionGetStreamBasicDescription(description);
    if asbd_ptr.is_null() {
        return Err("missing audio stream description".to_string());
    }
    let asbd = *asbd_ptr;
    if asbd.format_id != K_AUDIO_FORMAT_LINEAR_PCM {
        return Err(format!(
            "unsupported audio format id 0x{:08x}",
            asbd.format_id
        ));
    }
    if asbd.format_flags & K_AUDIO_FORMAT_FLAG_IS_BIG_ENDIAN != 0 {
        return Err("big-endian PCM is unsupported".to_string());
    }
    let channels = asbd.channels_per_frame.clamp(1, 2) as usize;
    let frame_count = CMSampleBufferGetNumSamples(sample_buffer).max(0) as usize;
    if frame_count == 0 {
        return Ok((
            Vec::new(),
            asbd.sample_rate.round() as u32,
            channels as u16,
            0,
        ));
    }

    let mut list = std::mem::MaybeUninit::<AudioBufferList2>::zeroed();
    let mut needed = 0usize;
    let mut block_buffer: CMBlockBufferRef = std::ptr::null_mut();
    let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sample_buffer,
        &mut needed,
        list.as_mut_ptr(),
        std::mem::size_of::<AudioBufferList2>(),
        std::ptr::null(),
        std::ptr::null(),
        0,
        &mut block_buffer,
    );
    if status != 0 {
        return Err(format!(
            "CMSampleBuffer audio list failed with status {status} (needed {needed} bytes)"
        ));
    }
    let list = list.assume_init();
    let samples = audio_buffer_list_to_f32(&asbd, &list, frame_count);
    if !block_buffer.is_null() {
        CFRelease(block_buffer);
    }
    let samples = samples?;

    let timestamp = CMSampleBufferGetPresentationTimeStamp(sample_buffer);
    let timestamp_us = if timestamp.timescale > 0 {
        ((timestamp.value as i128 * 1_000_000i128) / timestamp.timescale as i128).max(0) as u64
    } else {
        0
    };
    Ok((
        samples,
        asbd.sample_rate.round() as u32,
        channels as u16,
        timestamp_us,
    ))
}

fn audio_buffer_list_to_f32(
    asbd: &AudioStreamBasicDescription,
    list: &AudioBufferList2,
    frame_count: usize,
) -> Result<Vec<f32>, String> {
    let channels = asbd.channels_per_frame.clamp(1, 2) as usize;
    let buffer_count = (list.number_buffers as usize).min(list.buffers.len());
    if buffer_count == 0 {
        return Err("audio buffer list is empty".to_string());
    }
    let non_interleaved =
        asbd.format_flags & K_AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED != 0 || buffer_count == channels;
    let is_float = asbd.format_flags & K_AUDIO_FORMAT_FLAG_IS_FLOAT != 0;
    let is_signed_integer = asbd.format_flags & K_AUDIO_FORMAT_FLAG_IS_SIGNED_INTEGER != 0;
    let bytes_per_sample = (asbd.bits_per_channel / 8) as usize;
    if bytes_per_sample == 0 {
        return Err("audio sample width is zero".to_string());
    }

    let mut output = Vec::with_capacity(frame_count * channels);
    for frame in 0..frame_count {
        for channel in 0..channels {
            let buffer_index = if non_interleaved {
                channel.min(buffer_count - 1)
            } else {
                0
            };
            let sample_index = if non_interleaved {
                frame
            } else {
                frame * channels + channel
            };
            let buffer = &list.buffers[buffer_index];
            if buffer.data.is_null() {
                return Err("audio buffer data is null".to_string());
            }
            let offset = sample_index.saturating_mul(bytes_per_sample);
            if offset + bytes_per_sample > buffer.data_byte_size as usize {
                return Err(format!(
                    "audio buffer too short for frame {frame}, channel {channel}"
                ));
            }
            let ptr = unsafe { (buffer.data as *const u8).add(offset) };
            let sample = match (is_float, is_signed_integer, asbd.bits_per_channel) {
                (true, _, 32) => unsafe { std::ptr::read_unaligned(ptr.cast::<f32>()) },
                (true, _, 64) => unsafe { std::ptr::read_unaligned(ptr.cast::<f64>()) as f32 },
                (_, true, 16) => unsafe {
                    std::ptr::read_unaligned(ptr.cast::<i16>()) as f32 / 32_768.0
                },
                (_, true, 32) => unsafe {
                    std::ptr::read_unaligned(ptr.cast::<i32>()) as f32 / 2_147_483_648.0
                },
                _ => {
                    return Err(format!(
                        "unsupported PCM flags=0x{:x}, bits={}",
                        asbd.format_flags, asbd.bits_per_channel
                    ));
                }
            };
            output.push(sample.clamp(-1.0, 1.0));
        }
    }
    Ok(output)
}

fn copy_nv12_pixel_buffer(
    pixel_buffer: CVPixelBufferRef,
    width: usize,
    height: usize,
) -> Option<Vec<u8>> {
    if unsafe { CVPixelBufferGetPlaneCount(pixel_buffer) } < 2 {
        return None;
    }
    let y_base = unsafe { CVPixelBufferGetBaseAddressOfPlane(pixel_buffer, 0) };
    let uv_base = unsafe { CVPixelBufferGetBaseAddressOfPlane(pixel_buffer, 1) };
    if y_base.is_null() || uv_base.is_null() {
        return None;
    }
    let y_stride = unsafe { CVPixelBufferGetBytesPerRowOfPlane(pixel_buffer, 0) };
    let uv_stride = unsafe { CVPixelBufferGetBytesPerRowOfPlane(pixel_buffer, 1) };
    let y = unsafe { std::slice::from_raw_parts(y_base as *const u8, y_stride * height) };
    let uv_height = height / 2;
    let uv = unsafe { std::slice::from_raw_parts(uv_base as *const u8, uv_stride * uv_height) };
    convert_nv12_to_yuv420(y, y_stride, uv, uv_stride, width, height)
}

/// Copies ScreenCaptureKit's hardware-friendly NV12 planes into the existing
/// planar YUV420 frame contract. This is a memory-only conversion; unlike the
/// BGRA fallback it performs no per-pixel RGB color math, which keeps Retina
/// capture viable at 20fps in the bundled application.
fn convert_nv12_to_yuv420(
    y_src: &[u8],
    y_stride: usize,
    uv_src: &[u8],
    uv_stride: usize,
    width: usize,
    height: usize,
) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return None;
    }
    let uv_width = width / 2;
    let uv_height = height / 2;
    if y_stride < width
        || uv_stride < width
        || y_src.len() < y_stride * height
        || uv_src.len() < uv_stride * uv_height
    {
        return None;
    }

    let y_size = width * height;
    let uv_size = uv_width * uv_height;
    let mut output = vec![0u8; y_size + uv_size * 2];
    for row in 0..height {
        output[row * width..(row + 1) * width]
            .copy_from_slice(&y_src[row * y_stride..row * y_stride + width]);
    }
    let (u_dst, v_dst) = output[y_size..].split_at_mut(uv_size);
    for row in 0..uv_height {
        let src_row = &uv_src[row * uv_stride..row * uv_stride + width];
        for col in 0..uv_width {
            u_dst[row * uv_width + col] = src_row[col * 2];
            v_dst[row * uv_width + col] = src_row[col * 2 + 1];
        }
    }
    Some(output)
}

fn y_from_rgb(r: u8, g: u8, b: u8) -> u8 {
    let r = r as i32;
    let g = g as i32;
    let b = b as i32;
    ((77 * r + 150 * g + 29 * b) >> 8).clamp(0, 255) as u8
}

fn uv_from_rgb(r: u8, g: u8, b: u8) -> (u8, u8) {
    let r = r as i32;
    let g = g as i32;
    let b = b as i32;
    let u = (((-43 * r - 85 * g + 128 * b) >> 8) + 128).clamp(0, 255) as u8;
    let v = (((128 * r - 107 * g - 21 * b) >> 8) + 128).clamp(0, 255) as u8;
    (u, v)
}

fn bgra_at(src: &[u8], offset: usize) -> Option<(u8, u8, u8)> {
    if offset + 3 >= src.len() {
        return None;
    }
    let b = src[offset];
    let g = src[offset + 1];
    let r = src[offset + 2];
    Some((b, g, r))
}

/// Converts BGRA (bottom-left origin, with stride) to YUV420 planar (top-left origin).
fn convert_bgra_to_yuv420(
    src: &[u8],
    width: usize,
    height: usize,
    bytes_per_row: usize,
) -> Option<Vec<u8>> {
    let y_size = width * height;
    let uv_w = width / 2;
    let uv_h = height / 2;
    let uv_size = uv_w * uv_h;
    let total = y_size + uv_size * 2;
    let mut yuv = vec![0u8; total];

    let (y_part, rest) = yuv.split_at_mut(y_size);
    let (u_part, v_part) = rest.split_at_mut(uv_size);

    for row in 0..height {
        let src_row = height - 1 - row; // flip vertically
        let src_offset = src_row * bytes_per_row;

        for col in 0..width {
            let px = src_offset + col * 4;
            let Some((b, g, r)) = bgra_at(src, px) else {
                continue;
            };
            y_part[row * width + col] = y_from_rgb(r, g, b);

            if row % 2 == 0 && col % 2 == 0 {
                let mut r_sum = r as u16;
                let mut g_sum = g as u16;
                let mut b_sum = b as u16;
                let mut samples = 1u16;
                for (dr, dc) in [(0usize, 1usize), (1, 0), (1, 1)] {
                    let rr = row + dr;
                    let cc = col + dc;
                    if rr >= height || cc >= width {
                        continue;
                    }
                    let src_rr = height - 1 - rr;
                    let p = src_rr * bytes_per_row + cc * 4;
                    if let Some((bb, gg, rr)) = bgra_at(src, p) {
                        b_sum += bb as u16;
                        g_sum += gg as u16;
                        r_sum += rr as u16;
                        samples += 1;
                    }
                }
                let avg_r = (r_sum / samples) as u8;
                let avg_g = (g_sum / samples) as u8;
                let avg_b = (b_sum / samples) as u8;
                let (u, v) = uv_from_rgb(avg_r, avg_g, avg_b);
                let idx = (row / 2) * uv_w + (col / 2);
                u_part[idx] = u;
                v_part[idx] = v;
            }
        }
    }

    Some(yuv)
}

// ── MacOSCaptureProvider ─────────────────────────────────────────────────────

pub struct MacOSCaptureProvider {
    stop_tx: Option<watch::Sender<bool>>,
    state_ptr: *const CaptureState,
    stream_ptr: *mut Object,
    output_ptr: *mut Object,
}

unsafe impl Send for MacOSCaptureProvider {}
unsafe impl Sync for MacOSCaptureProvider {}

impl MacOSCaptureProvider {
    pub fn new() -> Self {
        Self {
            stop_tx: None,
            state_ptr: std::ptr::null(),
            stream_ptr: std::ptr::null_mut(),
            output_ptr: std::ptr::null_mut(),
        }
    }
}

impl Default for MacOSCaptureProvider {
    fn default() -> Self {
        Self::new()
    }
}

pub fn check_screen_capture_permission_impl() -> bool {
    if SCREEN_CAPTURE_PERMISSION_GRANTED.load(Ordering::Acquire) {
        return true;
    }
    unsafe {
        let image = CGDisplayCreateImage(CGMainDisplayID());
        if image.is_null() {
            return false;
        }
        CGImageRelease(image);
        SCREEN_CAPTURE_PERMISSION_GRANTED.store(true, Ordering::Release);
        true
    }
}

impl Drop for MacOSCaptureProvider {
    fn drop(&mut self) {
        if !self.state_ptr.is_null() || !self.stream_ptr.is_null() {
            let _ = self.stop_capture();
        }
    }
}

impl CaptureProvider for MacOSCaptureProvider {
    fn request_permission(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<bool, String>> + Send + '_>> {
        Box::pin(async move {
            if check_screen_capture_permission_impl() {
                info!("[macOS] Screen recording permission already granted");
                return Ok(true);
            }
            info!("[macOS] Screen recording permission not granted. Directing user to System Settings.");
            Err("Screen recording permission not granted. Grant access in System Settings > Privacy & Security > Screen Recording, then restart OrbShare.".to_string())
        })
    }

    fn start_capture(&mut self, config: CaptureConfig) -> Result<CaptureStream, String> {
        let use_encoded = config.video_transport == VideoTransportMode::WebRtcVideoTrack;
        let requires_window_capture = !config.source_is_display;
        let fps = config.fps.max(1);
        let bitrate = config.video_bitrate;
        let keyframe_interval_seconds = config.keyframe_interval_seconds;
        let enable_sck = std::env::var("ORBSHARE_ENABLE_SCK")
            .map(|value| value != "0" && !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true); // Default to true for better performance
        let mut stream = if (use_encoded || requires_window_capture) && enable_sck {
            let sck_config = config.clone();
            match unsafe { start_sck_capture(self, sck_config) } {
                Ok(stream) => stream,
                Err(err) => {
                    if requires_window_capture {
                        // Falling back to the primary display here would leak
                        // unrelated applications after the host explicitly
                        // chose one window.
                        return Err(format!("无法采集所选窗口：{err}"));
                    }
                    tracing::warn!(
                        "[macOS] ScreenCaptureKit capture failed, falling back to CoreGraphics: {err}"
                    );
                    unsafe { start_coregraphics_capture(self, config) }?
                }
            }
        } else if requires_window_capture {
            return Err("窗口共享需要 ScreenCaptureKit，请启用屏幕录制权限后重试。".to_string());
        } else {
            unsafe { start_coregraphics_capture(self, config) }?
        };

        if use_encoded {
            let (encoded_tx, encoded_rx) = mpsc::channel::<EncodedVideoFrame>(VIDEO_FRAME_BUFFER);
            let (preview_tx, preview_rx) = mpsc::channel::<EncodedFrame>(1);
            let mut raw_rx = stream.video_rx;
            tokio::spawn(async move {
                let mut encoder: Option<VideoToolboxEncoder> = None;
                let mut last_preview_at = Instant::now() - HOST_PREVIEW_RAW_INTERVAL;
                while let Some(frame) = raw_rx.recv().await {
                    // Tee at most one raw frame per second to the host monitor.
                    // The bounded channel and capacity check ensure preview work
                    // can never backpressure the H.264 encoding path.
                    if last_preview_at.elapsed() >= HOST_PREVIEW_RAW_INTERVAL
                        && preview_tx.capacity() > 0
                    {
                        let preview = EncodedFrame {
                            data: frame.data.clone(),
                            timestamp_us: frame.timestamp_us,
                            is_keyframe: frame.is_keyframe,
                            width: frame.width,
                            height: frame.height,
                        };
                        if preview_tx.try_send(preview).is_ok() {
                            last_preview_at = Instant::now();
                        }
                    }
                    if encoder.is_none() {
                        match VideoToolboxEncoder::new_with_options(
                            frame.width,
                            frame.height,
                            fps,
                            bitrate,
                            keyframe_interval_seconds,
                        ) {
                            Ok(next_encoder) => {
                                tracing::info!(
                                    "[macOS] VideoToolbox H.264 encoder ready: {}x{} @ {}fps, bitrate={} kbps",
                                    frame.width,
                                    frame.height,
                                    fps,
                                    bitrate / 1000
                                );
                                encoder = Some(next_encoder);
                            }
                            Err(err) => {
                                tracing::error!(
                                    "[macOS] Failed to create VideoToolbox H.264 encoder: {err}"
                                );
                                break;
                            }
                        }
                    }

                    let Some(encoder) = encoder.as_mut() else {
                        break;
                    };
                    match encoder.encode(&frame.data, frame.width, frame.height) {
                        Ok(encoded_frames) => {
                            for mut encoded in encoded_frames {
                                encoded.width = frame.width;
                                encoded.height = frame.height;
                                // Encoded H.264 P-frames form a reference chain.
                                // Dropping one here corrupts every dependent frame
                                // until the next IDR, which is most visible while
                                // typing or scrolling. Apply backpressure after
                                // encoding; the bounded raw-frame channel upstream
                                // is the safe place to discard stale screen images.
                                if encoded_tx.send(encoded).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Err(err) => {
                            tracing::error!("[macOS] H.264 encode failed: {err}");
                            break;
                        }
                    }
                }
            });

            let (_unused_video_tx, video_rx) = mpsc::channel::<EncodedFrame>(1);
            stream.video_rx = video_rx;
            stream.encoded_video_rx = Some(encoded_rx);
            stream.preview_video_rx = Some(preview_rx);
        }

        Ok(stream)
    }

    fn stop_capture(&mut self) -> Result<(), String> {
        info!("[macOS] Stopping ScreenCaptureKit capture");

        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(true);
        }

        // Stop the SCStream synchronously
        if !self.stream_ptr.is_null() {
            unsafe {
                // Use a channel to wait for the completion handler
                let (tx, rx) = std::sync::mpsc::channel();
                let block = block::ConcreteBlock::new(move |_err: *mut Object| {
                    let _ = tx.send(());
                });
                let block = block.copy();
                let _: () = msg_send![self.stream_ptr, stopCaptureWithCompletionHandler: block];
                let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
            }
        }

        // Remove from registry and free state
        if !self.state_ptr.is_null() {
            let addr = self.output_ptr as usize;
            {
                let mut map = capture_state_registry().lock().unwrap();
                map.remove(&addr);
            }
            unsafe {
                drop(Box::from_raw(self.state_ptr as *mut CaptureState));
            }
            self.state_ptr = std::ptr::null();
        }

        // Release ObjC objects
        if !self.stream_ptr.is_null() {
            unsafe {
                let _: () = msg_send![self.stream_ptr, release];
            }
            self.stream_ptr = std::ptr::null_mut();
        }
        if !self.output_ptr.is_null() {
            unsafe {
                let _: () = msg_send![self.output_ptr, release];
            }
            self.output_ptr = std::ptr::null_mut();
        }

        Ok(())
    }

    fn list_sources(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CaptureSource>, String>> + Send + '_>> {
        Box::pin(async { list_sck_sources() })
    }

    fn capture_stats(&self) -> Option<CaptureStats> {
        if self.state_ptr.is_null() {
            return None;
        }
        let state = unsafe { &*self.state_ptr };
        Some(CaptureStats {
            width: state.width.load(Ordering::Relaxed),
            height: state.height.load(Ordering::Relaxed),
            fps: state.fps.load(Ordering::Relaxed),
            pixel_format: "BGRA→YUV420 (ScreenCaptureKit)".to_string(),
            frame_count: state.frame_count.load(Ordering::Relaxed),
        })
    }
}

// ── SCK helpers ──────────────────────────────────────────────────────────────

unsafe fn start_coregraphics_capture(
    provider: &mut MacOSCaptureProvider,
    config: CaptureConfig,
) -> Result<CaptureStream, String> {
    if !config.source_is_display {
        return Err("CoreGraphics 兼容模式不支持窗口级共享。".to_string());
    }
    if config.capture_system_audio {
        return Err(
            "共享系统声音需要 ScreenCaptureKit，当前 CoreGraphics 兼容采集不支持。".to_string(),
        );
    }
    let display_id = config.source_id.unwrap_or_else(|| CGMainDisplayID());
    let first_frame = capture_display_frame(
        display_id,
        0,
        config.fps.max(1) as u64,
        config.width,
        config.height,
    )?;
    let width = first_frame.width;
    let height = first_frame.height;

    info!(
        "[macOS] Starting CoreGraphics screen capture: {}x{} @ {}fps",
        width, height, config.fps
    );

    let (video_tx, video_rx) = mpsc::channel::<EncodedFrame>(VIDEO_FRAME_BUFFER);
    let state = Box::new(CaptureState {
        tx: video_tx.clone(),
        audio_tx: None,
        width: AtomicU32::new(width),
        height: AtomicU32::new(height),
        fps: AtomicU32::new(config.fps),
        frame_count: AtomicU64::new(0),
        audio_frame_count: AtomicU64::new(0),
        audio_drop_count: AtomicU64::new(0),
        frame_gate: Mutex::new(FrameRateGate::new(config.fps)),
    });
    let state_ptr = Box::into_raw(state);
    provider.state_ptr = state_ptr;

    let (stop_tx, mut stop_rx) = watch::channel(false);
    provider.stop_tx = Some(stop_tx);

    let fps = config.fps.max(1);
    tokio::spawn(async move {
        let frame_interval = tokio::time::Duration::from_micros(1_000_000 / fps as u64);
        let mut ticker = tokio::time::interval(frame_interval);
        // CRITICAL: when a frame capture takes longer than the tick period
        // (e.g. 88ms CG capture vs 33ms @30fps), the default Burst behavior
        // makes tick() return immediately forever — the task never suspends,
        // monopolizes its worker thread, and any task it wakes via
        // try_send (the H.264 encoder task) lands in this worker's
        // non-stealable LIFO slot and is never polled. Symptom: the encoder
        // goes silent for tens of seconds, emitting exactly 2 frames (the
        // raw channel capacity) whenever an unrelated event flushes the
        // LIFO slot. Delay makes the ticker actually sleep between frames,
        // yielding the worker so woken tasks get scheduled.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut frame_count = 0u64;

        let _ = video_tx.try_send(first_frame);
        frame_count += 1;

        loop {
            tokio::select! {
                r = stop_rx.changed() => {
                    if r.is_err() || *stop_rx.borrow() {
                        break;
                    }
                }
                _ = ticker.tick() => {
                    let t0 = std::time::Instant::now();
                    // CGDisplayCreateImage / CGContextDrawImage internally
                    // autorelease Objective-C objects (including full-screen
                    // IOSurface wrappers). Without a pool they accumulate on
                    // this worker thread forever: memory pressure climbs and
                    // each capture gets progressively slower (observed 60ms
                    // -> ~5s per frame). Drain the pool every frame.
                    let capture_result = objc::rc::autoreleasepool(|| {
                        unsafe {
                            capture_display_frame(display_id, frame_count, fps as u64, config.width, config.height)
                        }
                    });
                    let t_cap = t0.elapsed();
                    match capture_result {
                        Ok(frame) => {
                            match video_tx.try_send(frame) {
                                Ok(()) => {
                                    frame_count += 1;
                                    if frame_count % 5 == 0 {
                                        tracing::warn!(
                                            "[orbshare:cap] frame_n={} capture_dt={}ms (target per-frame {}ms @ {}fps)",
                                            frame_count,
                                            t_cap.as_millis(),
                                            1000 / fps.max(1),
                                            fps
                                        );
                                    }
                                }
                                Err(mpsc::error::TrySendError::Full(_)) => {
                                    frame_count += 1;
                                    if frame_count % 5 == 0 {
                                        tracing::warn!(
                                            "[orbshare:cap] frame_n={} capture_dt={}ms ENC_DROP (channel full)",
                                            frame_count,
                                            t_cap.as_millis()
                                        );
                                    }
                                }
                                Err(mpsc::error::TrySendError::Closed(_)) => break,
                            }
                        }
                        Err(err) => {
                            tracing::warn!("[macOS] CoreGraphics frame capture failed: {}", err);
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        }
                    }
                }
            }
        }
        info!("[macOS] CoreGraphics capture stopped");
    });

    Ok(CaptureStream {
        video_rx,
        encoded_video_rx: None,
        preview_video_rx: None,
        audio_rx: None,
    })
}

unsafe fn capture_display_frame(
    display_id: CGDirectDisplayID,
    frame_count: u64,
    fps: u64,
    target_width: u32,
    target_height: u32,
) -> Result<EncodedFrame, String> {
    let image = CGDisplayCreateImage(display_id);
    if image.is_null() {
        return Err(
            "CGDisplayCreateImage returned null. Check Screen Recording permission.".into(),
        );
    }

    let source_width = CGImageGetWidth(image);
    let source_height = CGImageGetHeight(image);
    if source_width == 0 || source_height == 0 {
        CGImageRelease(image);
        return Err("CGDisplayCreateImage returned empty image".into());
    }

    let (width, height) = fit_dimensions(
        source_width,
        source_height,
        target_width as usize,
        target_height as usize,
    );
    let bytes_per_row = width * 4;
    let mut bgra = vec![0u8; bytes_per_row * height];
    let color_space = CGColorSpaceCreateDeviceRGB();
    if color_space.is_null() {
        CGImageRelease(image);
        return Err("CGColorSpaceCreateDeviceRGB returned null".into());
    }

    let bitmap_info = K_CG_IMAGE_ALPHA_PREMULTIPLIED_FIRST | K_CG_BITMAP_BYTE_ORDER_32_LITTLE;
    let context = CGBitmapContextCreate(
        bgra.as_mut_ptr() as *mut c_void,
        width,
        height,
        8,
        bytes_per_row,
        color_space,
        bitmap_info,
    );
    if context.is_null() {
        CGColorSpaceRelease(color_space);
        CGImageRelease(image);
        return Err("CGBitmapContextCreate returned null".into());
    }

    let rect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: width as f64,
            height: height as f64,
        },
    };
    CGContextDrawImage(context, rect, image);
    CGContextRelease(context);
    CGColorSpaceRelease(color_space);
    CGImageRelease(image);

    let yuv = convert_bgra_to_yuv420_top_down(&bgra, width, height, bytes_per_row)
        .ok_or_else(|| "Failed to convert display frame to YUV420".to_string())?;

    Ok(EncodedFrame {
        data: yuv,
        timestamp_us: frame_count * 1_000_000 / fps.max(1),
        is_keyframe: frame_count % 120 == 0,
        width: width as u32,
        height: height as u32,
    })
}

fn fit_dimensions(
    source_width: usize,
    source_height: usize,
    max_width: usize,
    max_height: usize,
) -> (usize, usize) {
    let max_width = max_width.max(2);
    let max_height = max_height.max(2);
    let source_width = source_width.max(2);
    let source_height = source_height.max(2);
    let scale = (max_width as f64 / source_width as f64)
        .min(max_height as f64 / source_height as f64)
        .min(1.0);

    // YUV420 requires even dimensions. Preserve the display's aspect ratio
    // instead of stretching it into the quality tier's bounding box.
    let width = ((source_width as f64 * scale).round() as usize)
        .min(max_width)
        .max(2)
        & !1;
    let height = ((source_height as f64 * scale).round() as usize)
        .min(max_height)
        .max(2)
        & !1;
    (width, height)
}

fn scale_point_dimensions(width: usize, height: usize, point_pixel_scale: f64) -> (usize, usize) {
    let scale = if point_pixel_scale.is_finite() {
        point_pixel_scale.clamp(1.0, 4.0)
    } else {
        1.0
    };
    (
        ((width.max(2) as f64 * scale).round() as usize).max(2),
        ((height.max(2) as f64 * scale).round() as usize).max(2),
    )
}

/// `SCDisplay.width/height` and `CGDisplayPixelsWide/High` report logical
/// coordinates for a scaled Retina mode. Video capture must instead use the
/// display mode's physical pixel dimensions or the browser will have to enlarge
/// a 1x image on a 2x screen, making text look blurred even at a high bitrate.
unsafe fn display_physical_pixel_dimensions(
    display_id: CGDirectDisplayID,
    logical_width: usize,
    logical_height: usize,
) -> (usize, usize) {
    let mode = CGDisplayCopyDisplayMode(display_id);
    if mode.is_null() {
        return (logical_width, logical_height);
    }
    let pixel_width = CGDisplayModeGetPixelWidth(mode);
    let pixel_height = CGDisplayModeGetPixelHeight(mode);
    CFRelease(mode);
    if pixel_width == 0 || pixel_height == 0 {
        (logical_width, logical_height)
    } else {
        (pixel_width, pixel_height)
    }
}

#[cfg(test)]
mod dimension_tests {
    use super::{
        audio_buffer_list_to_f32, convert_nv12_to_yuv420, fit_dimensions, list_sck_sources,
        scale_point_dimensions, AudioBuffer, AudioBufferList2, AudioStreamBasicDescription,
        FrameRateGate, K_AUDIO_FORMAT_FLAG_IS_FLOAT, K_AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED,
        K_AUDIO_FORMAT_LINEAR_PCM,
    };
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    #[test]
    fn preserves_aspect_ratio_when_downscaling() {
        assert_eq!(fit_dimensions(1800, 1169, 1280, 720), (1108, 720));
        assert_eq!(fit_dimensions(1800, 1169, 1920, 1080), (1662, 1080));
    }

    #[test]
    fn does_not_upscale_smaller_displays() {
        assert_eq!(fit_dimensions(1280, 720, 1920, 1080), (1280, 720));
    }

    #[test]
    fn fits_retina_physical_pixels_into_clear_text_profile() {
        assert_eq!(fit_dimensions(3600, 2338, 2880, 1800), (2772, 1800));
        assert_eq!(fit_dimensions(3600, 2338, 3840, 2160), (3326, 2160));
    }

    #[test]
    fn scales_window_points_to_retina_pixels_before_fitting() {
        let retina_pixels = scale_point_dimensions(1800, 1064, 2.0);
        assert_eq!(retina_pixels, (3600, 2128));
        assert_eq!(
            fit_dimensions(retina_pixels.0, retina_pixels.1, 2880, 1800),
            (2880, 1702)
        );
    }

    #[test]
    fn converts_strided_nv12_without_rgb_color_conversion() {
        let y = [10, 11, 12, 13, 0, 0, 20, 21, 22, 23, 0, 0];
        let uv = [31, 41, 32, 42, 0, 0];
        let planar = convert_nv12_to_yuv420(&y, 6, &uv, 6, 4, 2).expect("convert NV12");
        assert_eq!(planar, [10, 11, 12, 13, 20, 21, 22, 23, 31, 32, 41, 42]);
    }

    #[test]
    fn frame_gate_enforces_selected_fps_without_bursts() {
        let start = Instant::now();
        let mut gate = FrameRateGate::new(20);

        assert!(gate.should_emit(start));
        assert!(!gate.should_emit(start + Duration::from_millis(16)));
        assert!(!gate.should_emit(start + Duration::from_millis(49)));
        assert!(gate.should_emit(start + Duration::from_millis(50)));
        assert!(!gate.should_emit(start + Duration::from_millis(66)));
        assert!(gate.should_emit(start + Duration::from_millis(100)));
    }

    #[test]
    fn source_listing_is_synchronous_and_returns_a_display() {
        let started = Instant::now();
        let sources = list_sck_sources().expect("list CoreGraphics capture sources");
        assert!(sources.iter().any(|source| source.is_display));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "source metadata listing must never wait on an asynchronous callback"
        );
    }

    #[test]
    fn converts_interleaved_float32_stereo_pcm() {
        let mut samples = [0.25f32, -0.25, 0.75, -0.75];
        let list = AudioBufferList2 {
            number_buffers: 1,
            buffers: [
                AudioBuffer {
                    number_channels: 2,
                    data_byte_size: std::mem::size_of_val(&samples) as u32,
                    data: samples.as_mut_ptr().cast::<c_void>(),
                },
                AudioBuffer {
                    number_channels: 0,
                    data_byte_size: 0,
                    data: std::ptr::null_mut(),
                },
            ],
        };
        let format = AudioStreamBasicDescription {
            sample_rate: 48_000.0,
            format_id: K_AUDIO_FORMAT_LINEAR_PCM,
            format_flags: K_AUDIO_FORMAT_FLAG_IS_FLOAT,
            bytes_per_packet: 8,
            frames_per_packet: 1,
            bytes_per_frame: 8,
            channels_per_frame: 2,
            bits_per_channel: 32,
            reserved: 0,
        };

        assert_eq!(
            audio_buffer_list_to_f32(&format, &list, 2).expect("convert interleaved PCM"),
            samples
        );
    }

    #[test]
    fn interleaves_planar_float32_stereo_pcm() {
        let mut left = [0.1f32, 0.3];
        let mut right = [0.2f32, 0.4];
        let list = AudioBufferList2 {
            number_buffers: 2,
            buffers: [
                AudioBuffer {
                    number_channels: 1,
                    data_byte_size: std::mem::size_of_val(&left) as u32,
                    data: left.as_mut_ptr().cast::<c_void>(),
                },
                AudioBuffer {
                    number_channels: 1,
                    data_byte_size: std::mem::size_of_val(&right) as u32,
                    data: right.as_mut_ptr().cast::<c_void>(),
                },
            ],
        };
        let format = AudioStreamBasicDescription {
            sample_rate: 48_000.0,
            format_id: K_AUDIO_FORMAT_LINEAR_PCM,
            format_flags: K_AUDIO_FORMAT_FLAG_IS_FLOAT | K_AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED,
            bytes_per_packet: 4,
            frames_per_packet: 1,
            bytes_per_frame: 4,
            channels_per_frame: 2,
            bits_per_channel: 32,
            reserved: 0,
        };

        assert_eq!(
            audio_buffer_list_to_f32(&format, &list, 2).expect("convert planar PCM"),
            [0.1, 0.2, 0.3, 0.4]
        );
    }
}

fn convert_bgra_to_yuv420_top_down(
    src: &[u8],
    width: usize,
    height: usize,
    bytes_per_row: usize,
) -> Option<Vec<u8>> {
    let y_size = width * height;
    let uv_w = width / 2;
    let uv_h = height / 2;
    let uv_size = uv_w * uv_h;
    let total = y_size + uv_size * 2;
    let mut yuv = vec![0u8; total];

    let (y_part, rest) = yuv.split_at_mut(y_size);
    let (u_part, v_part) = rest.split_at_mut(uv_size);

    for row in 0..height {
        let src_offset = row * bytes_per_row;
        for col in 0..width {
            let px = src_offset + col * 4;
            let Some((b, g, r)) = bgra_at(src, px) else {
                continue;
            };
            y_part[row * width + col] = y_from_rgb(r, g, b);
            if row % 2 == 0 && col % 2 == 0 {
                let mut r_sum = r as u16;
                let mut g_sum = g as u16;
                let mut b_sum = b as u16;
                let mut samples = 1u16;
                for (dr, dc) in [(0usize, 1usize), (1, 0), (1, 1)] {
                    let rr = row + dr;
                    let cc = col + dc;
                    if rr >= height || cc >= width {
                        continue;
                    }
                    let p = rr * bytes_per_row + cc * 4;
                    if let Some((bb, gg, rr)) = bgra_at(src, p) {
                        b_sum += bb as u16;
                        g_sum += gg as u16;
                        r_sum += rr as u16;
                        samples += 1;
                    }
                }
                let avg_r = (r_sum / samples) as u8;
                let avg_g = (g_sum / samples) as u8;
                let avg_b = (b_sum / samples) as u8;
                let (u, v) = uv_from_rgb(avg_r, avg_g, avg_b);
                let idx = (row / 2) * uv_w + (col / 2);
                u_part[idx] = u;
                v_part[idx] = v;
            }
        }
    }

    Some(yuv)
}

fn list_sck_sources() -> Result<Vec<CaptureSource>, String> {
    // Source discovery must not wait on SCShareableContent. macOS can omit its
    // completion callback while TCC is settling, leaving an unbounded recv()
    // and a permanently spinning picker. CoreGraphics exposes the same native
    // display/window IDs synchronously; ScreenCaptureKit resolves the selected
    // ID only when capture actually starts.
    let main_display_id = unsafe { CGMainDisplayID() };
    let own_pid = std::process::id() as i32;
    let display_ids = CGDisplay::active_displays()
        .map_err(|error| format!("读取显示器列表失败（CoreGraphics {error}）"))?;
    let mut display_sources = Vec::with_capacity(display_ids.len());
    for (index, display_id) in display_ids.into_iter().enumerate() {
        let display = CGDisplay::new(display_id);
        display_sources.push(CaptureSource {
            id: display_id,
            name: if display_id == main_display_id {
                "桌面 1（主屏幕）".to_string()
            } else {
                format!("桌面 {}", index + 1)
            },
            width: display.pixels_wide() as u32,
            height: display.pixels_high() as u32,
            is_display: true,
            thumbnail_data_url: None,
        });
    }
    display_sources.sort_by_key(|source| source.id != main_display_id);

    let window_info = copy_window_info(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    )
    .ok_or_else(|| "读取应用窗口列表失败。".to_string())?;
    let mut window_sources = Vec::new();
    for raw_info in window_info.iter() {
        if window_sources.len() >= 60 {
            break;
        }
        let info_type = unsafe { CFType::wrap_under_get_rule(*raw_info as _) };
        let Some(info) = info_type.downcast::<CFDictionary>() else {
            continue;
        };
        let window_id = cf_dictionary_i32(&info, unsafe { kCGWindowNumber }).unwrap_or(0) as u32;
        let layer = cf_dictionary_i32(&info, unsafe { kCGWindowLayer }).unwrap_or(-1);
        let pid = cf_dictionary_i32(&info, unsafe { kCGWindowOwnerPID }).unwrap_or(-1);
        if window_id == 0 || layer != 0 || pid == own_pid {
            continue;
        }
        let app_name =
            cf_dictionary_string(&info, unsafe { kCGWindowOwnerName }).unwrap_or_default();
        if app_name.trim().is_empty() {
            continue;
        }
        let title = cf_dictionary_string(&info, unsafe { kCGWindowName }).unwrap_or_default();
        let Some(bounds_value) = cf_dictionary_value(&info, unsafe { kCGWindowBounds }) else {
            continue;
        };
        let Some(bounds_dictionary) = bounds_value.downcast::<CFDictionary>() else {
            continue;
        };
        let mut bounds = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: 0.0,
                height: 0.0,
            },
        };
        let valid_bounds = unsafe {
            CGRectMakeWithDictionaryRepresentation(
                bounds_dictionary.as_concrete_TypeRef() as *const c_void,
                &mut bounds,
            )
        };
        let width = bounds.size.width.max(0.0).round() as u32;
        let height = bounds.size.height.max(0.0).round() as u32;
        if !valid_bounds || width < 160 || height < 90 {
            continue;
        }
        let display_name = if title.trim().is_empty() || title.trim() == app_name.trim() {
            app_name
        } else {
            format!("{} · {}", app_name.trim(), title.trim())
        };
        window_sources.push(CaptureSource {
            id: window_id,
            name: display_name,
            width,
            height,
            is_display: false,
            thumbnail_data_url: None,
        });
    }
    window_sources.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    display_sources.extend(window_sources);
    Ok(display_sources)
}

fn cf_dictionary_value(dictionary: &CFDictionary, key: CFStringRef) -> Option<CFType> {
    let raw_value = dictionary.find(key as *const c_void)?;
    Some(unsafe { CFType::wrap_under_get_rule(*raw_value as _) })
}

fn cf_dictionary_i32(dictionary: &CFDictionary, key: CFStringRef) -> Option<i32> {
    cf_dictionary_value(dictionary, key)?
        .downcast::<CFNumber>()?
        .to_i32()
}

fn cf_dictionary_string(dictionary: &CFDictionary, key: CFStringRef) -> Option<String> {
    Some(
        cf_dictionary_value(dictionary, key)?
            .downcast::<CFString>()?
            .to_string(),
    )
}

pub fn capture_source_thumbnail_impl(
    source_id: u32,
    source_is_display: bool,
) -> Result<Option<String>, String> {
    unsafe {
        let image = if source_is_display {
            CGDisplayCreateImage(source_id)
        } else {
            CGWindowListCreateImage(
                CGRectNull,
                K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW,
                source_id,
                K_CG_WINDOW_IMAGE_BOUNDS_IGNORE_FRAMING | K_CG_WINDOW_IMAGE_BEST_RESOLUTION,
            )
        };
        if image.is_null() {
            return Ok(None);
        }
        let thumbnail = cgimage_to_thumbnail_data_url(image);
        CGImageRelease(image);
        if thumbnail.is_some() {
            SCREEN_CAPTURE_PERMISSION_GRANTED.store(true, Ordering::Release);
        }
        Ok(thumbnail)
    }
}

unsafe fn cgimage_to_thumbnail_data_url(image: CGImageRef) -> Option<String> {
    if image.is_null() {
        return None;
    }
    let source_width = CGImageGetWidth(image);
    let source_height = CGImageGetHeight(image);
    if source_width == 0 || source_height == 0 {
        return None;
    }
    let (width, height) = fit_dimensions(source_width, source_height, 360, 220);
    let bytes_per_row = width * 4;
    let mut bgra = vec![0u8; bytes_per_row * height];
    let color_space = CGColorSpaceCreateDeviceRGB();
    if color_space.is_null() {
        return None;
    }
    let context = CGBitmapContextCreate(
        bgra.as_mut_ptr() as *mut c_void,
        width,
        height,
        8,
        bytes_per_row,
        color_space,
        K_CG_IMAGE_ALPHA_PREMULTIPLIED_FIRST | K_CG_BITMAP_BYTE_ORDER_32_LITTLE,
    );
    if context.is_null() {
        CGColorSpaceRelease(color_space);
        return None;
    }
    CGContextDrawImage(
        context,
        CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: width as f64,
                height: height as f64,
            },
        },
        image,
    );
    CGContextRelease(context);
    CGColorSpaceRelease(color_space);

    let mut rgb = Vec::with_capacity(width * height * 3);
    for pixel in bgra.chunks_exact(4) {
        rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 72)
        .encode(&rgb, width as u32, height as u32, ExtendedColorType::Rgb8)
        .ok()?;
    Some(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(jpeg)
    ))
}

unsafe fn start_sck_capture(
    provider: &mut MacOSCaptureProvider,
    config: CaptureConfig,
) -> Result<CaptureStream, String> {
    info!(
        "[macOS] Starting ScreenCaptureKit capture: {}x{} @ {}fps",
        config.width, config.height, config.fps
    );
    tracing::debug!(
        "[orbshare:sck] start {}x{} @ {}fps",
        config.width,
        config.height,
        config.fps
    );

    // 1. Resolve the selected full display or desktop-independent window.
    let (source_obj, source_w, source_h, fallback_pixel_scale, source_description) =
        if config.source_is_display {
            let display_id = config.source_id.unwrap_or_else(|| CGMainDisplayID());
            let (display_obj, display_w, display_h) = get_display(display_id)?;
            let (pixel_w, pixel_h) =
                display_physical_pixel_dimensions(display_id, display_w, display_h);
            (
                display_obj,
                pixel_w,
                pixel_h,
                1.0,
                format!("display {display_id} (logical={display_w}x{display_h})"),
            )
        } else {
            let window_id = config
                .source_id
                .ok_or("窗口共享缺少窗口标识，请重新选择共享内容。")?;
            let (window_obj, window_w, window_h, pixel_scale) = get_window(window_id)?;
            (
                window_obj,
                window_w,
                window_h,
                pixel_scale,
                format!("window {window_id} (logical={window_w}x{window_h})"),
            )
        };

    // 2. SCContentFilter
    tracing::debug!("[orbshare:sck] create filter");
    let filter_class = Class::get("SCContentFilter").ok_or("SCContentFilter not found")?;
    let filter_alloc: *mut Object = msg_send![filter_class, alloc];
    let filter: *mut Object = if config.source_is_display {
        let ns_array_class = Class::get("NSArray").ok_or("NSArray not found")?;
        let excluded_windows: *mut Object = msg_send![ns_array_class, array];
        msg_send![filter_alloc,
            initWithDisplay: source_obj
            excludingWindows: excluded_windows
        ]
    } else {
        let supports_window_filter: bool = msg_send![filter_alloc,
            respondsToSelector: sel!(initWithDesktopIndependentWindow:)
        ];
        if !supports_window_filter {
            let _: () = msg_send![source_obj, release];
            let _: () = msg_send![filter_alloc, release];
            return Err("当前 macOS 版本不支持独立窗口共享。".to_string());
        }
        msg_send![filter_alloc, initWithDesktopIndependentWindow: source_obj]
    };
    let _: () = msg_send![source_obj, release];
    if filter.is_null() {
        return Err("创建共享内容过滤器失败，请重新选择共享来源。".to_string());
    }

    // SCWindow and CoreGraphics window bounds are expressed in points. The
    // previous code tried to infer the pixel size from CGWindowListCreateImage,
    // which returns null while ScreenCaptureKit is active on recent macOS
    // releases. That silently forced every window to 1280x720. Resolve the
    // actual backing scale from the content filter (Retina is normally 2x),
    // with the window's display as a fallback for macOS 12/13.
    let pixel_scale = if config.source_is_display {
        1.0
    } else {
        content_filter_point_pixel_scale(filter).unwrap_or(fallback_pixel_scale)
    };
    let (pixel_w, pixel_h) = if config.source_is_display {
        (source_w, source_h)
    } else {
        scale_point_dimensions(source_w, source_h, pixel_scale)
    };
    let (stream_w, stream_h) = fit_dimensions(
        pixel_w,
        pixel_h,
        config.width as usize,
        config.height as usize,
    );
    info!(
        "[macOS] Selected {}: pixel_scale={:.2} source={}x{} stream={}x{}",
        source_description, pixel_scale, pixel_w, pixel_h, stream_w, stream_h
    );
    tracing::debug!(
        "[orbshare:sck] source {}x{} @ {:.2}x, stream {}x{}",
        source_w,
        source_h,
        pixel_scale,
        stream_w,
        stream_h
    );

    // 3. SCStreamConfiguration
    tracing::debug!("[orbshare:sck] create config");
    let cfg_class = Class::get("SCStreamConfiguration").ok_or("SCStreamConfiguration not found")?;
    let cfg_alloc: *mut Object = msg_send![cfg_class, alloc];
    let sc_config: *mut Object = msg_send![cfg_alloc, init];

    let _: () = msg_send![sc_config, setWidth: stream_w];
    let _: () = msg_send![sc_config, setHeight: stream_h];
    if !config.source_is_display {
        // Independent-window capture must be allowed to scale up from points
        // to its native Retina backing pixels.
        let _: () = msg_send![sc_config, setScalesToFit: true];
    }
    // Ask ScreenCaptureKit for NV12 so the system performs the colorspace
    // conversion. The previous BGRA path did expensive RGB math in Rust for
    // every pixel, which cannot sustain a Retina stream in a debug app build.
    let _: () = msg_send![sc_config, setPixelFormat: NV12_VIDEO_RANGE_PIXEL_FORMAT];
    if config.capture_system_audio {
        let supports_capture: bool = msg_send![sc_config,
            respondsToSelector: sel!(setCapturesAudio:)
        ];
        let supports_exclusion: bool = msg_send![sc_config,
            respondsToSelector: sel!(setExcludesCurrentProcessAudio:)
        ];
        let supports_sample_rate: bool = msg_send![sc_config,
            respondsToSelector: sel!(setSampleRate:)
        ];
        let supports_channels: bool = msg_send![sc_config,
            respondsToSelector: sel!(setChannelCount:)
        ];
        if !(supports_capture && supports_exclusion && supports_sample_rate && supports_channels) {
            let _: () = msg_send![filter, release];
            let _: () = msg_send![sc_config, release];
            return Err("当前 macOS 版本不支持 ScreenCaptureKit 系统声音采集。".to_string());
        }
        // ScreenCaptureKit does not capture audio unless explicitly enabled.
        // Keep OrbShare's own output excluded to prevent a feedback loop when
        // the host monitors meeting audio in the WebView.
        let _: () = msg_send![sc_config, setCapturesAudio: true];
        let _: () = msg_send![sc_config, setExcludesCurrentProcessAudio: true];
        let _: () = msg_send![sc_config, setSampleRate: 48_000isize];
        let _: () = msg_send![sc_config, setChannelCount: 2isize];
    }

    // 4. Create StreamOutput
    tracing::debug!("[orbshare:sck] create output");
    let output_cls = register_stream_output_class();
    let output_alloc: *mut Object = msg_send![output_cls, alloc];
    let output: *mut Object = msg_send![output_alloc, init];
    if output.is_null() {
        let _: () = msg_send![filter, release];
        let _: () = msg_send![sc_config, release];
        return Err("Failed to create StreamOutput".to_string());
    }

    // 5. CaptureState + registry
    let (video_tx, video_rx) = mpsc::channel::<EncodedFrame>(VIDEO_FRAME_BUFFER);
    let (audio_tx, audio_rx) = mpsc::channel::<AudioFrame>(CHANNEL_BUFFER);
    let state = Box::new(CaptureState {
        tx: video_tx,
        audio_tx: config.capture_system_audio.then_some(audio_tx),
        width: AtomicU32::new(stream_w as u32),
        height: AtomicU32::new(stream_h as u32),
        fps: AtomicU32::new(config.fps),
        frame_count: AtomicU64::new(0),
        audio_frame_count: AtomicU64::new(0),
        audio_drop_count: AtomicU64::new(0),
        frame_gate: Mutex::new(FrameRateGate::new(config.fps)),
    });
    let state_ptr = Box::into_raw(state);
    let output_addr = output as usize;

    {
        let mut map = capture_state_registry().lock().unwrap();
        map.insert(output_addr, SendPtr(state_ptr));
    }

    // 6. SCStream
    tracing::debug!("[orbshare:sck] create stream");
    let stream_class = Class::get("SCStream").ok_or("SCStream not found")?;
    let stream_alloc: *mut Object = msg_send![stream_class, alloc];
    let stream: *mut Object = msg_send![stream_alloc,
        initWithFilter: filter
        configuration: sc_config
        delegate: output
    ];
    let _: () = msg_send![filter, release];
    let _: () = msg_send![sc_config, release];

    if stream.is_null() {
        cleanup_sck(output_addr, state_ptr, stream, output);
        return Err("Failed to create SCStream".to_string());
    }

    // 7. Add output + start capture
    tracing::debug!("[orbshare:sck] add output");
    let queue_label = b"com.orbshare.capture\0".as_ptr() as *const std::os::raw::c_char;
    let queue = dispatch_queue_create(queue_label, std::ptr::null());

    let mut err: *mut Object = std::ptr::null_mut();
    let ok: bool = msg_send![stream,
        addStreamOutput: output
        type: SC_STREAM_OUTPUT_TYPE_SCREEN
        sampleHandlerQueue: queue
        error: &mut err
    ];
    if !ok {
        let msg = objc_error_description(err);
        cleanup_sck(output_addr, state_ptr, stream, output);
        return Err(format!("addStreamOutput failed: {}", msg));
    }
    if config.capture_system_audio {
        let audio_queue_label =
            b"com.orbshare.capture.audio\0".as_ptr() as *const std::os::raw::c_char;
        let audio_queue = dispatch_queue_create(audio_queue_label, std::ptr::null());
        let mut audio_err: *mut Object = std::ptr::null_mut();
        let audio_ok: bool = msg_send![stream,
            addStreamOutput: output
            type: SC_STREAM_OUTPUT_TYPE_AUDIO
            sampleHandlerQueue: audio_queue
            error: &mut audio_err
        ];
        if !audio_ok {
            let msg = objc_error_description(audio_err);
            cleanup_sck(output_addr, state_ptr, stream, output);
            return Err(format!("添加系统声音采集输出失败：{msg}"));
        }
    }

    let (start_tx, start_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let start_block = block::ConcreteBlock::new(move |error: *mut Object| {
        if error.is_null() {
            let _ = start_tx.send(Ok(()));
        } else {
            let _ = start_tx.send(Err(objc_error_description(error)));
        }
    });
    let start_block = start_block.copy();
    let _: () = msg_send![stream, startCaptureWithCompletionHandler: start_block];
    tracing::debug!("[orbshare:sck] start capture waiting");

    match start_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => info!("[macOS] ScreenCaptureKit capture started"),
        Ok(Err(e)) => {
            cleanup_sck(output_addr, state_ptr, stream, output);
            return Err(format!("startCapture failed: {}", e));
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            cleanup_sck(output_addr, state_ptr, stream, output);
            return Err("启动屏幕采集超过 5 秒，请重新选择共享内容。".to_string());
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            cleanup_sck(output_addr, state_ptr, stream, output);
            return Err("startCapture callback not invoked".to_string());
        }
    }

    provider.state_ptr = state_ptr;
    provider.stream_ptr = stream;
    provider.output_ptr = output;

    let (stop_tx, _stop_rx) = watch::channel(false);
    provider.stop_tx = Some(stop_tx);

    Ok(CaptureStream {
        video_rx,
        encoded_video_rx: None,
        preview_video_rx: None,
        audio_rx: if config.capture_system_audio {
            Some(audio_rx)
        } else {
            None
        },
    })
}

/// Get a display's SCDisplay object by its native display ID.
unsafe fn get_display(display_id: u32) -> Result<(*mut Object, usize, usize), String> {
    let started = Instant::now();
    let content_class = Class::get("SCShareableContent")
        .ok_or("SCShareableContent not found — requires macOS 12.3+")?;

    let (tx, rx) = std::sync::mpsc::channel();

    let block = block::ConcreteBlock::new(move |content: *mut Object, _error: *mut Object| {
        if content.is_null() {
            let _ = tx.send(Err("SCShareableContent returned nil".to_string()));
            return;
        }
        unsafe {
            let displays: *mut Object = msg_send![content, displays];
            let count: usize = msg_send![displays, count];
            for i in 0..count {
                let d: *mut Object = msg_send![displays, objectAtIndex: i];
                let did: u32 = msg_send![d, displayID];
                if did == display_id {
                    let w: usize = msg_send![d, width];
                    let h: usize = msg_send![d, height];
                    let _: () = msg_send![d, retain];
                    let _ = tx.send(Ok((d, w, h)));
                    return;
                }
            }
        }
        let _ = tx.send(Err(format!("Display {} not found", display_id)));
    });
    let block = block.copy();

    unsafe {
        let _: () = msg_send![content_class,
            getShareableContentExcludingDesktopWindows: false
            onScreenWindowsOnly: true
            completionHandler: block
        ];
    }

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(result) => {
            info!(
                "[macOS] Resolved display {display_id} through ScreenCaptureKit in {:?}",
                started.elapsed()
            );
            result
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err("读取显示器信息超过 5 秒，请重试。".to_string())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("读取显示器信息的系统回调未执行。".to_string())
        }
    }
}

fn window_logical_bounds(window_id: u32) -> Option<CGRect> {
    let window_info = copy_window_info(K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW, window_id)?;
    for raw_info in window_info.iter() {
        let info_type = unsafe { CFType::wrap_under_get_rule(*raw_info as _) };
        let Some(info) = info_type.downcast::<CFDictionary>() else {
            continue;
        };
        if cf_dictionary_i32(&info, unsafe { kCGWindowNumber })? as u32 != window_id {
            continue;
        }
        let bounds_value = cf_dictionary_value(&info, unsafe { kCGWindowBounds })?;
        let bounds_dictionary = bounds_value.downcast::<CFDictionary>()?;
        let mut bounds = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: 0.0,
                height: 0.0,
            },
        };
        let valid = unsafe {
            CGRectMakeWithDictionaryRepresentation(
                bounds_dictionary.as_concrete_TypeRef() as *const c_void,
                &mut bounds,
            )
        };
        if valid && bounds.size.width >= 2.0 && bounds.size.height >= 2.0 {
            return Some(bounds);
        }
    }
    None
}

fn window_display_pixel_scale(bounds: CGRect) -> f64 {
    let Ok(display_ids) = CGDisplay::active_displays() else {
        return 1.0;
    };
    let mut best_overlap = 0.0;
    let mut best_scale = 1.0;
    for display_id in display_ids {
        let display = CGDisplay::new(display_id);
        let display_bounds = display.bounds();
        let left = bounds.origin.x.max(display_bounds.origin.x);
        let top = bounds.origin.y.max(display_bounds.origin.y);
        let right = (bounds.origin.x + bounds.size.width)
            .min(display_bounds.origin.x + display_bounds.size.width);
        let bottom = (bounds.origin.y + bounds.size.height)
            .min(display_bounds.origin.y + display_bounds.size.height);
        let overlap = (right - left).max(0.0) * (bottom - top).max(0.0);
        if overlap <= best_overlap || display_bounds.size.width <= 0.0 {
            continue;
        }
        let (pixel_width, _) = unsafe {
            display_physical_pixel_dimensions(
                display_id,
                display_bounds.size.width.round() as usize,
                display_bounds.size.height.round() as usize,
            )
        };
        let scale = pixel_width as f64 / display_bounds.size.width;
        if scale.is_finite() && scale >= 1.0 {
            best_overlap = overlap;
            best_scale = scale.clamp(1.0, 4.0);
        }
    }
    best_scale
}

unsafe fn content_filter_point_pixel_scale(filter: *mut Object) -> Option<f64> {
    if filter.is_null() {
        return None;
    }
    let supports_scale: bool = msg_send![filter, respondsToSelector: sel!(pointPixelScale)];
    if !supports_scale {
        return None;
    }
    let scale: f32 = msg_send![filter, pointPixelScale];
    let scale = scale as f64;
    (scale.is_finite() && scale >= 1.0).then(|| scale.clamp(1.0, 4.0))
}

/// Resolve a shareable window and retain it for SCContentFilter creation.
unsafe fn get_window(window_id: u32) -> Result<(*mut Object, usize, usize, f64), String> {
    let started = Instant::now();
    let bounds = window_logical_bounds(window_id)
        .ok_or_else(|| format!("无法读取所选窗口（ID {window_id}）的尺寸，请刷新列表后重试。"))?;
    let logical_width = bounds.size.width.round().max(2.0) as usize;
    let logical_height = bounds.size.height.round().max(2.0) as usize;
    let fallback_pixel_scale = window_display_pixel_scale(bounds);
    let content_class = Class::get("SCShareableContent")
        .ok_or("SCShareableContent not found — requires macOS 12.3+")?;
    let (tx, rx) = std::sync::mpsc::channel();
    let block = block::ConcreteBlock::new(move |content: *mut Object, _error: *mut Object| {
        if content.is_null() {
            let _ = tx.send(Err("SCShareableContent returned nil".to_string()));
            return;
        }
        unsafe {
            let windows: *mut Object = msg_send![content, windows];
            let count: usize = msg_send![windows, count];
            for i in 0..count {
                let window: *mut Object = msg_send![windows, objectAtIndex: i];
                let candidate_id: u32 = msg_send![window, windowID];
                if candidate_id != window_id {
                    continue;
                }
                let _: () = msg_send![window, retain];
                let _ = tx.send(Ok((
                    window,
                    logical_width,
                    logical_height,
                    fallback_pixel_scale,
                )));
                return;
            }
        }
        let _ = tx.send(Err(format!(
            "所选窗口（ID {window_id}）已关闭或不再可共享，请重新选择。"
        )));
    });
    let block = block.copy();
    // The source picker is intentionally built from CoreGraphics'
    // `kCGWindowListOptionOnScreenOnly` list. Resolve the exact same subset
    // here. Asking ScreenCaptureKit for every background/off-space window can
    // stall for roughly 30 seconds while another SCStream is already active.
    let _: () = msg_send![content_class,
        getShareableContentExcludingDesktopWindows: true
        onScreenWindowsOnly: true
        completionHandler: block
    ];
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(result) => {
            info!(
                "[macOS] Resolved window {window_id} through ScreenCaptureKit in {:?}",
                started.elapsed()
            );
            result
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err("读取所选窗口超过 5 秒。请确认窗口没有最小化，然后刷新列表重试。".to_string())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("读取所选窗口的系统回调未执行。".to_string())
        }
    }
}

unsafe fn objc_error_description(err: *mut Object) -> String {
    if err.is_null() {
        return "unknown error".to_string();
    }
    let desc: *mut Object = msg_send![err, localizedDescription];
    if desc.is_null() {
        return "unknown error".to_string();
    }
    let c_str: *const std::os::raw::c_char = msg_send![desc, UTF8String];
    if c_str.is_null() {
        return "unknown error".to_string();
    }
    std::ffi::CStr::from_ptr(c_str)
        .to_string_lossy()
        .into_owned()
}

unsafe fn cleanup_sck(
    addr: usize,
    state_ptr: *const CaptureState,
    stream: *mut Object,
    output: *mut Object,
) {
    {
        let mut map = capture_state_registry().lock().unwrap();
        map.remove(&addr);
    }
    drop(Box::from_raw(state_ptr as *mut CaptureState));
    if !stream.is_null() {
        let _: () = msg_send![stream, release];
    }
    if !output.is_null() {
        let _: () = msg_send![output, release];
    }
}
