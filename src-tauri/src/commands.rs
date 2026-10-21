use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use orb_capture::{
    create_capture_provider, create_capture_provider_for_mode, CaptureMode, CaptureProvider,
    CaptureSource, CaptureStats, EncodedFrame, VideoTransportMode,
};
use orb_core::admission::{AdmissionDecision, RoomAdmission, EXTENDED_MAX_GUESTS};
use orb_core::network::is_lan_ip;
use orb_core::room::{
    hash_password_for_transport, normalize_password, verify_password_hash, RoomManager,
};
use orb_core::types::{
    AttendanceRecord, ChatMessage, MemberInfo, MemberRole, MsgType, RoomConfig, RoomInfo,
    SignalingMessage,
};
use orb_discovery::publisher::MdnsPublisher;
use orb_discovery::scanner::{MdnsScanner, ScannerEvent};
use orb_encoder::{JpegEncodeOptions, SoftwareJpegEncoder};
use orb_media::audio::OPUS_FRAME_SAMPLES_PER_CHANNEL;
use orb_media::{MediaRouter, SystemAudioOpusEncoder};
use orb_policy::{resolve_quality_profile, QualityProfile};
use orb_signaling::client::SignalingClient;
use orb_signaling::server::{SignalingEvent, SignalingServer};
use orb_webrtc::session::{HostSession, WebRtcEventKind, SYSTEM_AUDIO_SOURCE_ID};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{watch, Mutex, RwLock};
use tokio::task::AbortHandle;

const VIEWER_HTTP_PORT: u16 = 8899;
const VIEWER_HTML: &str = include_str!("../../viewer/index.html");
const VIEWER_PROTOCOL_PLACEHOLDER: &str = "__ORBSHARE_VIEWER_PROTOCOL_VERSION__";
const VIEWER_FAVICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><defs><linearGradient id="g" x1="8" y1="6" x2="56" y2="58" gradientUnits="userSpaceOnUse"><stop stop-color="#2563EB"/><stop offset="1" stop-color="#06B6D4"/></linearGradient></defs><rect width="64" height="64" rx="18" fill="url(#g)"/><circle cx="32" cy="32" r="11" fill="none" stroke="white" stroke-width="4"/><path d="M12 42 52 22" fill="none" stroke="white" stroke-width="4" stroke-linecap="round"/></svg>"##;
const VIEWER_PROTOCOL_VERSION: u32 = 12;
const HOST_AUDIO_MONITOR_ID: &str = "__orbshare_host_audio__";
const MAX_RECORDING_CHUNK_BASE64_BYTES: usize = 32 * 1024 * 1024;
const RECORDING_STARTED_CONTROL_MESSAGE: &str = r#"{"type":"recording_state","active":true}"#;
const RECORDING_STOPPED_CONTROL_MESSAGE: &str = r#"{"type":"recording_state","active":false}"#;

fn participant_audio_may_relay(member: Option<&MemberInfo>, is_host_monitor: bool) -> bool {
    is_host_monitor
        || member
            .map(|member| member.microphone_enabled && !member.host_muted)
            .unwrap_or(false)
}

fn update_member_hand_state(member: &mut MemberInfo, raised: bool, changed_at_ms: u64) -> bool {
    if member.hand_raised == raised {
        return false;
    }
    member.hand_raised = raised;
    member.hand_raised_at = raised.then_some(changed_at_ms);
    true
}

fn viewer_queue_sort_key(member: &MemberInfo) -> (bool, u64, u64) {
    (
        !member.hand_raised,
        member.hand_raised_at.unwrap_or(u64::MAX),
        member.connected_at,
    )
}
const HOST_PREVIEW_INTERVAL: Duration = Duration::from_secs(1);
const HOST_STOP_CONTROL_MESSAGE: &str = r#"{"type":"room_closed","reason":"host_stopped"}"#;
const HOST_REMOVED_CONTROL_MESSAGE: &str = r#"{"type":"removed_by_host","reason":"host_removed"}"#;
const MAX_CHAT_MESSAGES: usize = 100;
const MAX_CHAT_CONTENT_CHARS: usize = 500;
const CHAT_RATE_LIMIT_MS: u64 = 500;

#[derive(Clone, serde::Serialize)]
struct LaserPointerEvent {
    sender_id: String,
    x: f32,
    y: f32,
}

fn encode_laser_pointer(x: f32, y: f32) -> Result<[u8; 8], String> {
    if !x.is_finite() || !y.is_finite() || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return Err("激光笔坐标无效".to_string());
    }
    let mut payload = [0u8; 8];
    payload[..4].copy_from_slice(&x.to_le_bytes());
    payload[4..].copy_from_slice(&y.to_le_bytes());
    Ok(payload)
}

fn decode_laser_pointer(payload: &[u8]) -> Option<(f32, f32)> {
    if payload.len() != 8 {
        return None;
    }
    let x = f32::from_le_bytes(payload[..4].try_into().ok()?);
    let y = f32::from_le_bytes(payload[4..].try_into().ok()?);
    if !x.is_finite() || !y.is_finite() || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return None;
    }
    Some((x, y))
}

#[derive(Clone, serde::Serialize)]
pub struct StreamStats {
    width: u32,
    height: u32,
    actual_fps: f64,
    actual_bitrate_bps: u64,
    target_fps: u32,
    target_bitrate_bps: u32,
    peer_count: usize,
}

#[derive(Clone, serde::Serialize)]
pub struct ActiveCaptureSource {
    source_id: Option<u32>,
    source_is_display: bool,
}

fn h264_nal_summary(data: &[u8]) -> String {
    let mut counts = [0u8; 32];
    let mut i = 0usize;
    while i + 5 <= data.len() {
        let start_len = if data[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if data[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        let nal_start = i + start_len;
        if nal_start < data.len() {
            let nal_type = data[nal_start] & 0x1f;
            if (nal_type as usize) < counts.len() {
                counts[nal_type as usize] = counts[nal_type as usize].saturating_add(1);
            }
        }
        i = nal_start + 1;
    }

    let mut parts = Vec::new();
    for (nal_type, count) in counts.iter().enumerate() {
        if *count > 0 {
            parts.push(format!("{nal_type}:{count}"));
        }
    }
    if parts.is_empty() {
        "nal=none".to_string()
    } else {
        format!("nal={}", parts.join(","))
    }
}

fn room_closed_message() -> SignalingMessage {
    SignalingMessage {
        msg_type: MsgType::RoomClosed,
        room_id: "room".to_string(),
        sender_id: "host".to_string(),
        payload: serde_json::json!({ "reason": "host_stopped" }),
        timestamp: 0,
    }
}

fn viewer_name(raw: &str) -> String {
    let trimmed = raw.trim();
    let name: String = trimmed.chars().take(32).collect();
    if name.is_empty() {
        "网页观看者".to_string()
    } else {
        name
    }
}

fn stable_participant_id(raw: &str, fallback: &str) -> String {
    let id: String = raw
        .trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .take(96)
        .collect();
    if id.is_empty() {
        fallback.to_string()
    } else {
        id
    }
}

fn viewer_protocol_is_supported(version: u32) -> bool {
    version == VIEWER_PROTOCOL_VERSION
}

fn rendered_viewer_html() -> String {
    VIEWER_HTML.replace(
        VIEWER_PROTOCOL_PLACEHOLDER,
        &VIEWER_PROTOCOL_VERSION.to_string(),
    )
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn unix_timestamp_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
struct ChatRoomState {
    messages: VecDeque<ChatMessage>,
    last_sent_at: HashMap<String, u64>,
}

type ChatLog = Arc<RwLock<ChatRoomState>>;
type BlockedParticipants = Arc<RwLock<HashSet<String>>>;

#[derive(Clone, serde::Serialize)]
pub struct RecordingStatus {
    path: String,
    mime_type: String,
    started_at_ms: u64,
    bytes_written: u64,
    chunks_written: u64,
}

#[derive(Clone, serde::Serialize)]
pub struct RecordingResult {
    path: String,
    mime_type: String,
    started_at_ms: u64,
    duration_ms: u64,
    size_bytes: u64,
    chunks_written: u64,
}

struct RecordingSink {
    file: File,
    path: PathBuf,
    mime_type: String,
    started_at_ms: u64,
    bytes_written: u64,
    chunks_written: u64,
}

fn validated_recording_extension(mime_type: &str, extension: &str) -> Result<&'static str, String> {
    let mime = mime_type.trim().to_ascii_lowercase();
    match extension
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" if mime.starts_with("video/mp4") => Ok("mp4"),
        "webm" if mime.starts_with("video/webm") => Ok("webm"),
        _ => Err("当前录制格式不受支持，只允许应用内 MP4 或 WebM 录制。".to_string()),
    }
}

impl RecordingSink {
    fn create_in_directory(
        directory: &Path,
        mime_type: &str,
        extension: &str,
        started_at_ms: u64,
    ) -> Result<Self, String> {
        let extension = validated_recording_extension(mime_type, extension)?;
        std::fs::create_dir_all(directory).map_err(|error| format!("无法创建录制目录：{error}"))?;

        for suffix in 0..100u8 {
            let suffix = if suffix == 0 {
                String::new()
            } else {
                format!("-{suffix}")
            };
            let path = directory.join(format!("OrbShare-{started_at_ms}{suffix}.{extension}"));
            match OpenOptions::new().create_new(true).write(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        file,
                        path,
                        mime_type: mime_type.to_string(),
                        started_at_ms,
                        bytes_written: 0,
                        chunks_written: 0,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("无法创建录制文件：{error}")),
            }
        }
        Err("无法生成唯一的录制文件名，请稍后重试。".to_string())
    }

    fn status(&self) -> RecordingStatus {
        RecordingStatus {
            path: self.path.to_string_lossy().into_owned(),
            mime_type: self.mime_type.clone(),
            started_at_ms: self.started_at_ms,
            bytes_written: self.bytes_written,
            chunks_written: self.chunks_written,
        }
    }

    fn append(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.file
            .write_all(bytes)
            .map_err(|error| format!("写入录制文件失败：{error}"))?;
        self.bytes_written = self.bytes_written.saturating_add(bytes.len() as u64);
        self.chunks_written = self.chunks_written.saturating_add(1);
        Ok(())
    }

    fn finish(mut self, finished_at_ms: u64) -> Result<RecordingResult, String> {
        self.file
            .flush()
            .map_err(|error| format!("刷新录制文件失败：{error}"))?;
        self.file
            .sync_all()
            .map_err(|error| format!("保存录制文件失败：{error}"))?;
        let size_bytes = self
            .file
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(self.bytes_written);
        if size_bytes == 0 || self.chunks_written == 0 {
            let path = self.path.clone();
            drop(self.file);
            let _ = std::fs::remove_file(&path);
            return Err("录制没有生成有效内容，空文件已自动清理，请重新录制。".to_string());
        }
        Ok(RecordingResult {
            path: self.path.to_string_lossy().into_owned(),
            mime_type: self.mime_type,
            started_at_ms: self.started_at_ms,
            duration_ms: finished_at_ms.saturating_sub(self.started_at_ms),
            size_bytes,
            chunks_written: self.chunks_written,
        })
    }

    fn abort(mut self) -> Result<(), String> {
        let _ = self.file.flush();
        let path = self.path.clone();
        drop(self.file);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("清理未开始的录制文件失败：{error}")),
        }
    }
}

fn normalized_chat_content(raw: &str) -> Result<String, String> {
    let content = raw.trim();
    if content.is_empty() {
        return Err("消息内容不能为空".to_string());
    }
    if content.chars().count() > MAX_CHAT_CONTENT_CHARS {
        return Err(format!("消息不能超过 {MAX_CHAT_CONTENT_CHARS} 个字符"));
    }
    Ok(content.to_string())
}

fn append_chat_message(
    chat: &mut ChatRoomState,
    sender_participant_id: &str,
    sender_name: &str,
    sender_role: MemberRole,
    raw_content: &str,
    sent_at: u64,
) -> Result<ChatMessage, String> {
    let content = normalized_chat_content(raw_content)?;
    if let Some(previous) = chat.last_sent_at.get(sender_participant_id) {
        if sent_at.saturating_sub(*previous) < CHAT_RATE_LIMIT_MS {
            return Err("发送太快，请稍后再试".to_string());
        }
    }

    let message = ChatMessage {
        message_id: format!("{sender_participant_id}-{sent_at}-{}", chat.messages.len()),
        sender_participant_id: sender_participant_id.to_string(),
        sender_name: sender_name.to_string(),
        sender_role,
        content,
        sent_at,
    };
    chat.last_sent_at
        .insert(sender_participant_id.to_string(), sent_at);
    chat.messages.push_back(message.clone());
    while chat.messages.len() > MAX_CHAT_MESSAGES {
        chat.messages.pop_front();
    }
    Ok(message)
}

fn chat_control_payload(message: &ChatMessage) -> String {
    serde_json::json!({
        "type": "chat_message",
        "message": message,
    })
    .to_string()
}

fn encode_remote_jpeg(frame: &orb_capture::EncodedFrame, quality_name: &str) -> Option<Vec<u8>> {
    let (max_bytes, presets): (usize, &[(u8, u32, u32)]) = match quality_name {
        "pro" => (
            260 * 1024,
            &[
                (82, 1600, 1000),
                (76, 1600, 1000),
                (72, 1440, 900),
                (68, 1280, 800),
            ],
        ),
        "high" => (
            220 * 1024,
            &[
                (78, 1440, 900),
                (72, 1440, 900),
                (68, 1280, 800),
                (64, 1152, 720),
            ],
        ),
        _ => (
            150 * 1024,
            &[(68, 960, 600), (62, 960, 600), (58, 854, 534)],
        ),
    };

    let mut best: Option<Vec<u8>> = None;
    for (quality, width, height) in presets {
        let Some(jpeg) = SoftwareJpegEncoder::encode_frame(
            frame,
            JpegEncodeOptions {
                quality: *quality,
                target_width: Some(*width),
                target_height: Some(*height),
            },
        ) else {
            continue;
        };
        if jpeg.len() <= max_bytes {
            return Some(jpeg);
        }
        if best
            .as_ref()
            .map(|current| jpeg.len() < current.len())
            .unwrap_or(true)
        {
            best = Some(jpeg);
        }
    }
    best
}

fn spawn_host_preview_pump(
    app: AppHandle,
    mut preview_rx: tokio::sync::mpsc::Receiver<EncodedFrame>,
) {
    tokio::spawn(async move {
        while let Some(mut frame) = preview_rx.recv().await {
            // If the UI or JPEG encoder was briefly busy, monitor the newest
            // available frame instead of building a stale preview queue.
            while let Ok(next) = preview_rx.try_recv() {
                frame = next;
            }
            let encoded = tokio::task::spawn_blocking(move || {
                SoftwareJpegEncoder::encode_frame(
                    &frame,
                    JpegEncodeOptions {
                        quality: 76,
                        target_width: Some(1280),
                        target_height: Some(720),
                    },
                )
            })
            .await;
            let Ok(Some(jpeg)) = encoded else {
                continue;
            };
            let data_url = format!(
                "data:image/jpeg;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&jpeg)
            );
            let _ = app.emit(
                "host-video-frame",
                serde_json::json!({ "dataUrl": data_url }),
            );
        }
    });
}

struct ViewerHttpServer {
    abort_handle: AbortHandle,
    lifecycle_tx: watch::Sender<bool>,
}

/// State for one browser viewer session. The host pushes its own ICE
/// candidates here when `pc.on_ice_candidate` fires, and the viewer's
/// `GET /poll-ice?guest_id=...` drains the buffer.
struct BrowserViewerState {
    host_candidates: Vec<HostIceCandidate>,
    participant_id: String,
}

type AttendanceLog = Arc<RwLock<HashMap<String, AttendanceRecord>>>;

async fn wait_for_browser_terminal_event(
    participant_id: Option<String>,
    blocked_participants: BlockedParticipants,
    mut lifecycle_rx: watch::Receiver<bool>,
) -> &'static str {
    loop {
        if *lifecycle_rx.borrow() {
            return HOST_STOP_CONTROL_MESSAGE;
        }
        if let Some(ref participant_id) = participant_id {
            if blocked_participants.read().await.contains(participant_id) {
                return HOST_REMOVED_CONTROL_MESSAGE;
            }
        }

        tokio::select! {
            changed = lifecycle_rx.changed() => {
                if changed.is_err() {
                    return HOST_STOP_CONTROL_MESSAGE;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}

fn record_attendance_join(
    records: &mut HashMap<String, AttendanceRecord>,
    participant_id: &str,
    name: &str,
    platform: &str,
    joined_at: u64,
) {
    if let Some(record) = records.get_mut(participant_id) {
        record.name = name.to_string();
        record.platform = platform.to_string();
        if !record.online {
            record.rejoined_at.push(joined_at);
            record.last_joined_at = joined_at;
            record.current_session_started_at = Some(joined_at);
            record.join_count = record.join_count.saturating_add(1);
            record.online = true;
        }
        return;
    }

    records.insert(
        participant_id.to_string(),
        AttendanceRecord {
            participant_id: participant_id.to_string(),
            name: name.to_string(),
            platform: platform.to_string(),
            first_joined_at: joined_at,
            last_joined_at: joined_at,
            last_left_at: None,
            rejoined_at: Vec::new(),
            left_at: Vec::new(),
            total_online_seconds: 0,
            current_session_started_at: Some(joined_at),
            join_count: 1,
            online: true,
        },
    );
}

fn record_attendance_leave(
    records: &mut HashMap<String, AttendanceRecord>,
    participant_id: &str,
    left_at: u64,
) {
    let Some(record) = records.get_mut(participant_id) else {
        return;
    };
    if let Some(joined_at) = record.current_session_started_at.take() {
        record.total_online_seconds = record
            .total_online_seconds
            .saturating_add(left_at.saturating_sub(joined_at));
        record.left_at.push(left_at);
        record.last_left_at = Some(left_at);
    }
    record.online = false;
}

fn attendance_snapshot(
    records: &HashMap<String, AttendanceRecord>,
    now: u64,
) -> Vec<AttendanceRecord> {
    let mut result: Vec<AttendanceRecord> = records
        .values()
        .cloned()
        .map(|mut record| {
            if let Some(joined_at) = record.current_session_started_at {
                record.total_online_seconds = record
                    .total_online_seconds
                    .saturating_add(now.saturating_sub(joined_at));
            }
            record
        })
        .collect();
    result.sort_by_key(|record| record.first_joined_at);
    result
}

fn close_all_attendance_sessions(records: &mut HashMap<String, AttendanceRecord>, left_at: u64) {
    let active_ids: Vec<String> = records
        .values()
        .filter(|record| record.online)
        .map(|record| record.participant_id.clone())
        .collect();
    for participant_id in active_ids {
        record_attendance_leave(records, &participant_id, left_at);
    }
}

#[derive(Clone, serde::Serialize)]
struct HostIceCandidate {
    candidate: String,
    sdp_mid: String,
    sdp_mline_index: u16,
}

impl ViewerHttpServer {
    /// Bind the viewer HTTP server, trying `preferred_port` first and falling
    /// back to the next few ports when the preferred one is occupied.
    ///
    /// We intentionally do NOT rely on SO_REUSEADDR/SO_REUSEPORT here: tokio
    /// already sets SO_REUSEADDR on Unix, but that only helps with TIME_WAIT
    /// sockets — it does NOT let us bind over a port that is *actively
    /// listening* in another process (e.g. a leftover `python -m http.server
    /// 8899`). Port fallback is the robust fix for that case.
    ///
    /// In addition to serving the static viewer page, this server implements
    /// a tiny HTTP signaling channel so that a browser on another device can
    /// join as a viewer without going through the WebSocket signaling server
    /// (which is reserved for Tauri WebView guests). Endpoints:
    ///   - `POST /offer?guest_id=...`   body: {sdp: "<JSON-wrapped offer>"}
    ///       Registers a browser peer, creates an SDP answer, returns
    ///       `{sdp, host_ip, room_name, guest_id}`.
    ///   - `POST /ice?guest_id=...`     body: {candidate, sdp_mid, sdp_mline_index}
    ///       Forwards a viewer-side ICE candidate to the host peer.
    ///   - `GET  /poll-ice?guest_id=...` Returns and drains host ICE candidates.
    ///   - `GET  /events?guest_id=...`   Waits for a terminal meeting event.
    ///   - `GET  /leave?guest_id=...`    Closes the peer connection.
    async fn start(
        preferred_port: u16,
        host_session: Arc<RwLock<HostSession>>,
        host_ip: String,
        room_name: String,
        system_audio_active: bool,
        recording_active: Arc<AtomicBool>,
        password_hash: String,
        password_salt: String,
        browser_viewers: Arc<Mutex<HashMap<String, BrowserViewerState>>>,
        members: Arc<RwLock<HashMap<String, MemberInfo>>>,
        attendance: AttendanceLog,
        chat: ChatLog,
        blocked_participants: BlockedParticipants,
        room_admission: RoomAdmission,
    ) -> Result<(Self, u16), String> {
        const PORT_RANGE: u16 = 5; // try preferred_port ..= preferred_port + 4
        let last_port = preferred_port.saturating_add(PORT_RANGE - 1);
        let mut attempts: Vec<String> = Vec::new();
        for port in preferred_port..=last_port {
            match TcpListener::bind(("0.0.0.0", port)).await {
                Ok(listener) => {
                    let (lifecycle_tx, _) = watch::channel(false);
                    let accept_lifecycle_tx = lifecycle_tx.clone();
                    let handle = tokio::spawn(async move {
                        loop {
                            let Ok((mut stream, addr)) = listener.accept().await else {
                                break;
                            };
                            if !is_lan_ip(addr.ip()) {
                                tracing::warn!(
                                    "[viewer] rejected non-LAN HTTP connection from {addr}"
                                );
                                tokio::spawn(async move {
                                    write_simple(
                                        &mut stream,
                                        "403 Forbidden",
                                        "application/json; charset=utf-8",
                                        br#"{"error":"lan_only"}"#.to_vec(),
                                    )
                                    .await;
                                });
                                continue;
                            }
                            let host_session = host_session.clone();
                            let host_ip = host_ip.clone();
                            let room_name = room_name.clone();
                            let recording_active = recording_active.clone();
                            let password_hash = password_hash.clone();
                            let password_salt = password_salt.clone();
                            let browser_viewers = browser_viewers.clone();
                            let members = members.clone();
                            let attendance = attendance.clone();
                            let chat = chat.clone();
                            let blocked_participants = blocked_participants.clone();
                            let room_admission = room_admission.clone();
                            let lifecycle_rx = accept_lifecycle_tx.subscribe();
                            tokio::spawn(async move {
                                handle_viewer_http_request(
                                    &mut stream,
                                    host_session,
                                    host_ip,
                                    room_name,
                                    system_audio_active,
                                    recording_active,
                                    password_hash,
                                    password_salt,
                                    browser_viewers,
                                    members,
                                    attendance,
                                    chat,
                                    blocked_participants,
                                    room_admission,
                                    lifecycle_rx,
                                )
                                .await;
                            });
                        }
                    });
                    tracing::info!("[viewer] HTTP server bound on 0.0.0.0:{}", port);
                    return Ok((
                        Self {
                            abort_handle: handle.abort_handle(),
                            lifecycle_tx,
                        },
                        port,
                    ));
                }
                Err(e) => {
                    tracing::warn!("[viewer] bind 0.0.0.0:{} failed: {}", port, e);
                    attempts.push(format!("{}: {}", port, e));
                }
            }
        }
        Err(format!(
            "无法在内置浏览器观看端口 {}-{} 上启动服务（均被占用）。请先关闭占用这些端口的旧服务（例如 Python SimpleHTTPServer），然后重新创建房间。详情：{}",
            preferred_port, last_port, attempts.join("; ")
        ))
    }

    fn shutdown(&self) {
        self.abort_handle.abort();
    }

    fn notify_room_closed(&self) {
        // A pending /events request receives this even after the listener is
        // stopped, because accepted request tasks own their watch receiver.
        self.lifecycle_tx.send_replace(true);
    }
}

/// Read a single HTTP request (headers + body) from `stream` and dispatch it.
async fn handle_viewer_http_request(
    stream: &mut tokio::net::TcpStream,
    host_session: Arc<RwLock<HostSession>>,
    host_ip: String,
    room_name: String,
    system_audio_active: bool,
    recording_active: Arc<AtomicBool>,
    password_hash: String,
    password_salt: String,
    browser_viewers: Arc<Mutex<HashMap<String, BrowserViewerState>>>,
    members: Arc<RwLock<HashMap<String, MemberInfo>>>,
    attendance: AttendanceLog,
    chat: ChatLog,
    blocked_participants: BlockedParticipants,
    room_admission: RoomAdmission,
    lifecycle_rx: watch::Receiver<bool>,
) {
    // Cap request size at 64 KiB. SDP bodies are typically <8 KiB.
    const MAX_HEADER_BYTES: usize = 16 * 1024;
    const MAX_TOTAL_BYTES: usize = 64 * 1024;
    let mut buf: Vec<u8> = Vec::with_capacity(MAX_HEADER_BYTES);
    let header_end = loop {
        let mut chunk = [0u8; 4096];
        let n = match stream.read(&mut chunk).await {
            Ok(0) => return,
            Ok(n) => n,
            Err(_) => return,
        };
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_TOTAL_BYTES {
            return;
        }
        if let Some(idx) = find_double_crlf(&buf) {
            break idx;
        }
        if buf.len() >= MAX_HEADER_BYTES {
            return;
        }
    };
    let header_str = match std::str::from_utf8(&buf[..header_end]) {
        Ok(s) => s,
        Err(_) => return,
    };

    let mut lines = header_str.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let full_path = parts.next().unwrap_or("/").to_string();

    let (path, query) = match full_path.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (full_path.clone(), String::new()),
    };
    let query_params = parse_query_string(&query);

    let mut content_length: usize = 0;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    if content_length > MAX_TOTAL_BYTES {
        return;
    }

    let mut body = Vec::new();
    if content_length > 0 {
        let already_have = buf.len().saturating_sub(header_end + 4);
        if already_have >= content_length {
            body.extend_from_slice(&buf[header_end + 4..header_end + 4 + content_length]);
        } else {
            body.extend_from_slice(&buf[header_end + 4..]);
            while body.len() < content_length {
                let mut chunk = [0u8; 4096];
                let n = match stream.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => return,
                };
                body.extend_from_slice(&chunk[..n]);
                if body.len() > MAX_TOTAL_BYTES {
                    return;
                }
            }
            body.truncate(content_length);
        }
    }

    let (status, content_type, response_body): (&str, &str, Vec<u8>) =
        match (method.as_str(), path.as_str()) {
            ("GET", "/") | ("GET", "/index.html") => (
                "200 OK",
                "text/html; charset=utf-8",
                rendered_viewer_html().into_bytes(),
            ),
            ("GET", "/favicon.ico") => (
                "200 OK",
                "image/svg+xml; charset=utf-8",
                VIEWER_FAVICON_SVG.as_bytes().to_vec(),
            ),
            ("GET", "/health") => ("200 OK", "text/plain; charset=utf-8", b"ok".to_vec()),
            ("GET", "/events") => {
                let guest_id = query_params
                    .get("guest_id")
                    .map(String::as_str)
                    .unwrap_or("unknown");
                tracing::info!("[viewer] lifecycle event channel connected: {guest_id}");
                let participant_id = browser_viewers
                    .lock()
                    .await
                    .get(guest_id)
                    .map(|viewer| viewer.participant_id.clone());
                let payload = wait_for_browser_terminal_event(
                    participant_id,
                    blocked_participants.clone(),
                    lifecycle_rx,
                )
                .await;
                tracing::info!(
                    "[viewer] terminal event delivered over HTTP to {guest_id}: {payload}"
                );
                (
                    "200 OK",
                    "application/json; charset=utf-8",
                    payload.as_bytes().to_vec(),
                )
            }
            ("GET", "/room-info") => (
                "200 OK",
                "application/json; charset=utf-8",
                serde_json::to_vec(&serde_json::json!({
                    "host_ip": host_ip,
                    "room_name": room_name,
                    "system_audio_active": system_audio_active,
                    "recording_active": recording_active.load(Ordering::Relaxed),
                    "requires_password": !password_hash.is_empty(),
                    "viewer_protocol_version": VIEWER_PROTOCOL_VERSION,
                    "max_guests": room_admission.max_guests(),
                    "current_guests": room_admission.current_guests(),
                }))
                .unwrap_or_else(|_| b"{}".to_vec()),
            ),
            ("POST", "/offer") => {
                let guest_id_param = query_params
                    .get("guest_id")
                    .cloned()
                    .unwrap_or_else(|| format!("browser-{}", uuid_like()));
                match handle_browser_offer(
                    &host_session,
                    &host_ip,
                    &room_name,
                    &browser_viewers,
                    &members,
                    &attendance,
                    &chat,
                    &blocked_participants,
                    &room_admission,
                    recording_active.load(Ordering::Relaxed),
                    &guest_id_param,
                    &password_hash,
                    &password_salt,
                    &body,
                )
                .await
                {
                    Ok(json) => (
                        "200 OK",
                        "application/json; charset=utf-8",
                        json.into_bytes(),
                    ),
                    Err(e) => {
                        tracing::warn!("[viewer] /offer failed: {}", e);
                        if e == "invalid password" {
                            (
                                "401 Unauthorized",
                                "application/json; charset=utf-8",
                                br#"{"error":"invalid_password"}"#.to_vec(),
                            )
                        } else if e == "viewer_update_required" {
                            (
                                "426 Upgrade Required",
                                "application/json; charset=utf-8",
                                serde_json::to_vec(&serde_json::json!({
                                    "error": "viewer_update_required",
                                    "viewer_protocol_version": VIEWER_PROTOCOL_VERSION,
                                }))
                                .unwrap_or_else(|_| {
                                    br#"{"error":"viewer_update_required"}"#.to_vec()
                                }),
                            )
                        } else if e == "participant_removed" {
                            (
                                "403 Forbidden",
                                "application/json; charset=utf-8",
                                br#"{"error":"participant_removed"}"#.to_vec(),
                            )
                        } else if e == "room_full" {
                            (
                                "409 Conflict",
                                "application/json; charset=utf-8",
                                serde_json::to_vec(&serde_json::json!({
                                    "error": "room_full",
                                    "max_guests": room_admission.max_guests(),
                                }))
                                .unwrap_or_else(|_| br#"{"error":"room_full"}"#.to_vec()),
                            )
                        } else {
                            (
                                "500 Internal Server Error",
                                "text/plain; charset=utf-8",
                                format!("offer failed: {e}").into_bytes(),
                            )
                        }
                    }
                }
            }
            ("POST", "/ice") => {
                let guest_id_param = match query_params.get("guest_id") {
                    Some(g) => g.clone(),
                    None => {
                        return write_simple(
                            stream,
                            "400 Bad Request",
                            "text/plain; charset=utf-8",
                            b"missing guest_id".to_vec(),
                        )
                        .await;
                    }
                };
                match handle_browser_ice(&host_session, &guest_id_param, &body).await {
                    Ok(()) => ("200 OK", "text/plain; charset=utf-8", b"ok".to_vec()),
                    Err(e) => {
                        tracing::warn!("[viewer] /ice failed: {}", e);
                        (
                            "500 Internal Server Error",
                            "text/plain; charset=utf-8",
                            format!("ice failed: {e}").into_bytes(),
                        )
                    }
                }
            }
            ("GET", "/poll-ice") => {
                let guest_id_param = match query_params.get("guest_id") {
                    Some(g) => g.clone(),
                    None => {
                        return write_simple(
                            stream,
                            "400 Bad Request",
                            "text/plain; charset=utf-8",
                            b"missing guest_id".to_vec(),
                        )
                        .await;
                    }
                };
                match handle_browser_poll_ice(&browser_viewers, &guest_id_param).await {
                    Ok(json) => (
                        "200 OK",
                        "application/json; charset=utf-8",
                        json.into_bytes(),
                    ),
                    Err(e) => (
                        "500 Internal Server Error",
                        "text/plain; charset=utf-8",
                        format!("poll failed: {e}").into_bytes(),
                    ),
                }
            }
            ("GET", "/leave") | ("POST", "/leave") => {
                if let Some(guest_id) = query_params.get("guest_id") {
                    handle_browser_leave(
                        &host_session,
                        &browser_viewers,
                        &members,
                        &attendance,
                        &room_admission,
                        guest_id,
                    )
                    .await;
                }
                ("200 OK", "text/plain; charset=utf-8", b"ok".to_vec())
            }
            _ => (
                "404 Not Found",
                "text/plain; charset=utf-8",
                b"not found".to_vec(),
            ),
        };

    write_simple(stream, status, content_type, response_body).await;
}

async fn write_simple(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    content_type: &str,
    body: Vec<u8>,
) {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes()).await;
    let _ = stream.write_all(&body).await;
    let _ = stream.shutdown().await;
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_query_string(q: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        if let Some((k, v)) = pair.split_once('=') {
            out.insert(url_decode(k), url_decode(v));
        } else {
            out.insert(url_decode(pair), String::new());
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_val(bytes[i + 1]);
                let lo = hex_val(bytes[i + 2]);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h << 4) | l);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn uuid_like() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
}

async fn handle_browser_offer(
    host_session: &Arc<RwLock<HostSession>>,
    host_ip: &str,
    room_name: &str,
    browser_viewers: &Arc<Mutex<HashMap<String, BrowserViewerState>>>,
    members: &Arc<RwLock<HashMap<String, MemberInfo>>>,
    attendance: &AttendanceLog,
    chat: &ChatLog,
    blocked_participants: &BlockedParticipants,
    room_admission: &RoomAdmission,
    recording_active: bool,
    guest_id: &str,
    stored_password_hash: &str,
    password_salt: &str,
    body: &[u8],
) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct OfferBody {
        sdp: String,
        #[serde(default)]
        password_hash: String,
        #[serde(default)]
        guest_name: String,
        #[serde(default)]
        participant_id: String,
        #[serde(default)]
        viewer_protocol_version: u32,
    }
    let parsed: OfferBody =
        serde_json::from_slice(body).map_err(|e| format!("invalid offer body: {e}"))?;

    if !viewer_protocol_is_supported(parsed.viewer_protocol_version) {
        return Err("viewer_update_required".to_string());
    }

    if !verify_password_hash(&parsed.password_hash, stored_password_hash, password_salt) {
        return Err("invalid password".to_string());
    }

    let joined_at = unix_timestamp();
    let participant_id = stable_participant_id(&parsed.participant_id, guest_id);
    if blocked_participants.read().await.contains(&participant_id) {
        return Err("participant_removed".to_string());
    }
    let admission_decision = room_admission.try_admit(guest_id);
    if admission_decision == AdmissionDecision::Full {
        return Err("room_full".to_string());
    }
    let newly_admitted = admission_decision == AdmissionDecision::Admitted;
    let name = viewer_name(&parsed.guest_name);
    let platform = "网页".to_string();

    members.write().await.insert(
        guest_id.to_string(),
        MemberInfo {
            member_id: guest_id.to_string(),
            participant_id: participant_id.clone(),
            name: name.clone(),
            role: MemberRole::Guest,
            platform: platform.clone(),
            connected_at: joined_at,
            microphone_enabled: false,
            host_muted: false,
            hand_raised: false,
            hand_raised_at: None,
        },
    );

    // Register the HTTP viewer before creating the answer: host ICE candidates
    // may fire immediately from setLocalDescription and must not be routed to
    // the WebSocket signaling path.
    {
        let mut map = browser_viewers.lock().await;
        map.insert(
            guest_id.to_string(),
            BrowserViewerState {
                host_candidates: Vec::new(),
                participant_id: participant_id.clone(),
            },
        );
    }

    // add_peer returns () and we have to swallow errors since webrtc-rs may
    // emit via the existing event stream for late ICE candidates.
    {
        let mut session = host_session.write().await;
        let _ = session.remove_peer(guest_id).await;
        if let Err(error) = session.add_peer(guest_id.to_string(), vec![]).await {
            browser_viewers.lock().await.remove(guest_id);
            members.write().await.remove(guest_id);
            if newly_admitted {
                room_admission.release(guest_id);
            }
            return Err(format!("add_peer failed: {error}"));
        }
    }

    let answer_result = {
        let session = host_session.read().await;
        session
            .set_peer_sdp_offer(guest_id, parsed.sdp.clone())
            .await
            .map_err(|e| format!("set_peer_sdp_offer failed: {e}"))
    };
    let answer_json = match answer_result {
        Ok(answer) => answer,
        Err(error) => {
            browser_viewers.lock().await.remove(guest_id);
            members.write().await.remove(guest_id);
            let _ = host_session.write().await.remove_peer(guest_id).await;
            if newly_admitted {
                room_admission.release(guest_id);
            }
            return Err(error);
        }
    };

    let chat_history: Vec<ChatMessage> = chat.read().await.messages.iter().cloned().collect();
    let response = serde_json::json!({
        "sdp": answer_json,
        "host_ip": host_ip,
        "room_name": room_name,
        "guest_id": guest_id,
        "chat_history": chat_history,
        "recording_active": recording_active,
    });
    record_attendance_join(
        &mut *attendance.write().await,
        &participant_id,
        &name,
        &platform,
        joined_at,
    );
    Ok(serde_json::to_string(&response).unwrap_or_default())
}

async fn handle_browser_ice(
    host_session: &Arc<RwLock<HostSession>>,
    guest_id: &str,
    body: &[u8],
) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct IceBody {
        #[serde(default)]
        candidate: String,
        #[serde(default)]
        sdp_mid: Option<String>,
        #[serde(default)]
        sdp_mline_index: Option<u16>,
    }
    let parsed: IceBody =
        serde_json::from_slice(body).map_err(|e| format!("invalid ice body: {e}"))?;
    if parsed.candidate.is_empty() {
        // Empty candidate string == "gathering complete" sentinel from
        // browser RTCPeerConnection; ignore it (no addIceCandidate needed).
        return Ok(());
    }
    let peer = {
        let session = host_session.read().await;
        session
            .peer_connection(guest_id)
            .ok_or_else(|| format!("peer {guest_id} not found"))?
    };
    use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
    let init = RTCIceCandidateInit {
        candidate: parsed.candidate.clone(),
        sdp_mid: parsed.sdp_mid.clone(),
        sdp_mline_index: parsed.sdp_mline_index,
        username_fragment: None,
    };
    peer.add_ice_candidate(init)
        .await
        .map_err(|e| format!("add_ice_candidate failed: {e}"))?;
    Ok(())
}

async fn handle_browser_poll_ice(
    browser_viewers: &Arc<Mutex<HashMap<String, BrowserViewerState>>>,
    guest_id: &str,
) -> Result<String, String> {
    let mut map = browser_viewers.lock().await;
    let entry = map
        .get_mut(guest_id)
        .ok_or_else(|| format!("guest {guest_id} not registered"))?;
    let drained: Vec<HostIceCandidate> = entry.host_candidates.drain(..).collect();
    let response = serde_json::json!({
        "candidates": drained,
    });
    Ok(serde_json::to_string(&response).unwrap_or_default())
}

async fn handle_browser_leave(
    host_session: &Arc<RwLock<HostSession>>,
    browser_viewers: &Arc<Mutex<HashMap<String, BrowserViewerState>>>,
    members: &Arc<RwLock<HashMap<String, MemberInfo>>>,
    attendance: &AttendanceLog,
    room_admission: &RoomAdmission,
    guest_id: &str,
) {
    let participant_id = browser_viewers
        .lock()
        .await
        .remove(guest_id)
        .map(|state| state.participant_id);
    members.write().await.remove(guest_id);
    if let Some(participant_id) = participant_id {
        record_attendance_leave(
            &mut *attendance.write().await,
            &participant_id,
            unix_timestamp(),
        );
    }
    let _ = host_session.write().await.remove_peer(guest_id).await;
    room_admission.release(guest_id);
}

/// Application state shared across all Tauri commands.
pub struct AppState {
    room_manager: RoomManager,
    // The normalized plain-text password exists only for the lifetime of the
    // active room so the local host can reveal/copy it from the invite dialog.
    // It is never published, logged, written to disk, or returned to guests.
    active_room_password: Option<String>,
    capture_provider: Option<Box<dyn CaptureProvider>>,
    // Meeting-scoped capture settings are retained so the host can switch
    // from a desktop to a window (or vice versa) without recreating the room.
    active_capture_config: Option<orb_capture::CaptureConfig>,
    active_capture_mode: Option<CaptureMode>,
    active_quality: Option<QualityProfile>,
    host_session: Option<Arc<RwLock<HostSession>>>,
    signaling_server: Option<SignalingServer>,
    viewer_http_server: Option<ViewerHttpServer>,
    mdns_publisher: Option<MdnsPublisher>,
    mdns_scanner: Option<MdnsScanner>,
    // Guest-side state (browser-driven WebRTC via signaling relay)
    guest_signaling: Option<orb_signaling::client::SignalingSender>,
    guest_id: Option<String>,
    guest_room_id: Option<String>,
    // Shared with ViewerHttpServer and host event task: tracks browser-viewer
    // ICE candidates that need to be drained via HTTP /poll-ice.
    browser_viewers: Arc<Mutex<HashMap<String, BrowserViewerState>>>,
    // Meeting-scoped attendance history retained after stop_room so a later
    // host-only summary/export screen can read the completed meeting.
    attendance: AttendanceLog,
    // Room-local, in-memory public chat. It is cleared when a new meeting is
    // created and never leaves the OrbShare process.
    chat: ChatLog,
    // A host removal blocks the stable participant identity for the remainder
    // of the current room, preventing an automatic reconnect loop.
    blocked_participants: BlockedParticipants,
    // Shared admission gate for native-client and browser viewers.
    room_admission: Option<RoomAdmission>,
    // The WebView's MediaRecorder streams small chunks into this app-owned
    // file. Only one host recording may exist at a time.
    recording: Option<RecordingSink>,
    // Shared with the LAN viewer server so a late joiner immediately sees the
    // current recording state from /room-info.
    recording_active: Arc<AtomicBool>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            room_manager: RoomManager::new(),
            active_room_password: None,
            capture_provider: None,
            active_capture_config: None,
            active_capture_mode: None,
            active_quality: None,
            host_session: None,
            signaling_server: None,
            viewer_http_server: None,
            mdns_publisher: None,
            mdns_scanner: None,
            guest_signaling: None,
            guest_id: None,
            guest_room_id: None,
            browser_viewers: Arc::new(Mutex::new(HashMap::new())),
            attendance: Arc::new(RwLock::new(HashMap::new())),
            chat: Arc::new(RwLock::new(ChatRoomState::default())),
            blocked_participants: Arc::new(RwLock::new(HashSet::new())),
            room_admission: None,
            recording: None,
            recording_active: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Thread-safe shared state managed by Tauri.
pub type SharedState = Arc<Mutex<AppState>>;

/// Return the room that is still being hosted by this process.
///
/// The frontend can be navigated or reloaded without stopping capture, so the
/// backend remains the source of truth for whether a meeting is active.
#[tauri::command]
pub async fn get_active_room(
    state: tauri::State<'_, SharedState>,
) -> Result<Option<RoomInfo>, String> {
    let s = state.lock().await;
    if s.signaling_server.is_some() && s.capture_provider.is_some() {
        Ok(s.room_manager.room.clone())
    } else {
        Ok(None)
    }
}

/// Return the active meeting password to the local host WebView only.
///
/// The password is retained in process memory for the current room and is
/// cleared as soon as the room stops. Guest/browser endpoints never call or
/// expose this command.
#[tauri::command]
pub async fn get_active_room_password(
    state: tauri::State<'_, SharedState>,
) -> Result<Option<String>, String> {
    let s = state.lock().await;
    if s.signaling_server.is_some() && s.capture_provider.is_some() {
        Ok(s.active_room_password.clone())
    } else {
        Ok(None)
    }
}

#[tauri::command]
pub async fn get_connected_viewers(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<MemberInfo>, String> {
    let members = {
        let state = state.lock().await;
        state.room_manager.members.clone()
    };
    let mut viewers: Vec<MemberInfo> = members
        .read()
        .await
        .values()
        .filter(|member| member.role == MemberRole::Guest)
        .cloned()
        .collect();
    viewers.sort_by_key(viewer_queue_sort_key);
    Ok(viewers)
}

/// Host-only meeting attendance snapshot. This command is exposed only to the
/// native Tauri frontend; the LAN viewer HTTP server has no matching route.
#[tauri::command]
pub async fn get_attendance_stats(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<AttendanceRecord>, String> {
    let attendance = {
        let state = state.lock().await;
        state.attendance.clone()
    };
    let records = attendance.read().await;
    let snapshot = attendance_snapshot(&records, unix_timestamp());
    Ok(snapshot)
}

#[tauri::command]
pub async fn get_chat_messages(
    state: tauri::State<'_, SharedState>,
) -> Result<Vec<ChatMessage>, String> {
    let chat = {
        let state = state.lock().await;
        state.chat.clone()
    };
    let messages = chat.read().await.messages.iter().cloned().collect();
    Ok(messages)
}

#[tauri::command]
pub async fn send_host_chat_message(
    app: AppHandle,
    state: tauri::State<'_, SharedState>,
    content: String,
) -> Result<ChatMessage, String> {
    let (chat, host_session) = {
        let state = state.lock().await;
        let host_session = state.host_session.clone().ok_or("当前没有正在进行的会议")?;
        (state.chat.clone(), host_session)
    };
    let message = append_chat_message(
        &mut *chat.write().await,
        "host",
        "主持人",
        MemberRole::Host,
        &content,
        unix_timestamp_millis(),
    )?;
    let payload = chat_control_payload(&message);
    host_session
        .read()
        .await
        .broadcast_control_message(&payload)
        .await;
    let _ = app.emit("chat-message", &message);
    Ok(message)
}

#[tauri::command]
pub async fn send_host_pointer(
    state: tauri::State<'_, SharedState>,
    x: f32,
    y: f32,
) -> Result<usize, String> {
    let payload = encode_laser_pointer(x, y)?;
    let host_session = {
        let state = state.lock().await;
        state.host_session.clone().ok_or("当前没有正在进行的会议")?
    };
    let notified = host_session
        .read()
        .await
        .broadcast_pointer_data(&payload, None)
        .await;
    Ok(notified)
}

#[tauri::command]
pub async fn remove_viewer(
    app: AppHandle,
    state: tauri::State<'_, SharedState>,
    member_id: String,
) -> Result<(), String> {
    tracing::info!("[moderation] remove requested for member_id={member_id}");
    let (host_session, browser_viewers, members, attendance, blocked_participants) = {
        let state = state.lock().await;
        (
            state.host_session.clone().ok_or("当前没有正在进行的会议")?,
            state.browser_viewers.clone(),
            state.room_manager.members.clone(),
            state.attendance.clone(),
            state.blocked_participants.clone(),
        )
    };

    let member = members
        .read()
        .await
        .get(&member_id)
        .cloned()
        .ok_or("观看者已离开会议")?;
    if member.role != MemberRole::Guest {
        return Err("不能移除主持人".to_string());
    }
    blocked_participants
        .write()
        .await
        .insert(member.participant_id.clone());

    if let Err(error) = host_session
        .read()
        .await
        .send_control_message(&member_id, HOST_REMOVED_CONTROL_MESSAGE)
        .await
    {
        tracing::warn!("[moderation] removal notice to {member_id} failed: {error}");
    }
    // Give WebRTC a small opportunity to flush the reliable control packet
    // before closing the peer connection.
    tokio::time::sleep(Duration::from_millis(60)).await;

    browser_viewers.lock().await.remove(&member_id);
    members.write().await.remove(&member_id);
    record_attendance_leave(
        &mut *attendance.write().await,
        &member.participant_id,
        unix_timestamp(),
    );
    host_session
        .write()
        .await
        .remove_peer(&member_id)
        .await
        .map_err(|error| format!("移除观看者失败：{error}"))?;
    let _ = app.emit("peer-disconnected", &member_id);
    tracing::info!(
        "[moderation] host removed viewer {} ({})",
        member.name,
        member.participant_id
    );
    Ok(())
}

#[tauri::command]
pub async fn set_viewer_muted(
    app: AppHandle,
    state: tauri::State<'_, SharedState>,
    member_id: String,
    muted: bool,
) -> Result<MemberInfo, String> {
    let (host_session, members) = {
        let state = state.lock().await;
        (
            state.host_session.clone().ok_or("当前没有正在进行的会议")?,
            state.room_manager.members.clone(),
        )
    };

    let member = {
        let mut members = members.write().await;
        let member = members.get_mut(&member_id).ok_or("观看者已离开会议")?;
        if member.role != MemberRole::Guest {
            return Err("不能禁言主持人".to_string());
        }
        member.host_muted = muted;
        if muted {
            member.microphone_enabled = false;
        }
        member.clone()
    };

    let payload = serde_json::json!({
        "type": "set_muted",
        "muted": muted,
    })
    .to_string();
    if let Err(error) = host_session
        .read()
        .await
        .send_control_message(&member_id, &payload)
        .await
    {
        // The host drops muted RTP packets independently of client state, so a
        // temporarily unavailable control channel must not undo moderation.
        tracing::warn!("[moderation] mute notice to {member_id} failed: {error}");
    }
    let _ = app.emit("member-updated", &member);
    Ok(member)
}

#[tauri::command]
pub async fn set_viewer_hand_raised(
    app: AppHandle,
    state: tauri::State<'_, SharedState>,
    member_id: String,
    raised: bool,
) -> Result<MemberInfo, String> {
    let (host_session, members) = {
        let state = state.lock().await;
        (
            state.host_session.clone().ok_or("当前没有正在进行的会议")?,
            state.room_manager.members.clone(),
        )
    };
    let member = {
        let mut members = members.write().await;
        let member = members.get_mut(&member_id).ok_or("观看者已离开会议")?;
        if member.role != MemberRole::Guest {
            return Err("不能修改主持人的举手状态".to_string());
        }
        update_member_hand_state(member, raised, unix_timestamp_millis());
        member.clone()
    };
    let payload = serde_json::json!({
        "type": "hand_state",
        "raised": member.hand_raised,
        "raised_at": member.hand_raised_at,
    })
    .to_string();
    if let Err(error) = host_session
        .read()
        .await
        .send_control_message(&member_id, &payload)
        .await
    {
        tracing::debug!("[raise-hand] state notice to {member_id} failed: {error}");
    }
    let _ = app.emit("member-updated", &member);
    Ok(member)
}

#[tauri::command]
pub async fn connect_host_audio_monitor(
    state: tauri::State<'_, SharedState>,
    offer_sdp: String,
) -> Result<String, String> {
    let host_session = {
        let state = state.lock().await;
        state.host_session.clone().ok_or("当前没有正在进行的会议")?
    };

    {
        let mut host = host_session.write().await;
        let _ = host.remove_peer(HOST_AUDIO_MONITOR_ID).await;
        host.add_monitor_peer(HOST_AUDIO_MONITOR_ID.to_string(), vec![])
            .await
            .map_err(|error| format!("创建主持人媒体监视连接失败：{error}"))?;
    }

    let answer = host_session
        .read()
        .await
        .set_peer_sdp_offer_with_complete_ice(HOST_AUDIO_MONITOR_ID, offer_sdp)
        .await;
    match answer {
        Ok(answer) => Ok(answer),
        Err(error) => {
            let _ = host_session
                .write()
                .await
                .remove_peer(HOST_AUDIO_MONITOR_ID)
                .await;
            Err(format!("主持人媒体监视协商失败：{error}"))
        }
    }
}

#[tauri::command]
pub async fn disconnect_host_audio_monitor(
    state: tauri::State<'_, SharedState>,
) -> Result<(), String> {
    let host_session = {
        let state = state.lock().await;
        state.host_session.clone()
    };
    if let Some(host_session) = host_session {
        host_session
            .write()
            .await
            .remove_peer(HOST_AUDIO_MONITOR_ID)
            .await
            .map_err(|error| format!("关闭主持人音频连接失败：{error}"))?;
    }
    Ok(())
}

async fn notify_recording_state(host_session: Option<Arc<RwLock<HostSession>>>, active: bool) {
    let Some(host_session) = host_session else {
        return;
    };
    let payload = if active {
        RECORDING_STARTED_CONTROL_MESSAGE
    } else {
        RECORDING_STOPPED_CONTROL_MESSAGE
    };
    let notified = host_session
        .read()
        .await
        .broadcast_control_message(payload)
        .await;
    tracing::info!("[recording] recording_active={active}; notified {notified} viewers");
}

#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    state: tauri::State<'_, SharedState>,
    mime_type: String,
    extension: String,
) -> Result<RecordingStatus, String> {
    let output_directory = app
        .path()
        .video_dir()
        .map_err(|error| format!("无法读取系统影片目录：{error}"))?
        .join("OrbShare");
    let (status, host_session) = {
        let mut state = state.lock().await;
        if state.host_session.is_none() || state.capture_provider.is_none() {
            return Err("当前没有正在进行的屏幕共享。".to_string());
        }
        if state.recording.is_some() {
            return Err("当前会议已经在录制。".to_string());
        }
        let sink = RecordingSink::create_in_directory(
            &output_directory,
            &mime_type,
            &extension,
            unix_timestamp_millis(),
        )?;
        let status = sink.status();
        state.recording = Some(sink);
        state.recording_active.store(true, Ordering::Relaxed);
        (status, state.host_session.clone())
    };
    notify_recording_state(host_session, true).await;
    Ok(status)
}

#[tauri::command]
pub async fn append_recording_chunk(
    state: tauri::State<'_, SharedState>,
    data_base64: String,
) -> Result<RecordingStatus, String> {
    if data_base64.len() > MAX_RECORDING_CHUNK_BASE64_BYTES {
        return Err("单个录制分片过大，已停止写入以保护应用内存。".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_base64.as_bytes())
        .map_err(|error| format!("录制分片解码失败：{error}"))?;
    let mut state = state.lock().await;
    let sink = state.recording.as_mut().ok_or("当前没有正在进行的录制。")?;
    sink.append(&bytes)?;
    Ok(sink.status())
}

#[tauri::command]
pub async fn finish_recording(
    state: tauri::State<'_, SharedState>,
) -> Result<RecordingResult, String> {
    let (sink, host_session) = {
        let mut state = state.lock().await;
        let sink = state.recording.take().ok_or("当前没有正在进行的录制。")?;
        state.recording_active.store(false, Ordering::Relaxed);
        (sink, state.host_session.clone())
    };
    let result = sink.finish(unix_timestamp_millis());
    notify_recording_state(host_session, false).await;
    result
}

#[tauri::command]
pub async fn abort_recording(state: tauri::State<'_, SharedState>) -> Result<(), String> {
    let (sink, host_session) = {
        let mut state = state.lock().await;
        let sink = state.recording.take();
        state.recording_active.store(false, Ordering::Relaxed);
        (sink, state.host_session.clone())
    };
    let result = if let Some(sink) = sink {
        sink.abort()
    } else {
        Ok(())
    };
    notify_recording_state(host_session, false).await;
    result
}

#[tauri::command]
pub async fn get_recording_status(
    state: tauri::State<'_, SharedState>,
) -> Result<Option<RecordingStatus>, String> {
    let state = state.lock().await;
    Ok(state.recording.as_ref().map(RecordingSink::status))
}

#[tauri::command]
pub fn reveal_recording(app: AppHandle, path: String) -> Result<(), String> {
    let recording_directory = app
        .path()
        .video_dir()
        .map_err(|error| format!("无法读取系统影片目录：{error}"))?
        .join("OrbShare");
    let recording_directory = std::fs::canonicalize(&recording_directory)
        .map_err(|error| format!("无法读取录制目录：{error}"))?;
    let recording_path = std::fs::canonicalize(PathBuf::from(path))
        .map_err(|error| format!("录制文件不存在：{error}"))?;
    let supported_extension = recording_path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "mp4" | "webm"))
        .unwrap_or(false);
    if !recording_path.is_file()
        || !recording_path.starts_with(&recording_directory)
        || !supported_extension
    {
        return Err("只能打开 OrbShare 录制目录中的视频文件。".to_string());
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(&recording_path)
            .spawn()
            .map_err(|error| format!("无法在 Finder 中显示录制文件：{error}"))?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer.exe")
            .arg(format!("/select,{}", recording_path.display()))
            .spawn()
            .map_err(|error| format!("无法在文件资源管理器中显示录制文件：{error}"))?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(recording_path.parent().unwrap_or(&recording_directory))
            .spawn()
            .map_err(|error| format!("无法打开录制目录：{error}"))?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("当前系统暂不支持打开录制目录。".to_string())
}

#[tauri::command]
pub async fn create_room(
    app: AppHandle,
    state: tauri::State<'_, SharedState>,
    config: RoomConfig,
) -> Result<RoomInfo, String> {
    let mut s = state.lock().await;

    // Never silently replace an active room. Navigating back to the home page
    // must not destroy the meeting that is still serving connected viewers.
    if s.signaling_server.is_some() || s.capture_provider.is_some() {
        tracing::warn!("[create_room] rejected because another room is still active");
        return Err("已有会议正在进行，请先返回会议或停止当前共享。".to_string());
    }

    s.browser_viewers.lock().await.clear();
    s.attendance.write().await.clear();
    *s.chat.write().await = ChatRoomState::default();
    s.blocked_participants.write().await.clear();
    s.recording_active.store(false, Ordering::Relaxed);

    // Generate room ID
    let room_id = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    // Set password in room manager (normalize: empty/whitespace = no password)
    let normalized_pw = normalize_password(&config.password);
    let share_system_audio = config.share_system_audio.unwrap_or(false);
    tracing::info!(
        "[create_room] password_len: {}, normalized_is_none: {}",
        config.password.len(),
        normalized_pw.is_none()
    );
    s.room_manager.set_password(&config.password);
    let password_required = s.room_manager.requires_password();
    tracing::info!("[create_room] password_required: {}", password_required);

    // Get local IP and hostname
    let host_ip = local_ip_address::local_ip()
        .map_err(|e| format!("Failed to get local IP: {}", e))?
        .to_string();
    let host_name = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let max_guests = config
        .max_guests
        .unwrap_or(EXTENDED_MAX_GUESTS)
        .clamp(1, EXTENDED_MAX_GUESTS);
    let room_admission = RoomAdmission::new(max_guests);
    s.room_admission = Some(room_admission.clone());

    let mut room_info = RoomInfo {
        room_id: room_id.clone(),
        room_name: config
            .room_name
            .unwrap_or_else(|| format!("{}'s Screen", host_name)),
        host_name,
        host_ip: host_ip.clone(),
        signaling_port: 38472,
        viewer_port: VIEWER_HTTP_PORT,
        version: "1.0.0".to_string(),
        max_guests,
        current_guests: 0,
        capabilities: vec![
            "screen-share".to_string(),
            "public-chat".to_string(),
            "chat-danmaku".to_string(),
            "moderation-remove".to_string(),
            "meeting-audio".to_string(),
            "moderation-mute".to_string(),
            "host-local-recording".to_string(),
        ],
        requires_password: password_required,
    };

    // Create and start signaling server
    let (mut server, mut event_rx) = SignalingServer::new(
        format!("0.0.0.0:38472"),
        room_id.clone(),
        s.room_manager.password_hash.clone().unwrap_or_default(),
        s.room_manager.password_salt.clone(),
        room_admission.clone(),
    );
    server.start().await.map_err(|e| {
        let msg = format!("{}", e);
        if msg.contains("AddrInUse") || msg.contains("端口") {
            tracing::error!("[create_room] Port already in use: {}", msg);
            msg
        } else {
            tracing::error!("[create_room] Failed to start signaling server: {}", msg);
            format!("Failed to start signaling server: {}", msg)
        }
    })?;

    // Parse video transport mode (needed before ViewerHttpServer and HostSession)
    let transport_mode = match config.video_transport.as_deref() {
        Some("webrtc-video-track") => VideoTransportMode::WebRtcVideoTrack,
        _ => VideoTransportMode::DebugJpegDataChannel,
    };
    let is_encoded_mode = transport_mode == VideoTransportMode::WebRtcVideoTrack;

    // Parse capture mode
    let capture_mode = match config.capture_mode.as_deref() {
        Some("screen-capture") => CaptureMode::ScreenCapture,
        _ => CaptureMode::TestPattern,
    };

    if share_system_audio
        && (capture_mode != CaptureMode::ScreenCapture
            || transport_mode != VideoTransportMode::WebRtcVideoTrack)
    {
        server.shutdown().await;
        return Err("共享系统声音仅支持“共享屏幕 + H.264 视频轨道”模式。".to_string());
    }
    if share_system_audio {
        room_info
            .capabilities
            .push("system-audio-active".to_string());
    }

    tracing::info!(
        "[create_room] capture_mode={:?}, video_transport={:?}, room_id={}",
        capture_mode,
        transport_mode,
        room_id
    );
    // The open-source edition exposes every quality profile. The room-wide
    // admission cap still protects the host from accidental overload.
    let quality = resolve_quality_profile(config.quality_tier.as_deref());
    tracing::info!(
        "[create_room] quality_profile={:?} resolved_h264_level={}",
        quality,
        quality.h264_profile_level_id()
    );

    // Create WebRTC host session early so ViewerHttpServer can use it
    // Profile/level must match the encoder so the browser SDP and the actual bitstream agree.
    let (mut host_session, host_event_rx) =
        HostSession::new(is_encoded_mode, quality.h264_profile_level_id());
    if share_system_audio {
        if let Err(error) = host_session.reserve_audio_source(SYSTEM_AUDIO_SOURCE_ID) {
            server.shutdown().await;
            return Err(format!("无法预留系统声音通道：{error}"));
        }
    }
    s.host_session = Some(Arc::new(RwLock::new(host_session)));

    let viewer_room_name = room_info.room_name.clone();
    match ViewerHttpServer::start(
        VIEWER_HTTP_PORT,
        s.host_session
            .as_ref()
            .ok_or("host_session not initialized")?
            .clone(),
        host_ip.clone(),
        viewer_room_name,
        share_system_audio,
        s.recording_active.clone(),
        s.room_manager.password_hash.clone().unwrap_or_default(),
        s.room_manager.password_salt.clone(),
        s.browser_viewers.clone(),
        s.room_manager.members.clone(),
        s.attendance.clone(),
        s.chat.clone(),
        s.blocked_participants.clone(),
        room_admission.clone(),
    )
    .await
    {
        Ok((viewer_server, viewer_port)) => {
            tracing::info!(
                "[create_room] Viewer HTTP server listening on 0.0.0.0:{}",
                viewer_port
            );
            room_info.viewer_port = viewer_port;
            s.viewer_http_server = Some(viewer_server);
        }
        Err(e) => {
            tracing::error!("[create_room] {}", e);
            server.shutdown().await;
            return Err(e);
        }
    }

    // Start capture based on mode
    tracing::info!(
        "[create_room] Creating capture provider for mode: {:?}",
        capture_mode
    );
    let mut capture = create_capture_provider_for_mode(capture_mode);
    tracing::info!("[create_room] Requesting permission...");
    match capture.request_permission().await {
        Ok(true) => tracing::info!("[create_room] Permission granted"),
        Ok(false) => tracing::warn!("[create_room] Permission returned false"),
        Err(e) => {
            tracing::error!("[create_room] Permission denied: {}", e);
            if let Some(ref viewer_server) = s.viewer_http_server {
                viewer_server.shutdown();
            }
            s.viewer_http_server = None;
            server.shutdown().await;
            return Err(format!("Capture permission denied: {}", e));
        }
    }
    let capture_config = match transport_mode {
        VideoTransportMode::DebugJpegDataChannel => orb_capture::CaptureConfig {
            source_id: config.source_id,
            source_is_display: config.source_is_display.unwrap_or(true),
            width: quality.width,
            height: quality.height,
            fps: quality.fps.min(10),
            capture_system_audio: false,
            video_transport: transport_mode,
            ..orb_capture::CaptureConfig::default()
        },
        VideoTransportMode::WebRtcVideoTrack => orb_capture::CaptureConfig {
            source_id: config.source_id,
            source_is_display: config.source_is_display.unwrap_or(true),
            width: quality.width,
            height: quality.height,
            fps: quality.fps,
            video_bitrate: quality.bitrate,
            keyframe_interval_seconds: quality.keyframe_interval_seconds,
            capture_system_audio: share_system_audio,
            video_transport: transport_mode,
            ..orb_capture::CaptureConfig::default()
        },
    };
    tracing::info!("[create_room] Starting capture...");
    let mut stream = match capture.start_capture(capture_config.clone()) {
        Ok(stream) => stream,
        Err(e) => {
            tracing::error!("[create_room] Failed to start capture: {}", e);
            if let Some(ref viewer_server) = s.viewer_http_server {
                viewer_server.shutdown();
            }
            s.viewer_http_server = None;
            server.shutdown().await;
            return Err(format!("Failed to start capture: {}", e));
        }
    };

    if is_encoded_mode && stream.encoded_video_rx.is_none() {
        tracing::error!("[create_room] capture backend does not provide H.264 encoded frames");
        let _ = capture.stop_capture();
        if let Some(ref viewer_server) = s.viewer_http_server {
            viewer_server.shutdown();
        }
        s.viewer_http_server = None;
        s.host_session = None;
        server.shutdown().await;
        return Err(
            "当前采集后端暂不支持 H.264 视频轨道，请切换到“兼容模式（MJPEG）”后重试。".to_string(),
        );
    }

    if share_system_audio && stream.audio_rx.is_none() {
        tracing::error!("[create_room] capture backend did not provide system audio PCM");
        let _ = capture.stop_capture();
        if let Some(ref viewer_server) = s.viewer_http_server {
            viewer_server.shutdown();
        }
        s.viewer_http_server = None;
        s.host_session = None;
        server.shutdown().await;
        return Err("当前采集后端没有提供系统声音，请关闭该选项后重试。".to_string());
    }

    // Publish room via mDNS only after capture is ready. If capture permission
    // fails, this avoids advertising a dead room and avoids mDNS startup churn.
    let mut publisher = match MdnsPublisher::new() {
        Ok(publisher) => publisher,
        Err(e) => {
            tracing::error!("[create_room] Failed to create mDNS publisher: {}", e);
            let _ = capture.stop_capture();
            if let Some(ref viewer_server) = s.viewer_http_server {
                viewer_server.shutdown();
            }
            s.viewer_http_server = None;
            server.shutdown().await;
            return Err(e);
        }
    };
    if let Err(e) = publisher.publish(&room_info) {
        tracing::error!("[create_room] Failed to publish mDNS room: {}", e);
        let _ = capture.stop_capture();
        if let Some(ref viewer_server) = s.viewer_http_server {
            viewer_server.shutdown();
        }
        s.viewer_http_server = None;
        server.shutdown().await;
        return Err(e);
    }
    s.mdns_publisher = Some(publisher);

    tracing::info!("[create_room] Capture started successfully");
    s.capture_provider = Some(capture);
    s.active_capture_config = Some(capture_config);
    s.active_capture_mode = Some(capture_mode);
    s.active_quality = Some(quality);

    // ScreenCaptureKit supplies 48 kHz interleaved PCM. Encode it with the
    // pure-Rust Opus codec bundled into OrbShare and relay it through the same
    // WebRTC audio tracks used by participant microphones.
    if let Some(mut audio_rx) = stream.audio_rx {
        let host_session = s
            .host_session
            .as_ref()
            .expect("host session exists while room is active")
            .clone();
        tokio::spawn(async move {
            let mut encoder = match SystemAudioOpusEncoder::new(2) {
                Ok(encoder) => encoder,
                Err(error) => {
                    tracing::error!("[orbshare:system-audio] {error}");
                    return;
                }
            };
            let mut sequence_number = 0u16;
            let mut timestamp = 0u32;
            let mut encode_errors = 0u64;
            let mut packet_count = 0u64;
            while let Some(frame) = audio_rx.recv().await {
                let packets =
                    match encoder.push_pcm(&frame.samples, frame.sample_rate, frame.channels) {
                        Ok(packets) => packets,
                        Err(error) => {
                            encode_errors += 1;
                            if encode_errors == 1 || encode_errors % 100 == 0 {
                                tracing::warn!(
                                "[orbshare:system-audio] encode failure #{encode_errors}: {error}"
                            );
                            }
                            continue;
                        }
                    };
                for payload in packets {
                    let relayed = host_session
                        .read()
                        .await
                        .relay_audio_payload(
                            SYSTEM_AUDIO_SOURCE_ID,
                            payload,
                            sequence_number,
                            timestamp,
                        )
                        .await;
                    sequence_number = sequence_number.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(OPUS_FRAME_SAMPLES_PER_CHANNEL as u32);
                    packet_count += 1;
                    if packet_count == 1 || packet_count % 500 == 0 {
                        tracing::debug!(
                            "[orbshare:system-audio] packet={packet_count} destinations={relayed}"
                        );
                    }
                }
            }
            tracing::info!("[orbshare:system-audio] capture stream ended");
        });
    }

    // Spawn task: pump captured video to WebRTC peers
    tracing::info!(
        "[create_room] Spawning video pump task, is_encoded_mode={}",
        is_encoded_mode
    );
    if let Some(preview_rx) = stream.preview_video_rx.take() {
        spawn_host_preview_pump(app.clone(), preview_rx);
    }
    if is_encoded_mode {
        // H.264 over WebRTC Video Track (production)
        tracing::info!("[create_room] Using H.264 encoded video path");
        let state_clone = (*state).clone();
        let app_clone_stats = app.clone();
        if let Some(mut encoded_rx) = stream.encoded_video_rx {
            tokio::spawn(async move {
                let mut frame_count: u64 = 0;
                let mut sent_count: u64 = 0;
                let mut sent_bytes_total: u64 = 0;
                let mut prev_captured_count: u64 = 0;
                let mut prev_sent_count: u64 = 0;
                let mut prev_sent_bytes: u64 = 0;
                // Encoded H.264 frames must remain ordered and gap-free. Raw
                // capture frames may be dropped before encoding, but dropping a
                // P-frame here breaks the decoder's reference chain.
                let encoded_drop_count: u64 = 0;
                let mut send_timeout_count: u64 = 0;
                let mut waiting_for_join_keyframe = true;
                let mut had_track = false;
                let mut last_h264_log_at = Instant::now();
                // Periodic INFO heartbeat for the encoded-frame pump. Critical for
                // diagnosing "host isn't sending frames" — without this, an empty
                // peer set makes the loop run silently forever.
                let mut last_status_log_at = Instant::now();
                let mut last_no_track_log_at = Instant::now();
                // A 720p keyframe is ~100-500KB and must be packetized into
                // hundreds of RTP packets inside write_sample; a per-frame budget
                // tied to the target fps (33-41ms) is far too tight and every
                // keyframe write times out, resetting the join-keyframe wait and
                // stalling the stream. Use a generous floor instead. The bounded
                // raw-frame channel keeps latency bounded before H.264 encoding.
                let send_budget =
                    Duration::from_millis(500.max(1_000u64 / quality.fps.max(1) as u64));
                while let Some(frame) = encoded_rx.recv().await {
                    let track = {
                        let st = state_clone.lock().await;
                        st.host_session.clone()
                    };
                    let host = match track {
                        Some(h) => h,
                        None => {
                            // Should be impossible: create_room always sets host_session
                            // in encoded mode. Log loudly so we notice if state plumbing
                            // ever regresses.
                            tracing::error!(
                            "[orbshare][h264-pump] host_session is None in encoded-mode pump - \
                            captured={frame_count} (state plumbing regression)"
                        );
                            frame_count += 1;
                            continue;
                        }
                    };
                    {
                        let peer_count = host.read().await.peers.len();
                        let track = { host.read().await.video_track_for_send() };
                        let Some(track) = track else {
                            // No track available. Two possible causes:
                            //   (a) video_track is None (HostSession was created
                            //       with enable_video_track=false — shouldn't
                            //       happen in is_encoded_mode branch).
                            //   (b) peers HashMap is empty — guest hasn't joined
                            //       yet, OR add_peer() failed silently.
                            // Log every 2s so we can tell which case we're in.
                            if last_no_track_log_at.elapsed() >= Duration::from_secs(2) {
                                let has_video_track = host.read().await.video_track.is_some();
                                tracing::warn!(
                                "[orbshare][h264-pump] no track: peer_count={peer_count} host_video_track_some={has_video_track} captured={frame_count} encoded_dropped={encoded_drop_count} (waiting for guest to join)",
                            );
                                last_no_track_log_at = Instant::now();
                            }
                            had_track = false;
                            waiting_for_join_keyframe = true;
                            frame_count += 1;
                            continue;
                        };
                        if !had_track {
                            tracing::info!(
                            "[orbshare][h264-pump] got video track, peer_count={peer_count}, waiting for first keyframe"
                        );
                            waiting_for_join_keyframe = true;
                            had_track = true;
                        }
                        let bytes = frame.data.len();
                        let is_keyframe = frame.is_keyframe;
                        if waiting_for_join_keyframe && !is_keyframe {
                            frame_count += 1;
                            // Heartbeat while skipping frames, otherwise the pump
                            // looks dead during the keyframe wait and we can't
                            // tell "no frames coming" from "frames but no keyframe".
                            if last_no_track_log_at.elapsed() >= Duration::from_secs(2) {
                                tracing::info!(
                                    "[orbshare][h264-pump] waiting for join keyframe: \
                                captured={frame_count} sent={sent_count} \
                                encoded_dropped={encoded_drop_count} peer_count={peer_count}"
                                );
                                last_no_track_log_at = Instant::now();
                            }
                            continue;
                        }
                        // Only announce the join-keyframe on the actual waiting →
                        // streaming transition, not on every frame.
                        if waiting_for_join_keyframe {
                            waiting_for_join_keyframe = false;
                            tracing::info!(
                            "[orbshare][h264-pump] got join keyframe, sending first frame! \
                            captured={frame_count} encoded_dropped={encoded_drop_count} peer_count={peer_count}"
                        );
                        }
                        let nal = if last_h264_log_at.elapsed() >= Duration::from_secs(2) {
                            Some(h264_nal_summary(&frame.data))
                        } else {
                            None
                        };
                        match tokio::time::timeout(
                            send_budget,
                            MediaRouter::write_h264_sample(track, frame.data, quality.fps),
                        )
                        .await
                        {
                            Ok(Ok(())) => {
                                sent_count += 1;
                                sent_bytes_total = sent_bytes_total.saturating_add(bytes as u64);
                                if let Some(nal) = nal {
                                    tracing::debug!(
                                    "[orbshare] h264 sent={sent_count} captured={frame_count} encoded_dropped={encoded_drop_count} send_timeouts={send_timeout_count} size={bytes} keyframe={is_keyframe} {nal} {}x{} fps={}",
                                    frame.width,
                                    frame.height,
                                    quality.fps
                                );
                                    last_h264_log_at = Instant::now();
                                }
                            }
                            Ok(Err(e)) => {
                                tracing::error!(
                                    "[orbshare][h264-pump] write_sample FAILED: {e} \
                                captured={frame_count} sent={sent_count} \
                                peer_count={peer_count} \
                                size={bytes} keyframe={is_keyframe} {}x{}",
                                    frame.width,
                                    frame.height
                                );
                            }
                            Err(_) => {
                                send_timeout_count += 1;
                                waiting_for_join_keyframe = true;
                                if last_h264_log_at.elapsed() >= Duration::from_secs(2) {
                                    tracing::debug!(
                                    "[orbshare] h264 send timeout captured={frame_count} encoded_dropped={encoded_drop_count} send_timeouts={send_timeout_count} budget={}ms size={bytes}",
                                    send_budget.as_millis()
                                );
                                    last_h264_log_at = Instant::now();
                                }
                            }
                        }
                    }
                    frame_count += 1;
                    if frame_count % 30 == 0 {
                        let stats = CaptureStats {
                            width: frame.width,
                            height: frame.height,
                            fps: quality.fps,
                            pixel_format: format!("BGRA→YUV420→H264 ({})", quality.name),
                            frame_count,
                        };
                        let _ = app_clone_stats.emit("capture-stats", &stats);
                    }
                    // Periodic pump heartbeat. Once a guest is connected this is
                    // the strongest "frames are flowing into the WebRTC track"
                    // signal; if it stops appearing, capture is the bottleneck.
                    if last_status_log_at.elapsed() >= Duration::from_secs(5) {
                        let peer_count = host.read().await.peers.len();
                        let elapsed_s = last_status_log_at.elapsed().as_secs_f64();
                        let captured_delta = frame_count.saturating_sub(prev_captured_count);
                        let sent_delta = sent_count.saturating_sub(prev_sent_count);
                        let sent_bytes_delta = sent_bytes_total.saturating_sub(prev_sent_bytes);
                        let captured_fps = captured_delta as f64 / elapsed_s;
                        let sent_fps = sent_delta as f64 / elapsed_s;
                        let actual_bitrate_bps =
                            ((sent_bytes_delta as f64 * 8.0) / elapsed_s).round() as u64;
                        prev_captured_count = frame_count;
                        prev_sent_count = sent_count;
                        prev_sent_bytes = sent_bytes_total;
                        let _ = app_clone_stats.emit(
                            "stream-stats",
                            &StreamStats {
                                width: frame.width,
                                height: frame.height,
                                actual_fps: sent_fps,
                                actual_bitrate_bps,
                                target_fps: quality.fps,
                                target_bitrate_bps: quality.bitrate,
                                peer_count,
                            },
                        );
                        tracing::info!(
                        "[orbshare][h264-pump] status: captured={frame_count}(+{captured_delta}, {captured_fps:.1}fps) \
                        sent={sent_count}(+{sent_delta}, {sent_fps:.1}fps) \
                        actual_bitrate={actual_bitrate_bps}bps target_bitrate={}bps \
                        encoded_dropped={encoded_drop_count} send_timeouts={send_timeout_count} \
                        peer_count={peer_count} {}x{}",
                        quality.bitrate,
                        frame.width,
                        frame.height
                    );
                        last_status_log_at = Instant::now();
                    }
                }
                tracing::warn!(
                "[orbshare][h264-pump] encoded_rx channel closed; total captured={frame_count} sent={sent_count} encoded_dropped={encoded_drop_count}"
            );
            });
        }
    } else {
        // JPEG over DataChannel
        tracing::info!("[create_room] Using JPEG DataChannel path");
        let state_clone = (*state).clone();
        let app_clone_stats = app.clone();
        let mut video_rx = stream.video_rx;
        tokio::spawn(async move {
            let mut frame_count: u64 = 0;
            let mut last_preview_at = Instant::now() - HOST_PREVIEW_INTERVAL;
            while let Some(mut frame) = video_rx.recv().await {
                while let Ok(next_frame) = video_rx.try_recv() {
                    frame = next_frame;
                }

                let host = {
                    let st = state_clone.lock().await;
                    st.host_session.clone()
                };
                let peer_count = if let Some(host) = &host {
                    host.read().await.ready_video_peer_count().await
                } else {
                    0
                };

                if peer_count > 0 {
                    if let Some(jpeg) = encode_remote_jpeg(&frame, quality.name) {
                        if frame_count % 30 == 0 {
                            tracing::debug!(
                                "[orbshare] frame {frame_count}: {peer_count} peers, jpeg={}bytes",
                                jpeg.len()
                            );
                        }
                        if let Some(host) = host {
                            let host = host.read().await;
                            match MediaRouter::send_mjpeg_frame(&host, jpeg).await {
                                Ok(send_stats) => {
                                    if frame_count % 30 == 0 {
                                        tracing::debug!(
                                            "[orbshare] send stats frame {frame_count}: peers={} sent={} skipped={} dropped={} chunks={} bytes={}",
                                            send_stats.peer_count,
                                            send_stats.sent_peer_count,
                                            send_stats.skipped_peer_count,
                                            send_stats.dropped_peer_count,
                                            send_stats.chunks,
                                            send_stats.bytes
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to send video frame: {}", e);
                                }
                            }
                        }
                    }
                }

                // Host local preview is throttled separately so the idle host
                // does not keep JPEG-encoding at capture FPS when nobody joins.
                if last_preview_at.elapsed() >= HOST_PREVIEW_INTERVAL {
                    if let Some(hd_jpeg) = SoftwareJpegEncoder::encode_frame(
                        &frame,
                        JpegEncodeOptions {
                            quality: 76,
                            target_width: Some(1280),
                            target_height: Some(720),
                        },
                    ) {
                        let data_url = format!(
                            "data:image/jpeg;base64,{}",
                            base64::engine::general_purpose::STANDARD.encode(&hd_jpeg)
                        );
                        let payload = serde_json::json!({ "dataUrl": data_url });
                        let _ = app_clone_stats.emit("host-video-frame", &payload);
                    }
                    last_preview_at = Instant::now();
                }
                frame_count += 1;
                if frame_count % 30 == 0 {
                    let stats = CaptureStats {
                        width: frame.width,
                        height: frame.height,
                        fps: quality.fps,
                        pixel_format: format!("BGRA→YUV420→MJPEG ({})", quality.name),
                        frame_count,
                    };
                    let _ = app_clone_stats.emit("capture-stats", &stats);
                }
            }
        });
    }

    // Spawn task: relay host WebRTC events to browser and signaling server
    {
        let state_clone = (*state).clone();
        let app_clone = app.clone();
        let room_id_clone = room_id.clone();
        let browser_viewers_clone = s.browser_viewers.clone();
        let attendance_clone = s.attendance.clone();
        let chat_clone = s.chat.clone();
        let room_admission_clone = room_admission.clone();
        tokio::spawn(async move {
            let mut rx = host_event_rx;
            while let Some(event) = rx.recv().await {
                match event.kind {
                    WebRtcEventKind::IceCandidate {
                        candidate,
                        sdp_mid,
                        sdp_mline_index,
                    } => {
                        if event.guest_id == HOST_AUDIO_MONITOR_ID {
                            // The in-process monitor gathers ICE into its SDP;
                            // it intentionally has no trickle signaling route.
                            continue;
                        }
                        // Browser viewer? Push to its drain buffer so the
                        // next /poll-ice returns it. WebSocket signaling guest?
                        // Forward via signaling server as before.
                        let mut pushed_to_browser = false;
                        {
                            let mut map = browser_viewers_clone.lock().await;
                            if let Some(state) = map.get_mut(&event.guest_id) {
                                if let Some(mid) = sdp_mid.as_ref() {
                                    state.host_candidates.push(HostIceCandidate {
                                        candidate: candidate.clone(),
                                        sdp_mid: mid.clone(),
                                        sdp_mline_index: sdp_mline_index.unwrap_or(0),
                                    });
                                    pushed_to_browser = true;
                                }
                            }
                        }
                        if !pushed_to_browser {
                            let msg = SignalingMessage {
                                msg_type: MsgType::IceCandidate,
                                room_id: room_id_clone.clone(),
                                sender_id: "host".to_string(),
                                payload: serde_json::json!({
                                    "candidate": candidate,
                                    "sdp_mid": sdp_mid,
                                    "sdp_mline_index": sdp_mline_index,
                                }),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs(),
                            };
                            let st = state_clone.lock().await;
                            if let Some(ref server) = st.signaling_server {
                                let _ = server.send_to_guest(&event.guest_id, msg).await;
                            }
                        }
                    }
                    WebRtcEventKind::Connected => {
                        if event.guest_id != HOST_AUDIO_MONITOR_ID {
                            let _ = app_clone.emit("peer-connected", &event.guest_id);
                            let (host, recording_active) = {
                                let state = state_clone.lock().await;
                                (
                                    state.host_session.clone(),
                                    state.recording_active.load(Ordering::Relaxed),
                                )
                            };
                            if recording_active {
                                let guest_id = event.guest_id.clone();
                                tokio::spawn(async move {
                                    let Some(host) = host else {
                                        return;
                                    };
                                    for _ in 0..5 {
                                        tokio::time::sleep(Duration::from_millis(120)).await;
                                        if host
                                            .read()
                                            .await
                                            .send_control_message(
                                                &guest_id,
                                                RECORDING_STARTED_CONTROL_MESSAGE,
                                            )
                                            .await
                                            .is_ok()
                                        {
                                            break;
                                        }
                                    }
                                });
                            }
                        }
                    }
                    WebRtcEventKind::ConnectionInterrupted => {
                        // A backgrounded iPhone commonly enters the recoverable
                        // WebRTC `Disconnected` state. Keep both the peer and
                        // participant entry so it can resume in place.
                        tracing::info!(
                            "[viewer] peer {} temporarily interrupted; waiting for recovery",
                            event.guest_id
                        );
                    }
                    WebRtcEventKind::Disconnected => {
                        if event.guest_id == HOST_AUDIO_MONITOR_ID {
                            let host = state_clone.lock().await.host_session.clone();
                            if let Some(host) = host {
                                let _ = host.write().await.remove_peer(HOST_AUDIO_MONITOR_ID).await;
                            }
                            continue;
                        }
                        let _ = app_clone.emit("peer-disconnected", &event.guest_id);
                        let participant_id = browser_viewers_clone
                            .lock()
                            .await
                            .remove(&event.guest_id)
                            .map(|state| state.participant_id);
                        let browser_viewer_disconnected = participant_id.is_some();
                        if let Some(participant_id) = participant_id {
                            record_attendance_leave(
                                &mut *attendance_clone.write().await,
                                &participant_id,
                                unix_timestamp(),
                            );
                        }
                        let (host, members) = {
                            let st = state_clone.lock().await;
                            (st.host_session.clone(), st.room_manager.members.clone())
                        };
                        let removed_member = members.write().await.remove(&event.guest_id);
                        if let Some(member) = removed_member {
                            record_attendance_leave(
                                &mut *attendance_clone.write().await,
                                &member.participant_id,
                                unix_timestamp(),
                            );
                        }
                        if let Some(host) = host {
                            let _ = host.write().await.remove_peer(&event.guest_id).await;
                        }
                        // Native-client admission is owned by its WebSocket and
                        // released by SignalingServer. Browser peers have no
                        // WebSocket, so their terminal WebRTC state owns release.
                        if browser_viewer_disconnected {
                            room_admission_clone.release(&event.guest_id);
                        }
                    }
                    WebRtcEventKind::PointerData(data) => {
                        if event.guest_id == HOST_AUDIO_MONITOR_ID {
                            continue;
                        }
                        let Some((x, y)) = decode_laser_pointer(&data) else {
                            tracing::debug!(
                                "[laser] dropped malformed pointer payload from {}",
                                event.guest_id
                            );
                            continue;
                        };
                        let host = state_clone.lock().await.host_session.clone();
                        if let Some(host) = host {
                            host.read()
                                .await
                                .broadcast_pointer_data(&data, Some(&event.guest_id))
                                .await;
                        }
                        let _ = app_clone.emit(
                            "laser-pointer",
                            LaserPointerEvent {
                                sender_id: event.guest_id,
                                x,
                                y,
                            },
                        );
                    }
                    WebRtcEventKind::ControlData(data) => {
                        let parsed: serde_json::Value = match serde_json::from_slice(&data) {
                            Ok(value) => value,
                            Err(_) => continue,
                        };
                        let message_type = parsed
                            .get("type")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default();
                        let (host, members) = {
                            let state = state_clone.lock().await;
                            (
                                state.host_session.clone(),
                                state.room_manager.members.clone(),
                            )
                        };

                        if message_type == "audio_state" {
                            let requested_enabled = parsed
                                .get("enabled")
                                .and_then(|value| value.as_bool())
                                .unwrap_or(false);
                            let updated = {
                                let mut members = members.write().await;
                                members.get_mut(&event.guest_id).map(|member| {
                                    member.microphone_enabled =
                                        requested_enabled && !member.host_muted;
                                    member.clone()
                                })
                            };
                            if let Some(member) = updated {
                                if member.host_muted {
                                    if let Some(ref host) = host {
                                        let payload = serde_json::json!({
                                            "type": "set_muted",
                                            "muted": true,
                                        })
                                        .to_string();
                                        let _ = host
                                            .read()
                                            .await
                                            .send_control_message(&event.guest_id, &payload)
                                            .await;
                                    }
                                }
                                let _ = app_clone.emit("member-updated", &member);
                            }
                            continue;
                        }
                        if message_type == "raise_hand" {
                            let requested_raised = parsed
                                .get("raised")
                                .and_then(|value| value.as_bool())
                                .unwrap_or(false);
                            let updated = {
                                let mut members = members.write().await;
                                members.get_mut(&event.guest_id).map(|member| {
                                    let changed = update_member_hand_state(
                                        member,
                                        requested_raised,
                                        unix_timestamp_millis(),
                                    );
                                    (member.clone(), changed)
                                })
                            };
                            let Some((member, changed)) = updated else {
                                continue;
                            };
                            if let Some(ref host) = host {
                                let payload = serde_json::json!({
                                    "type": "hand_state",
                                    "raised": member.hand_raised,
                                    "raised_at": member.hand_raised_at,
                                })
                                .to_string();
                                let _ = host
                                    .read()
                                    .await
                                    .send_control_message(&event.guest_id, &payload)
                                    .await;
                            }
                            if changed {
                                let _ = app_clone.emit("member-updated", &member);
                            }
                            continue;
                        }
                        if message_type != "chat_send" {
                            continue;
                        }
                        let content = parsed
                            .get("content")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default();
                        let member = members.read().await.get(&event.guest_id).cloned();
                        let Some(member) = member else {
                            continue;
                        };
                        let append_result = {
                            let mut chat = chat_clone.write().await;
                            append_chat_message(
                                &mut chat,
                                &member.participant_id,
                                &member.name,
                                MemberRole::Guest,
                                content,
                                unix_timestamp_millis(),
                            )
                        };
                        match append_result {
                            Ok(message) => {
                                let payload = chat_control_payload(&message);
                                if let Some(host) = host {
                                    host.read().await.broadcast_control_message(&payload).await;
                                }
                                let _ = app_clone.emit("chat-message", &message);
                            }
                            Err(error) => {
                                if let Some(host) = host {
                                    let payload = serde_json::json!({
                                        "type": "chat_error",
                                        "message": error,
                                    })
                                    .to_string();
                                    let _ = host
                                        .read()
                                        .await
                                        .send_control_message(&event.guest_id, &payload)
                                        .await;
                                }
                            }
                        }
                    }
                    WebRtcEventKind::AudioRtp(packet) => {
                        let (host, members) = {
                            let state = state_clone.lock().await;
                            (
                                state.host_session.clone(),
                                state.room_manager.members.clone(),
                            )
                        };
                        let is_host_monitor = event.guest_id == HOST_AUDIO_MONITOR_ID;
                        let member = if is_host_monitor {
                            None
                        } else {
                            members.read().await.get(&event.guest_id).cloned()
                        };
                        let should_relay =
                            participant_audio_may_relay(member.as_ref(), is_host_monitor);
                        if should_relay {
                            if let Some(host) = host {
                                host.read()
                                    .await
                                    .relay_audio_rtp(&event.guest_id, &packet)
                                    .await;
                            }
                        }
                    }
                }
            }
        });
    }

    // Spawn task: handle signaling events (guest joins, SDP, ICE)
    {
        let state_clone = (*state).clone();
        tokio::spawn(async move {
            while let Ok(event) = event_rx.recv().await {
                match event {
                    SignalingEvent::GuestJoined {
                        guest_id,
                        guest_name,
                    } => {
                        tracing::debug!("[orbshare] guest joined: {guest_id}");
                        let (host, members, attendance) = {
                            let st = state_clone.lock().await;
                            (
                                st.host_session.clone(),
                                st.room_manager.members.clone(),
                                st.attendance.clone(),
                            )
                        };
                        let joined_at = unix_timestamp();
                        let name = viewer_name(&guest_name);
                        let platform = "OrbShare 客户端".to_string();
                        members.write().await.insert(
                            guest_id.clone(),
                            MemberInfo {
                                member_id: guest_id.clone(),
                                participant_id: guest_id.clone(),
                                name: name.clone(),
                                role: MemberRole::Guest,
                                platform: platform.clone(),
                                connected_at: joined_at,
                                microphone_enabled: false,
                                host_muted: false,
                                hand_raised: false,
                                hand_raised_at: None,
                            },
                        );
                        record_attendance_join(
                            &mut *attendance.write().await,
                            &guest_id,
                            &name,
                            &platform,
                            joined_at,
                        );
                        if let Some(host) = host {
                            if let Err(e) =
                                host.write().await.add_peer(guest_id.clone(), vec![]).await
                            {
                                members.write().await.remove(&guest_id);
                                record_attendance_leave(
                                    &mut *attendance.write().await,
                                    &guest_id,
                                    unix_timestamp(),
                                );
                                tracing::debug!("[orbshare] failed to add peer {guest_id}: {e}");
                                tracing::error!("Failed to add peer {}: {}", guest_id, e);
                            }
                        }
                    }
                    SignalingEvent::GuestLeft { guest_id } => {
                        let (host, members, attendance) = {
                            let st = state_clone.lock().await;
                            (
                                st.host_session.clone(),
                                st.room_manager.members.clone(),
                                st.attendance.clone(),
                            )
                        };
                        let participant_id = members
                            .write()
                            .await
                            .remove(&guest_id)
                            .map(|member| member.participant_id)
                            .unwrap_or_else(|| guest_id.clone());
                        record_attendance_leave(
                            &mut *attendance.write().await,
                            &participant_id,
                            unix_timestamp(),
                        );
                        if let Some(host) = host {
                            let _ = host.write().await.remove_peer(&guest_id).await;
                        }
                    }
                    SignalingEvent::SdpOffer { guest_id, sdp } => {
                        tracing::debug!("[orbshare] sdp offer received from {guest_id}");
                        // Dump the raw offer so we can see what m= sections the
                        // browser actually proposed (in particular, whether m=video
                        // section exists and what codecs are listed).
                        if let Ok(offer) = serde_json::from_str::<serde_json::Value>(&sdp) {
                            if let Some(offer_str) = offer.get("sdp").and_then(|v| v.as_str()) {
                                let m_lines: Vec<&str> =
                                    offer_str.lines().filter(|l| l.starts_with("m=")).collect();
                                tracing::warn!(
                                    "[orbshare:offer] guest={guest_id} m_lines={:?}",
                                    m_lines
                                );
                            }
                        }
                        let host = {
                            let st = state_clone.lock().await;
                            st.host_session.clone()
                        };
                        let h264_level = if let Some(h) = &host {
                            h.read().await.h264_profile_level_id.clone()
                        } else {
                            "42e01f".to_string()
                        };
                        let pc = if let Some(host) = host {
                            host.read().await.peer_connection(&guest_id)
                        } else {
                            None
                        };
                        let answer = if let Some(pc) = pc {
                            match orb_webrtc::session::HostSession::create_answer_for_peer_connection(pc, sdp, &h264_level).await {
                                Ok(answer) => Some(answer),
                                Err(e) => {
                                    tracing::error!(
                                        "[orbshare] failed to create sdp answer for {guest_id}: {e}"
                                    );
                                    None
                                }
                            }
                        } else {
                            tracing::debug!(
                                "[orbshare] no host session for sdp offer from {guest_id}"
                            );
                            None
                        };
                        if let Some(answer_sdp) = answer {
                            let st = state_clone.lock().await;
                            if let Some(ref server) = st.signaling_server {
                                tracing::info!(
                                    "[orbshare] SdpAnswer for {guest_id}: host_intended_h264_level={h264_level}, answer_level(s)={:?}",
                                    orb_webrtc::session::collect_profile_levels(&answer_sdp)
                                );
                                // Dump the answer's m= lines so we can see if m=video
                                // survived negotiation (and which direction it's in).
                                if let Ok(answer_val) =
                                    serde_json::from_str::<serde_json::Value>(&answer_sdp)
                                {
                                    if let Some(answer_str) =
                                        answer_val.get("sdp").and_then(|v| v.as_str())
                                    {
                                        let answer_lines: Vec<&str> = answer_str
                                            .lines()
                                            .filter(|l| {
                                                l.starts_with("m=")
                                                    || l.starts_with("a=rtpmap")
                                                    || l.starts_with("a=fmtp")
                                            })
                                            .take(40)
                                            .collect();
                                        let video_m_present = answer_str.contains("m=video");
                                        tracing::warn!(
                                            "[orbshare:answer] guest={guest_id} m_video_present={video_m_present} first_40_lines={:?}",
                                            answer_lines
                                        );
                                    }
                                }
                                let msg = SignalingMessage {
                                    msg_type: MsgType::SdpAnswer,
                                    room_id: room_id.clone(),
                                    sender_id: "host".to_string(),
                                    payload: serde_json::json!({ "sdp": answer_sdp, "h264_level": h264_level }),
                                    timestamp: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap()
                                        .as_secs(),
                                };
                                tracing::debug!("[orbshare] sending sdp answer to {guest_id}");
                                let _ = server.send_to_guest(&guest_id, msg).await;
                            }
                        }
                    }
                    SignalingEvent::IceCandidate {
                        guest_id,
                        candidate,
                        sdp_mid,
                        sdp_mline_index,
                    } => {
                        let host = {
                            let st = state_clone.lock().await;
                            st.host_session.clone()
                        };
                        let pc = if let Some(host) = host {
                            host.read().await.peer_connection(&guest_id)
                        } else {
                            None
                        };
                        if let Some(pc) = pc {
                            let _ =
                                orb_webrtc::session::HostSession::add_ice_candidate_to_peer_connection(
                                    pc,
                                    candidate,
                                    sdp_mid,
                                    sdp_mline_index.map(|i| i as u16),
                                )
                                .await;
                        }
                    }
                }
            }
        });
    }

    // Drop the lock before storing signaling server
    drop(s);

    let mut st = state.lock().await;
    st.signaling_server = Some(server);
    st.active_room_password = normalized_pw;
    st.room_manager.room = Some(room_info.clone());
    Ok(room_info)
}

fn finish_recording_during_shutdown(state: &mut AppState) {
    state.recording_active.store(false, Ordering::Relaxed);
    let Some(sink) = state.recording.take() else {
        return;
    };
    match sink.finish(unix_timestamp_millis()) {
        Ok(result) => tracing::info!(
            "[recording] finalized during room shutdown: {} ({} bytes)",
            result.path,
            result.size_bytes
        ),
        Err(error) => tracing::error!("[recording] shutdown finalization failed: {error}"),
    }
}

#[tauri::command]
pub async fn stop_room(state: tauri::State<'_, SharedState>) -> Result<(), String> {
    tracing::info!("[stop_room] Stopping room...");
    let mut s = state.lock().await;
    finish_recording_during_shutdown(&mut s);
    close_all_attendance_sessions(&mut *s.attendance.write().await, unix_timestamp());

    // Notify browser viewers through both WebRTC and a pending HTTP event
    // request before either transport is shut down. Native guests receive the
    // equivalent RoomClosed signaling message below.
    if let Some(ref viewer_server) = s.viewer_http_server {
        viewer_server.notify_room_closed();
    }
    if let Some(ref host_session) = s.host_session {
        let notified = host_session
            .read()
            .await
            .broadcast_control_message(HOST_STOP_CONTROL_MESSAGE)
            .await;
        tracing::info!("[stop_room] Notified {notified} viewer control channels");
    }

    // Shutdown signaling server with timeout to avoid blocking UI
    if let Some(ref mut server) = s.signaling_server {
        tracing::info!("[stop_room] Shutting down signaling server");
        let _ = server.broadcast(room_closed_message()).await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server.shutdown()).await;
    }
    if let Some(ref viewer_server) = s.viewer_http_server {
        tracing::info!("[stop_room] Shutting down viewer HTTP server");
        viewer_server.shutdown();
    }

    // Stop capture
    if let Some(ref mut provider) = s.capture_provider {
        tracing::info!("[stop_room] Stopping capture");
        let _ = provider.stop_capture();
    }

    // Unpublish mDNS
    if let Some(ref publisher) = s.mdns_publisher {
        tracing::info!("[stop_room] Unpublishing mDNS");
        let _ = publisher.unpublish();
    }

    s.capture_provider = None;
    s.active_capture_config = None;
    s.active_capture_mode = None;
    s.active_quality = None;
    s.host_session = None;
    s.signaling_server = None;
    s.viewer_http_server = None;
    s.mdns_publisher = None;
    s.active_room_password = None;
    if let Some(admission) = s.room_admission.take() {
        admission.clear();
    }
    s.room_manager = RoomManager::new();

    // Clean up guest state as well
    s.guest_signaling = None;
    s.guest_id = None;
    s.guest_room_id = None;

    tracing::info!("[stop_room] Room stopped successfully");
    Ok(())
}

#[tauri::command]
pub async fn scan_rooms(
    app: AppHandle,
    state: tauri::State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;

    let scanner = MdnsScanner::new()?;
    let mut rx = scanner.start_scan()?;

    let app_clone = app.clone();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                ScannerEvent::RoomDiscovered(room) => {
                    let _ = app_clone.emit("room-discovered", &room);
                }
                ScannerEvent::RoomRemoved(name) => {
                    let room_id = name.split('.').next().unwrap_or(&name);
                    let _ =
                        app_clone.emit("room-removed", serde_json::json!({ "room_id": room_id }));
                }
            }
        }
    });

    s.mdns_scanner = Some(scanner);
    Ok(())
}

#[tauri::command]
pub async fn join_room(
    app: AppHandle,
    state: tauri::State<'_, SharedState>,
    address: String,
    port: u16,
    password: String,
) -> Result<String, String> {
    {
        let s = state.lock().await;
        if s.guest_signaling.is_some() {
            return Err("Already joined a room. Leave first.".to_string());
        }
    }

    // Normalize and hash password
    let normalized = normalize_password(&password);
    tracing::info!(
        "[join_room] password_len: {}, normalized_is_none: {}, address: {}:{}",
        password.len(),
        normalized.is_none(),
        address,
        port
    );
    let password_hash = match normalized {
        Some(ref pw) => hash_password_for_transport(pw),
        None => String::new(),
    };
    tracing::info!(
        "[join_room] provided_password: {}, password_hash_empty: {}",
        normalized.is_some(),
        password_hash.is_empty()
    );

    // Connect to signaling server
    let ws_url = format!("ws://{}:{}", address, port);
    let mut client = SignalingClient::connect(&ws_url)
        .await
        .map_err(|e| format!("Failed to connect: {}", e))?;

    // Send join request
    client
        .join("room", "Guest", &password_hash)
        .map_err(|e| format!("Failed to send join request: {}", e))?;

    // Extract sender handle before moving client into spawned task
    let guest_sender = client.sender();

    // Use oneshot to wait for join result from spawned task
    let (join_tx, join_rx) = tokio::sync::oneshot::channel::<Result<String, String>>();

    let state_clone = (*state).clone();
    let app_clone = app.clone();
    tokio::spawn(async move {
        // Wait for JoinResult with timeout
        let timeout = std::time::Duration::from_secs(10);
        let join_result = tokio::time::timeout(timeout, client.recv_event()).await;

        match join_result {
            Ok(Some(orb_signaling::client::SignalingEvent::JoinResult {
                success: true,
                guest_id: Some(gid),
                ..
            })) => {
                {
                    let mut st = state_clone.lock().await;
                    st.guest_signaling = Some(guest_sender);
                    st.guest_id = Some(gid.clone());
                    st.guest_room_id = Some("room".to_string());
                }

                tracing::debug!("[orbshare] join accepted as {gid}");
                let _ = app_clone.emit("signaling://joined", &gid);
                let _ = join_tx.send(Ok(gid.clone()));

                // Continue processing signaling events
                while let Some(event) = client.recv_event().await {
                    match event {
                        orb_signaling::client::SignalingEvent::SdpAnswer { sdp } => {
                            tracing::debug!("[orbshare] guest received sdp answer");
                            let payload = serde_json::json!({ "sdp": sdp });
                            let _ = app_clone.emit("signaling://sdp-answer", &payload);
                        }
                        orb_signaling::client::SignalingEvent::IceCandidate {
                            candidate,
                            sdp_mid,
                            sdp_mline_index,
                        } => {
                            let payload = serde_json::json!({
                                "candidate": candidate,
                                "sdpMid": sdp_mid,
                                "sdpMLineIndex": sdp_mline_index,
                            });
                            let _ = app_clone.emit("signaling://ice-candidate", &payload);
                        }
                        orb_signaling::client::SignalingEvent::RoomClosed => {
                            let _ = app_clone.emit("signaling://room-closed", ());
                            break;
                        }
                        orb_signaling::client::SignalingEvent::VideoFrame { data_url } => {
                            let payload = serde_json::json!({ "dataUrl": data_url });
                            let _ = app_clone.emit("video-frame", &payload);
                        }
                        _ => {}
                    }
                }
            }
            Ok(Some(orb_signaling::client::SignalingEvent::JoinResult {
                success: false,
                error,
                ..
            })) => {
                let msg = error.unwrap_or_else(|| "Join rejected".to_string());
                let _ = app_clone.emit("signaling://error", &msg);
                let _ = join_tx.send(Err(msg));
            }
            Ok(_) => {
                let _ = app_clone.emit("signaling://error", "Unexpected response");
                let _ = join_tx.send(Err("Unexpected response".to_string()));
            }
            Err(_) => {
                let _ = app_clone.emit("signaling://error", "Join timed out");
                // join_tx dropped without sending — receiver gets Err
            }
        }

        // Connection ended: clean up guest state
        let mut st = state_clone.lock().await;
        st.guest_signaling = None;
        st.guest_id = None;
        st.guest_room_id = None;
    });

    // Wait for join result
    match join_rx.await {
        Ok(Ok(guest_id)) => Ok(guest_id),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("Join timed out".to_string()),
    }
}

#[tauri::command]
pub async fn leave_room(state: tauri::State<'_, SharedState>) -> Result<(), String> {
    let mut s = state.lock().await;

    let leave_result = match (
        s.guest_signaling.as_ref(),
        s.guest_room_id.as_deref(),
        s.guest_id.as_deref(),
    ) {
        (Some(sender), Some(room_id), Some(guest_id)) => sender
            .leave(room_id, guest_id)
            .map_err(|e| format!("Failed to leave room: {e}")),
        _ => Ok(()),
    };

    s.guest_signaling = None;
    s.guest_id = None;
    s.guest_room_id = None;

    leave_result
}

#[tauri::command]
pub async fn guest_send_sdp_offer(
    state: tauri::State<'_, SharedState>,
    sdp: String,
) -> Result<(), String> {
    let s = state.lock().await;

    let sender = s
        .guest_signaling
        .as_ref()
        .ok_or("Not connected to a room")?;
    let guest_id = s.guest_id.as_ref().ok_or("No guest ID")?;
    let room_id = s.guest_room_id.as_ref().ok_or("No room ID")?;

    tracing::debug!("[orbshare] guest sending sdp offer as {guest_id}");
    sender
        .send_sdp_offer(room_id, guest_id, &sdp)
        .map_err(|e| format!("Failed to send SDP offer: {}", e))?;

    Ok(())
}

#[tauri::command]
pub async fn guest_send_ice_candidate(
    state: tauri::State<'_, SharedState>,
    candidate: String,
    sdp_mid: Option<String>,
    sdp_mline_index: Option<u32>,
) -> Result<(), String> {
    let s = state.lock().await;

    let sender = s
        .guest_signaling
        .as_ref()
        .ok_or("Not connected to a room")?;
    let guest_id = s.guest_id.as_ref().ok_or("No guest ID")?;
    let room_id = s.guest_room_id.as_ref().ok_or("No room ID")?;

    sender
        .send_ice_candidate(room_id, guest_id, &candidate, sdp_mid, sdp_mline_index)
        .map_err(|e| format!("Failed to send ICE candidate: {}", e))?;

    Ok(())
}

#[tauri::command]
pub async fn list_capture_sources() -> Result<Vec<CaptureSource>, String> {
    let started = std::time::Instant::now();
    tracing::info!("[command] list_capture_sources called");
    let provider = create_capture_provider();
    let result = provider.list_sources().await;
    tracing::info!(
        "[command] list_capture_sources returned in {:?}: {} sources",
        started.elapsed(),
        result.as_ref().map(Vec::len).unwrap_or(0)
    );
    result
}

#[tauri::command]
pub async fn get_capture_source_thumbnail(
    source_id: u32,
    source_is_display: bool,
) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || {
        orb_capture::capture_source_thumbnail(source_id, source_is_display)
    })
    .await
    .map_err(|error| format!("生成共享来源预览失败：{error}"))?
}

#[tauri::command]
pub async fn get_active_capture_source(
    state: tauri::State<'_, SharedState>,
) -> Result<Option<ActiveCaptureSource>, String> {
    let state = state.lock().await;
    Ok(state
        .active_capture_config
        .as_ref()
        .map(|config| ActiveCaptureSource {
            source_id: config.source_id,
            source_is_display: config.source_is_display,
        }))
}

fn spawn_replacement_capture_pumps(
    app: AppHandle,
    state: SharedState,
    mut stream: orb_capture::CaptureStream,
    quality: QualityProfile,
    transport_mode: VideoTransportMode,
) {
    if let Some(preview_rx) = stream.preview_video_rx.take() {
        spawn_host_preview_pump(app.clone(), preview_rx);
    }
    if let Some(mut audio_rx) = stream.audio_rx.take() {
        let state_clone = state.clone();
        tokio::spawn(async move {
            let mut encoder = match SystemAudioOpusEncoder::new(2) {
                Ok(encoder) => encoder,
                Err(error) => {
                    tracing::error!("[orbshare:system-audio:switch] {error}");
                    return;
                }
            };
            // Avoid restarting at the same RTP sequence/timestamp values used
            // by the previous capture stream. Wraparound is valid RTP behavior.
            let clock_seed = unix_timestamp_millis();
            let mut sequence_number = clock_seed as u16;
            let mut timestamp = clock_seed.wrapping_mul(48) as u32;
            while let Some(frame) = audio_rx.recv().await {
                let packets =
                    match encoder.push_pcm(&frame.samples, frame.sample_rate, frame.channels) {
                        Ok(packets) => packets,
                        Err(error) => {
                            tracing::warn!("[orbshare:system-audio:switch] encode failed: {error}");
                            continue;
                        }
                    };
                let host_session = {
                    let state = state_clone.lock().await;
                    state.host_session.clone()
                };
                let Some(host_session) = host_session else {
                    return;
                };
                for payload in packets {
                    host_session
                        .read()
                        .await
                        .relay_audio_payload(
                            SYSTEM_AUDIO_SOURCE_ID,
                            payload,
                            sequence_number,
                            timestamp,
                        )
                        .await;
                    sequence_number = sequence_number.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(OPUS_FRAME_SAMPLES_PER_CHANNEL as u32);
                }
            }
        });
    }

    if transport_mode == VideoTransportMode::WebRtcVideoTrack {
        let Some(mut encoded_rx) = stream.encoded_video_rx.take() else {
            tracing::error!("[capture-switch] replacement stream has no H.264 receiver");
            return;
        };
        let state_clone = state.clone();
        tokio::spawn(async move {
            let mut waiting_for_keyframe = true;
            let mut frame_count = 0u64;
            let mut sent_count = 0u64;
            let send_budget = Duration::from_millis(500.max(1_000u64 / quality.fps.max(1) as u64));
            while let Some(frame) = encoded_rx.recv().await {
                let host_session = {
                    let state = state_clone.lock().await;
                    state.host_session.clone()
                };
                let Some(host_session) = host_session else {
                    return;
                };
                let track = { host_session.read().await.video_track_for_send() };
                let Some(track) = track else {
                    waiting_for_keyframe = true;
                    frame_count += 1;
                    continue;
                };
                if waiting_for_keyframe && !frame.is_keyframe {
                    frame_count += 1;
                    continue;
                }
                waiting_for_keyframe = false;
                let width = frame.width;
                let height = frame.height;
                match tokio::time::timeout(
                    send_budget,
                    MediaRouter::write_h264_sample(track, frame.data, quality.fps),
                )
                .await
                {
                    Ok(Ok(())) => sent_count += 1,
                    Ok(Err(error)) => {
                        tracing::warn!("[capture-switch] H.264 send failed: {error}")
                    }
                    Err(_) => waiting_for_keyframe = true,
                }
                frame_count += 1;
                if frame_count % 30 == 0 {
                    let _ = app.emit(
                        "capture-stats",
                        &CaptureStats {
                            width,
                            height,
                            fps: quality.fps,
                            pixel_format: format!("BGRA→YUV420→H264 ({})", quality.name),
                            frame_count,
                        },
                    );
                }
            }
            tracing::info!(
                "[capture-switch] replacement H.264 pump ended: captured={frame_count} sent={sent_count}"
            );
        });
    } else {
        let mut video_rx = stream.video_rx;
        let state_clone = state;
        tokio::spawn(async move {
            let mut frame_count = 0u64;
            while let Some(mut frame) = video_rx.recv().await {
                while let Ok(next) = video_rx.try_recv() {
                    frame = next;
                }
                let host_session = {
                    let state = state_clone.lock().await;
                    state.host_session.clone()
                };
                if let Some(host_session) = host_session {
                    if let Some(jpeg) = encode_remote_jpeg(&frame, quality.name) {
                        let host = host_session.read().await;
                        let _ = MediaRouter::send_mjpeg_frame(&host, jpeg).await;
                    }
                }
                frame_count += 1;
                if frame_count % 30 == 0 {
                    let _ = app.emit(
                        "capture-stats",
                        &CaptureStats {
                            width: frame.width,
                            height: frame.height,
                            fps: quality.fps,
                            pixel_format: format!("BGRA→YUV420→MJPEG ({})", quality.name),
                            frame_count,
                        },
                    );
                }
            }
        });
    }
}

#[tauri::command]
pub async fn switch_capture_source(
    app: AppHandle,
    state: tauri::State<'_, SharedState>,
    source_id: u32,
    source_is_display: bool,
) -> Result<(), String> {
    let switch_started = Instant::now();
    let (capture_mode, mut capture_config, quality, host_session) = {
        let state = state.lock().await;
        (
            state
                .active_capture_mode
                .ok_or("当前没有正在进行的屏幕共享。")?,
            state
                .active_capture_config
                .clone()
                .ok_or("当前会议缺少采集配置，请重新发起会议。")?,
            state.active_quality.ok_or("当前会议缺少画质配置。")?,
            state.host_session.clone().ok_or("当前会议已结束。")?,
        )
    };
    if capture_mode != CaptureMode::ScreenCapture {
        return Err("测试画面模式不能切换桌面或窗口。".to_string());
    }
    if capture_config.source_id == Some(source_id)
        && capture_config.source_is_display == source_is_display
    {
        return Ok(());
    }
    tracing::info!(
        "[capture-switch] starting source_id={source_id} source_is_display={source_is_display}"
    );

    let switching_payload = serde_json::json!({
        "type": "share_source_state",
        "state": "switching"
    })
    .to_string();
    host_session
        .read()
        .await
        .broadcast_control_message(&switching_payload)
        .await;

    capture_config.source_id = Some(source_id);
    capture_config.source_is_display = source_is_display;
    let transport_mode = capture_config.video_transport;
    let replacement = create_capture_provider_for_mode(capture_mode);
    if let Err(error) = replacement.request_permission().await {
        host_session
            .read()
            .await
            .broadcast_control_message(r#"{"type":"share_source_state","state":"active"}"#)
            .await;
        return Err(format!("无法切换共享内容：{error}"));
    }
    let replacement_config = capture_config.clone();
    let replacement_started = Instant::now();
    let replacement_result = tokio::task::spawn_blocking(move || {
        let mut replacement = replacement;
        let stream = replacement.start_capture(replacement_config)?;
        Ok::<_, String>((replacement, stream))
    })
    .await;
    let (mut replacement, stream) = match replacement_result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            let active_payload = serde_json::json!({
                "type": "share_source_state",
                "state": "active"
            })
            .to_string();
            host_session
                .read()
                .await
                .broadcast_control_message(&active_payload)
                .await;
            return Err(format!("无法切换共享内容：{error}"));
        }
        Err(error) => {
            host_session
                .read()
                .await
                .broadcast_control_message(r#"{"type":"share_source_state","state":"active"}"#)
                .await;
            return Err(format!("切换共享内容的后台任务异常结束：{error}"));
        }
    };
    tracing::info!(
        "[capture-switch] replacement capture ready in {:?}",
        replacement_started.elapsed()
    );
    if transport_mode == VideoTransportMode::WebRtcVideoTrack && stream.encoded_video_rx.is_none() {
        let _ = replacement.stop_capture();
        host_session
            .read()
            .await
            .broadcast_control_message(r#"{"type":"share_source_state","state":"active"}"#)
            .await;
        return Err("新共享来源没有提供 H.264 视频流。".to_string());
    }
    if capture_config.capture_system_audio && stream.audio_rx.is_none() {
        let _ = replacement.stop_capture();
        host_session
            .read()
            .await
            .broadcast_control_message(r#"{"type":"share_source_state","state":"active"}"#)
            .await;
        return Err("新共享来源没有提供系统声音。".to_string());
    }

    let old_provider = {
        let mut state = state.lock().await;
        if state.host_session.is_none() || state.capture_provider.is_none() {
            drop(state);
            let _ = replacement.stop_capture();
            host_session
                .read()
                .await
                .broadcast_control_message(r#"{"type":"share_source_state","state":"active"}"#)
                .await;
            return Err("会议已经结束，未切换共享内容。".to_string());
        }
        let old = state.capture_provider.replace(replacement);
        state.active_capture_config = Some(capture_config);
        old
    };
    if let Some(mut old_provider) = old_provider {
        let _ = old_provider.stop_capture();
    }
    spawn_replacement_capture_pumps(
        app.clone(),
        (*state).clone(),
        stream,
        quality,
        transport_mode,
    );

    let active_payload = serde_json::json!({
        "type": "share_source_state",
        "state": "active",
        "source_id": source_id,
        "source_is_display": source_is_display
    })
    .to_string();
    host_session
        .read()
        .await
        .broadcast_control_message(&active_payload)
        .await;
    let _ = app.emit(
        "capture-source-changed",
        serde_json::json!({
            "source_id": source_id,
            "source_is_display": source_is_display
        }),
    );
    tracing::info!(
        "[capture-switch] completed in {:?}: source_id={source_id} source_is_display={source_is_display}",
        switch_started.elapsed()
    );
    Ok(())
}

#[tauri::command]
pub async fn capture_stats(
    state: tauri::State<'_, SharedState>,
) -> Result<Option<CaptureStats>, String> {
    let s = state.lock().await;
    Ok(s.capture_provider.as_ref().and_then(|p| p.capture_stats()))
}

#[tauri::command]
pub fn check_screen_capture_permission() -> Result<bool, String> {
    tracing::info!("[command] check_screen_capture_permission called");
    let result = orb_capture::check_screen_capture_permission();
    tracing::info!(
        "[command] check_screen_capture_permission returned: {}",
        result
    );
    Ok(result)
}

#[tauri::command]
pub fn request_screen_capture_permission() -> Result<bool, String> {
    // CGRequestScreenCaptureAccess is permanently disabled — it causes
    // foreign-exception aborts. Open System Settings instead.
    tracing::info!("[command] request_screen_capture_permission called — opening System Settings");
    let result = orb_capture::open_screen_recording_settings();
    tracing::info!(
        "[command] open_screen_recording_settings returned: {:?}",
        result
    );
    Ok(false)
}

#[tauri::command]
pub fn open_screen_recording_settings() -> Result<(), String> {
    tracing::info!("[command] open_screen_recording_settings called");
    let result = orb_capture::open_screen_recording_settings();
    tracing::info!(
        "[command] open_screen_recording_settings returned: {:?}",
        result
    );
    result
}

/// Dev-only: force-stop any active room and clean up all state.
/// Useful when a previous session left the port in a dangling state.
#[tauri::command]
pub async fn force_stop_room(state: tauri::State<'_, SharedState>) -> Result<String, String> {
    tracing::warn!("[force_stop_room] Force-stopping room (dev command)");
    let mut s = state.lock().await;
    finish_recording_during_shutdown(&mut s);
    close_all_attendance_sessions(&mut *s.attendance.write().await, unix_timestamp());

    if let Some(ref viewer_server) = s.viewer_http_server {
        viewer_server.notify_room_closed();
    }
    if let Some(ref host_session) = s.host_session {
        let notified = host_session
            .read()
            .await
            .broadcast_control_message(HOST_STOP_CONTROL_MESSAGE)
            .await;
        tracing::info!("[force_stop_room] Notified {notified} viewer control channels");
    }

    let mut actions = Vec::new();

    if s.signaling_server.is_some() {
        if let Some(ref mut server) = s.signaling_server {
            let _ = server.broadcast(room_closed_message()).await;
            tokio::time::sleep(Duration::from_millis(80)).await;
            server.shutdown().await;
        }
        actions.push("signaling server shut down");
    }
    if s.viewer_http_server.is_some() {
        if let Some(ref viewer_server) = s.viewer_http_server {
            viewer_server.shutdown();
        }
        actions.push("viewer http server shut down");
    }
    if s.capture_provider.is_some() {
        if let Some(ref mut provider) = s.capture_provider {
            let _ = provider.stop_capture();
        }
        actions.push("capture stopped");
    }
    if s.mdns_publisher.is_some() {
        if let Some(ref publisher) = s.mdns_publisher {
            let _ = publisher.unpublish();
        }
        actions.push("mDNS unpublished");
    }

    s.capture_provider = None;
    s.active_capture_config = None;
    s.active_capture_mode = None;
    s.active_quality = None;
    s.host_session = None;
    s.signaling_server = None;
    s.viewer_http_server = None;
    s.mdns_publisher = None;
    s.active_room_password = None;
    if let Some(admission) = s.room_admission.take() {
        admission.clear();
    }
    s.room_manager = RoomManager::new();
    s.guest_signaling = None;
    s.guest_id = None;
    s.guest_room_id = None;

    let msg = if actions.is_empty() {
        "No active room to stop".to_string()
    } else {
        format!("Force stopped: {}", actions.join(", "))
    };
    tracing::warn!("[force_stop_room] {}", msg);
    Ok(msg)
}

#[cfg(test)]
mod attendance_tests {
    use super::*;

    #[test]
    fn room_closed_control_payload_is_a_terminal_host_event() {
        let payload: serde_json::Value =
            serde_json::from_str(HOST_STOP_CONTROL_MESSAGE).expect("valid control JSON");
        assert_eq!(payload["type"], "room_closed");
        assert_eq!(payload["reason"], "host_stopped");
    }

    #[test]
    fn removed_control_payload_is_a_terminal_host_event() {
        let payload: serde_json::Value =
            serde_json::from_str(HOST_REMOVED_CONTROL_MESSAGE).expect("valid control JSON");
        assert_eq!(payload["type"], "removed_by_host");
        assert_eq!(payload["reason"], "host_removed");
    }

    #[test]
    fn recording_state_control_payloads_are_canonical_json() {
        let started: serde_json::Value =
            serde_json::from_str(RECORDING_STARTED_CONTROL_MESSAGE).expect("valid start JSON");
        let stopped: serde_json::Value =
            serde_json::from_str(RECORDING_STOPPED_CONTROL_MESSAGE).expect("valid stop JSON");
        assert_eq!(started["type"], "recording_state");
        assert_eq!(started["active"], true);
        assert_eq!(stopped["active"], false);
    }

    #[test]
    fn laser_pointer_payload_round_trips_normalized_coordinates() {
        let payload = encode_laser_pointer(0.25, 0.75).expect("valid pointer coordinate");
        let (x, y) = decode_laser_pointer(&payload).expect("decode pointer coordinate");
        assert!((x - 0.25).abs() < f32::EPSILON);
        assert!((y - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn laser_pointer_rejects_malformed_or_out_of_range_coordinates() {
        assert!(encode_laser_pointer(-0.1, 0.5).is_err());
        assert!(encode_laser_pointer(0.5, 1.1).is_err());
        assert!(encode_laser_pointer(f32::NAN, 0.5).is_err());
        assert!(decode_laser_pointer(&[0; 7]).is_none());

        let mut malformed = [0u8; 8];
        malformed[..4].copy_from_slice(&f32::INFINITY.to_le_bytes());
        malformed[4..].copy_from_slice(&0.5f32.to_le_bytes());
        assert!(decode_laser_pointer(&malformed).is_none());
    }

    #[test]
    fn recording_sink_writes_incrementally_and_finalizes_the_owned_file() {
        let directory = std::env::temp_dir().join(format!("orbshare-recording-{}", uuid_like()));
        let mut sink = RecordingSink::create_in_directory(
            &directory,
            "video/mp4;codecs=avc1.42E01E,mp4a.40.2",
            "mp4",
            1_000,
        )
        .expect("create recording sink");
        sink.append(b"fragment-one").expect("append first fragment");
        sink.append(b"fragment-two")
            .expect("append second fragment");
        assert_eq!(sink.status().chunks_written, 2);

        let result = sink.finish(4_500).expect("finalize recording");
        assert_eq!(result.duration_ms, 3_500);
        assert_eq!(result.size_bytes, 24);
        assert_eq!(
            std::fs::read(&result.path).unwrap(),
            b"fragment-onefragment-two"
        );

        std::fs::remove_file(&result.path).expect("remove test recording");
        std::fs::remove_dir(&directory).expect("remove test directory");
    }

    #[test]
    fn empty_recording_is_rejected_and_removed() {
        let directory = std::env::temp_dir().join(format!("orbshare-recording-{}", uuid_like()));
        let sink = RecordingSink::create_in_directory(
            &directory,
            "video/mp4;codecs=avc1.42E01E,mp4a.40.2",
            "mp4",
            1_500,
        )
        .expect("create recording sink");
        let path = sink.path.clone();

        let error = match sink.finish(2_500) {
            Ok(_) => panic!("empty recording must not be reported as saved"),
            Err(error) => error,
        };
        assert!(error.contains("空文件"));
        assert!(!path.exists());

        std::fs::remove_dir(&directory).expect("remove test directory");
    }

    #[test]
    fn aborted_recording_removes_only_its_new_file() {
        let directory = std::env::temp_dir().join(format!("orbshare-recording-{}", uuid_like()));
        let sink = RecordingSink::create_in_directory(
            &directory,
            "video/webm;codecs=vp8,opus",
            ".webm",
            2_000,
        )
        .expect("create recording sink");
        let path = sink.path.clone();
        sink.abort().expect("abort recording");
        assert!(!path.exists());
        std::fs::remove_dir(&directory).expect("remove test directory");
    }

    #[test]
    fn recording_format_rejects_mime_extension_mismatch() {
        assert!(validated_recording_extension("video/mp4", "webm").is_err());
        assert!(validated_recording_extension("text/plain", "mp4").is_err());
        assert_eq!(
            validated_recording_extension("video/webm", ".WEBM").unwrap(),
            "webm"
        );
    }

    #[test]
    fn stale_in_memory_viewer_page_requires_protocol_upgrade() {
        assert!(viewer_protocol_is_supported(VIEWER_PROTOCOL_VERSION));
        assert!(!viewer_protocol_is_supported(0));
        assert!(!viewer_protocol_is_supported(
            VIEWER_PROTOCOL_VERSION.saturating_sub(1)
        ));
    }

    #[test]
    fn rendered_viewer_page_uses_the_server_protocol_version() {
        assert!(VIEWER_HTML.contains(VIEWER_PROTOCOL_PLACEHOLDER));

        let html = rendered_viewer_html();
        assert!(!html.contains(VIEWER_PROTOCOL_PLACEHOLDER));
        assert!(html.contains(&format!(
            "const VIEWER_PROTOCOL_VERSION = Number('{}')",
            VIEWER_PROTOCOL_VERSION
        )));
    }

    #[test]
    fn viewer_page_uses_the_bundled_orbshare_favicon() {
        assert!(VIEWER_HTML.contains("href=\"/favicon.ico\""));
        assert!(VIEWER_HTML.contains("type=\"image/svg+xml\""));
        assert!(VIEWER_FAVICON_SVG.contains("#2563EB"));
        assert!(VIEWER_FAVICON_SVG.contains("#06B6D4"));
        assert!(VIEWER_FAVICON_SVG.contains("<circle"));
        assert!(!VIEWER_FAVICON_SVG.contains("<image"));
    }

    #[test]
    fn viewer_chat_bundles_emoji_picker_without_external_assets() {
        assert!(VIEWER_HTML.contains("id=\"emoji-picker\""));
        assert!(VIEWER_HTML.contains("id=\"emoji-grid\""));
        assert!(VIEWER_HTML.contains("const commonChatEmojis = ["));
        assert!(VIEWER_HTML.contains("insertChatEmoji"));
        assert!(VIEWER_HTML.contains("chatForm.requestSubmit()"));
        assert!(!VIEWER_HTML.contains("emoji-picker.js"));
    }

    #[test]
    fn viewer_laser_pointer_uses_the_unreliable_realtime_channel() {
        assert!(VIEWER_HTML.contains("id=\"laser-toggle\""));
        assert!(VIEWER_HTML.contains("id=\"laser-dot\""));
        assert!(VIEWER_HTML.contains("pc.createDataChannel('pointer'"));
        assert!(VIEWER_HTML.contains("maxRetransmits: 0"));
        assert!(VIEWER_HTML.contains("handleLaserPayload"));
        assert!(VIEWER_HTML.contains("lastLaserSendAt < 40"));
    }

    #[test]
    fn raise_hand_transition_preserves_first_timestamp_and_clears_cleanly() {
        let mut member = test_guest_member("guest-1", 1);
        assert!(update_member_hand_state(&mut member, true, 1_000));
        assert!(member.hand_raised);
        assert_eq!(member.hand_raised_at, Some(1_000));

        assert!(!update_member_hand_state(&mut member, true, 2_000));
        assert_eq!(member.hand_raised_at, Some(1_000));

        assert!(update_member_hand_state(&mut member, false, 3_000));
        assert!(!member.hand_raised);
        assert_eq!(member.hand_raised_at, None);
    }

    #[test]
    fn viewer_queue_orders_many_raised_hands_without_copying_member_lists() {
        let mut viewers: Vec<MemberInfo> = (0..256u64)
            .map(|index| {
                let mut member = test_guest_member(&format!("guest-{index}"), index + 1);
                if index % 17 == 0 {
                    update_member_hand_state(&mut member, true, 10_000 - index);
                }
                member
            })
            .collect();

        viewers.sort_by_key(viewer_queue_sort_key);
        let raised_count = viewers
            .iter()
            .take_while(|member| member.hand_raised)
            .count();
        assert_eq!(raised_count, 16);
        assert!(viewers[..raised_count]
            .windows(2)
            .all(|pair| pair[0].hand_raised_at <= pair[1].hand_raised_at));
        assert!(viewers[raised_count..]
            .windows(2)
            .all(|pair| pair[0].connected_at <= pair[1].connected_at));
    }

    #[tokio::test]
    async fn room_closed_watch_wakes_a_waiting_browser_viewer() {
        let (tx, mut rx) = watch::channel(false);
        tx.send_replace(true);
        tokio::time::timeout(Duration::from_millis(50), rx.changed())
            .await
            .expect("viewer event wait timed out")
            .expect("viewer event channel closed");
        assert!(*rx.borrow());
    }

    #[tokio::test]
    async fn blocked_participant_receives_http_removal_fallback() {
        let (_tx, rx) = watch::channel(false);
        let blocked: BlockedParticipants = Arc::new(RwLock::new(HashSet::new()));
        blocked.write().await.insert("participant-1".to_string());

        let payload = tokio::time::timeout(
            Duration::from_millis(50),
            wait_for_browser_terminal_event(Some("participant-1".to_string()), blocked, rx),
        )
        .await
        .expect("removal fallback timed out");

        assert_eq!(payload, HOST_REMOVED_CONTROL_MESSAGE);
    }

    #[tokio::test]
    async fn room_closed_event_is_delivered_as_no_store_http_response() {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("test listener address");
        let (tx, mut rx) = watch::channel(false);

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept test viewer");
            rx.changed().await.expect("room closed signal");
            write_simple(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                HOST_STOP_CONTROL_MESSAGE.as_bytes().to_vec(),
            )
            .await;
        });

        let mut client = tokio::net::TcpStream::connect(address)
            .await
            .expect("connect test viewer");
        tx.send_replace(true);
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("read room closed response");
        server.await.expect("event response task");

        let response = String::from_utf8(response).expect("UTF-8 HTTP response");
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Cache-Control: no-store"));
        assert!(response.contains(HOST_STOP_CONTROL_MESSAGE));
    }

    #[test]
    fn records_join_leave_and_rejoin_timeline() {
        let mut records = HashMap::new();
        record_attendance_join(&mut records, "p1", "小王", "网页", 100);
        record_attendance_leave(&mut records, "p1", 160);
        record_attendance_join(&mut records, "p1", "小王", "网页", 200);

        let snapshot = attendance_snapshot(&records, 260);
        let record = &snapshot[0];
        assert_eq!(record.first_joined_at, 100);
        assert_eq!(record.left_at, vec![160]);
        assert_eq!(record.rejoined_at, vec![200]);
        assert_eq!(record.join_count, 2);
        assert_eq!(record.total_online_seconds, 120);
        assert!(record.online);
    }

    #[test]
    fn duplicate_offer_does_not_count_as_rejoin() {
        let mut records = HashMap::new();
        record_attendance_join(&mut records, "p1", "小王", "网页", 100);
        record_attendance_join(&mut records, "p1", "小王", "网页", 110);

        let record = records.get("p1").unwrap();
        assert_eq!(record.join_count, 1);
        assert!(record.rejoined_at.is_empty());
    }

    #[test]
    fn meeting_stop_closes_all_online_sessions() {
        let mut records = HashMap::new();
        record_attendance_join(&mut records, "p1", "小王", "网页", 100);
        record_attendance_join(&mut records, "p2", "小李", "网页", 120);
        close_all_attendance_sessions(&mut records, 180);

        assert!(records.values().all(|record| !record.online));
        assert_eq!(records["p1"].last_left_at, Some(180));
        assert_eq!(records["p2"].last_left_at, Some(180));
    }

    #[test]
    fn chat_is_trimmed_canonicalized_and_rate_limited() {
        let mut chat = ChatRoomState::default();
        let first = append_chat_message(
            &mut chat,
            "p1",
            "小王",
            MemberRole::Guest,
            "  大家好  ",
            1_000,
        )
        .expect("first chat message");
        assert_eq!(first.content, "大家好");
        assert_eq!(first.sender_name, "小王");
        assert_eq!(chat.messages.len(), 1);

        let error = append_chat_message(
            &mut chat,
            "p1",
            "小王",
            MemberRole::Guest,
            "发送太快",
            1_200,
        )
        .expect_err("rate limit must reject spam");
        assert!(error.contains("发送太快"));

        append_chat_message(&mut chat, "p1", "小王", MemberRole::Guest, "第二条", 1_500)
            .expect("message after rate window");
        assert_eq!(chat.messages.len(), 2);
    }

    #[test]
    fn chat_rejects_empty_and_overlong_content() {
        assert!(normalized_chat_content("  \n ").is_err());
        let overlong = "字".repeat(MAX_CHAT_CONTENT_CHARS + 1);
        assert!(normalized_chat_content(&overlong).is_err());
    }

    #[test]
    fn chat_history_keeps_only_the_latest_messages() {
        let mut chat = ChatRoomState::default();
        for index in 0..MAX_CHAT_MESSAGES + 5 {
            append_chat_message(
                &mut chat,
                &format!("p{index}"),
                "观看者",
                MemberRole::Guest,
                &format!("消息 {index}"),
                10_000 + index as u64,
            )
            .expect("distinct participant is not rate limited");
        }
        assert_eq!(chat.messages.len(), MAX_CHAT_MESSAGES);
        assert_eq!(chat.messages.front().unwrap().content, "消息 5");
    }

    #[test]
    fn canonical_chat_control_payload_is_broadcastable_json() {
        let mut chat = ChatRoomState::default();
        let message =
            append_chat_message(&mut chat, "host", "主持人", MemberRole::Host, "收到", 2_000)
                .unwrap();
        let payload: serde_json::Value =
            serde_json::from_str(&chat_control_payload(&message)).unwrap();
        assert_eq!(payload["type"], "chat_message");
        assert_eq!(payload["message"]["content"], "收到");
        assert_eq!(payload["message"]["sender_role"], "Host");
    }

    #[test]
    fn audio_relay_requires_visible_microphone_and_host_permission() {
        let mut member = test_guest_member("guest-1", 1);

        assert!(!participant_audio_may_relay(Some(&member), false));
        member.microphone_enabled = true;
        assert!(participant_audio_may_relay(Some(&member), false));
        member.host_muted = true;
        assert!(!participant_audio_may_relay(Some(&member), false));
        assert!(!participant_audio_may_relay(None, false));
        assert!(participant_audio_may_relay(None, true));
    }

    fn test_guest_member(member_id: &str, connected_at: u64) -> MemberInfo {
        MemberInfo {
            member_id: member_id.to_string(),
            participant_id: format!("participant-{member_id}"),
            name: "小王".to_string(),
            role: MemberRole::Guest,
            platform: "Web".to_string(),
            connected_at,
            microphone_enabled: false,
            host_muted: false,
            hand_raised: false,
            hand_raised_at: None,
        }
    }
}
