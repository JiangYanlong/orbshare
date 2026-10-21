#![allow(dead_code, non_snake_case)]

use crate::EncodedVideoFrame;
use std::ffi::{c_char, c_void};
use std::ptr;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, trace};

// ── CoreMedia / VideoToolbox FFI ─────────────────────────────────────────

type OSStatus = i32;
type CMFormatDescriptionRef = *mut c_void;
type CMSampleBufferRef = *mut c_void;
type CMBlockBufferRef = *mut c_void;
type CVPixelBufferRef = *mut c_void;
type CVPixelBufferPoolRef = *mut c_void;
type CFTypeRef = *mut c_void;
type CFDictionaryRef = *mut c_void;
type CFNumberRef = *mut c_void;
type CFBooleanRef = *mut c_void;
type CFArrayRef = *mut c_void;
type CFStringRef = *mut c_void;
type VTCompressionSessionRef = *mut c_void;
type CFAllocatorRef = *mut c_void;

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
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation_time_stamp: CMTime,
    decode_time_stamp: CMTime,
}

const CM_TIME_FLAGS_VALID: u32 = 0x0001;

fn cm_time_from_seconds(seconds: f64, timescale: i32) -> CMTime {
    CMTime {
        value: (seconds * timescale as f64) as i64,
        timescale,
        flags: CM_TIME_FLAGS_VALID,
        epoch: 0,
    }
}

type VTCompressionOutputCallback = extern "C" fn(
    output_callback_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    sample_buffer: CMSampleBufferRef,
);

extern "C" {
    // VideoToolbox
    fn VTCompressionSessionCreate(
        allocator: CFAllocatorRef,
        width: i32,
        height: i32,
        codec_type: u32,
        encoder_specification: CFDictionaryRef,
        source_image_buffer_attributes: CFDictionaryRef,
        compressed_data_allocator: CFAllocatorRef,
        output_callback: VTCompressionOutputCallback,
        output_callback_ref_con: *mut c_void,
        compression_session_out: *mut VTCompressionSessionRef,
    ) -> OSStatus;

    fn VTCompressionSessionEncodeFrame(
        session: VTCompressionSessionRef,
        image_buffer: CVPixelBufferRef,
        presentation_timestamp: CMTime,
        duration: CMTime,
        frame_properties: CFDictionaryRef,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;

    fn VTCompressionSessionCompleteFrames(
        session: VTCompressionSessionRef,
        complete_until_presentation_timestamp: CMTime,
    ) -> OSStatus;

    fn VTCompressionSessionInvalidate(session: VTCompressionSessionRef);
    // VTCompressionSessionRelease is a macro for CFRelease on modern SDKs
    // fn VTCompressionSessionRelease(session: VTCompressionSessionRef);
    fn VTCompressionSessionGetPixelBufferPool(
        session: VTCompressionSessionRef,
    ) -> CVPixelBufferPoolRef;

    // CoreMedia
    fn CMSampleBufferGetFormatDescription(sbuf: CMSampleBufferRef) -> CMFormatDescriptionRef;

    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        video_desc: CMFormatDescriptionRef,
        parameter_set_index: usize,
        parameter_set_pointer: *mut *const u8,
        parameter_set_size: *mut usize,
        parameter_set_count: *mut usize,
        nal_unit_header_length: *mut i32,
    ) -> OSStatus;

    fn CMSampleBufferGetDataBuffer(sbuf: CMSampleBufferRef) -> CMBlockBufferRef;
    fn CMBlockBufferGetDataPointer(
        buf: CMBlockBufferRef,
        offset: usize,
        length_at_offset: *mut usize,
        total_length: *mut usize,
        data_pointer: *mut *mut u8,
    ) -> OSStatus;

    fn CMSampleBufferGetSampleTimingInfoArray(
        sbuf: CMSampleBufferRef,
        num_entries: usize,
        timing_array_out: *mut c_void,
        timing_array_entries_needed_out: *mut usize,
    ) -> OSStatus;

    fn CMSampleBufferGetSampleAttachmentsArray(
        sbuf: CMSampleBufferRef,
        create_if_necessary: u8,
    ) -> CFArrayRef;

    fn CFArrayGetValueAtIndex(the_array: CFArrayRef, idx: isize) -> *const c_void;
    fn CFDictionaryGetValue(the_dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
    fn CFRelease(cf: CFTypeRef);

    // CoreVideo
    fn CVPixelBufferPoolCreatePixelBuffer(
        allocator: CFAllocatorRef,
        pool: CVPixelBufferPoolRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> OSStatus;

    fn CVPixelBufferLockBaseAddress(pixel_buffer: CVPixelBufferRef, lock_flags: u64) -> OSStatus;
    fn CVPixelBufferUnlockBaseAddress(pixel_buffer: CVPixelBufferRef, lock_flags: u64) -> OSStatus;
    fn CVPixelBufferGetBaseAddressOfPlane(
        pixel_buffer: CVPixelBufferRef,
        plane_index: usize,
    ) -> *mut u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(
        pixel_buffer: CVPixelBufferRef,
        plane_index: usize,
    ) -> usize;
    fn CVPixelBufferGetPlaneCount(pixel_buffer: CVPixelBufferRef) -> usize;

    // CoreFoundation
    fn CFDictionaryCreateMutable(
        allocator: CFAllocatorRef,
        capacity: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;

    fn CFDictionarySetValue(the_dict: CFDictionaryRef, key: *const c_void, value: *const c_void);

    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: CFNumberType,
        value_ptr: *const c_void,
    ) -> CFNumberRef;

    fn CFArrayCreate(
        allocator: CFAllocatorRef,
        values: *const *const c_void,
        num_values: isize,
        callbacks: *const c_void,
    ) -> CFArrayRef;

    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    static kCFTypeArrayCallBacks: c_void;
    static kCFBooleanTrue: CFBooleanRef;
    static kCFBooleanFalse: CFBooleanRef;
}

#[repr(u32)]
#[allow(dead_code)]
enum CFNumberType {
    SInt32 = 3,
    Float64 = 13,
}

// Codec type: 'avc1'
const K_CMCODEC_TYPE_H264: u32 = 0x61766331;
// CoreVideo kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange: '420v' (NV12)
const K_CV_PIXEL_FORMAT_TYPE_420YPCBCR8BIPLANAR_VIDEO_RANGE: i32 = 0x3432_3076;

// ── VideoToolbox constant keys (linker symbols) ──────────────────────────
// These are extern statics from the VideoToolbox / CoreMedia frameworks.
// We define them as opaque pointers and access via linker symbols.

extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    static kVTCompressionPropertyKey_Quality: CFStringRef;
    static kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration: CFStringRef;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    static kVTCompressionPropertyKey_RealTime: CFStringRef;
    static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;
    static kCMSampleAttachmentKey_DependsOnOthers: CFStringRef;
    static kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms: CFStringRef;
    static kVTProfileLevel_H264_Main_AutoLevel: CFStringRef;
    static kVTProfileLevel_H264_Baseline_3_1: CFStringRef;
    static kVTProfileLevel_H264_Baseline_4_0: CFStringRef;
    static kVTProfileLevel_H264_Baseline_5_0: CFStringRef;
    static kVTProfileLevel_H264_Baseline_5_1: CFStringRef;
}

// ── Encoder state shared with callback ───────────────────────────────────

struct EncoderSharedState {
    frames: Vec<EncodedVideoFrame>,
}

// ── Public encoder ───────────────────────────────────────────────────────

pub struct VideoToolboxEncoder {
    session: VTCompressionSessionRef,
    width: i32,
    height: i32,
    timescale: i32,
    frame_index: u64,
    keyframe_interval: u64,
    keyframe_interval_seconds: f64,
    /// Wall-clock time of the last keyframe request.
    ///
    /// The frame-index-based interval (`frame_index % keyframe_interval`)
    /// assumes the encoder is actually fed at `fps`. When the capture →
    /// encode pipeline is slower than realtime (e.g. debug builds, or a
    /// busy machine), encoded frames trickle out at a fraction of `fps`,
    /// and frame-count-based keyframes arrive every many *wall-clock*
    /// seconds. Viewers that join mid-stream then wait tens of seconds
    /// for the first decodable keyframe. Keying off wall-clock time
    /// guarantees a keyframe at least every `keyframe_interval_seconds`
    /// regardless of encode throughput.
    last_keyframe_at: std::time::Instant,
    state: Arc<Mutex<EncoderSharedState>>,
}

unsafe impl Send for VideoToolboxEncoder {}
unsafe impl Sync for VideoToolboxEncoder {}

impl VideoToolboxEncoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, String> {
        Self::new_with_options(
            width,
            height,
            fps,
            estimate_bitrate(width, height, fps) as u32,
            1.0,
        )
    }

    pub fn new_with_options(
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        keyframe_interval_seconds: f64,
    ) -> Result<Self, String> {
        unsafe {
            let width_i = width as i32;
            let height_i = height as i32;
            let timescale = fps as i32 * 1000; // millisecond precision
            let keyframe_interval =
                ((fps as f64 * keyframe_interval_seconds.max(0.25)).round() as u64).max(1);

            // Create session attributes with pixel format
            let session_attrs = CFDictionaryCreateMutable(
                ptr::null_mut(),
                0,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            if session_attrs.is_null() {
                return Err("Failed to create source image buffer attributes dict".into());
            }
            let pixel_format = CFNumberCreate(
                ptr::null_mut(),
                CFNumberType::SInt32,
                &K_CV_PIXEL_FORMAT_TYPE_420YPCBCR8BIPLANAR_VIDEO_RANGE as *const _ as *const c_void,
            );
            if pixel_format.is_null() {
                CFRelease(session_attrs);
                return Err("Failed to create pixel format number".into());
            }
            CFDictionarySetValue(
                session_attrs,
                kCVPixelBufferPixelFormatTypeKey as *const c_void,
                pixel_format as *const c_void,
            );
            CFRelease(pixel_format);

            let shared_state = Arc::new(Mutex::new(EncoderSharedState { frames: Vec::new() }));
            let state_ptr = Arc::into_raw(shared_state.clone()) as *mut c_void;

            let mut session: VTCompressionSessionRef = ptr::null_mut();
            let status = VTCompressionSessionCreate(
                ptr::null_mut(), // allocator
                width_i,
                height_i,
                K_CMCODEC_TYPE_H264,
                ptr::null_mut(),
                session_attrs,
                ptr::null_mut(), // compressed data allocator
                output_callback,
                state_ptr,
                &mut session,
            );

            CFRelease(session_attrs);

            if status != 0 || session.is_null() {
                return Err(format!(
                    "VTCompressionSessionCreate failed: {}",
                    cvt_status(status)
                ));
            }

            // Set compression properties
            set_bool_property(session, &kVTCompressionPropertyKey_RealTime, true);
            // Constrained Baseline gives the broadest browser/WebRTC decoder
            // compatibility.  We pick an EXPLICIT level that covers the
            // resolution so the actual bitstream level matches the SDP
            // profile-level-id advertised to the browser (otherwise the
            // browser rejects the stream and the guest never sees a frame).
            let level = h264_baseline_level_for(width, height, bitrate);
            set_string_property(session, &kVTCompressionPropertyKey_ProfileLevel, level);
            set_number_property(
                session,
                &kVTCompressionPropertyKey_AverageBitRate,
                bitrate.min(i32::MAX as u32) as i32,
            );
            // AverageBitRate is intentionally a soft target in VideoToolbox.
            // With mostly-static desktop content the hardware encoder can
            // undershoot it aggressively (we observed 1.5-1.8 Mbps against a
            // 12 Mbps target), leaving small text visibly soft. Keep the rate
            // target, but also establish a real quality floor. MaxAllowedFrameQP
            // is resolved dynamically so the app still starts on older macOS
            // versions where that optional symbol is unavailable.
            let (compression_quality, max_frame_qp) = quality_controls(bitrate);
            let quality_status = set_number_property_f64(
                session,
                &kVTCompressionPropertyKey_Quality,
                compression_quality,
            );
            let max_qp_status = set_optional_number_property(
                session,
                b"kVTCompressionPropertyKey_MaxAllowedFrameQP\0",
                max_frame_qp,
            );
            let speed_priority_status = set_optional_bool_property(
                session,
                b"kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality\0",
                false,
            );
            tracing::info!(
                "[VideoToolbox] Quality controls: quality={compression_quality:.2} status={} max_qp={max_frame_qp} status={} prioritize_speed=false status={}",
                cvt_status(quality_status),
                optional_status(max_qp_status),
                optional_status(speed_priority_status)
            );
            set_number_property(
                session,
                &kVTCompressionPropertyKey_ExpectedFrameRate,
                fps as i32,
            );
            set_number_property_f64(
                session,
                &kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
                keyframe_interval_seconds.max(0.25),
            );
            // Force an IDR every N frames too (belt + suspenders with the
            // per-frame ForceKeyFrame option). For screen sharing, frequent
            // keyframes are desirable — late joiners need a decodable frame
            // immediately, and screen content is mostly static.
            set_number_property(
                session,
                &kVTCompressionPropertyKey_MaxKeyFrameInterval,
                keyframe_interval as i32,
            );
            set_bool_property(
                session,
                &kVTCompressionPropertyKey_AllowFrameReordering,
                false,
            );

            // Log the actual encoder configuration for debugging
            tracing::info!(
                "[VideoToolbox] Encoder config: {}x{} @ {}fps, bitrate={} bps ({} Mbps), keyframe_interval={}s, level={:?}",
                width, height, fps, bitrate, bitrate / 1_000_000, keyframe_interval_seconds, level
            );

            // Verify bitrate was set correctly by reading it back
            extern "C" {
                fn VTSessionCopyProperty(
                    session: VTCompressionSessionRef,
                    key: CFStringRef,
                    allocator: CFAllocatorRef,
                    value_out: *mut CFTypeRef,
                ) -> OSStatus;
            }
            let mut actual_bitrate: i32 = 0;
            let mut bitrate_ref: CFTypeRef = ptr::null_mut();
            let status = VTSessionCopyProperty(
                session,
                kVTCompressionPropertyKey_AverageBitRate,
                ptr::null_mut(),
                &mut bitrate_ref,
            );
            if status == 0 && !bitrate_ref.is_null() {
                extern "C" {
                    fn CFNumberGetValue(
                        number: CFNumberRef,
                        the_type: CFNumberType,
                        value_ptr: *mut c_void,
                    ) -> u8;
                }
                CFNumberGetValue(
                    bitrate_ref as CFNumberRef,
                    CFNumberType::SInt32,
                    &mut actual_bitrate as *mut i32 as *mut c_void,
                );
                CFRelease(bitrate_ref);
                tracing::info!("[VideoToolbox] Verified bitrate: {} bps", actual_bitrate);
            }

            Ok(Self {
                session,
                width: width_i,
                height: height_i,
                timescale,
                frame_index: 0,
                keyframe_interval,
                keyframe_interval_seconds: keyframe_interval_seconds.max(0.25),
                last_keyframe_at: std::time::Instant::now(),
                state: shared_state,
            })
        }
    }

    /// Encode a YUV420 planar frame. Returns 0 or more encoded H.264 frames.
    /// Typically returns 1 frame per call, but may return 0 if the encoder is buffering.
    pub fn encode(
        &mut self,
        yuv_data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Vec<EncodedVideoFrame>, String> {
        unsafe {
            let w = width as usize;
            let h = height as usize;
            let y_size = w * h;
            let uv_w = w / 2;
            let uv_h = h / 2;

            if yuv_data.len() < y_size + uv_w * uv_h * 2 {
                return Err("YUV420 data too short".into());
            }

            // Get a pixel buffer from the pool
            let pool = VTCompressionSessionGetPixelBufferPool(self.session);
            if pool.is_null() {
                return Err("Pixel buffer pool is null".into());
            }

            let mut pixel_buf: CVPixelBufferRef = ptr::null_mut();
            let status = CVPixelBufferPoolCreatePixelBuffer(ptr::null_mut(), pool, &mut pixel_buf);
            if status != 0 || pixel_buf.is_null() {
                return Err(format!(
                    "CVPixelBufferPoolCreatePixelBuffer failed: {}",
                    cvt_status(status)
                ));
            }

            // Copy YUV420 data into pixel buffer planes
            CVPixelBufferLockBaseAddress(pixel_buf, 0); // kCVPixelBufferLock_ReadOnly = 0
            let plane_count = CVPixelBufferGetPlaneCount(pixel_buf);
            if plane_count < 2 {
                CVPixelBufferUnlockBaseAddress(pixel_buf, 0);
                CFRelease(pixel_buf as CFTypeRef);
                return Err(format!(
                    "Expected NV12 pixel buffer with at least 2 planes, got {}",
                    plane_count
                ));
            }

            // Y plane
            let y_dst = CVPixelBufferGetBaseAddressOfPlane(pixel_buf, 0);
            let y_stride = CVPixelBufferGetBytesPerRowOfPlane(pixel_buf, 0);
            if y_dst.is_null() {
                CVPixelBufferUnlockBaseAddress(pixel_buf, 0);
                CFRelease(pixel_buf as CFTypeRef);
                return Err("Y plane is null".into());
            }
            if y_stride < w {
                CVPixelBufferUnlockBaseAddress(pixel_buf, 0);
                CFRelease(pixel_buf as CFTypeRef);
                return Err(format!("Y stride {} < width {}", y_stride, w));
            }
            if y_stride == w {
                std::ptr::copy_nonoverlapping(yuv_data.as_ptr(), y_dst, y_size);
            } else {
                for row in 0..h {
                    let src_off = row * w;
                    let dst_off = row * y_stride;
                    std::ptr::copy_nonoverlapping(
                        yuv_data[src_off..].as_ptr(),
                        y_dst.add(dst_off),
                        w,
                    );
                }
            }

            // NV12 UV plane: interleaved U/V bytes.
            let uv_dst = CVPixelBufferGetBaseAddressOfPlane(pixel_buf, 1);
            let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(pixel_buf, 1);
            if uv_dst.is_null() {
                CVPixelBufferUnlockBaseAddress(pixel_buf, 0);
                CFRelease(pixel_buf as CFTypeRef);
                return Err("UV plane is null".into());
            }
            if uv_stride < uv_w * 2 {
                CVPixelBufferUnlockBaseAddress(pixel_buf, 0);
                CFRelease(pixel_buf as CFTypeRef);
                return Err(format!("UV stride {} < expected {}", uv_stride, uv_w * 2));
            }
            let u_src = &yuv_data[y_size..];
            let v_src = &yuv_data[y_size + uv_w * uv_h..];
            for row in 0..uv_h {
                let dst_row = uv_dst.add(row * uv_stride);
                for col in 0..uv_w {
                    let src_idx = row * uv_w + col;
                    let dst_idx = col * 2;
                    *dst_row.add(dst_idx) = u_src[src_idx];
                    *dst_row.add(dst_idx + 1) = v_src[src_idx];
                }
            }

            CVPixelBufferUnlockBaseAddress(pixel_buf, 0);

            // Encode the frame
            let pts = cm_time_from_seconds(
                self.frame_index as f64 / (self.timescale as f64 / 1000.0),
                self.timescale,
            );
            let duration =
                cm_time_from_seconds(1.0 / (self.timescale as f64 / 1000.0), self.timescale);

            // Keyframe policy (wall-clock based). The old frame-index rule
            // (`frame_index % keyframe_interval == 0`) silently degraded to
            // "one keyframe every N *seconds*" when the pipeline ran below
            // realtime, leaving late-joining viewers without a decodable
            // keyframe for tens of seconds. Force a keyframe when either:
            //   - this is the very first frame, or
            //   - more than `keyframe_interval_seconds` of wall-clock time
            //     has elapsed since the last forced keyframe.
            let now = std::time::Instant::now();
            let force_keyframe = self.frame_index == 0
                || now.duration_since(self.last_keyframe_at).as_secs_f64()
                    >= self.keyframe_interval_seconds;
            if force_keyframe {
                self.last_keyframe_at = now;
            }
            trace!(
                "[VT] encode idx={} force_kf={} since_last_kf={:.1}s",
                self.frame_index,
                force_keyframe,
                now.duration_since(self.last_keyframe_at).as_secs_f64()
            );
            let frame_props = if force_keyframe {
                create_force_keyframe_dict()
            } else {
                ptr::null_mut()
            };

            let mut info_flags: u32 = 0;
            let status = VTCompressionSessionEncodeFrame(
                self.session,
                pixel_buf,
                pts,
                duration,
                frame_props,
                ptr::null_mut(), // source frame refcon
                &mut info_flags,
            );
            if !frame_props.is_null() {
                CFRelease(frame_props as CFTypeRef);
            }

            if status != 0 {
                CFRelease(pixel_buf as CFTypeRef);
                return Err(format!(
                    "VTCompressionSessionEncodeFrame failed: {}",
                    cvt_status(status)
                ));
            }

            // Complete pending frames to trigger the callback
            VTCompressionSessionCompleteFrames(self.session, pts);
            CFRelease(pixel_buf as CFTypeRef);

            self.frame_index += 1;

            // Drain the shared state
            let mut st = self.state.lock().unwrap();
            Ok(std::mem::take(&mut st.frames))
        }
    }
}

impl Drop for VideoToolboxEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                VTCompressionSessionCompleteFrames(
                    self.session,
                    cm_time_from_seconds(f64::MAX, self.timescale),
                );
                VTCompressionSessionInvalidate(self.session);
                CFRelease(self.session as CFTypeRef);
                self.session = ptr::null_mut();
                debug!("[VideoToolbox] Encoder released");
            }
        }
    }
}

// ── Callback ─────────────────────────────────────────────────────────────

extern "C" fn output_callback(
    output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: u32,
    sample_buffer: CMSampleBufferRef,
) {
    if status != 0 {
        error!(
            "[VideoToolbox] Encode callback error: {}",
            cvt_status(status)
        );
        return;
    }
    if sample_buffer.is_null() {
        return;
    }

    unsafe {
        let state = &*(output_callback_ref_con as *const Mutex<EncoderSharedState>);

        // Extract SPS/PPS only for keyframes.
        let mut annex_b_header = Vec::new();
        let is_keyframe = is_keyframe_sample(sample_buffer);
        trace!("[VT] callback is_kf={}", is_keyframe);
        if is_keyframe {
            let format_desc = CMSampleBufferGetFormatDescription(sample_buffer);
            if !format_desc.is_null() {
                extract_parameter_sets(format_desc, &mut annex_b_header);
            }
        }
        let mut timestamp_us = 0u64;

        // Get timing info
        let mut timing_info = std::mem::zeroed::<CMSampleTimingInfo>();
        let mut timing_count: usize = 0;
        let _ = CMSampleBufferGetSampleTimingInfoArray(
            sample_buffer,
            1,
            &mut timing_info as *mut _ as *mut c_void,
            &mut timing_count,
        );
        let pts = timing_info.presentation_time_stamp;
        if timing_count > 0 && pts.flags & CM_TIME_FLAGS_VALID != 0 && pts.timescale > 0 {
            timestamp_us = ((pts.value as f64 / pts.timescale as f64) * 1_000_000.0) as u64;
        }

        // Get encoded data
        let data_buf = CMSampleBufferGetDataBuffer(sample_buffer);
        if data_buf.is_null() {
            return;
        }

        let mut total_length: usize = 0;
        let mut data_ptr: *mut u8 = ptr::null_mut();
        let status = CMBlockBufferGetDataPointer(
            data_buf,
            0,
            ptr::null_mut(),
            &mut total_length,
            &mut data_ptr,
        );
        if status != 0 || data_ptr.is_null() || total_length == 0 {
            // NOTE: do NOT CFRelease the sample buffer here. The output
            // callback does NOT own it — VTCompressionSession owns the
            // buffer and recycles it after the callback returns. Releasing
            // it here over-releases and crashes in CF_IS_OBJC (SIGTRAP).
            return;
        }

        // Build Annex B output: SPS/PPS (for keyframes) + AVCC length-prefixed NALs.
        let mut output = Vec::with_capacity(annex_b_header.len() + total_length + 16);
        if is_keyframe && !annex_b_header.is_empty() {
            output.extend_from_slice(&annex_b_header);
        }
        let encoded_slice = std::slice::from_raw_parts(data_ptr, total_length);
        append_avcc_as_annex_b(encoded_slice, &mut output);

        if !output.is_empty() {
            let frame = EncodedVideoFrame {
                data: output,
                timestamp_us,
                is_keyframe,
                width: 0,  // caller fills in
                height: 0, // caller fills in
            };

            let mut st = state.lock().unwrap();
            st.frames.push(frame);
        }

        // NOTE: intentionally NO CFRelease(sample_buffer). The output
        // callback does not own the CMSampleBuffer; the compression session
        // retains ownership and reuses the buffer after we return. We copy
        // all payload bytes synchronously above, so nothing outlives the
        // callback. An extra CFRelease here over-releases and VT crashes
        // with SIGTRAP (CF_IS_OBJC) on its own subsequent release.
    }
}

fn append_avcc_as_annex_b(input: &[u8], output: &mut Vec<u8>) {
    let mut offset = 0usize;
    while offset + 4 <= input.len() {
        let nal_len = u32::from_be_bytes([
            input[offset],
            input[offset + 1],
            input[offset + 2],
            input[offset + 3],
        ]) as usize;
        offset += 4;

        if nal_len == 0 || offset + nal_len > input.len() {
            break;
        }

        output.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        output.extend_from_slice(&input[offset..offset + nal_len]);
        offset += nal_len;
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Extract SPS/PPS from CMFormatDescription and write as Annex B NAL units.
unsafe fn extract_parameter_sets(format_desc: CMFormatDescriptionRef, out: &mut Vec<u8>) {
    let mut param_count: usize = 0;
    let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        format_desc,
        0,
        ptr::null_mut(),
        ptr::null_mut(),
        &mut param_count,
        ptr::null_mut(),
    );
    if status != 0 {
        return;
    }

    for i in 0..param_count {
        let mut param_ptr: *const u8 = ptr::null();
        let mut param_size: usize = 0;
        let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            format_desc,
            i,
            &mut param_ptr,
            &mut param_size,
            ptr::null_mut(),
            ptr::null_mut(),
        );
        if status == 0 && !param_ptr.is_null() && param_size > 0 {
            out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            out.extend_from_slice(std::slice::from_raw_parts(param_ptr, param_size));
        }
    }
}

unsafe fn create_force_keyframe_dict() -> CFDictionaryRef {
    let dict = CFDictionaryCreateMutable(
        ptr::null_mut(),
        1,
        &kCFTypeDictionaryKeyCallBacks,
        &kCFTypeDictionaryValueCallBacks,
    );
    if !dict.is_null() {
        CFDictionarySetValue(
            dict,
            kVTEncodeFrameOptionKey_ForceKeyFrame as *const c_void,
            kCFBooleanTrue as *const c_void,
        );
    }
    dict
}

/// Check if a sample buffer is a keyframe by inspecting the sample attachment.
unsafe fn is_keyframe_sample(sbuf: CMSampleBufferRef) -> bool {
    let attachments = CMSampleBufferGetSampleAttachmentsArray(sbuf, 0);
    if attachments.is_null() {
        return true; // assume keyframe if no attachment info
    }
    let dict = CFArrayGetValueAtIndex(attachments, 0);
    if dict.is_null() {
        return true;
    }
    let depends = CFDictionaryGetValue(
        dict as CFDictionaryRef,
        kCMSampleAttachmentKey_DependsOnOthers as *const c_void,
    );
    // If DependsOnOthers is false/absent, it's a keyframe
    if depends.is_null() {
        return true;
    }
    depends != kCFBooleanTrue as *const c_void
}

/// Pick the H.264 Baseline level for the encoder.
///
/// Match the encoder SPS level to the level advertised by the quality policy.
unsafe fn h264_baseline_level_for(width: u32, height: u32, bitrate: u32) -> CFStringRef {
    if bitrate >= 30_000_000 || width > 3328 || height > 2160 {
        kVTProfileLevel_H264_Baseline_5_1
    } else if width > 1920 || height > 1080 {
        kVTProfileLevel_H264_Baseline_5_0
    } else if width > 1280 || height > 720 {
        kVTProfileLevel_H264_Baseline_4_0
    } else {
        kVTProfileLevel_H264_Baseline_3_1
    }
}

fn estimate_bitrate(width: u32, height: u32, fps: u32) -> i32 {
    let pixels = width as f64 * height as f64;
    let bits_per_pixel = if pixels >= 1_900_000.0 { 0.18 } else { 0.22 };
    ((pixels * bits_per_pixel * fps as f64) as i32).clamp(2_500_000, 40_000_000)
}

fn quality_controls(bitrate: u32) -> (f64, i32) {
    if bitrate >= 30_000_000 {
        (0.98, 16)
    } else if bitrate >= 20_000_000 {
        (0.96, 18)
    } else if bitrate >= 10_000_000 {
        (0.92, 20)
    } else {
        (0.78, 28)
    }
}

fn optional_status(status: Option<OSStatus>) -> String {
    status
        .map(cvt_status)
        .unwrap_or_else(|| "symbol unavailable".to_string())
}

unsafe fn optional_vt_property(symbol: &[u8]) -> Option<CFStringRef> {
    if symbol.last() != Some(&0) {
        return None;
    }
    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    // Darwin's RTLD_DEFAULT searches all images already loaded into the app.
    const RTLD_DEFAULT: *mut c_void = -2_isize as *mut c_void;
    let address = dlsym(RTLD_DEFAULT, symbol.as_ptr().cast());
    if address.is_null() {
        return None;
    }
    let property = *(address as *const CFStringRef);
    (!property.is_null()).then_some(property)
}

unsafe fn set_optional_number_property(
    session: VTCompressionSessionRef,
    symbol: &[u8],
    value: i32,
) -> Option<OSStatus> {
    optional_vt_property(symbol).map(|property| set_number_property(session, &property, value))
}

unsafe fn set_optional_bool_property(
    session: VTCompressionSessionRef,
    symbol: &[u8],
    value: bool,
) -> Option<OSStatus> {
    optional_vt_property(symbol).map(|property| set_bool_property(session, &property, value))
}

unsafe fn set_number_property(
    session: VTCompressionSessionRef,
    key: &CFStringRef,
    value: i32,
) -> OSStatus {
    let num = CFNumberCreate(
        ptr::null_mut(),
        CFNumberType::SInt32,
        &value as *const _ as *const c_void,
    );
    if !num.is_null() {
        extern "C" {
            fn VTSessionSetProperty(
                session: VTCompressionSessionRef,
                key: CFStringRef,
                value: CFTypeRef,
            ) -> OSStatus;
        }
        let status = VTSessionSetProperty(session, *key, num);
        CFRelease(num);
        status
    } else {
        -12902
    }
}

unsafe fn set_number_property_f64(
    session: VTCompressionSessionRef,
    key: &CFStringRef,
    value: f64,
) -> OSStatus {
    let num = CFNumberCreate(
        ptr::null_mut(),
        CFNumberType::Float64,
        &value as *const _ as *const c_void,
    );
    if !num.is_null() {
        extern "C" {
            fn VTSessionSetProperty(
                session: VTCompressionSessionRef,
                key: CFStringRef,
                value: CFTypeRef,
            ) -> OSStatus;
        }
        let status = VTSessionSetProperty(session, *key, num);
        CFRelease(num);
        status
    } else {
        -12902
    }
}

unsafe fn set_bool_property(
    session: VTCompressionSessionRef,
    key: &CFStringRef,
    value: bool,
) -> OSStatus {
    extern "C" {
        fn VTSessionSetProperty(
            session: VTCompressionSessionRef,
            key: CFStringRef,
            value: CFTypeRef,
        ) -> OSStatus;
    }
    if value {
        VTSessionSetProperty(session, *key, kCFBooleanTrue as CFTypeRef)
    } else {
        VTSessionSetProperty(session, *key, kCFBooleanFalse as CFTypeRef)
    }
}

unsafe fn set_string_property(
    session: VTCompressionSessionRef,
    key: &CFStringRef,
    value: CFStringRef,
) -> OSStatus {
    extern "C" {
        fn VTSessionSetProperty(
            session: VTCompressionSessionRef,
            key: CFStringRef,
            value: CFTypeRef,
        ) -> OSStatus;
    }
    VTSessionSetProperty(session, *key, value as CFTypeRef)
}

fn cvt_status(status: OSStatus) -> String {
    match status {
        0 => "noErr".to_string(),
        -12900 => "kVTPropertyNotSupportedErr".to_string(),
        -12901 => "kVTPropertyReadOnlyErr".to_string(),
        -12902 => "kVTParameterErr".to_string(),
        -12903 => "kVTInvalidSessionErr".to_string(),
        -12904 => "kVTAllocationFailedErr".to_string(),
        -12905 => "kVTPixelTransferNotSupportedErr".to_string(),
        -12906 => "kVTCouldNotFindVideoDecoderErr".to_string(),
        -12907 => "kVTCouldNotCreateInstanceErr".to_string(),
        -12908 => "kVTCouldNotFindVideoEncoderErr".to_string(),
        -12909 => "kVTVideoDecoderBadDataErr".to_string(),
        -12910 => "kVTVideoDecoderUnsupportedDataFormatErr".to_string(),
        -12911 => "kVTVideoDecoderMalfunctionErr".to_string(),
        -12912 => "kVTVideoEncoderMalfunctionErr".to_string(),
        -12913 => "kVTVideoDecoderNotAvailableNowErr".to_string(),
        -12914 => "kVTImageRotationNotSupportedErr".to_string(),
        -12915 => "kVTVideoEncoderNotAvailableNowErr".to_string(),
        -12918 => "kVTFormatDescriptionChangeNotSupportedErr".to_string(),
        -12919 => "kVTInsufficientSourceColorDataErr".to_string(),
        -12920 => "kVTCouldNotFindTemporalFilterErr".to_string(),
        -12921 => "kVTPixelTransferNotPermittedErr".to_string(),
        -12210 => "kVTVideoDecoderAuthorizationErr".to_string(),
        -12211 => "kVTVideoEncoderAuthorizationErr".to_string(),
        -12212 => "kVTFormatDescriptionAuthorizationErr".to_string(),
        -12213 => "kVTVideoDecoderNeedsRosettaErr".to_string(),
        _ => format!("OSStatus({})", status),
    }
}

unsafe fn CFStringCreateWithCString(c_str: *const u8) -> CFStringRef {
    extern "C" {
        fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const u8,
            encoding: u32,
        ) -> CFStringRef;
    }
    CFStringCreateWithCString(ptr::null_mut(), c_str, 0x08000100) // kCFStringEncodingUTF8
}

/// Extract the H.264 `profile-level-id` (hex string) from an Annex-B SPS.
/// SPS layout after the (optional) 00 00 00 01 / 00 00 01 start code:
///   byte0 = NAL header (type 7 for SPS)
///   byte1 = profile_idc
///   byte2 = constraint_set flags (constraint_set0/1/2/3/4/5 + reserved)
///   byte3 = level_idc
fn profile_level_id_from_sps(sps: &[u8]) -> Option<String> {
    // Find the offset just past any Annex-B start code.
    let start = if sps.starts_with(&[0, 0, 0, 1]) {
        4
    } else if sps.starts_with(&[0, 0, 1]) {
        3
    } else {
        0
    };
    if sps.len() < start + 4 {
        return None;
    }
    let nal_header = sps[start];
    if (nal_header & 0x1f) != 7 {
        return None; // not an SPS NAL
    }
    let profile_idc = sps[start + 1];
    let constraint = sps[start + 2];
    let level_idc = sps[start + 3];
    Some(format!(
        "{:02x}{:02x}{:02x}",
        profile_idc, constraint, level_idc
    ))
}

/// Collect the SPS bytes (first SPS NAL) from an encoded keyframe.
fn first_sps_from_frame(data: &[u8]) -> Option<Vec<u8>> {
    // The frame is Annex-B: [SPS NAL][PPS NAL][slice NALs].
    // Find the SPS NAL (type 7) start code.
    let mut i = 0;
    while i + 4 < data.len() {
        let is_start = data[i..].starts_with(&[0, 0, 0, 1]) || data[i..].starts_with(&[0, 0, 1]);
        if is_start {
            let off = if data[i..].starts_with(&[0, 0, 0, 1]) {
                i + 4
            } else {
                i + 3
            };
            let nal_header = data[off];
            if (nal_header & 0x1f) == 7 {
                // Find the next start code to bound this NAL.
                let rest = &data[off + 1..];
                let mut end = data.len();
                for j in 0..rest.len().saturating_sub(3) {
                    if rest[j..].starts_with(&[0, 0, 1]) {
                        end = off + 1 + j;
                        break;
                    }
                }
                return Some(data[off..end].to_vec());
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_share_quality_controls_prevent_aggressive_quantization() {
        assert_eq!(quality_controls(2_000_000), (0.78, 28));
        assert_eq!(quality_controls(12_000_000), (0.92, 20));
        assert_eq!(quality_controls(24_000_000), (0.96, 18));
        assert_eq!(quality_controls(32_000_000), (0.98, 16));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn video_toolbox_applies_screen_share_qp_floor_when_supported() {
        let encoder = VideoToolboxEncoder::new_with_options(1920, 1080, 20, 12_000_000, 1.0)
            .expect("create VideoToolbox encoder");
        unsafe {
            let key = optional_vt_property(b"kVTCompressionPropertyKey_MaxAllowedFrameQP\0")
                .expect("MaxAllowedFrameQP is available on this macOS version");
            extern "C" {
                fn VTSessionCopyProperty(
                    session: VTCompressionSessionRef,
                    key: CFStringRef,
                    allocator: CFAllocatorRef,
                    value_out: *mut CFTypeRef,
                ) -> OSStatus;
                fn CFNumberGetValue(
                    number: CFNumberRef,
                    the_type: CFNumberType,
                    value_ptr: *mut c_void,
                ) -> u8;
            }
            let mut value_ref: CFTypeRef = ptr::null_mut();
            let status =
                VTSessionCopyProperty(encoder.session, key, ptr::null_mut(), &mut value_ref);
            // GitHub's virtual macOS runners expose the symbol but their
            // encoder reports kVTPropertyNotSupportedErr. Real Macs with a
            // supporting VideoToolbox implementation must still return the
            // configured value; only the documented capability miss is
            // accepted here.
            const K_VT_PROPERTY_NOT_SUPPORTED_ERR: OSStatus = -12900;
            if status == K_VT_PROPERTY_NOT_SUPPORTED_ERR {
                eprintln!("MaxAllowedFrameQP is unavailable on this VideoToolbox encoder");
                return;
            }
            assert_eq!(status, 0, "read MaxAllowedFrameQP: {}", cvt_status(status));
            assert!(!value_ref.is_null());
            let mut value = 0i32;
            assert_ne!(
                CFNumberGetValue(
                    value_ref as CFNumberRef,
                    CFNumberType::SInt32,
                    &mut value as *mut i32 as *mut c_void,
                ),
                0
            );
            CFRelease(value_ref);
            assert_eq!(value, 20);
        }
    }

    /// Reproduces the production stall: the app's CG capture loop feeds the
    /// encoder at ~12fps (capture_dt ≈ 83ms) but the encoder task goes silent
    /// for 10-30s between 2-frame bursts. This test feeds frames at the same
    /// cadence and measures each encode() call's wall time. Run with:
    ///   cargo test -p orb-capture sustained -- --nocapture
    #[test]
    #[cfg(target_os = "macos")]
    fn sustained_encode_throughput() {
        let w = 1280u32;
        let h = 720u32;
        let fps = 30u32;
        let mut encoder = VideoToolboxEncoder::new_with_options(w, h, fps, 10_000_000, 2.0)
            .expect("create encoder");
        let y_size = (w * h) as usize;
        let uv_size = ((w / 2) * (h / 2)) as usize;
        let mut yuv = vec![128u8; y_size + uv_size * 2];
        yuv[..y_size].fill(76);

        let n_frames = 240u32; // ~20s at 83ms/frame
        let mut max_dt_ms: u128 = 0;
        let mut slow: Vec<(u32, u128)> = Vec::new();
        let start = std::time::Instant::now();
        for i in 0..n_frames {
            // Vary the content so the encoder does real work every frame.
            let region = (i as usize * 4096) % (y_size - 8192);
            yuv[region..region + 8192].fill((i % 251) as u8);

            let t0 = std::time::Instant::now();
            let frames = encoder.encode(&yuv, w, h).expect("encode");
            let dt = t0.elapsed().as_millis();
            if dt > max_dt_ms {
                max_dt_ms = dt;
            }
            if dt > 200 {
                slow.push((i, dt));
            }
            if i % 20 == 0 {
                eprintln!(
                    "[sustained] frame {}/{} encode_dt={}ms out_frames={} total_elapsed={:.1}s",
                    i,
                    n_frames,
                    dt,
                    frames.len(),
                    start.elapsed().as_secs_f64()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(83));
        }
        eprintln!(
            "[sustained] DONE: max_encode_dt={}ms slow_frames(>200ms)={:?}",
            max_dt_ms, slow
        );
        assert!(
            max_dt_ms < 1000,
            "encode() stalled: max_dt={}ms slow={:?}",
            max_dt_ms,
            slow
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn creates_h264_encoder() {
        let _encoder = VideoToolboxEncoder::new(640, 360, 30).expect("create VideoToolbox encoder");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn encodes_h264_frame() {
        let mut encoder =
            VideoToolboxEncoder::new(640, 360, 30).expect("create VideoToolbox encoder");
        let y_size = 640 * 360;
        let uv_size = (640 / 2) * (360 / 2);
        let mut yuv = vec![128u8; y_size + uv_size * 2];
        yuv[..y_size].fill(76);
        let _frames = encoder
            .encode(&yuv, 640, 360)
            .expect("encode VideoToolbox frame");
    }

    /// Prints the actual SPS profile-level-id VideoToolbox emits for each
    /// quality tier resolution. This is the ground truth we must echo in SDP,
    /// otherwise the browser rejects the bitstream ("first frame timeout").
    #[test]
    #[cfg(target_os = "macos")]
    fn log_sps_profile_level_id_per_resolution() {
        for (w, h, fps, label, expected_level) in [
            (960, 540, 15, "standard", "42001f"),
            (2880, 1800, 20, "high", "420032"),
            (3840, 2160, 20, "pro", "420033"),
        ] {
            let bitrate = match label {
                "high" => 24_000_000,
                "pro" => 32_000_000,
                _ => 2_000_000,
            };
            let mut encoder = VideoToolboxEncoder::new_with_options(w, h, fps, bitrate, 1.0)
                .expect("create VideoToolbox encoder");
            let y_size = (w * h) as usize;
            let uv_size = ((w / 2) * (h / 2)) as usize;
            let mut yuv = vec![128u8; y_size + uv_size * 2];
            yuv[..y_size].fill(76);
            let mut found: Option<String> = None;
            let mut first_bytes: Vec<u8> = Vec::new();
            let mut saw_keyframe = false;
            for n in 0..5 {
                let frames = encoder.encode(&yuv, w, h).expect("encode");
                for f in &frames {
                    if f.is_keyframe {
                        saw_keyframe = true;
                    }
                    if n == 0 && first_bytes.is_empty() {
                        first_bytes = f.data.iter().take(24).copied().collect();
                    }
                    // Scan the raw frame bytes for any SPS NAL (type 7).
                    if let Some(sps) = first_sps_from_frame(&f.data) {
                        if let Some(pli) = profile_level_id_from_sps(&sps) {
                            if found.is_none() {
                                found = Some(pli);
                            }
                        }
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            println!(
                "[SPS] {} ({}x{}): profile-level-id={:?} saw_keyframe={} first24={:02x?}",
                label, w, h, found, saw_keyframe, first_bytes
            );
            let actual = found
                .as_deref()
                .unwrap_or_else(|| panic!("no SPS found for {}x{}", w, h));
            // VideoToolbox may emit Baseline (4200xx) or Constrained Baseline
            // (42e0xx) depending on the macOS version and available encoder.
            // Both use profile_idc 0x42; the constraint flags are allowed to
            // vary, while the negotiated level must remain exact.
            assert_eq!(&actual[..2], &expected_level[..2]);
            assert_eq!(&actual[4..], &expected_level[4..]);
        }
    }
}
