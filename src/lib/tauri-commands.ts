import { invoke } from "@tauri-apps/api/core";
import type { ActiveCaptureSource, AttendanceRecord, CaptureSource, CaptureStats, ChatMessage, MemberInfo, RecordingResult, RecordingStatus, RoomConfig, RoomInfo } from "./types";

export async function createRoom(config: RoomConfig): Promise<RoomInfo> {
  return invoke("create_room", { config });
}

export async function getActiveRoom(): Promise<RoomInfo | null> {
  return invoke("get_active_room");
}

export async function getActiveRoomPassword(): Promise<string | null> {
  return invoke("get_active_room_password");
}

export async function getConnectedViewers(): Promise<MemberInfo[]> {
  return invoke("get_connected_viewers");
}

export async function getAttendanceStats(): Promise<AttendanceRecord[]> {
  return invoke("get_attendance_stats");
}

export async function getChatMessages(): Promise<ChatMessage[]> {
  return invoke("get_chat_messages");
}

export async function sendHostChatMessage(content: string): Promise<ChatMessage> {
  return invoke("send_host_chat_message", { content });
}

export async function sendHostPointer(x: number, y: number): Promise<number> {
  return invoke("send_host_pointer", { x, y });
}

export async function removeViewer(memberId: string): Promise<void> {
  return invoke("remove_viewer", { memberId });
}

export async function setViewerMuted(memberId: string, muted: boolean): Promise<MemberInfo> {
  return invoke("set_viewer_muted", { memberId, muted });
}

export async function setViewerHandRaised(memberId: string, raised: boolean): Promise<MemberInfo> {
  return invoke("set_viewer_hand_raised", { memberId, raised });
}

export async function connectHostAudioMonitor(offerSdp: string): Promise<string> {
  return invoke("connect_host_audio_monitor", { offerSdp });
}

export async function disconnectHostAudioMonitor(): Promise<void> {
  return invoke("disconnect_host_audio_monitor");
}

export async function startRecording(mimeType: string, extension: string): Promise<RecordingStatus> {
  return invoke("start_recording", { mimeType, extension });
}

export async function appendRecordingChunk(dataBase64: string): Promise<RecordingStatus> {
  return invoke("append_recording_chunk", { dataBase64 });
}

export async function finishRecording(): Promise<RecordingResult> {
  return invoke("finish_recording");
}

export async function abortRecording(): Promise<void> {
  return invoke("abort_recording");
}

export async function getRecordingStatus(): Promise<RecordingStatus | null> {
  return invoke("get_recording_status");
}

export async function revealRecording(path: string): Promise<void> {
  return invoke("reveal_recording", { path });
}

export async function stopRoom(): Promise<void> {
  return invoke("stop_room");
}

export async function forceStopRoom(): Promise<string> {
  return invoke("force_stop_room");
}

export async function scanRooms(): Promise<void> {
  return invoke("scan_rooms");
}

export async function joinRoom(address: string, port: number, password: string): Promise<string> {
  return invoke("join_room", { address, port, password });
}

export async function leaveRoom(): Promise<void> {
  return invoke("leave_room");
}

export async function guestSendSdpOffer(sdp: string): Promise<void> {
  return invoke("guest_send_sdp_offer", { sdp });
}

export async function guestSendIceCandidate(candidate: string, sdpMid: string | null, sdpMlineIndex: number | null): Promise<void> {
  return invoke("guest_send_ice_candidate", { candidate, sdpMid, sdpMlineIndex });
}

export async function listCaptureSources(): Promise<CaptureSource[]> {
  return invokeWithTimeout("list_capture_sources", {}, 5000, "读取可共享内容超时，请关闭弹窗后重试。");
}

function invokeWithTimeout<T>(command: string, args: Record<string, unknown>, timeoutMs: number, message: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = window.setTimeout(() => reject(new Error(message)), timeoutMs);
    invoke<T>(command, args).then(
      (value) => {
        window.clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        window.clearTimeout(timer);
        reject(error);
      },
    );
  });
}

export async function getCaptureSourceThumbnail(sourceId: number, sourceIsDisplay: boolean): Promise<string | null> {
  return invoke("get_capture_source_thumbnail", { sourceId, sourceIsDisplay });
}

export async function getActiveCaptureSource(): Promise<ActiveCaptureSource | null> {
  return invoke("get_active_capture_source");
}

export async function switchCaptureSource(sourceId: number, sourceIsDisplay: boolean): Promise<void> {
  return invokeWithTimeout(
    "switch_capture_source",
    { sourceId, sourceIsDisplay },
    12000,
    "切换共享内容超时。原共享内容仍在继续，请刷新来源列表后重试。",
  );
}

export async function getCaptureStats(): Promise<CaptureStats | null> {
  return invoke("capture_stats");
}

export async function checkScreenCapturePermission(): Promise<boolean> {
  return invoke("check_screen_capture_permission");
}

export async function requestScreenCapturePermission(): Promise<boolean> {
  return invoke("request_screen_capture_permission");
}

export async function openScreenRecordingSettings(): Promise<void> {
  return invoke("open_screen_recording_settings");
}
