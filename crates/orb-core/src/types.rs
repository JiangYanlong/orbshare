use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInfo {
    pub room_id: String,
    pub room_name: String,
    pub host_name: String,
    pub host_ip: String,
    pub signaling_port: u16,
    /// Actual TCP port of the built-in browser-viewer HTTP server.
    /// May differ from the preferred 8899 when the default port is occupied.
    pub viewer_port: u16,
    pub version: String,
    pub max_guests: u32,
    pub current_guests: u32,
    pub capabilities: Vec<String>,
    pub requires_password: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberInfo {
    pub member_id: String,
    /// Stable identity for attendance aggregation. A reconnect creates a new
    /// transport member_id but keeps this participant_id.
    pub participant_id: String,
    pub name: String,
    pub role: MemberRole,
    pub platform: String,
    pub connected_at: u64,
    /// Client-reported microphone capture state. The host still enforces
    /// moderation independently before relaying incoming audio packets.
    #[serde(default)]
    pub microphone_enabled: bool,
    /// Host-owned moderation state. A muted participant's incoming audio is
    /// discarded even if a stale or modified client keeps sending packets.
    #[serde(default)]
    pub host_muted: bool,
    /// Canonical host-owned raise-hand state. Guests may request a change over
    /// the control DataChannel, but the host stores and orders the queue.
    #[serde(default)]
    pub hand_raised: bool,
    /// Unix timestamp in milliseconds, set only while `hand_raised` is true.
    #[serde(default)]
    pub hand_raised_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttendanceRecord {
    pub participant_id: String,
    pub name: String,
    pub platform: String,
    pub first_joined_at: u64,
    pub last_joined_at: u64,
    pub last_left_at: Option<u64>,
    pub rejoined_at: Vec<u64>,
    pub left_at: Vec<u64>,
    pub total_online_seconds: u64,
    pub current_session_started_at: Option<u64>,
    pub join_count: u32,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub message_id: String,
    pub sender_participant_id: String,
    pub sender_name: String,
    pub sender_role: MemberRole,
    pub content: String,
    /// Unix timestamp in milliseconds.
    pub sent_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MemberRole {
    Host,
    Guest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalingMessage {
    pub msg_type: MsgType,
    pub room_id: String,
    pub sender_id: String,
    pub payload: serde_json::Value,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MsgType {
    JoinRequest,
    JoinResponse,
    Leave,
    RoomClosed,
    SdpOffer,
    SdpAnswer,
    IceCandidate,
    PointerUpdate,
    VideoFrame,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequestPayload {
    pub guest_name: String,
    pub password_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResponsePayload {
    pub success: bool,
    pub error: Option<String>,
    pub guest_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdpPayload {
    pub sdp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceCandidatePayload {
    pub candidate: String,
    pub sdp_mid: Option<String>,
    pub sdp_mline_index: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointerPayload {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomConfig {
    pub room_name: Option<String>,
    pub password: String,
    pub max_guests: Option<u32>,
    /// Native display/window identifier selected by the host. None uses the primary display.
    pub source_id: Option<u32>,
    /// True for a full desktop, false for one application window.
    pub source_is_display: Option<bool>,
    /// Video transport mode: "debug-jpeg-datachannel" (default) or "webrtc-video-track".
    pub video_transport: Option<String>,
    /// Capture mode: "test-pattern" (default) or "screen-capture".
    pub capture_mode: Option<String>,
    /// Quality tier: "standard", "high", or "pro"; policy layer gates paid tiers.
    pub quality_tier: Option<String>,
    /// Capture and relay the selected display's system audio with the screen.
    pub share_system_audio: Option<bool>,
}
