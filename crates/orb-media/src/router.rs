use std::sync::Arc;
use std::time::Duration;

use orb_webrtc::session::{HostSession, JpegSendStats};
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

#[derive(Debug)]
pub struct MediaRouterError(String);

impl std::fmt::Display for MediaRouterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for MediaRouterError {}

pub struct MediaRouter;

impl MediaRouter {
    pub async fn write_h264_sample(
        track: Arc<TrackLocalStaticSample>,
        frame_data: Vec<u8>,
        fps: u32,
    ) -> Result<(), MediaRouterError> {
        let duration = Duration::from_micros((1_000_000u64 / fps.max(1) as u64).max(1));
        HostSession::write_video_sample_to_track(track, frame_data, duration)
            .await
            .map_err(|error| MediaRouterError(error.to_string()))
    }

    pub async fn send_mjpeg_frame(
        host: &HostSession,
        jpeg_data: Vec<u8>,
    ) -> Result<JpegSendStats, MediaRouterError> {
        host.send_video_frame_to_peers(jpeg_data)
            .await
            .map_err(|error| MediaRouterError(error.to_string()))
    }
}
