use orb_capture::EncodedFrame;

use crate::StillFrameEncoder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegEncodeOptions {
    pub quality: u8,
    pub target_width: Option<u32>,
    pub target_height: Option<u32>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SoftwareJpegEncoder;

impl StillFrameEncoder for SoftwareJpegEncoder {
    fn encode(&self, frame: &EncodedFrame, options: JpegEncodeOptions) -> Option<Vec<u8>> {
        orb_capture::encode_frame_to_jpeg(
            frame,
            options.quality,
            options.target_width,
            options.target_height,
        )
    }
}

impl SoftwareJpegEncoder {
    pub fn encode_frame(frame: &EncodedFrame, options: JpegEncodeOptions) -> Option<Vec<u8>> {
        Self.encode(frame, options)
    }
}
