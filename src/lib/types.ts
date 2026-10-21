export interface RoomInfo {
  room_id: string;
  room_name: string;
  host_name: string;
  host_ip: string;
  signaling_port: number;
  viewer_port: number;
  version: string;
  max_guests: number;
  current_guests: number;
  capabilities: string[];
  requires_password: boolean;
}

export interface RoomConfig {
  room_name?: string;
  password: string;
  max_guests?: number;
  /** Native display/window identifier; omitted to use the primary display. */
  source_id?: number;
  /** true for a full desktop; false for one application window. */
  source_is_display?: boolean;
  /** "debug-jpeg-datachannel" (default) or "webrtc-video-track" */
  video_transport?: string;
  /** "test-pattern" (default) or "screen-capture" */
  capture_mode?: string;
  /** "standard", "high", or "pro". */
  quality_tier?: string;
  /** Capture the selected display's system audio and relay it as bundled Opus. */
  share_system_audio?: boolean;
}

export interface CaptureSource {
  id: number;
  name: string;
  width: number;
  height: number;
  is_display: boolean;
  thumbnail_data_url?: string | null;
}

export interface ActiveCaptureSource {
  source_id?: number | null;
  source_is_display: boolean;
}

export interface CaptureStats {
  width: number;
  height: number;
  fps: number;
  pixel_format: string;
  frame_count: number;
}

export interface MemberInfo {
  member_id: string;
  participant_id: string;
  name: string;
  role: "Host" | "Guest";
  platform: string;
  connected_at: number;
  microphone_enabled: boolean;
  host_muted: boolean;
  hand_raised: boolean;
  hand_raised_at: number | null;
}

export interface AttendanceRecord {
  participant_id: string;
  name: string;
  platform: string;
  first_joined_at: number;
  last_joined_at: number;
  last_left_at: number | null;
  rejoined_at: number[];
  left_at: number[];
  total_online_seconds: number;
  current_session_started_at: number | null;
  join_count: number;
  online: boolean;
}

export interface ChatMessage {
  message_id: string;
  sender_participant_id: string;
  sender_name: string;
  sender_role: "Host" | "Guest";
  content: string;
  sent_at: number;
}

export interface StreamStats {
  width: number;
  height: number;
  actual_fps: number;
  actual_bitrate_bps: number;
  target_fps: number;
  target_bitrate_bps: number;
  peer_count: number;
}

export interface RecordingStatus {
  path: string;
  mime_type: string;
  started_at_ms: number;
  bytes_written: number;
  chunks_written: number;
}

export interface RecordingResult {
  path: string;
  mime_type: string;
  started_at_ms: number;
  duration_ms: number;
  size_bytes: number;
  chunks_written: number;
}
