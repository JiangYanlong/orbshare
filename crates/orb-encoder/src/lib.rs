use orb_capture::EncodedFrame;

pub mod software_jpeg;

pub use software_jpeg::{JpegEncodeOptions, SoftwareJpegEncoder};

pub trait StillFrameEncoder {
    fn encode(&self, frame: &EncodedFrame, options: JpegEncodeOptions) -> Option<Vec<u8>>;
}
