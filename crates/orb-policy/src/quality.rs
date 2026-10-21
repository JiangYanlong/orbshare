use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QualityTier {
    Standard,
    High,
    Pro,
}

impl QualityTier {
    pub fn from_raw(value: Option<&str>) -> Self {
        match value {
            Some("standard") => Self::Standard,
            Some("pro") => Self::Pro,
            Some("high") | _ => Self::High,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityProfile {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub keyframe_interval_seconds: f64,
}

impl QualityProfile {
    /// Returns the H.264 Baseline `profile-level-id` to advertise in the SDP.
    ///
    /// 720p fits H.264 Level 3.1, 1080p uses Level 4.0, and Retina text-sharing
    /// profiles require Level 5.0/5.1. The encoder uses the same Baseline level
    /// so SDP and the SPS stay consistent.
    pub fn h264_profile_level_id(&self) -> &'static str {
        if self.width > 2880 || self.height > 1800 {
            "420033"
        } else if self.width > 1920 || self.height > 1080 {
            "420032"
        } else if self.width > 1280 || self.height > 720 {
            "420028"
        } else {
            "42001f"
        }
    }
}

pub fn resolve_quality_profile(requested: Option<&str>) -> QualityProfile {
    match QualityTier::from_raw(requested) {
        QualityTier::Standard => QualityProfile {
            name: "standard",
            width: 960,
            height: 540,
            fps: 15,
            bitrate: 2_000_000,
            keyframe_interval_seconds: 1.0,
        },
        QualityTier::High => QualityProfile {
            name: "high",
            width: 2880,
            height: 1800,
            fps: 20,
            bitrate: 24_000_000,
            keyframe_interval_seconds: 1.0,
        },
        QualityTier::Pro => QualityProfile {
            name: "pro",
            width: 3840,
            height: 2160,
            fps: 20,
            bitrate: 32_000_000,
            keyframe_interval_seconds: 1.0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_profile_uses_low_bandwidth_defaults() {
        let profile = resolve_quality_profile(Some("standard"));

        assert_eq!(profile.name, "standard");
        assert_eq!((profile.width, profile.height, profile.fps), (960, 540, 15));
        assert_eq!(profile.h264_profile_level_id(), "42001f");
    }

    #[test]
    fn pro_profile_preserves_text_detail() {
        let profile = resolve_quality_profile(Some("pro"));

        assert_eq!(profile.name, "pro");
        assert_eq!(
            (profile.width, profile.height, profile.fps),
            (3840, 2160, 20)
        );
        assert_eq!(profile.bitrate, 32_000_000);
        assert_eq!(profile.h264_profile_level_id(), "420033");
    }

    #[test]
    fn high_profile_preserves_retina_text_detail() {
        let profile = resolve_quality_profile(Some("high"));

        assert_eq!(
            (profile.width, profile.height, profile.fps),
            (2880, 1800, 20)
        );
        assert_eq!(profile.bitrate, 24_000_000);
        assert_eq!(profile.h264_profile_level_id(), "420032");
    }
}
