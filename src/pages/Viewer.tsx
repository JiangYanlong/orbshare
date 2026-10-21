import { useRef, useState, useEffect, useCallback, type FormEvent, type PointerEvent as ReactPointerEvent } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  stopRoom,
  leaveRoom,
  guestSendSdpOffer,
  guestSendIceCandidate,
  getConnectedViewers,
  getAttendanceStats,
  getChatMessages,
  sendHostChatMessage,
  sendHostPointer,
  removeViewer,
  setViewerMuted,
  setViewerHandRaised,
  connectHostAudioMonitor,
  disconnectHostAudioMonitor,
  startRecording,
  appendRecordingChunk,
  finishRecording,
  abortRecording,
  listCaptureSources,
  getActiveCaptureSource,
  getActiveRoomPassword,
  switchCaptureSource,
  revealRecording,
} from "../lib/tauri-commands";
import type { AttendanceRecord, CaptureSource, CaptureStats, ChatMessage, MemberInfo, RecordingResult, RoomInfo, StreamStats } from "../lib/types";
import { hydrateCaptureSourceThumbnails } from "../lib/capture-source-thumbnails";
import SourcePicker from "../components/SourcePicker";
import MeetingInviteDialog from "../components/MeetingInviteDialog";
import { describeMicrophoneError, LanguageSwitch, useLanguage } from "../i18n";

interface ViewerProps {
  room?: RoomInfo;
  address?: string;
  port?: number;
  isHost?: boolean;
  onBack: () => void;
  onStopped?: () => void;
  onRecordingSaved?: (result: RecordingResult) => void;
}

interface DanmakuItem {
  id: string;
  senderName: string;
  senderRole: ChatMessage["sender_role"];
  content: string;
  lane: number;
  durationSeconds: number;
}

interface LaserPoint {
  leftPercent: number;
  topPercent: number;
  senderId: string;
}

interface LaserPointerEvent {
  sender_id: string;
  x: number;
  y: number;
}

type RecordingPhase = "idle" | "starting" | "recording" | "stopping";

const RECORDING_MAX_FPS = 20;
const RECORDING_CHUNK_INTERVAL_MS = 1000;

function selectRecordingFormat(): { mimeType: string; extension: "mp4" | "webm" } | null {
  if (typeof MediaRecorder === "undefined") return null;
  const candidates: Array<{ mimeType: string; extension: "mp4" | "webm" }> = [
    { mimeType: "video/mp4;codecs=avc1.42E01E,mp4a.40.2", extension: "mp4" },
    { mimeType: "video/mp4", extension: "mp4" },
    { mimeType: "video/webm;codecs=vp9,opus", extension: "webm" },
    { mimeType: "video/webm;codecs=vp8,opus", extension: "webm" },
    { mimeType: "video/webm", extension: "webm" },
  ];
  return candidates.find(({ mimeType }) => MediaRecorder.isTypeSupported(mimeType)) ?? null;
}

function blobToBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("无法读取录制分片。"));
    reader.onload = () => {
      const result = typeof reader.result === "string" ? reader.result : "";
      // MP4 MIME values may contain a codec list such as
      // `codecs=avc1.42E01E,mp4a.40.2`; only the final comma separates the
      // data URL metadata from its Base64 payload.
      const separator = result.lastIndexOf(",");
      if (separator < 0) {
        reject(new Error("录制分片格式无效。"));
        return;
      }
      resolve(result.slice(separator + 1));
    };
    reader.readAsDataURL(blob);
  });
}

function sortViewerQueue(members: MemberInfo[]): MemberInfo[] {
  return [...members].sort((left, right) => {
    if (left.hand_raised !== right.hand_raised) return left.hand_raised ? -1 : 1;
    if (left.hand_raised && right.hand_raised) {
      return (left.hand_raised_at ?? Number.MAX_SAFE_INTEGER)
        - (right.hand_raised_at ?? Number.MAX_SAFE_INTEGER);
    }
    return left.connected_at - right.connected_at;
  });
}

const AUDIO_RELAY_SLOTS = 24;
const COMMON_CHAT_EMOJIS = [
  "😀", "😄", "😁", "😂", "😊", "😍", "🥳", "😎",
  "🤔", "😅", "😭", "😮", "😡", "👍", "👏", "🙌",
  "👌", "🤝", "🙏", "💪", "👋", "🎉", "❤️", "🔥",
  "✨", "✅", "💡", "👀", "🚀", "📌", "💯", "🌟",
] as const;

function waitForIceGatheringComplete(pc: RTCPeerConnection, timeoutMs = 4000): Promise<void> {
  if (pc.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const finish = () => {
      window.clearTimeout(timer);
      pc.removeEventListener("icegatheringstatechange", handleStateChange);
      resolve();
    };
    const handleStateChange = () => {
      if (pc.iceGatheringState === "complete") finish();
    };
    const timer = window.setTimeout(finish, timeoutMs);
    pc.addEventListener("icegatheringstatechange", handleStateChange);
  });
}

export default function Viewer({
  room,
  address,
  port,
  isHost,
  onBack,
  onStopped,
  onRecordingSaved,
}: ViewerProps) {
  const { language, t } = useLanguage();
  const videoRef = useRef<HTMLVideoElement>(null);
  const jpegImageRef = useRef<HTMLImageElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const audioRef = useRef<HTMLAudioElement>(null);
  const pcRef = useRef<RTCPeerConnection | null>(null);
  const hostAudioPcRef = useRef<RTCPeerConnection | null>(null);
  const controlChannelRef = useRef<RTCDataChannel | null>(null);
  const pointerChannelRef = useRef<RTCDataChannel | null>(null);
  const audioSenderRef = useRef<RTCRtpSender | null>(null);
  const microphoneTrackRef = useRef<MediaStreamTrack | null>(null);
  const remoteAudioStreamRef = useRef<MediaStream>(new MediaStream());
  const recordingVideoTrackRef = useRef<MediaStreamTrack | null>(null);
  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const recordingAudioContextRef = useRef<AudioContext | null>(null);
  const recordingAudioDestinationRef = useRef<MediaStreamAudioDestinationNode | null>(null);
  const recordingAudioNodesRef = useRef<Map<string, MediaStreamAudioSourceNode>>(new Map());
  const recordingStreamRef = useRef<MediaStream | null>(null);
  const recordingWriteQueueRef = useRef<Promise<void>>(Promise.resolve());
  const recordingStopPromiseRef = useRef<Promise<RecordingResult | null> | null>(null);
  const recordingStopResolverRef = useRef<((result: RecordingResult | null) => void) | null>(null);
  const recordingStopRejecterRef = useRef<((error: unknown) => void) | null>(null);
  const recordingBackendActiveRef = useRef(false);
  const recordingWriteErrorRef = useRef<Error | null>(null);
  const recordingPhaseRef = useRef<RecordingPhase>("idle");
  const lastRecordingResultRef = useRef<RecordingResult | null>(null);
  const microphoneEnabledRef = useRef(false);
  const hostMutedRef = useRef(false);
  const handRaisedRef = useRef(false);
  const jpegFrameUrlRef = useRef<string | null>(null);
  const jpegChunksRef = useRef<
    Map<number, { total: number; received: number; chunks: Array<Uint8Array | undefined> }>
  >(new Map());
  const unlistenRefs = useRef<Array<() => void>>([]);
  const hasVideoTrackRef = useRef(false);
  const firstFrameReceivedRef = useRef(false);
  const h264FirstFrameRef = useRef(false);
  const lastRenderedFrameIdRef = useRef(-1);
  const firstFrameTimerRef = useRef<number | null>(null);
  const danmakuLaneRef = useRef(0);
  const danmakuEnabledRef = useRef(true);
  const danmakuSeenRef = useRef<Set<string>>(new Set());
  const danmakuTimersRef = useRef<Set<number>>(new Set());
  const chatInputRef = useRef<HTMLTextAreaElement>(null);
  const chatScrollRef = useRef<HTMLDivElement>(null);
  const laserHideTimerRef = useRef<number | null>(null);
  const lastLaserSendAtRef = useRef(0);
  const hostPointerSendingRef = useRef(false);
  const pendingHostPointerRef = useRef<{ x: number; y: number } | null>(null);
  const [peerCount, setPeerCount] = useState(0);
  const [viewers, setViewers] = useState<MemberInfo[]>([]);
  const [attendance, setAttendance] = useState<AttendanceRecord[]>([]);
  const [streamStats, setStreamStats] = useState<StreamStats | null>(null);
  const [connectionState, setConnectionState] = useState<string>("new");
  const [captureStats, setCaptureStats] = useState<CaptureStats | null>(null);
  const [jpegFrameUrl, setJpegFrameUrl] = useState<string | null>(null);
  const [jpegFrameCount, setJpegFrameCount] = useState(0);
  const [jpegChannelState, setJpegChannelState] = useState("");
  const [viewerError, setViewerError] = useState("");
  const [hasFirstVideoFrame, setHasFirstVideoFrame] = useState(false);
  const [roomEnded, setRoomEnded] = useState(false);
  const [linkCopied, setLinkCopied] = useState(false);
  const [inviteDialogOpen, setInviteDialogOpen] = useState(false);
  const [meetingPassword, setMeetingPassword] = useState("");
  const [chatMessages, setChatMessages] = useState<ChatMessage[]>([]);
  const [chatInput, setChatInput] = useState("");
  const [chatError, setChatError] = useState("");
  const [chatSending, setChatSending] = useState(false);
  const [emojiPickerOpen, setEmojiPickerOpen] = useState(false);
  const [danmakuEnabled, setDanmakuEnabled] = useState(() => {
    try {
      return window.localStorage.getItem("orbshare-danmaku-enabled") !== "false";
    } catch {
      return true;
    }
  });
  const [danmakuItems, setDanmakuItems] = useState<DanmakuItem[]>([]);
  const [laserEnabled, setLaserEnabled] = useState(false);
  const [laserPoint, setLaserPoint] = useState<LaserPoint | null>(null);
  const [panelTab, setPanelTab] = useState<"participants" | "chat">(
    isHost ? "participants" : "chat",
  );
  const [sidePanelOpen, setSidePanelOpen] = useState(false);
  const [removingMemberId, setRemovingMemberId] = useState<string | null>(null);
  const [mutingMemberId, setMutingMemberId] = useState<string | null>(null);
  const [loweringHandMemberId, setLoweringHandMemberId] = useState<string | null>(null);
  const [microphoneEnabled, setMicrophoneEnabled] = useState(false);
  const [hostMuted, setHostMuted] = useState(false);
  const [microphoneError, setMicrophoneError] = useState("");
  const [hostAudioConnected, setHostAudioConnected] = useState(false);
  const [recordingVideoReady, setRecordingVideoReady] = useState(false);
  const [recordingPhase, setRecordingPhase] = useState<RecordingPhase>("idle");
  const [recordingStartedAt, setRecordingStartedAt] = useState(0);
  const [recordingElapsedSeconds, setRecordingElapsedSeconds] = useState(0);
  const [recordingResult, setRecordingResult] = useState<RecordingResult | null>(null);
  const [recordingError, setRecordingError] = useState("");
  const [remoteRecordingActive, setRemoteRecordingActive] = useState(false);
  const [handRaised, setHandRaised] = useState(false);
  const [handActionPending, setHandActionPending] = useState(false);
  const [roomEndTitle, setRoomEndTitle] = useState("投屏已结束");
  const [roomEndMessage, setRoomEndMessage] = useState("房主已停止共享，当前连接已经关闭。");
  const [sourcePickerOpen, setSourcePickerOpen] = useState(false);
  const [captureSources, setCaptureSources] = useState<CaptureSource[]>([]);
  const [selectedCaptureSource, setSelectedCaptureSource] = useState<CaptureSource | null>(null);
  const [sourcePickerLoading, setSourcePickerLoading] = useState(false);
  const [sourceSwitching, setSourceSwitching] = useState(false);
  const [sourceSwitchError, setSourceSwitchError] = useState("");

  useEffect(() => {
    danmakuEnabledRef.current = danmakuEnabled;
  }, [danmakuEnabled]);

  useEffect(() => {
    if (!isHost || !room?.requires_password) {
      setMeetingPassword("");
      return;
    }
    let cancelled = false;
    void getActiveRoomPassword()
      .then((password) => {
        if (!cancelled) setMeetingPassword(password ?? "");
      })
      .catch(() => {
        if (!cancelled) setMeetingPassword("");
      });
    return () => {
      cancelled = true;
    };
  }, [isHost, room?.requires_password, room?.room_id]);

  useEffect(() => () => {
    for (const timer of danmakuTimersRef.current) window.clearTimeout(timer);
    danmakuTimersRef.current.clear();
  }, []);

  useEffect(() => {
    if (!sidePanelOpen || panelTab !== "chat") return;
    const frame = window.requestAnimationFrame(() => {
      const container = chatScrollRef.current;
      if (container) container.scrollTop = container.scrollHeight;
    });
    return () => window.cancelAnimationFrame(frame);
  }, [chatMessages, panelTab, sidePanelOpen]);

  useEffect(() => {
    if (!sidePanelOpen || panelTab !== "chat") setEmojiPickerOpen(false);
  }, [panelTab, sidePanelOpen]);

  const updateRecordingPhase = useCallback((phase: RecordingPhase) => {
    recordingPhaseRef.current = phase;
    setRecordingPhase(phase);
  }, []);

  useEffect(() => {
    if (recordingPhase !== "recording" || !recordingStartedAt) return;
    const update = () => setRecordingElapsedSeconds(
      Math.max(0, Math.floor((Date.now() - recordingStartedAt) / 1000)),
    );
    update();
    const timer = window.setInterval(update, 1000);
    return () => window.clearInterval(timer);
  }, [recordingPhase, recordingStartedAt]);

  const pushDanmaku = useCallback((message: ChatMessage) => {
    if (!danmakuEnabledRef.current || danmakuSeenRef.current.has(message.message_id)) return;
    if (danmakuSeenRef.current.size > 500) danmakuSeenRef.current.clear();
    danmakuSeenRef.current.add(message.message_id);
    const normalized = message.content.replace(/\s+/g, " ").trim();
    if (!normalized) return;
    const content = normalized.length > 80 ? `${normalized.slice(0, 80)}…` : normalized;
    const durationSeconds = 8 + Math.min(5, content.length * 0.055);
    const item: DanmakuItem = {
      id: `${message.message_id}-${Date.now()}`,
      senderName: message.sender_name,
      senderRole: message.sender_role,
      content,
      lane: danmakuLaneRef.current++ % 7,
      durationSeconds,
    };
    setDanmakuItems((items) => [...items, item].slice(-24));
    const timer = window.setTimeout(() => {
      setDanmakuItems((items) => items.filter((candidate) => candidate.id !== item.id));
      danmakuTimersRef.current.delete(timer);
    }, (durationSeconds + 0.5) * 1000);
    danmakuTimersRef.current.add(timer);
  }, []);

  const toggleDanmaku = useCallback(() => {
    setDanmakuEnabled((enabled) => {
      const next = !enabled;
      danmakuEnabledRef.current = next;
      try {
        window.localStorage.setItem("orbshare-danmaku-enabled", String(next));
      } catch {
        // Local preference persistence is best-effort.
      }
      if (!next) {
        setDanmakuItems([]);
        for (const timer of danmakuTimersRef.current) window.clearTimeout(timer);
        danmakuTimersRef.current.clear();
      }
      return next;
    });
  }, []);

  const getLaserMediaRect = useCallback(() => {
    const stage = stageRef.current;
    if (!stage) return null;
    const stageRect = stage.getBoundingClientRect();
    const mediaCandidates: Array<HTMLVideoElement | HTMLImageElement | null> = [
      videoRef.current,
      jpegImageRef.current,
    ];
    const mediaRect = mediaCandidates
      .map((element) => element?.getBoundingClientRect())
      .find((rect) => rect && rect.width > 1 && rect.height > 1);
    return { stageRect, mediaRect: mediaRect ?? stageRect };
  }, []);

  const showLaserPoint = useCallback((x: number, y: number, senderId: string) => {
    if (!Number.isFinite(x) || !Number.isFinite(y) || x < 0 || x > 1 || y < 0 || y > 1) return;
    const rects = getLaserMediaRect();
    if (!rects || rects.stageRect.width <= 0 || rects.stageRect.height <= 0) return;
    const left = rects.mediaRect.left - rects.stageRect.left + x * rects.mediaRect.width;
    const top = rects.mediaRect.top - rects.stageRect.top + y * rects.mediaRect.height;
    setLaserPoint({
      leftPercent: (left / rects.stageRect.width) * 100,
      topPercent: (top / rects.stageRect.height) * 100,
      senderId,
    });
    if (laserHideTimerRef.current !== null) window.clearTimeout(laserHideTimerRef.current);
    laserHideTimerRef.current = window.setTimeout(() => {
      setLaserPoint(null);
      laserHideTimerRef.current = null;
    }, 900);
  }, [getLaserMediaRect]);

  const handleLaserPayload = useCallback(async (data: string | ArrayBuffer | Blob) => {
    try {
      const buffer = data instanceof ArrayBuffer
        ? data
        : data instanceof Blob
          ? await data.arrayBuffer()
          : new TextEncoder().encode(data).buffer;
      if (buffer.byteLength !== 8) return;
      const view = new DataView(buffer);
      showLaserPoint(view.getFloat32(0, true), view.getFloat32(4, true), "remote");
    } catch {
      // Malformed or stale pointer packets are intentionally ignored.
    }
  }, [showLaserPoint]);

  const queueHostPointer = useCallback((x: number, y: number) => {
    pendingHostPointerRef.current = { x, y };
    if (hostPointerSendingRef.current) return;
    hostPointerSendingRef.current = true;
    void (async () => {
      try {
        while (pendingHostPointerRef.current) {
          const next = pendingHostPointerRef.current;
          pendingHostPointerRef.current = null;
          await sendHostPointer(next.x, next.y);
        }
      } catch {
        // The pointer is ephemeral; a dropped sample is preferable to queueing.
      } finally {
        hostPointerSendingRef.current = false;
      }
    })();
  }, []);

  const transmitLaserPoint = useCallback((x: number, y: number) => {
    showLaserPoint(x, y, "local");
    if (isHost) {
      queueHostPointer(x, y);
      return;
    }
    const channel = pointerChannelRef.current;
    if (!channel || channel.readyState !== "open") return;
    const payload = new ArrayBuffer(8);
    const view = new DataView(payload);
    view.setFloat32(0, x, true);
    view.setFloat32(4, y, true);
    try {
      channel.send(payload);
    } catch {
      // A later movement sample will resume when the channel recovers.
    }
  }, [isHost, queueHostPointer, showLaserPoint]);

  const handleStageLaserPointer = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    if (!laserEnabled || roomEnded || sourceSwitching) return;
    const now = performance.now();
    if (event.type === "pointermove" && now - lastLaserSendAtRef.current < 40) return;
    const rects = getLaserMediaRect();
    if (!rects || rects.mediaRect.width <= 0 || rects.mediaRect.height <= 0) return;
    const x = (event.clientX - rects.mediaRect.left) / rects.mediaRect.width;
    const y = (event.clientY - rects.mediaRect.top) / rects.mediaRect.height;
    if (x < 0 || x > 1 || y < 0 || y > 1) return;
    lastLaserSendAtRef.current = now;
    transmitLaserPoint(x, y);
  }, [getLaserMediaRect, laserEnabled, roomEnded, sourceSwitching, transmitLaserPoint]);

  useEffect(() => () => {
    if (laserHideTimerRef.current !== null) window.clearTimeout(laserHideTimerRef.current);
    laserHideTimerRef.current = null;
  }, []);

  const announceMicrophoneState = useCallback((enabled: boolean) => {
    if (isHost) return;
    const channel = controlChannelRef.current;
    if (channel?.readyState === "open") {
      channel.send(JSON.stringify({ type: "audio_state", enabled }));
    }
  }, [isHost]);

  const connectRecordingAudioTrack = useCallback((track: MediaStreamTrack) => {
    const context = recordingAudioContextRef.current;
    const destination = recordingAudioDestinationRef.current;
    if (!context || !destination || track.kind !== "audio") return;
    if (recordingAudioNodesRef.current.has(track.id)) return;
    try {
      const source = context.createMediaStreamSource(new MediaStream([track]));
      source.connect(destination);
      recordingAudioNodesRef.current.set(track.id, source);
      track.addEventListener("ended", () => {
        const node = recordingAudioNodesRef.current.get(track.id);
        if (node) node.disconnect();
        recordingAudioNodesRef.current.delete(track.id);
      }, { once: true });
    } catch (error) {
      setRecordingError(error instanceof Error ? error.message : String(error));
    }
  }, []);

  const stopLocalMicrophone = useCallback((announce = true) => {
    const track = microphoneTrackRef.current;
    microphoneTrackRef.current = null;
    microphoneEnabledRef.current = false;
    setMicrophoneEnabled(false);
    if (track) track.stop();
    void audioSenderRef.current?.replaceTrack(null).catch(() => {});
    if (announce) announceMicrophoneState(false);
  }, [announceMicrophoneState]);

  const toggleLocalMicrophone = useCallback(async () => {
    setMicrophoneError("");
    if (microphoneEnabledRef.current) {
      stopLocalMicrophone();
      return;
    }
    if (!isHost && hostMutedRef.current) {
      setMicrophoneError("主持人已将你禁言");
      return;
    }
    const sender = audioSenderRef.current;
    if (!sender) {
      setMicrophoneError("音频通道正在连接，请稍后再试");
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        video: false,
        audio: {
          echoCancellation: true,
          noiseSuppression: true,
          autoGainControl: true,
        },
      });
      const track = stream.getAudioTracks()[0];
      if (!track) throw new Error("没有检测到可用麦克风");
      await sender.replaceTrack(track);
      microphoneTrackRef.current = track;
      microphoneEnabledRef.current = true;
      setMicrophoneEnabled(true);
      connectRecordingAudioTrack(track);
      announceMicrophoneState(true);
      track.onended = () => stopLocalMicrophone();
    } catch (error) {
      setMicrophoneError(describeMicrophoneError(error));
      stopLocalMicrophone();
    }
  }, [announceMicrophoneState, connectRecordingAudioTrack, isHost, stopLocalMicrophone]);

  const attachRemoteAudioTrack = useCallback((track: MediaStreamTrack) => {
    const stream = remoteAudioStreamRef.current;
    if (!stream.getAudioTracks().some((existing) => existing.id === track.id)) {
      stream.addTrack(track);
    }
    connectRecordingAudioTrack(track);
    track.onended = () => {
      stream.removeTrack(track);
    };
    if (audioRef.current) {
      const audioElement = audioRef.current;
      audioElement.srcObject = stream;
      void audioElement.play().catch(() => {
        // Some browsers require a user gesture before playing meeting audio.
        // Retry silently on the next interaction instead of covering the
        // shared picture with a technical warning.
        window.addEventListener("pointerdown", () => {
          void audioElement.play().catch(() => {});
        }, { once: true });
      });
    }
  }, [connectRecordingAudioTrack]);

  const refreshViewers = useCallback(async () => {
    if (!isHost) return;
    try {
      const [nextViewers, nextAttendance] = await Promise.all([
        getConnectedViewers(),
        getAttendanceStats(),
      ]);
      setViewers(sortViewerQueue(nextViewers));
      setAttendance(nextAttendance);
      setPeerCount(nextViewers.length);
    } catch {
      // A join/leave event may arrive while the room is shutting down.
    }
  }, [isHost]);

  // ── Host mode: listen for peer join/leave ──────────────────────────
  useEffect(() => {
    if (!isHost) return;

    const setup = async () => {
      const un1 = await listen<string>("peer-connected", (event) => {
        void event.payload;
        void refreshViewers();
      });
      const un2 = await listen<string>("peer-disconnected", (event) => {
        void event.payload;
        void refreshViewers();
      });
      const un3 = await listen<CaptureStats>("capture-stats", (event) => {
        setCaptureStats(event.payload);
      });
      const un4 = await listen<{ dataUrl: string }>("host-video-frame", (event) => {
        setJpegFrameUrl(event.payload.dataUrl);
        setJpegFrameCount((count) => count + 1);
      });
      const un5 = await listen<StreamStats>("stream-stats", (event) => {
        setStreamStats(event.payload);
      });
      const un6 = await listen<ChatMessage>("chat-message", (event) => {
        pushDanmaku(event.payload);
        setChatMessages((messages) => messages.some((message) => message.message_id === event.payload.message_id)
          ? messages
          : [...messages, event.payload].slice(-100));
      });
      const un7 = await listen<MemberInfo>("member-updated", (event) => {
        setViewers((current) => {
          const index = current.findIndex((member) => member.member_id === event.payload.member_id);
          if (index < 0) return current;
          const next = [...current];
          next[index] = event.payload;
          return sortViewerQueue(next);
        });
      });
      const un8 = await listen<LaserPointerEvent>("laser-pointer", (event) => {
        showLaserPoint(event.payload.x, event.payload.y, event.payload.sender_id);
      });
      unlistenRefs.current.push(un1, un2, un3, un4, un5, un6, un7, un8);
      const [initialMessages] = await Promise.all([getChatMessages(), refreshViewers()]);
      setChatMessages(initialMessages);
      const attendanceTimer = window.setInterval(() => void refreshViewers(), 15000);
      unlistenRefs.current.push(() => window.clearInterval(attendanceTimer));
    };
    setup();

    return () => {
      unlistenRefs.current.forEach((un) => un());
      unlistenRefs.current = [];
      setViewers([]);
      setAttendance([]);
      setChatMessages([]);
    };
  }, [isHost, pushDanmaku, refreshViewers, showLaserPoint]);

  // ── Host audio: in-process WebRTC monitor + optional microphone ─────
  useEffect(() => {
    if (!isHost) return;

    let cancelled = false;
    const pc = new RTCPeerConnection({ iceServers: [] });
    hostAudioPcRef.current = pc;
    const videoTransceiver = pc.addTransceiver("video", { direction: "recvonly" });
    try {
      const caps = RTCRtpReceiver.getCapabilities?.("video");
      const codecs = caps?.codecs ?? [];
      const h264 = codecs.filter((codec) => /video\/H264/i.test(codec.mimeType));
      const rtx = codecs.filter((codec) => /video\/rtx/i.test(codec.mimeType));
      if (h264.length && videoTransceiver.setCodecPreferences) {
        videoTransceiver.setCodecPreferences([...h264, ...rtx]);
      }
    } catch {
      // Codec preference is best-effort; the SDP still negotiates H.264.
    }
    const audioTransceiver = pc.addTransceiver("audio", { direction: "sendrecv" });
    audioSenderRef.current = audioTransceiver.sender;
    for (let slot = 1; slot < AUDIO_RELAY_SLOTS; slot += 1) {
      pc.addTransceiver("audio", { direction: "recvonly" });
    }
    pc.createDataChannel("host-audio-control");
    pc.ontrack = (event) => {
      if (cancelled) return;
      if (event.track.kind === "audio") {
        attachRemoteAudioTrack(event.track);
      } else if (event.track.kind === "video") {
        try {
          event.track.contentHint = "detail";
        } catch {
          // Some WebKit builds expose contentHint as read-only.
        }
        recordingVideoTrackRef.current = event.track;
        setRecordingVideoReady(true);
        event.track.addEventListener("ended", () => {
          if (recordingVideoTrackRef.current?.id === event.track.id) {
            recordingVideoTrackRef.current = null;
            setRecordingVideoReady(false);
          }
        }, { once: true });
      }
    };
    pc.onconnectionstatechange = () => {
      if (!cancelled) setHostAudioConnected(pc.connectionState === "connected");
    };

    const connect = async () => {
      try {
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        await waitForIceGatheringComplete(pc);
        if (cancelled || !pc.localDescription) return;
        const answerSdp = await connectHostAudioMonitor(JSON.stringify(pc.localDescription));
        if (cancelled) return;
        await pc.setRemoteDescription(new RTCSessionDescription(JSON.parse(answerSdp)));
      } catch (error) {
        if (!cancelled) {
          setMicrophoneError(error instanceof Error ? error.message : String(error));
        }
      }
    };
    void connect();

    return () => {
      cancelled = true;
      stopLocalMicrophone(false);
      pc.ontrack = null;
      pc.onconnectionstatechange = null;
      pc.close();
      hostAudioPcRef.current = null;
      audioSenderRef.current = null;
      recordingVideoTrackRef.current = null;
      setRecordingVideoReady(false);
      setHostAudioConnected(false);
      void disconnectHostAudioMonitor().catch(() => {});
    };
  }, [attachRemoteAudioTrack, isHost, stopLocalMicrophone]);

  // ── Guest mode: full WebRTC handshake ──────────────────────────────
  useEffect(() => {
    if (isHost) return;

    let cancelled = false;
    const clearFirstFrameTimer = () => {
      if (firstFrameTimerRef.current !== null) {
        window.clearTimeout(firstFrameTimerRef.current);
        firstFrameTimerRef.current = null;
      }
    };
    const armFirstFrameTimer = (reason: string) => {
      clearFirstFrameTimer();
      firstFrameTimerRef.current = window.setTimeout(() => {
        if (!firstFrameReceivedRef.current) {
          setViewerError(`${reason}: 首帧超时 (8s)`);
        }
      }, 8000);
    };

    hasVideoTrackRef.current = false;
    firstFrameReceivedRef.current = false;
    h264FirstFrameRef.current = false;
    lastRenderedFrameIdRef.current = -1;
    setHasFirstVideoFrame(false);
    setJpegFrameUrl(null);
    setJpegFrameCount(0);
    setJpegChannelState("");
    setViewerError("");
    setRoomEnded(false);
    setRemoteRecordingActive(false);
    handRaisedRef.current = false;
    setHandRaised(false);
    setHandActionPending(false);
    setRoomEndTitle("投屏已结束");
    setRoomEndMessage("房主已停止共享，当前连接已经关闭。");
    controlChannelRef.current = null;
    const pc = new RTCPeerConnection({ iceServers: [] });
    pcRef.current = pc;

    const videoTransceiver = pc.addTransceiver("video", { direction: "recvonly" });
    const audioTransceiver = pc.addTransceiver("audio", { direction: "sendrecv" });
    audioSenderRef.current = audioTransceiver.sender;
    for (let slot = 1; slot < AUDIO_RELAY_SLOTS; slot += 1) {
      pc.addTransceiver("audio", { direction: "recvonly" });
    }
    try {
      const caps = RTCRtpReceiver.getCapabilities?.("video");
      const codecs = caps?.codecs ?? [];
      const h264 = codecs.filter((codec) => /video\/H264/i.test(codec.mimeType));
      const rtx = codecs.filter((codec) => /video\/rtx/i.test(codec.mimeType));
      if (h264.length && videoTransceiver.setCodecPreferences) {
        videoTransceiver.setCodecPreferences([...h264, ...rtx]);
      }
    } catch {
      // Best-effort browser optimization.
    }

    const pointerChannel = pc.createDataChannel("pointer", {
      ordered: false,
      maxRetransmits: 0,
    });
    pointerChannel.binaryType = "arraybuffer";
    pointerChannelRef.current = pointerChannel;
    pointerChannel.onclose = () => {
      if (pointerChannelRef.current === pointerChannel) pointerChannelRef.current = null;
    };

    const renderJpeg = (data: BlobPart) => {
      const blob = new Blob([data], { type: "image/jpeg" });
      const nextUrl = URL.createObjectURL(blob);
      const previousUrl = jpegFrameUrlRef.current;
      jpegFrameUrlRef.current = nextUrl;
      firstFrameReceivedRef.current = true;
      setJpegFrameUrl(nextUrl);
      setJpegFrameCount((count) => count + 1);
      if (!h264FirstFrameRef.current) {
        setHasFirstVideoFrame(false);
      }
      clearFirstFrameTimer();
      if (previousUrl) {
        URL.revokeObjectURL(previousUrl);
      }
    };

    const handleJpegFrame = async (msg: MessageEvent) => {
      if (h264FirstFrameRef.current) return;
      try {
        const buffer = msg.data instanceof Blob
          ? await msg.data.arrayBuffer()
          : msg.data instanceof ArrayBuffer
            ? msg.data
            : await new Blob([msg.data]).arrayBuffer();
        const view = new DataView(buffer);
        const bytes = new Uint8Array(buffer);
        const isChunkedJpeg =
          bytes.length >= 12 &&
          bytes[0] === 0x4f &&
          bytes[1] === 0x52 &&
          bytes[2] === 0x42 &&
          bytes[3] === 0x4a;

        if (!isChunkedJpeg) {
          renderJpeg(buffer);
          return;
        }

        const frameId = view.getUint32(4, false);
        const total = view.getUint16(8, false);
        const index = view.getUint16(10, false);
        if (total === 0 || index >= total) return;

        let frame = jpegChunksRef.current.get(frameId);
        if (!frame) {
          frame = { total, received: 0, chunks: new Array(total) };
          jpegChunksRef.current.set(frameId, frame);
        }
        if (!frame.chunks[index]) {
          frame.chunks[index] = bytes.slice(12);
          frame.received += 1;
        }

        if (frame.received === frame.total) {
          if (frameId <= lastRenderedFrameIdRef.current) {
            jpegChunksRef.current.delete(frameId);
            return;
          }
          const size = frame.chunks.reduce((sum, chunk) => sum + (chunk?.length ?? 0), 0);
          const jpeg = new Uint8Array(size);
          let offset = 0;
          for (const chunk of frame.chunks) {
            if (!chunk) return;
            jpeg.set(chunk, offset);
            offset += chunk.length;
          }
          jpegChunksRef.current.delete(frameId);
          lastRenderedFrameIdRef.current = frameId;
          renderJpeg(jpeg);
        }

        for (const id of Array.from(jpegChunksRef.current.keys())) {
          if (id < frameId - 1) jpegChunksRef.current.delete(id);
        }
        if (jpegChunksRef.current.size > 2) {
          const oldest = jpegChunksRef.current.keys().next().value;
          if (oldest !== undefined) jpegChunksRef.current.delete(oldest);
        }
      } catch (error) {
        setViewerError(error instanceof Error ? error.message : String(error));
      }
    };

    const videoChannel = pc.createDataChannel("video", {
      ordered: false,
      maxRetransmits: 0,
    });
    videoChannel.binaryType = "arraybuffer";
    videoChannel.onopen = () => {
      setJpegChannelState("视频连接已建立");
      armFirstFrameTimer("视频连接已建立");
    };
    videoChannel.onerror = () => setJpegChannelState("视频连接异常");
    videoChannel.onclose = () => setJpegChannelState("视频连接已关闭");
    videoChannel.onmessage = handleJpegFrame;

    pc.onconnectionstatechange = () => {
      setConnectionState(pc.connectionState);
      if (pc.connectionState === "connected") {
        armFirstFrameTimer("已连接");
      }
    };

    pc.ontrack = (event: RTCTrackEvent) => {
      if (cancelled) return;
      const track = event.track;
      if (track.kind === "audio") {
        attachRemoteAudioTrack(track);
        return;
      }
      if (track?.contentHint !== undefined) {
        try {
          track.contentHint = "detail";
        } catch {
          // Browser may reject; ignore.
        }
      }
      const stream = event.streams[0] ?? new MediaStream([event.track]);
      if (videoRef.current) {
        videoRef.current.srcObject = stream;
        videoRef.current.onloadeddata = () => {
          clearFirstFrameTimer();
          firstFrameReceivedRef.current = true;
          h264FirstFrameRef.current = true;
          setHasFirstVideoFrame(true);
        };
        videoRef.current.onplaying = () => {
          clearFirstFrameTimer();
          firstFrameReceivedRef.current = true;
          h264FirstFrameRef.current = true;
          setHasFirstVideoFrame(true);
        };
        void videoRef.current.play().catch(() => {});
        hasVideoTrackRef.current = true;
        armFirstFrameTimer("高清视频已连接");
      }
    };

    pc.ondatachannel = (event) => {
      const channel = event.channel;
      if (channel.label === "video") {
        channel.binaryType = "arraybuffer";
        channel.onmessage = handleJpegFrame;
      } else if (channel.label === "pointer") {
        channel.binaryType = "arraybuffer";
        channel.onmessage = (messageEvent) => void handleLaserPayload(messageEvent.data);
      } else if (channel.label === "control") {
        controlChannelRef.current = channel;
        channel.onopen = () => {
          setChatError("");
          channel.send(JSON.stringify({
            type: "audio_state",
            enabled: microphoneEnabledRef.current && !hostMutedRef.current,
          }));
          channel.send(JSON.stringify({
            type: "raise_hand",
            raised: handRaisedRef.current,
          }));
        };
        channel.onclose = () => {
          if (controlChannelRef.current === channel) controlChannelRef.current = null;
        };
        channel.onmessage = async (messageEvent) => {
          try {
            const text = typeof messageEvent.data === "string"
              ? messageEvent.data
              : messageEvent.data instanceof Blob
                ? await messageEvent.data.text()
                : new TextDecoder().decode(messageEvent.data);
            const message = JSON.parse(text);
            if (message.type === "chat_message" && message.message) {
              const next = message.message as ChatMessage;
              pushDanmaku(next);
              setChatMessages((messages) => messages.some((item) => item.message_id === next.message_id)
                ? messages
                : [...messages, next].slice(-100));
            } else if (message.type === "chat_error") {
              setChatError(message.message || "消息发送失败");
            } else if (message.type === "set_muted") {
              const muted = Boolean(message.muted);
              hostMutedRef.current = muted;
              setHostMuted(muted);
              if (muted) {
                stopLocalMicrophone(false);
                channel.send(JSON.stringify({ type: "audio_state", enabled: false }));
                setMicrophoneError("主持人已将你禁言");
              } else {
                setMicrophoneError("");
              }
            } else if (message.type === "recording_state") {
              setRemoteRecordingActive(Boolean(message.active));
            } else if (message.type === "share_source_state") {
              setSourceSwitching(message.state === "switching");
            } else if (message.type === "hand_state") {
              const raised = Boolean(message.raised);
              handRaisedRef.current = raised;
              setHandRaised(raised);
              setHandActionPending(false);
            } else if (message.type === "removed_by_host") {
              clearFirstFrameTimer();
              firstFrameReceivedRef.current = false;
              setRoomEndTitle("已被移出会议");
              setRoomEndMessage("主持人已结束你的观看权限，本次会议中无法自动重新加入。");
              setRemoteRecordingActive(false);
              handRaisedRef.current = false;
              setHandRaised(false);
              setHandActionPending(false);
              setRoomEnded(true);
              setConnectionState("closed");
              setViewerError("");
              pc.close();
            } else if (message.type === "room_closed") {
              clearFirstFrameTimer();
              setRoomEndTitle("投屏已结束");
              setRoomEndMessage("房主已停止共享，当前连接已经关闭。");
              setRemoteRecordingActive(false);
              handRaisedRef.current = false;
              setHandRaised(false);
              setHandActionPending(false);
              setRoomEnded(true);
              setConnectionState("closed");
              pc.close();
            }
          } catch {
            // Ignore unknown control payloads.
          }
        };
      }
    };

    pc.onicecandidate = (event) => {
      if (event.candidate) {
        guestSendIceCandidate(
          event.candidate.candidate,
          event.candidate.sdpMid,
          event.candidate.sdpMLineIndex
        );
      }
    };

    const setup = async () => {
      const un1 = await listen<{ sdp: string }>(
        "signaling://sdp-answer",
        async (event) => {
          if (cancelled) return;
          try {
            await pc.setRemoteDescription(
              new RTCSessionDescription(JSON.parse(event.payload.sdp))
            );
          } catch (error) {
            setViewerError(error instanceof Error ? error.message : String(error));
          }
        }
      );

      const un2 = await listen<{
        candidate: string;
        sdpMid: string | null;
        sdpMLineIndex: number | null;
      }>("signaling://ice-candidate", async (event) => {
        if (cancelled) return;
        try {
          await pc.addIceCandidate(
            new RTCIceCandidate({
              candidate: event.payload.candidate,
              sdpMid: event.payload.sdpMid ?? undefined,
              sdpMLineIndex: event.payload.sdpMLineIndex ?? undefined,
            })
          );
        } catch (error) {
          setViewerError(error instanceof Error ? error.message : String(error));
        }
      });

      const un3 = await listen<{ dataUrl: string }>("video-frame", (event) => {
        if (cancelled) return;
        const previousUrl = jpegFrameUrlRef.current;
        jpegFrameUrlRef.current = event.payload.dataUrl;
        firstFrameReceivedRef.current = true;
        setJpegFrameUrl(event.payload.dataUrl);
        setJpegFrameCount((count) => count + 1);
        clearFirstFrameTimer();
        if (previousUrl?.startsWith("blob:")) {
          URL.revokeObjectURL(previousUrl);
        }
      });
      const un4 = await listen("signaling://room-closed", () => {
        if (cancelled) return;
        clearFirstFrameTimer();
        firstFrameReceivedRef.current = false;
        pc.close();
        setRoomEnded(true);
        setRoomEndTitle("投屏已结束");
        setRoomEndMessage("房主已停止共享，当前连接已经关闭。");
        setRemoteRecordingActive(false);
        setConnectionState("closed");
        setViewerError("");
        setJpegChannelState("");
      });

      unlistenRefs.current.push(un1, un2, un3, un4);

      if (cancelled) return;
      try {
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        await guestSendSdpOffer(JSON.stringify(offer));
      } catch (error) {
        setViewerError(error instanceof Error ? error.message : String(error));
      }
    };

    setup();

    return () => {
      cancelled = true;
      stopLocalMicrophone(false);
      pc.close();
      controlChannelRef.current = null;
      pointerChannelRef.current = null;
      pcRef.current = null;
      audioSenderRef.current = null;
      hasVideoTrackRef.current = false;
      firstFrameReceivedRef.current = false;
      h264FirstFrameRef.current = false;
      lastRenderedFrameIdRef.current = -1;
      setHasFirstVideoFrame(false);
      if (jpegFrameUrlRef.current?.startsWith("blob:")) {
        URL.revokeObjectURL(jpegFrameUrlRef.current);
      }
      jpegFrameUrlRef.current = null;
      jpegChunksRef.current.clear();
      clearFirstFrameTimer();
      setJpegFrameUrl(null);
      setRemoteRecordingActive(false);
      handRaisedRef.current = false;
      setHandRaised(false);
      setHandActionPending(false);
      remoteAudioStreamRef.current = new MediaStream();
      if (audioRef.current) audioRef.current.srcObject = null;
      unlistenRefs.current.forEach((un) => un());
      unlistenRefs.current = [];
    };
  }, [attachRemoteAudioTrack, handleLaserPayload, isHost, pushDanmaku, stopLocalMicrophone]);

  const cleanupRecordingMedia = useCallback(async () => {
    for (const source of recordingAudioNodesRef.current.values()) {
      try {
        source.disconnect();
      } catch {
        // The node may already have disconnected when its source track ended.
      }
    }
    recordingAudioNodesRef.current.clear();
    const destination = recordingAudioDestinationRef.current;
    destination?.stream.getAudioTracks().forEach((track) => track.stop());
    recordingAudioDestinationRef.current = null;
    recordingStreamRef.current = null;
    mediaRecorderRef.current = null;
    const context = recordingAudioContextRef.current;
    recordingAudioContextRef.current = null;
    if (context && context.state !== "closed") {
      await context.close().catch(() => {});
    }
    recordingWriteQueueRef.current = Promise.resolve();
  }, []);

  const startHostRecording = useCallback(async () => {
    if (!isHost || recordingPhaseRef.current !== "idle") return;
    const videoTrack = recordingVideoTrackRef.current;
    if (!videoTrack || videoTrack.readyState !== "live") {
      setRecordingError("录制画面还在准备，请稍后再试。");
      return;
    }
    const format = selectRecordingFormat();
    if (!format) {
      setRecordingError("当前系统 WebView 不支持应用内 MP4/WebM 录制，请升级 macOS 后重试。");
      return;
    }

    updateRecordingPhase("starting");
    setRecordingError("");
    setRecordingResult(null);
    setRecordingElapsedSeconds(0);
    recordingWriteErrorRef.current = null;
    recordingStopPromiseRef.current = null;
    recordingStopResolverRef.current = null;
    recordingStopRejecterRef.current = null;

    try {
      const AudioContextConstructor = window.AudioContext;
      const audioContext = new AudioContextConstructor();
      await audioContext.resume();
      const destination = audioContext.createMediaStreamDestination();
      recordingAudioContextRef.current = audioContext;
      recordingAudioDestinationRef.current = destination;
      for (const track of remoteAudioStreamRef.current.getAudioTracks()) {
        connectRecordingAudioTrack(track);
      }
      if (microphoneTrackRef.current) {
        connectRecordingAudioTrack(microphoneTrackRef.current);
      }

      const recordingFps = Math.max(
        1,
        Math.min(
          RECORDING_MAX_FPS,
          streamStats?.target_fps || captureStats?.fps || RECORDING_MAX_FPS,
        ),
      );
      const recordingStream = new MediaStream([
        videoTrack,
        ...destination.stream.getAudioTracks(),
      ]);
      recordingStreamRef.current = recordingStream;
      const targetBitrate = streamStats?.target_bitrate_bps || streamStats?.actual_bitrate_bps || 24_000_000;
      const recordingBitrate = Math.max(
        8_000_000,
        Math.min(18_000_000, Math.round(targetBitrate)),
      );
      console.info(
        `[recording] source track @ ${recordingFps}fps, target bitrate=${recordingBitrate}`,
      );
      const recorder = new MediaRecorder(recordingStream, {
        mimeType: format.mimeType,
        videoBitsPerSecond: recordingBitrate,
        audioBitsPerSecond: 192_000,
      });
      mediaRecorderRef.current = recorder;

      const status = await startRecording(format.mimeType, format.extension);
      recordingBackendActiveRef.current = true;
      setRecordingStartedAt(status.started_at_ms);

      recorder.ondataavailable = (event) => {
        if (!event.data || event.data.size === 0) return;
        const chunk = event.data;
        recordingWriteQueueRef.current = recordingWriteQueueRef.current
          .then(async () => {
            await appendRecordingChunk(await blobToBase64(chunk));
          })
          .catch((error) => {
            const normalized = error instanceof Error ? error : new Error(String(error));
            recordingWriteErrorRef.current = normalized;
            setRecordingError(normalized.message);
            if (recorder.state !== "inactive") {
              updateRecordingPhase("stopping");
              recorder.stop();
            }
          });
      };

      recorder.onerror = (event) => {
        const recorderError = (event as Event & { error?: DOMException }).error;
        const error = new Error(recorderError?.message || "系统录制组件发生错误。");
        recordingWriteErrorRef.current = error;
        setRecordingError(error.message);
        if (recorder.state !== "inactive") {
          updateRecordingPhase("stopping");
          recorder.stop();
        }
      };

      recorder.onstop = () => {
        void (async () => {
          let savedResult: RecordingResult | null = null;
          let finalizationError: unknown = null;
          try {
            await recordingWriteQueueRef.current;
            const result = await finishRecording();
            recordingBackendActiveRef.current = false;
            savedResult = result;
            lastRecordingResultRef.current = result;
            setRecordingResult(result);
            onRecordingSaved?.(result);
            if (recordingWriteErrorRef.current) {
              setRecordingError(`部分录制分片写入失败，已保留可恢复文件：${recordingWriteErrorRef.current.message}`);
            }
          } catch (error) {
            finalizationError = error;
            setRecordingError(error instanceof Error ? error.message : String(error));
            if (recordingBackendActiveRef.current) {
              await abortRecording().catch(() => {});
              recordingBackendActiveRef.current = false;
            }
          } finally {
            await cleanupRecordingMedia();
            setRecordingStartedAt(0);
            updateRecordingPhase("idle");
            const resolveStop = recordingStopResolverRef.current;
            const rejectStop = recordingStopRejecterRef.current;
            recordingStopPromiseRef.current = null;
            recordingStopResolverRef.current = null;
            recordingStopRejecterRef.current = null;
            if (finalizationError) rejectStop?.(finalizationError);
            else resolveStop?.(savedResult);
          }
        })();
      };

      recorder.start(RECORDING_CHUNK_INTERVAL_MS);
      updateRecordingPhase("recording");
    } catch (error) {
      setRecordingError(error instanceof Error ? error.message : String(error));
      if (recordingBackendActiveRef.current) {
        await abortRecording().catch(() => {});
        recordingBackendActiveRef.current = false;
      }
      await cleanupRecordingMedia();
      setRecordingStartedAt(0);
      updateRecordingPhase("idle");
    }
  }, [captureStats, cleanupRecordingMedia, connectRecordingAudioTrack, isHost, onRecordingSaved, streamStats, updateRecordingPhase]);

  const stopHostRecording = useCallback(async (): Promise<RecordingResult | null> => {
    if (!isHost || recordingPhaseRef.current === "idle") return lastRecordingResultRef.current;
    const recorder = mediaRecorderRef.current;
    if (!recorder || recorder.state === "inactive") {
      if (recordingBackendActiveRef.current) {
        try {
          const result = await finishRecording();
          lastRecordingResultRef.current = result;
          setRecordingResult(result);
          onRecordingSaved?.(result);
          return result;
        } finally {
          recordingBackendActiveRef.current = false;
          await cleanupRecordingMedia();
          updateRecordingPhase("idle");
        }
      }
      return lastRecordingResultRef.current;
    }
    if (recordingPhaseRef.current === "stopping") {
      return recordingStopPromiseRef.current ?? lastRecordingResultRef.current;
    }
    updateRecordingPhase("stopping");
    const stopPromise = new Promise<RecordingResult | null>((resolve, reject) => {
      recordingStopResolverRef.current = resolve;
      recordingStopRejecterRef.current = reject;
      try {
        recorder.requestData();
      } catch {
        // requestData is optional; stop still emits the final buffered chunk.
      }
      recorder.stop();
    });
    recordingStopPromiseRef.current = stopPromise;
    return stopPromise;
  }, [cleanupRecordingMedia, isHost, onRecordingSaved, updateRecordingPhase]);

  // ── Disconnect handler ─────────────────────────────────────────────
  const handleDisconnect = useCallback(async () => {
    if (isHost && (recordingPhaseRef.current === "starting" || recordingPhaseRef.current === "stopping")) {
      setRecordingError("录制正在切换状态，请稍后再停止共享。");
      return;
    }
    try {
      if (isHost) {
        if (recordingPhaseRef.current === "recording") {
          await stopHostRecording();
        }
        stopLocalMicrophone(false);
        await stopRoom();
        onStopped?.();
      } else {
        stopLocalMicrophone(false);
        pcRef.current?.close();
        pcRef.current = null;
        await leaveRoom();
        onBack();
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (isHost) setRecordingError(message);
      else setViewerError(message);
    }
  }, [isHost, onBack, onStopped, stopHostRecording, stopLocalMicrophone]);

  useEffect(() => {
    if (!isHost) return;
    let allowClose = false;
    let unlisten: (() => void) | undefined;
    void getCurrentWindow().onCloseRequested(async (event) => {
      if (allowClose || recordingPhaseRef.current === "idle") return;
      event.preventDefault();
      try {
        const deadline = Date.now() + 5000;
        while (recordingPhaseRef.current === "starting" && Date.now() < deadline) {
          await new Promise((resolve) => window.setTimeout(resolve, 50));
        }
        if (recordingPhaseRef.current === "starting") {
          throw new Error("录制画面仍在准备，请稍后再关闭应用。");
        }
        await stopHostRecording();
        stopLocalMicrophone(false);
        await stopRoom();
        allowClose = true;
        await getCurrentWindow().close();
      } catch (error) {
        setRecordingError(error instanceof Error ? error.message : String(error));
      }
    }).then((dispose) => {
      unlisten = dispose;
    });
    return () => unlisten?.();
  }, [isHost, stopHostRecording, stopLocalMicrophone]);

  const handleNavigateBack = useCallback(() => {
    if (isHost) {
      if (recordingPhaseRef.current !== "idle") {
        setRecordingError("请先停止录制并等待文件保存完成，再返回主页。");
        return;
      }
      // Returning to the home page keeps capture and connected viewers alive.
      onBack();
    } else {
      void handleDisconnect();
    }
  }, [handleDisconnect, isHost, onBack]);

  const handleCopyInviteLink = useCallback(async () => {
    if (!room) return;
    const url = `http://${room.host_ip}:${room.viewer_port}/`;
    try {
      await navigator.clipboard.writeText(url);
      setLinkCopied(true);
      window.setTimeout(() => setLinkCopied(false), 1600);
    } catch {
      window.prompt(t("复制以下链接到其他设备浏览器打开："), url);
    }
  }, [room, t]);

  const refreshCaptureSourcePicker = useCallback(async () => {
    if (!isHost) return;
    setSourcePickerLoading(true);
    setSourceSwitchError("");
    try {
      const [sources, activeSource] = await Promise.all([
        listCaptureSources(),
        getActiveCaptureSource(),
      ]);
      setCaptureSources(sources);
      setSelectedCaptureSource((current) => {
        if (current && sources.some((source) => source.id === current.id
          && source.is_display === current.is_display)) return current;
        if (activeSource?.source_id != null) {
          const active = sources.find((source) => source.id === activeSource.source_id
            && source.is_display === activeSource.source_is_display);
          if (active) return active;
        }
        return sources[0] ?? null;
      });
      setSourcePickerLoading(false);
      void hydrateCaptureSourceThumbnails(sources, (source, thumbnailDataUrl) => {
        setCaptureSources((current) => current.map((candidate) => (
          candidate.id === source.id && candidate.is_display === source.is_display
            ? { ...candidate, thumbnail_data_url: thumbnailDataUrl }
            : candidate
        )));
        setSelectedCaptureSource((current) => (
          current?.id === source.id && current.is_display === source.is_display
            ? { ...current, thumbnail_data_url: thumbnailDataUrl }
            : current
        ));
      });
    } catch (error) {
      setCaptureSources([]);
      setSourceSwitchError(error instanceof Error ? error.message : String(error));
    } finally {
      setSourcePickerLoading(false);
    }
  }, [isHost]);

  const openCaptureSourcePicker = useCallback(() => {
    if (!isHost || sourceSwitching) return;
    setSourcePickerOpen(true);
    void refreshCaptureSourcePicker();
  }, [isHost, refreshCaptureSourcePicker, sourceSwitching]);

  const confirmCaptureSourceSwitch = useCallback(async () => {
    if (!isHost || !selectedCaptureSource || sourceSwitching) return;
    setSourceSwitching(true);
    setSourceSwitchError("");
    try {
      await switchCaptureSource(selectedCaptureSource.id, selectedCaptureSource.is_display);
      setSourcePickerOpen(false);
    } catch (error) {
      setSourceSwitchError(error instanceof Error ? error.message : String(error));
    } finally {
      setSourceSwitching(false);
    }
  }, [isHost, selectedCaptureSource, sourceSwitching]);

  const handleRemoveViewer = useCallback(async (viewer: MemberInfo) => {
    if (!isHost || removingMemberId) return;
    setRemovingMemberId(viewer.member_id);
    try {
      await removeViewer(viewer.member_id);
      setViewers((current) => {
        const next = current.filter((member) => member.member_id !== viewer.member_id);
        setPeerCount(next.length);
        return next;
      });
    } catch (error) {
      window.alert(t(error instanceof Error ? error.message : String(error)));
    } finally {
      setRemovingMemberId(null);
    }
  }, [isHost, removingMemberId]);

  const handleSetViewerMuted = useCallback(async (viewer: MemberInfo) => {
    if (!isHost || mutingMemberId) return;
    setMutingMemberId(viewer.member_id);
    try {
      const updated = await setViewerMuted(viewer.member_id, !viewer.host_muted);
      setViewers((current) => sortViewerQueue(current.map((member) =>
        member.member_id === updated.member_id ? updated : member)));
    } catch (error) {
      window.alert(t(error instanceof Error ? error.message : String(error)));
    } finally {
      setMutingMemberId(null);
    }
  }, [isHost, mutingMemberId]);

  const handleLowerViewerHand = useCallback(async (viewer: MemberInfo) => {
    if (!isHost || loweringHandMemberId || !viewer.hand_raised) return;
    setLoweringHandMemberId(viewer.member_id);
    try {
      const updated = await setViewerHandRaised(viewer.member_id, false);
      setViewers((current) => sortViewerQueue(current.map((member) =>
        member.member_id === updated.member_id ? updated : member)));
    } catch (error) {
      window.alert(t(error instanceof Error ? error.message : String(error)));
    } finally {
      setLoweringHandMemberId(null);
    }
  }, [isHost, loweringHandMemberId]);

  const toggleHandRaised = useCallback(() => {
    if (isHost || handActionPending) return;
    const channel = controlChannelRef.current;
    if (!channel || channel.readyState !== "open") {
      setMicrophoneError("控制通道正在连接，请稍后再举手。");
      return;
    }
    const next = !handRaisedRef.current;
    try {
      channel.send(JSON.stringify({ type: "raise_hand", raised: next }));
      handRaisedRef.current = next;
      setHandRaised(next);
      setHandActionPending(true);
      window.setTimeout(() => setHandActionPending(false), 1500);
    } catch (error) {
      setMicrophoneError(error instanceof Error ? error.message : String(error));
    }
  }, [handActionPending, isHost]);

  const handleSendChat = useCallback(async (event: FormEvent) => {
    event.preventDefault();
    const content = chatInput.trim();
    if (!content || chatSending) return;
    setChatError("");
    setChatSending(true);
    try {
      if (isHost) {
        await sendHostChatMessage(content);
      } else {
        const channel = controlChannelRef.current;
        if (!channel || channel.readyState !== "open") {
          throw new Error("聊天通道正在连接，请稍后再试");
        }
        channel.send(JSON.stringify({ type: "chat_send", content }));
      }
      setChatInput("");
      setEmojiPickerOpen(false);
    } catch (error) {
      setChatError(error instanceof Error ? error.message : String(error));
    } finally {
      setChatSending(false);
    }
  }, [chatInput, chatSending, isHost]);

  const insertChatEmoji = useCallback((emoji: string) => {
    const input = chatInputRef.current;
    const start = input?.selectionStart ?? chatInput.length;
    const end = input?.selectionEnd ?? start;
    const next = `${chatInput.slice(0, start)}${emoji}${chatInput.slice(end)}`;
    if (Array.from(next).length > 500) return;
    setChatInput(next);
    setChatError("");
    window.requestAnimationFrame(() => {
      const cursor = start + emoji.length;
      chatInputRef.current?.focus({ preventScroll: true });
      chatInputRef.current?.setSelectionRange(cursor, cursor);
    });
  }, [chatInput]);

  const openChatPanel = useCallback(() => {
    setPanelTab("chat");
    setSidePanelOpen(true);
  }, []);

  // ── Status derivation ──────────────────────────────────────────────
  const isLive = isHost
    ? peerCount > 0
    : hasFirstVideoFrame || jpegFrameCount > 0;
  const viewerResLabel = streamStats
    ? `${streamStats.width}×${streamStats.height}`
    : captureStats
    ? `${captureStats.width}×${captureStats.height}`
    : "—";
  const actualMbps = streamStats ? streamStats.actual_bitrate_bps / 1_000_000 : 0;
  const targetMbps = streamStats ? streamStats.target_bitrate_bps / 1_000_000 : 0;
  const smoothnessLabel = t(!streamStats
    ? "等待统计"
    : streamStats.actual_fps >= streamStats.target_fps * 0.85
      ? "流畅"
      : streamStats.actual_fps >= streamStats.target_fps * 0.6
        ? "一般"
        : "较慢");
  const networkUsageLabel = t(!streamStats
    ? "等待统计"
    : targetMbps <= 0 || actualMbps < targetMbps * 0.4
      ? "较低"
      : actualMbps < targetMbps * 0.8
        ? "适中"
        : "较高");
  const connectionLabel = t(connectionState === "connected"
    ? "已连接"
    : connectionState === "connecting"
      ? "连接中"
      : connectionState === "disconnected"
        ? "正在恢复"
        : connectionState === "failed"
          ? "连接失败"
          : connectionState === "closed"
            ? "已断开"
            : "准备中");
  const attendanceByParticipant = new Map(
    attendance.map((record) => [record.participant_id, record]),
  );
  const totalRejoins = attendance.reduce(
    (sum, record) => sum + Math.max(0, record.join_count - 1),
    0,
  );
  const raisedHandCount = viewers.reduce(
    (count, viewer) => count + (viewer.hand_raised ? 1 : 0),
    0,
  );
  const formatAttendanceTime = (timestamp?: number | null) => {
    if (!timestamp) return "—";
    return new Date(timestamp * 1000).toLocaleTimeString(language, {
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    });
  };
  const formatOnlineDuration = (seconds?: number) => {
    const value = Math.max(0, Math.floor(seconds ?? 0));
    if (value < 60) return t("{seconds}秒", { seconds: value });
    const minutes = Math.floor(value / 60);
    if (minutes < 60) return t("{minutes}分钟", { minutes });
    return t("{hours}小时{minutes}分", { hours: Math.floor(minutes / 60), minutes: minutes % 60 });
  };
  const formatChatTime = (timestamp: number) => new Date(timestamp).toLocaleTimeString(language, {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
  const formatRecordingDuration = (seconds: number) => {
    const minutes = Math.floor(seconds / 60);
    return `${String(minutes).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
  };
  const formatRecordingSize = (bytes: number) => {
    if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
    return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  };

  return (
    <div
      className="flex h-full flex-col overflow-hidden bg-[#0E1014] text-slate-200"
      onPointerDownCapture={() => {
        if (audioRef.current?.srcObject) {
          void audioRef.current.play().catch(() => {});
        }
      }}
    >
      {/* Top bar */}
      <header className="orbshare-meeting-header flex shrink-0 items-center justify-between border-b border-white/5 bg-[#16191F]/90 px-3 py-2.5 backdrop-blur-md">
        <div className="orbshare-meeting-title flex min-w-0 shrink-0 items-center gap-2">
          <button
            onClick={handleNavigateBack}
            className="flex h-7 w-7 items-center justify-center rounded-md text-slate-400 transition hover:bg-white/5 hover:text-white"
            title={isHost ? t("返回主页（会议继续）") : t("断开并返回")}
          >
            <svg viewBox="0 0 24 24" className="h-4 w-4" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="m15 18-6-6 6-6" />
            </svg>
          </button>
          <div className="min-w-0 leading-tight">
            <div className="truncate text-[12px] font-semibold text-white" title={room?.room_name ? t(room.room_name) : t("屏幕共享")}>
              {room?.room_name ? t(room.room_name) : t("屏幕共享")}
            </div>
            <div className="text-[10.5px] text-slate-400">
              {address && port ? `${address}:${port}` : room?.host_ip ?? t("本地")}
            </div>
          </div>
        </div>

        <div className="orbshare-meeting-toolbar flex min-w-0 flex-1 items-center justify-end gap-1.5">
          {isHost && room && (
            <button
              onClick={() => setInviteDialogOpen(true)}
              className="inline-flex items-center gap-1.5 rounded-md border border-blue-400/20 bg-blue-400/10 px-3 py-1 text-[11.5px] font-medium text-blue-300 transition hover:bg-blue-400/20"
            >
              <svg viewBox="0 0 24 24" className="h-3.5 w-3.5" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"><path d="M3 3h6v6H3zM15 3h6v6h-6zM3 15h6v6H3z" /><path d="M15 15h2v2h-2zM19 15h2v6h-2zM15 19h2v2h-2z" /></svg>
              {t("邀请参会")}
            </button>
          )}
          {room?.capabilities.includes("system-audio-active") && (
            <span
              title={t("正在共享当前屏幕的系统声音")}
              className="inline-flex items-center gap-1.5 rounded-md border border-emerald-400/25 bg-emerald-500/10 px-2.5 py-1 text-[11.5px] font-medium text-emerald-300"
            >
              <svg aria-hidden="true" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M11 5 6 9H3v6h3l5 4V5Z" /><path d="M15.5 8.5a5 5 0 0 1 0 7M18 6a8.5 8.5 0 0 1 0 12" /></svg>
              {t("系统声音")}
            </span>
          )}
          {!isHost && remoteRecordingActive && (
            <span className="inline-flex items-center gap-1.5 rounded-md border border-rose-400/30 bg-rose-500/15 px-2.5 py-1 text-[11.5px] font-semibold text-rose-200">
              <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-rose-400 shadow-[0_0_8px_rgba(251,113,133,0.8)]" />
              {t("正在录制")}
            </span>
          )}
          <button
            onClick={() => void toggleLocalMicrophone()}
            disabled={Boolean(isHost && !hostAudioConnected)}
            title={microphoneError ? t(microphoneError) : (hostMuted ? t("主持人已将你禁言") : microphoneEnabled ? t("关闭麦克风") : t("开启麦克风"))}
            className={`inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-[11.5px] font-medium transition disabled:cursor-wait disabled:opacity-50 ${
              hostMuted
                ? "border-rose-400/20 bg-rose-500/10 text-rose-300"
                : microphoneEnabled
                  ? "border-emerald-400/25 bg-emerald-500/12 text-emerald-300"
                  : "border-white/5 bg-white/[0.03] text-slate-300 hover:bg-white/[0.07]"
            }`}
          >
            {hostMuted || !microphoneEnabled ? (
              <svg aria-hidden="true" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.1" strokeLinecap="round" strokeLinejoin="round">
                <path d="M9 9v3a3 3 0 0 0 4.8 2.4M15 9V5a3 3 0 0 0-5.6-1.5M17 16.2A7 7 0 0 1 5 12v-2M19 10v2c0 .7-.1 1.4-.3 2M12 19v3M8 22h8M3 3l18 18" />
              </svg>
            ) : (
              <svg aria-hidden="true" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.1" strokeLinecap="round" strokeLinejoin="round">
                <path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z" /><path d="M19 10v2a7 7 0 0 1-14 0v-2M12 19v3M8 22h8" />
              </svg>
            )}
            {hostMuted ? t("已禁言") : microphoneEnabled ? t("麦克风已开") : t("麦克风已关")}
          </button>
          {!isHost && (
            <button
              onClick={toggleHandRaised}
              disabled={handActionPending || roomEnded}
              title={handRaised ? t("放下手") : t("向主持人举手")}
              aria-pressed={handRaised}
              className={`inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-[11.5px] font-semibold transition disabled:cursor-wait disabled:opacity-55 ${
                handRaised
                  ? "border-amber-300/35 bg-amber-400/15 text-amber-200 shadow-[0_0_14px_rgba(251,191,36,0.10)]"
                  : "border-white/5 bg-white/[0.03] text-slate-300 hover:bg-white/[0.07]"
              }`}
            >
              <svg aria-hidden="true" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round">
                <path d="M7.5 11V5.5a1.5 1.5 0 0 1 3 0V10M10.5 10V4a1.5 1.5 0 0 1 3 0v6M13.5 10V5a1.5 1.5 0 0 1 3 0v6M16.5 11V7.5a1.5 1.5 0 0 1 3 0V14c0 4.4-2.7 7-7 7h-1.2a7 7 0 0 1-5.2-2.3L3.4 15.8a1.6 1.6 0 0 1 2.2-2.3L7.5 15" />
              </svg>
              {handActionPending ? t("处理中") : handRaised ? t("已举手") : t("举手")}
            </button>
          )}
          <button
            onClick={toggleDanmaku}
            title={danmakuEnabled ? t("关闭弹幕") : t("开启弹幕")}
            aria-pressed={danmakuEnabled}
            className={`inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-[11.5px] font-semibold transition ${
              danmakuEnabled
                ? "border-emerald-400/30 bg-emerald-500/15 text-emerald-200 shadow-[0_0_14px_rgba(16,185,129,0.08)]"
                : "border-slate-500/20 bg-slate-500/[0.07] text-slate-500 hover:bg-white/[0.07]"
            }`}
          >
            {danmakuEnabled ? (
              <svg aria-hidden="true" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <rect x="3" y="5" width="18" height="14" rx="3" /><path d="M7 10h4M7 14h7M17 10h.01M17 14h.01" />
              </svg>
            ) : (
              <svg aria-hidden="true" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M7 5h11a3 3 0 0 1 3 3v7c0 .7-.2 1.3-.6 1.8M15 19H6a3 3 0 0 1-3-3V8c0-.8.3-1.5.8-2.1M7 10h2M7 14h6M3 3l18 18" />
              </svg>
            )}
            {danmakuEnabled ? t("弹幕已开") : t("弹幕已关")}
          </button>
          <button
            onClick={() => {
              setLaserEnabled((enabled) => {
                const next = !enabled;
                if (!next) setLaserPoint(null);
                return next;
              });
            }}
            title={laserEnabled ? t("关闭激光笔") : t("开启激光笔")}
            aria-pressed={laserEnabled}
            className={`inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-[11.5px] font-semibold transition ${
              laserEnabled
                ? "border-rose-400/35 bg-rose-500/15 text-rose-200 shadow-[0_0_14px_rgba(251,113,133,0.10)]"
                : "border-white/5 bg-white/[0.03] text-slate-300 hover:bg-white/[0.07]"
            }`}
          >
            <svg aria-hidden="true" viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="7" /><circle cx="12" cy="12" r="2" /><path d="M12 2v3M12 19v3M2 12h3M19 12h3" />
            </svg>
            {laserEnabled ? t("激光笔已开") : t("激光笔")}
          </button>
          <button
            onClick={openChatPanel}
            className={`inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-[11.5px] font-medium transition ${
              sidePanelOpen && panelTab === "chat"
                ? "border-blue-400/30 bg-blue-500/15 text-blue-200"
                : "border-white/5 bg-white/[0.03] text-slate-300 hover:bg-white/[0.07]"
            }`}
          >
            <svg aria-hidden="true" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinejoin="round"><path d="M21 15a4 4 0 0 1-4 4H8l-5 3V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4Z" /></svg>
            {t("聊天")}{chatMessages.length > 0 ? ` ${chatMessages.length}` : ""}
          </button>
          {isHost ? (
            <button
              onClick={() => {
                setPanelTab("participants");
                setSidePanelOpen(true);
              }}
              title={t("展开成员列表")}
              className={`inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-[11.5px] font-medium transition ${
                raisedHandCount > 0
                  ? "border-amber-300/35 bg-amber-400/15 text-amber-200 shadow-[0_0_14px_rgba(251,191,36,0.10)]"
                  : sidePanelOpen && panelTab === "participants"
                  ? "border-emerald-400/30 bg-emerald-500/15 text-emerald-200"
                  : "border-emerald-400/20 bg-emerald-500/10 text-emerald-300 hover:bg-emerald-500/15"
              }`}
            >
              {raisedHandCount > 0 ? (
                <svg aria-hidden="true" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round"><path d="M7.5 11V5.5a1.5 1.5 0 0 1 3 0V10M10.5 10V4a1.5 1.5 0 0 1 3 0v6M13.5 10V5a1.5 1.5 0 0 1 3 0v6M16.5 11V7.5a1.5 1.5 0 0 1 3 0V14c0 4.4-2.7 7-7 7h-1.2a7 7 0 0 1-5.2-2.3L3.4 15.8a1.6 1.6 0 0 1 2.2-2.3L7.5 15" /></svg>
              ) : (
                <span className="h-1.5 w-1.5 rounded-full bg-emerald-400 shadow-[0_0_8px_rgba(52,211,153,0.6)]" />
              )}
              {raisedHandCount > 0
                ? t("{count} 人举手 · {total} 人", { count: raisedHandCount, total: peerCount })
                : peerCount > 0 ? t("{count} 位观看者", { count: peerCount }) : t("等待接入")}
            </button>
          ) : (
            <span
              className={`inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-[11.5px] font-medium ${
                viewerError
                  ? "border-rose-400/20 bg-rose-500/10 text-rose-300"
                  : isLive
                    ? "border-blue-400/20 bg-blue-500/10 text-blue-300"
                    : connectionState === "connected"
                      ? "border-emerald-400/20 bg-emerald-500/10 text-emerald-300"
                      : "border-white/5 bg-white/5 text-slate-400"
              }`}
            >
              <span className={`h-1.5 w-1.5 rounded-full ${
                viewerError ? "bg-rose-400"
                  : isLive ? "bg-blue-400 shadow-[0_0_8px_rgba(96,165,250,0.6)] animate-pulse"
                  : connectionState === "connected" ? "bg-emerald-400"
                  : "bg-slate-500"
              }`} />
              {viewerError
                ? t(viewerError)
                : isLive
                  ? t("正在观看 · 高清画面")
                  : roomEnded
                    ? t("已结束")
                    : connectionState === "connected"
                      ? t(jpegChannelState || "等待视频帧")
                      : t("正在连接…")}
            </span>
          )}

          {isHost && (
            <button
              onClick={openCaptureSourcePicker}
              disabled={sourceSwitching || recordingPhase === "starting" || recordingPhase === "stopping"}
              title={t("切换桌面或应用窗口，会议和观看者保持连接")}
              className="inline-flex items-center gap-1.5 rounded-md border border-blue-400/25 bg-blue-500/10 px-2.5 py-1 text-[11.5px] font-semibold text-blue-200 transition hover:bg-blue-500/20 disabled:cursor-wait disabled:opacity-50"
            >
              <svg aria-hidden="true" viewBox="0 0 24 24" className="h-3.5 w-3.5" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><rect x="3" y="4" width="14" height="11" rx="2" /><path d="M7 19h10M10 15v4M18 8h3v10a2 2 0 0 1-2 2M21 8l-2-2M21 8l-2 2" /></svg>
              {sourceSwitching ? t("正在切换") : t("切换共享")}
            </button>
          )}

          {isHost && (
            <button
              onClick={() => void (recordingPhase === "recording"
                ? stopHostRecording().catch(() => {})
                : startHostRecording())}
              disabled={recordingPhase === "starting" || recordingPhase === "stopping" || (recordingPhase === "idle" && !recordingVideoReady)}
              title={!recordingVideoReady ? t("正在准备录制画面") : recordingPhase === "recording" ? t("停止并保存录制") : t("开始本地录制")}
              className={`inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-[11.5px] font-semibold transition disabled:cursor-wait disabled:opacity-50 ${
                recordingPhase === "recording" || recordingPhase === "stopping"
                  ? "border-rose-400/35 bg-rose-500/15 text-rose-200"
                  : "border-white/10 bg-white/[0.04] text-slate-200 hover:bg-white/[0.08]"
              }`}
            >
              {recordingPhase === "recording" ? (
                <span className="h-2 w-2 animate-pulse rounded-sm bg-rose-400 shadow-[0_0_8px_rgba(251,113,133,0.8)]" />
              ) : (
                <span className="h-2 w-2 rounded-full border border-current" />
              )}
              {recordingPhase === "starting"
                ? t("准备录制")
                : recordingPhase === "stopping"
                  ? t("正在保存")
                  : recordingPhase === "recording"
                    ? t("录制 {duration}", { duration: formatRecordingDuration(recordingElapsedSeconds) })
                    : t("录制")}
            </button>
          )}

          {(captureStats || streamStats) && (
            <span className="inline-flex items-center gap-1.5 rounded-md border border-white/5 bg-white/[0.03] px-2.5 py-1 font-mono text-[11px] text-slate-300">
              <span className="text-slate-500">{t("画面")}</span>
              <span className="text-white">{viewerResLabel}</span>
            </span>
          )}

          <button
            onClick={handleDisconnect}
            disabled={Boolean(isHost && (recordingPhase === "starting" || recordingPhase === "stopping"))}
            className="rounded-md border border-rose-500/20 bg-rose-500/10 px-3 py-1 text-[11.5px] font-semibold text-rose-300 transition hover:bg-rose-500/20 disabled:cursor-wait disabled:opacity-50"
          >
            {isHost ? t("停止共享") : t("断开")}
          </button>
          <LanguageSwitch compact />
        </div>
      </header>

      {/* Video area — full bleed black, video centered */}
      <div className="relative flex min-h-0 flex-1 gap-3 overflow-hidden p-3">
        <div
          ref={stageRef}
          onPointerDown={handleStageLaserPointer}
          onPointerMove={handleStageLaserPointer}
          className={`relative flex min-w-0 flex-1 items-center justify-center overflow-hidden rounded-md border border-white/5 bg-black ${laserEnabled ? "cursor-crosshair" : ""}`}
          style={{ touchAction: laserEnabled ? "none" : "auto" }}
        >
          <audio ref={audioRef} autoPlay playsInline className="hidden" />
          <video
            ref={videoRef}
            autoPlay
            playsInline
            muted
            className={`max-h-full max-w-full ${
              hasFirstVideoFrame ? "block" : "hidden"
            }`}
            style={{ objectFit: "contain" }}
          />
          {jpegFrameUrl && !hasFirstVideoFrame && (
            <img
              ref={jpegImageRef}
              src={jpegFrameUrl}
              alt=""
              className="max-h-full max-w-full"
              style={{ objectFit: "contain" }}
              draggable={false}
            />
          )}
          {danmakuEnabled && danmakuItems.length > 0 && (
            <div className="pointer-events-none absolute inset-0 z-[12] overflow-hidden" aria-hidden="true">
              {danmakuItems.map((item) => (
                <div
                  key={item.id}
                  className={`orbshare-danmaku-item flex items-center gap-2 rounded-full border px-3 py-1.5 text-[12px] font-medium shadow-lg backdrop-blur-sm ${
                    item.senderRole === "Host"
                      ? "border-blue-300/30 bg-blue-500/25 text-blue-50"
                      : "border-white/20 bg-black/45 text-white"
                  }`}
                  style={{
                    top: `${9 + item.lane * 10.5}%`,
                    animationDuration: `${item.durationSeconds}s`,
                  }}
                >
                  <span className={item.senderRole === "Host" ? "text-blue-200" : "text-cyan-200"}>
                    {item.senderName}
                  </span>
                  <span>{item.content}</span>
                </div>
              ))}
            </div>
          )}
          {laserPoint && (
            <div
              className="pointer-events-none absolute z-[16] -translate-x-1/2 -translate-y-1/2"
              style={{ left: `${laserPoint.leftPercent}%`, top: `${laserPoint.topPercent}%` }}
              title={laserPoint.senderId}
              aria-hidden="true"
            >
              <span className="absolute left-1/2 top-1/2 h-8 w-8 -translate-x-1/2 -translate-y-1/2 animate-ping rounded-full border border-rose-300/45 bg-rose-400/10" />
              <span className="relative block h-3.5 w-3.5 rounded-full border-2 border-white bg-rose-500 shadow-[0_0_18px_5px_rgba(244,63,94,0.72)]" />
            </div>
          )}
          {sourceSwitching && (
            <div className="absolute inset-0 z-[18] flex items-center justify-center bg-black/45 backdrop-blur-[1px]">
              <div className="flex items-center gap-2 rounded-xl border border-blue-300/20 bg-[#111722]/95 px-4 py-3 text-[11.5px] font-medium text-blue-100 shadow-2xl">
                <span className="h-4 w-4 animate-spin rounded-full border-2 border-blue-300/25 border-t-blue-300" />
                {t("正在切换共享内容…")}
              </div>
            </div>
          )}
          {microphoneError && (
            <div className="absolute bottom-3 left-3 z-20 max-w-sm rounded-lg border border-amber-400/20 bg-[#17140d]/95 px-3 py-2 text-[10.5px] text-amber-200 shadow-xl">
              {t(microphoneError)}
            </div>
          )}
          {isHost && (recordingError || recordingResult) && (
            <div className={`absolute left-3 top-3 z-20 max-w-[520px] rounded-lg border px-3 py-2 text-[10.5px] shadow-xl backdrop-blur-md ${
              recordingError
                ? "border-rose-400/25 bg-[#241217]/95 text-rose-200"
                : "border-emerald-400/20 bg-[#0d1c18]/95 text-emerald-100"
            }`}>
              {recordingError ? (
                <div className="flex items-start gap-2">
                  <span className="mt-0.5 text-rose-400">●</span>
                  <span>{t(recordingError)}</span>
                  <button onClick={() => setRecordingError("")} className="ml-2 text-rose-300/70 hover:text-white">×</button>
                </div>
              ) : recordingResult ? (
                <div className="flex items-start gap-2">
                  <span className="mt-0.5 text-emerald-400">✓</span>
                  <div className="min-w-0">
                    <div className="font-semibold">{t("录制已保存 · {size}", { size: formatRecordingSize(recordingResult.size_bytes) })}</div>
                    <div className="mt-1 truncate font-mono text-[9.5px] text-emerald-200/65" title={recordingResult.path}>{recordingResult.path}</div>
                    <button
                      type="button"
                      onClick={() => void revealRecording(recordingResult.path).catch((error) => {
                        setRecordingError(error instanceof Error ? error.message : String(error));
                      })}
                      className="mt-1.5 rounded-md border border-emerald-300/20 bg-emerald-300/10 px-2 py-1 text-[10px] font-medium text-emerald-100 transition hover:bg-emerald-300/20"
                    >
                      {t("打开所在文件夹")}
                    </button>
                  </div>
                  <button onClick={() => setRecordingResult(null)} className="ml-2 text-emerald-200/60 hover:text-white">×</button>
                </div>
              ) : null}
            </div>
          )}
          {isHost && peerCount === 0 && !jpegFrameUrl && (
            <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 bg-[#0E1014]/85">
              <div className="flex h-12 w-12 items-center justify-center rounded-xl bg-gradient-to-br from-blue-500 to-cyan-500 text-white shadow-lg shadow-blue-500/30">
                <svg viewBox="0 0 24 24" className="h-6 w-6" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                  <rect x="2" y="4" width="20" height="13" rx="2" />
                  <path d="M8 21h8M12 17v4" />
                </svg>
              </div>
              <div className="text-[14px] font-semibold text-white">{t("正在共享屏幕")}</div>
              <div className="text-[11.5px] text-slate-400">
                {t("等待局域网设备接入…")}
              </div>
              {room && (
                <div className="mt-2 flex flex-col items-center gap-2">
                  <code className="rounded-md border border-white/10 bg-white/[0.04] px-2.5 py-1.5 font-mono text-[11.5px] text-blue-300">
                    http://{room.host_ip}:{room.viewer_port}
                  </code>
                  <div className="flex items-center gap-2">
                    <button
                      onClick={handleCopyInviteLink}
                      className="rounded-md border border-white/10 bg-white/[0.06] px-3 py-1.5 text-[11.5px] font-medium text-slate-200 transition hover:bg-white/[0.12]"
                    >
                      {linkCopied ? t("已复制 ✓") : t("复制链接")}
                    </button>
                    <button
                      onClick={() => setInviteDialogOpen(true)}
                      className="rounded-md border border-blue-400/20 bg-blue-400/10 px-3 py-1.5 text-[11.5px] font-medium text-blue-300 transition hover:bg-blue-400/20"
                    >
                      {t("显示二维码")}
                    </button>
                  </div>
                  <div className="text-[10.5px] text-slate-500">
                    {t("局域网内任意设备浏览器直接打开即可观看")}
                  </div>
                </div>
              )}
            </div>
          )}
          {!isHost && !isLive && !roomEnded && (
            <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 bg-[#0E1014]/80">
              <div className="h-9 w-9 animate-spin rounded-full border-2 border-blue-400/30 border-t-blue-400" />
              <div className="text-[12.5px] text-slate-300">
                {viewerError ? t(viewerError) : t("正在等待视频流…")}
              </div>
            </div>
          )}
          {!isHost && roomEnded && (
            <div className="absolute inset-0 flex items-center justify-center bg-[#0E1014]/95">
              <div className="w-full max-w-sm rounded-2xl border border-white/10 bg-[#16191F] p-6 text-center shadow-2xl">
                <div className="mx-auto mb-4 flex h-10 w-10 items-center justify-center rounded-xl bg-white/5 text-slate-300">
                  <svg viewBox="0 0 24 24" className="h-5 w-5" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M18 6 6 18M6 6l12 12" />
                  </svg>
                </div>
                <div className="text-[14px] font-semibold text-white">{t(roomEndTitle)}</div>
                <p className="mt-2 text-[12px] text-slate-400">
                  {t(roomEndMessage)}
                </p>
                <button
                  onClick={handleDisconnect}
                  className="mt-4 w-full rounded-md bg-gradient-to-r from-blue-500 to-cyan-500 px-4 py-2 text-[12.5px] font-semibold text-white shadow-sm transition hover:from-blue-600 hover:to-cyan-600"
                >
                  {t("返回")}
                </button>
              </div>
            </div>
          )}
          <button
            type="button"
            onClick={() => setSidePanelOpen((open) => !open)}
            title={sidePanelOpen ? t("收起成员与聊天") : t("展开成员与聊天")}
            aria-label={sidePanelOpen ? t("收起成员与聊天") : t("展开成员与聊天")}
            aria-expanded={sidePanelOpen}
            className="absolute right-0 top-1/2 z-30 flex h-16 w-7 -translate-y-1/2 items-center justify-center rounded-l-xl border border-r-0 border-white/10 bg-[#1B2029]/95 text-slate-300 shadow-xl backdrop-blur-md transition hover:w-8 hover:bg-[#242B37] hover:text-white"
          >
            <svg viewBox="0 0 24 24" className="h-4 w-4" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
              <path d={sidePanelOpen ? "m15 18-6-6 6-6" : "m9 18 6-6-6-6"} />
            </svg>
          </button>
        </div>

        {sidePanelOpen && (
          <aside className="flex w-[320px] shrink-0 flex-col overflow-hidden rounded-xl border border-white/[0.08] bg-gradient-to-b from-[#171C25] to-[#11151C] shadow-[-18px_0_48px_rgba(0,0,0,0.22)]">
            <div className="flex items-center justify-between border-b border-white/[0.06] px-3 py-2.5">
              {isHost ? (
                <div className="flex rounded-lg bg-black/20 p-0.5">
                  <button
                    onClick={() => setPanelTab("participants")}
                    className={`rounded-md px-3 py-1.5 text-[10.5px] font-medium ${panelTab === "participants" ? "bg-white/[0.08] text-white" : "text-slate-500"}`}
                  >
                    {t("成员 {count}", { count: viewers.length })}
                  </button>
                  <button
                    onClick={() => setPanelTab("chat")}
                    className={`rounded-md px-3 py-1.5 text-[10.5px] font-medium ${panelTab === "chat" ? "bg-white/[0.08] text-white" : "text-slate-500"}`}
                  >
                    {t("聊天 {count}", { count: chatMessages.length })}
                  </button>
                </div>
              ) : (
                <div>
                  <div className="text-[12px] font-semibold text-white">{t("会内聊天")}</div>
                  <div className="mt-0.5 text-[9.5px] text-slate-500">{t("仅当前局域网会议可见")}</div>
                </div>
              )}
              <button
                onClick={() => setSidePanelOpen(false)}
                title={t("收起面板")}
                aria-label={t("收起面板")}
                className="flex h-7 w-7 items-center justify-center rounded-md bg-white/[0.04] text-slate-400 transition hover:bg-white/[0.08] hover:text-white"
              >
                ×
              </button>
            </div>

            {isHost && panelTab === "participants" ? (
              <>
                <div className="border-b border-white/[0.04] px-3 py-2 text-[9.5px] text-slate-500">
                  {t("本场 {count} 人 · 重新加入 {rejoins} 次", { count: attendance.length, rejoins: totalRejoins })}
                  {raisedHandCount > 0 && <span className="ml-1 text-amber-300">· {t("举手 {count} 人", { count: raisedHandCount })}</span>}
                </div>
                <div className="min-h-0 flex-1 overflow-y-auto p-2.5 [scrollbar-width:none]">
                  {viewers.length === 0 ? (
                    <div className="flex h-full flex-col items-center justify-center text-center">
                      <div className="text-[11px] font-medium text-slate-300">{t("暂无观看者")}</div>
                      <div className="mt-1 text-[9.5px] leading-4 text-slate-500">{t("设备加入后会显示名称和接入方式")}</div>
                    </div>
                  ) : (
                    <div className="space-y-1.5">
                      {viewers.map((viewer) => {
                        const record = attendanceByParticipant.get(viewer.participant_id);
                        return (
                          <div key={viewer.member_id} className={`group flex items-center gap-2 rounded-lg border px-2.5 py-2 ${viewer.hand_raised ? "border-amber-300/25 bg-amber-400/[0.07]" : "border-white/[0.05] bg-white/[0.025]"}`}>
                            <span className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-[11px] font-semibold ${viewer.hand_raised ? "bg-amber-400/15 text-amber-200" : "bg-blue-500/10 text-blue-200"}`}>
                              {viewer.name.slice(0, 1).toUpperCase()}
                            </span>
                            <span className="min-w-0 flex-1">
                              <span className="block truncate text-[10.5px] font-medium text-slate-200">{viewer.name}</span>
                              <span className="mt-0.5 block truncate text-[9px] text-slate-500">
                                {t("{platform} · {time} 加入", { platform: viewer.platform, time: formatAttendanceTime(record?.first_joined_at) })}
                              </span>
                              <span className="mt-0.5 block truncate text-[8.5px] text-slate-600">
                                {t("在线 {duration}", { duration: formatOnlineDuration(record?.total_online_seconds) })}
                                {record && record.join_count > 1 ? ` · ${t("重进 {count} 次", { count: record.join_count - 1 })}` : ""}
                              </span>
                              <span className={`mt-0.5 block truncate text-[8.5px] ${viewer.host_muted ? "text-rose-300" : viewer.microphone_enabled ? "text-emerald-300" : "text-slate-600"}`}>
                                {viewer.host_muted ? t("主持人已禁言") : viewer.microphone_enabled ? t("麦克风已开启") : t("麦克风关闭")}
                              </span>
                              {viewer.hand_raised && (
                                <span className="mt-0.5 block truncate text-[8.5px] font-medium text-amber-300">
                                  {viewer.hand_raised_at ? t("{time} 举手", { time: formatChatTime(viewer.hand_raised_at) }) : t("已举手")}
                                </span>
                              )}
                            </span>
                            <span className="flex shrink-0 flex-col gap-1">
                              {viewer.hand_raised && (
                                <button
                                  onClick={() => void handleLowerViewerHand(viewer)}
                                  disabled={loweringHandMemberId === viewer.member_id}
                                  className="rounded-md border border-amber-300/20 bg-amber-400/10 px-2 py-1 text-[9px] text-amber-200 transition hover:bg-amber-400/20 disabled:cursor-wait"
                                >
                                  {loweringHandMemberId === viewer.member_id ? t("处理中") : t("放下")}
                                </button>
                              )}
                              <button
                                onClick={() => void handleSetViewerMuted(viewer)}
                                disabled={mutingMemberId === viewer.member_id}
                                className={`rounded-md border px-2 py-1 text-[9px] transition disabled:cursor-wait ${viewer.host_muted ? "border-emerald-400/15 bg-emerald-400/5 text-emerald-300 hover:bg-emerald-400/15" : "border-amber-400/15 bg-amber-400/5 text-amber-300 hover:bg-amber-400/15"}`}
                              >
                                {mutingMemberId === viewer.member_id ? t("处理中") : viewer.host_muted ? t("允许发言") : t("禁言")}
                              </button>
                              <button
                                onClick={() => void handleRemoveViewer(viewer)}
                                disabled={removingMemberId === viewer.member_id}
                                className="rounded-md border border-rose-400/15 bg-rose-400/5 px-2 py-1 text-[9px] text-rose-300 opacity-70 transition hover:bg-rose-400/15 hover:opacity-100 disabled:cursor-wait"
                              >
                                {removingMemberId === viewer.member_id ? t("移除中") : t("移除")}
                              </button>
                            </span>
                          </div>
                        );
                      })}
                    </div>
                  )}
                </div>
                <div className="border-t border-white/[0.06] p-3">
                  <div className="mb-2 text-[9.5px] font-medium text-slate-400">{t("实时共享状态")}</div>
                  <div className="grid grid-cols-2 gap-2">
                    <div className="rounded-lg bg-black/20 px-2.5 py-2">
                      <div className="text-[8.5px] text-slate-500">{t("画面流畅度")}</div>
                      <div className="mt-1 text-[11px] font-medium text-white">{smoothnessLabel}</div>
                    </div>
                    <div className="rounded-lg bg-black/20 px-2.5 py-2">
                      <div className="text-[8.5px] text-slate-500">{t("当前网络占用")}</div>
                      <div className="mt-1 text-[11px] font-medium text-white">{networkUsageLabel}</div>
                    </div>
                  </div>
                  <div className="mt-2 text-[9px] text-slate-500">{t("画质会根据当前画面和网络情况自动调整")}</div>
                </div>
              </>
            ) : (
              <>
                <div
                  ref={chatScrollRef}
                  className="min-h-0 flex-1 overflow-y-auto bg-[radial-gradient(circle_at_top,rgba(59,130,246,0.055),transparent_38%)] p-3.5 [scrollbar-width:none]"
                >
                  {chatMessages.length === 0 ? (
                    <div className="flex h-full flex-col items-center justify-center px-5 text-center">
                      <div className="flex h-11 w-11 items-center justify-center rounded-2xl border border-blue-400/10 bg-blue-500/[0.07] text-blue-300">
                        <svg aria-hidden="true" viewBox="0 0 24 24" className="h-5 w-5" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                          <path d="M21 15a4 4 0 0 1-4 4H8l-5 3V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4Z" /><path d="M8 9h8M8 13h5" />
                        </svg>
                      </div>
                      <div className="mt-3 text-[11.5px] font-medium text-slate-300">{t("开始会内交流")}</div>
                      <div className="mt-1 text-[9.5px] leading-4 text-slate-500">{t("消息仅在本次局域网会议中可见")}<br />{t("支持文字和常用表情")}</div>
                    </div>
                  ) : (
                    <div className="space-y-3.5">
                      {chatMessages.map((message) => {
                        const isOwnMessage = Boolean(isHost && message.sender_role === "Host");
                        return (
                          <div key={message.message_id} className={`flex items-start gap-2 ${isOwnMessage ? "flex-row-reverse" : ""}`}>
                            <div className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-xl border text-[10px] font-semibold shadow-sm ${message.sender_role === "Host" ? "border-blue-300/15 bg-blue-500/15 text-blue-200" : "border-white/[0.07] bg-white/[0.055] text-slate-300"}`}>
                              {message.sender_role === "Host" ? t("主") : (message.sender_name || t("访")).slice(0, 1).toUpperCase()}
                            </div>
                            <div className={`flex min-w-0 max-w-[82%] flex-col ${isOwnMessage ? "items-end" : "items-start"}`}>
                              <div className={`mb-1 flex items-center gap-1.5 text-[8.5px] text-slate-500 ${isOwnMessage ? "flex-row-reverse" : ""}`}>
                                <span className={message.sender_role === "Host" ? "font-medium text-blue-300" : "text-slate-400"}>{message.sender_name}</span>
                                {message.sender_role === "Host" && <span className="rounded bg-blue-500/10 px-1 py-0.5 text-[7.5px] text-blue-300">{t("主持人")}</span>}
                                <span>{formatChatTime(message.sent_at)}</span>
                              </div>
                              <div className={`whitespace-pre-wrap break-words border px-3 py-2 text-[11px] leading-[1.55] shadow-sm ${isOwnMessage ? "rounded-l-xl rounded-br-sm border-blue-300/15 bg-gradient-to-br from-blue-500/30 to-cyan-500/20 text-blue-50" : "rounded-r-xl rounded-bl-sm border-white/[0.06] bg-white/[0.06] text-slate-200"}`}>
                                {message.content}
                              </div>
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  )}
                </div>
                <form onSubmit={handleSendChat} className="relative border-t border-white/[0.07] bg-[#12161E]/95 p-3">
                  {emojiPickerOpen && (
                    <div className="absolute bottom-[105px] left-3 right-3 z-20 overflow-hidden rounded-xl border border-white/[0.1] bg-[#1B212C]/98 p-2.5 shadow-[0_18px_50px_rgba(0,0,0,0.55)] backdrop-blur-xl">
                      <div className="mb-2 flex items-center justify-between px-1">
                        <span className="text-[9.5px] font-medium text-slate-300">{t("常用表情")}</span>
                        <button type="button" onClick={() => setEmojiPickerOpen(false)} className="text-[9px] text-slate-500 transition hover:text-white">{t("收起")}</button>
                      </div>
                      <div className="grid grid-cols-8 gap-1">
                        {COMMON_CHAT_EMOJIS.map((emoji) => (
                          <button
                            key={emoji}
                            type="button"
                            onClick={() => insertChatEmoji(emoji)}
                            aria-label={t("插入表情 {emoji}", { emoji })}
                            className="flex aspect-square items-center justify-center rounded-lg text-[17px] transition hover:bg-white/[0.09] active:scale-90"
                          >
                            {emoji}
                          </button>
                        ))}
                      </div>
                    </div>
                  )}
                  <div className="rounded-xl border border-white/[0.09] bg-black/20 p-2 transition focus-within:border-blue-400/35 focus-within:bg-black/25">
                    <textarea
                      ref={chatInputRef}
                      value={chatInput}
                      onChange={(event) => setChatInput(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter" && !event.shiftKey) {
                          event.preventDefault();
                          event.currentTarget.form?.requestSubmit();
                        }
                      }}
                      maxLength={500}
                      rows={2}
                      placeholder={t("输入消息，Enter 发送…")}
                      className="block max-h-24 min-h-[42px] w-full resize-none border-0 bg-transparent px-1 text-[11px] leading-5 text-white outline-none placeholder:text-slate-600"
                    />
                    <div className="mt-1 flex items-center justify-between">
                      <div className="flex items-center gap-1.5">
                        <button
                          type="button"
                          onClick={() => setEmojiPickerOpen((open) => !open)}
                          title={t("选择常用表情")}
                          aria-label={t("选择常用表情")}
                          aria-expanded={emojiPickerOpen}
                          className={`flex h-7 w-7 items-center justify-center rounded-lg transition ${emojiPickerOpen ? "bg-amber-400/15 text-amber-200" : "text-slate-500 hover:bg-white/[0.07] hover:text-slate-200"}`}
                        >
                          <svg aria-hidden="true" viewBox="0 0 24 24" className="h-4 w-4" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round">
                            <circle cx="12" cy="12" r="9" /><path d="M8.5 10h.01M15.5 10h.01M8.5 14.5a5 5 0 0 0 7 0" />
                          </svg>
                        </button>
                        <span className="text-[8.5px] text-slate-600">{t("Shift + Enter 换行")}</span>
                      </div>
                      <div className="flex items-center gap-2">
                        <span className="font-mono text-[8px] text-slate-600">{Array.from(chatInput).length}/500</span>
                        <button
                          type="submit"
                          disabled={!chatInput.trim() || chatSending}
                          className="inline-flex h-7 items-center gap-1.5 rounded-lg bg-gradient-to-r from-blue-600 to-cyan-500 px-3 text-[9.5px] font-semibold text-white shadow-sm transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-35"
                        >
                          {chatSending ? t("发送中") : t("发送")}
                          <svg aria-hidden="true" viewBox="0 0 24 24" className="h-3 w-3" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m22 2-7 20-4-9-9-4Z" /><path d="M22 2 11 13" /></svg>
                        </button>
                      </div>
                    </div>
                  </div>
                  <div className="min-h-4 px-1 pt-1 text-[9px] text-rose-300">{t(chatError)}</div>
                </form>
              </>
            )}
          </aside>
        )}
      </div>

      {/* Bottom bar */}
      <footer className="flex shrink-0 items-center justify-between border-t border-white/5 bg-[#16191F]/90 px-5 py-2 text-[11px] text-slate-500 backdrop-blur-md">
        <span>OrbShare · {t("局域网安全直连")}</span>
        <span>
          {isHost ? t("主持人") : t("观看者")} · {connectionLabel}
        </span>
      </footer>
      <SourcePicker
        open={Boolean(isHost && sourcePickerOpen)}
        sources={captureSources}
        selected={selectedCaptureSource}
        loading={sourcePickerLoading}
        switching={sourceSwitching}
        title={t("切换共享内容")}
        confirmLabel={t("切换共享")}
        error={sourceSwitchError}
        onSelect={(source) => {
          setSelectedCaptureSource(source);
          setSourceSwitchError("");
        }}
        onRefresh={() => void refreshCaptureSourcePicker()}
        onCancel={() => {
          if (!sourceSwitching) setSourcePickerOpen(false);
        }}
        onConfirm={() => void confirmCaptureSourceSwitch()}
      />
      {isHost && room && (
        <MeetingInviteDialog
          open={inviteDialogOpen}
          room={room}
          password={meetingPassword}
          onClose={() => setInviteDialogOpen(false)}
        />
      )}
    </div>
  );
}
