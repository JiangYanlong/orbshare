import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  checkScreenCapturePermission,
  createRoom,
  joinRoom,
  listCaptureSources,
  openScreenRecordingSettings,
  scanRooms,
} from "../lib/tauri-commands";
import { hydrateCaptureSourceThumbnails } from "../lib/capture-source-thumbnails";
import type { CaptureSource, RoomInfo } from "../lib/types";
import SourcePicker from "../components/SourcePicker";
import { LanguageSwitch, useLanguage } from "../i18n";

interface HomeProps {
  activeRoom: RoomInfo | null;
  onResumeRoom: () => void;
  onHost: (room: RoomInfo) => void;
  onJoin: (address: string, port: number) => void;
}

type Mode = "host" | "join";

const OPEN_SOURCE_MAX_GUESTS = 20;

const qualityOptions = [
  { value: "standard", label: "流畅", meta: "网络较慢时使用", network: "节省网络" },
  { value: "high", label: "高清", meta: "日常演示推荐", network: "清晰流畅", recommended: true },
  { value: "pro", label: "文字优先", meta: "代码与文档更清楚", network: "占用网络较多" },
];

export default function Home({ activeRoom, onResumeRoom, onHost, onJoin }: HomeProps) {
  const { language, t } = useLanguage();
  const [mode, setMode] = useState<Mode>("host");
  const [roomName, setRoomName] = useState(() => t("内部屏幕会议"));
  const [hostPassword, setHostPassword] = useState("");
  const [hostLoading, setHostLoading] = useState(false);
  const [hostError, setHostError] = useState("");
  const [qualityTier, setQualityTier] = useState("high");
  const [shareSystemAudio, setShareSystemAudio] = useState(false);
  const [hasPermission, setHasPermission] = useState<boolean | null>(null);
  const [showPermDialog, setShowPermDialog] = useState(false);
  const [captureSources, setCaptureSources] = useState<CaptureSource[]>([]);
  const [sourceId, setSourceId] = useState<number | undefined>(undefined);
  const [sourceIsDisplay, setSourceIsDisplay] = useState(true);
  const [showSourcePicker, setShowSourcePicker] = useState(false);
  const [sourcePickerLoading, setSourcePickerLoading] = useState(false);

  const [rooms, setRooms] = useState<RoomInfo[]>([]);
  const [scanning, setScanning] = useState(false);
  const [manualIp, setManualIp] = useState("");
  const [manualPort, setManualPort] = useState("38472");
  const [manualPassword, setManualPassword] = useState("");

  useEffect(() => {
    setRoomName((current) => (
      current === "内部屏幕会议" || current === "Internal screen meeting"
        ? t("内部屏幕会议")
        : current
    ));
  }, [language, t]);
  const [joinLoading, setJoinLoading] = useState(false);
  const [joinError, setJoinError] = useState("");

  useEffect(() => {
    checkScreenCapturePermission().then(setHasPermission).catch(() => setHasPermission(false));
  }, []);

  useEffect(() => {
    if (!hasPermission) return;
    // Warm only the lightweight metadata. Preview JPEGs are generated after
    // the user actually opens the picker.
    void refreshCaptureSources(false);
  }, [hasPermission]);

  const refreshCaptureSources = async (loadThumbnails = true) => {
    setSourcePickerLoading(true);
    try {
      const sources = await listCaptureSources();
      setCaptureSources(sources);
      setHasPermission(true);
      const currentExists = sources.some((source) => source.id === sourceId
        && source.is_display === sourceIsDisplay);
      if (!currentExists) {
        setSourceId(sources[0]?.id);
        setSourceIsDisplay(sources[0]?.is_display ?? true);
      }
      setSourcePickerLoading(false);
      if (loadThumbnails) {
        void hydrateCaptureSourceThumbnails(sources, (source, thumbnailDataUrl) => {
          setCaptureSources((current) => current.map((candidate) => (
            candidate.id === source.id && candidate.is_display === source.is_display
              ? { ...candidate, thumbnail_data_url: thumbnailDataUrl }
              : candidate
          )));
        });
      }
    } catch (error) {
      setCaptureSources([]);
      const message = error instanceof Error ? error.message : String(error);
      setHostError(message);
      if (/权限|permission|not authorized/i.test(message)) {
        setHasPermission(false);
        setShowSourcePicker(false);
        setShowPermDialog(true);
      }
    } finally {
      setSourcePickerLoading(false);
    }
  };

  useEffect(() => {
    if (mode !== "join") return;

    const discovered = listen<RoomInfo>("room-discovered", (event) => {
      setRooms((previous) => {
        const existing = previous.some((room) => room.room_id === event.payload.room_id);
        return existing
          ? previous.map((room) => room.room_id === event.payload.room_id ? event.payload : room)
          : [...previous, event.payload];
      });
    });
    const removed = listen<{ room_id: string }>("room-removed", (event) => {
      setRooms((previous) => previous.filter((room) => room.room_id !== event.payload.room_id));
    });

    return () => {
      discovered.then((unlisten) => unlisten());
      removed.then((unlisten) => unlisten());
    };
  }, [mode]);

  const handleCreateRoom = async (sourceOverride?: CaptureSource) => {
    setHostError("");
    if (activeRoom) {
      setHostError("已有会议正在进行，请返回当前会议后再操作。");
      return;
    }
    if (!roomName.trim()) {
      setHostError("请输入会议名称");
      return;
    }

    setHostLoading(true);
    try {
      const room = await createRoom({
        room_name: roomName.trim(),
        password: hostPassword,
        source_id: sourceOverride?.id ?? sourceId,
        source_is_display: sourceOverride?.is_display ?? sourceIsDisplay,
        video_transport: "webrtc-video-track",
        capture_mode: "screen-capture",
        quality_tier: qualityTier,
        share_system_audio: shareSystemAudio,
      });
      // Home stays mounted while the meeting view is visible. Clear the modal
      // before navigating so returning to Home never resurrects the picker.
      setShowSourcePicker(false);
      onHost(room);
    } catch (error) {
      setHostError(error instanceof Error ? error.message : String(error));
    } finally {
      setHostLoading(false);
    }
  };

  const openSourcePicker = async () => {
    setHostError("");
    setShowSourcePicker(true);
    await refreshCaptureSources(true);
  };

  const handleScan = async () => {
    setScanning(true);
    setJoinError("");
    try {
      await scanRooms();
      window.setTimeout(() => setScanning(false), 1000);
    } catch (error) {
      setScanning(false);
      setJoinError(error instanceof Error ? error.message : String(error));
    }
  };

  const prepareRoomJoin = (room: RoomInfo) => {
    setManualIp(room.host_ip);
    setManualPort(String(room.signaling_port));
    setManualPassword("");
    setJoinError("");
    if (!room.requires_password) void submitJoin(room.host_ip, room.signaling_port, "");
  };

  const submitJoin = async (
    address = manualIp,
    port = Number(manualPort),
    password = manualPassword
  ) => {
    if (!address.trim() || !Number.isInteger(port) || port < 1 || port > 65535) {
      setJoinError("请输入有效的主机 IP 和端口");
      return;
    }
    setJoinLoading(true);
    setJoinError("");
    try {
      await joinRoom(address.trim(), port, password);
      onJoin(address.trim(), port);
    } catch (error) {
      setJoinError(error instanceof Error ? error.message : String(error));
    } finally {
      setJoinLoading(false);
    }
  };

  const selectedQuality = qualityOptions.find((option) => option.value === qualityTier) ?? qualityOptions[1];
  const meetingCapacity = OPEN_SOURCE_MAX_GUESTS;
  const selectedSource = captureSources.find((source) => source.id === sourceId
    && source.is_display === sourceIsDisplay) ?? null;
  const selectedSourceName = selectedSource ? t(selectedSource.name) : null;
  const modeTitle = t(mode === "host" ? "发起内部会议" : "加入内部会议");
  const modeSubtitle = t(mode === "host" ? "共享屏幕给同一网络内的设备" : "扫描会议或输入主机地址");

  return (
    <div className="flex h-full min-h-0 overflow-hidden bg-[#0b0d12] text-slate-100">
      <aside className="home-sidebar flex w-[216px] shrink-0 flex-col border-r border-white/[0.06] bg-[#0d1016] px-3 py-4">
        <div className="flex items-center justify-between gap-2 px-2">
          <div className="flex min-w-0 items-center gap-3">
            <OrbMark />
            <div className="min-w-0">
              <div className="text-[15px] font-semibold tracking-[-0.02em] text-white">OrbShare</div>
              <div className="mt-0.5 truncate text-[10px] text-slate-500">{t("局域网内部会议")}</div>
            </div>
          </div>
          <LanguageSwitch compact />
        </div>

        <nav className="mt-8 space-y-1">
          <NavButton active={mode === "host"} onClick={() => setMode("host")} icon="share">{t("发起会议")}</NavButton>
          <NavButton active={mode === "join"} onClick={() => setMode("join")} icon="join">{t("加入会议")}</NavButton>
        </nav>

        <div className="mt-auto rounded-xl border border-white/[0.07] bg-white/[0.035] p-3">
          <div className="flex items-center gap-2 text-[11.5px] text-slate-300">
            <span className={`h-2 w-2 rounded-full ${
              hasPermission === null ? "bg-slate-500" : hasPermission
                ? "bg-emerald-400 shadow-[0_0_10px_rgba(52,211,153,.45)]" : "bg-amber-400"
            }`} />
            {hasPermission === null ? t("正在检测权限") : hasPermission ? t("屏幕采集已就绪") : t("需要屏幕录制权限")}
          </div>
          <div className="mt-2 border-t border-white/[0.06] pt-2 text-[10px] leading-4 text-slate-500">
            {t("高清低延迟共享")}<br />{t("局域网内直接连接")}
          </div>
        </div>
      </aside>

      <main className="flex min-w-0 flex-1 flex-col overflow-hidden">
        <header className="flex h-14 shrink-0 items-center justify-between border-b border-white/[0.06] bg-[#0b0d12]/90 px-6 backdrop-blur-xl">
          <div>
            <div className="text-[13px] font-medium text-white">{modeTitle}</div>
            <div className="mt-0.5 text-[10px] text-slate-500">{modeSubtitle}</div>
          </div>
          <div className="flex items-center gap-2 rounded-full border border-emerald-400/15 bg-emerald-400/[0.06] px-3 py-1.5 text-[10.5px] text-emerald-300">
            <span className="h-1.5 w-1.5 rounded-full bg-current" />{t("仅局域网")}
          </div>
        </header>

        {activeRoom && (
          <div className="mx-5 mt-4 flex h-[60px] shrink-0 items-center justify-between gap-4 rounded-xl border border-emerald-400/20 bg-emerald-400/[0.07] px-4">
            <div className="min-w-0">
              <div className="flex items-center gap-2 text-[11.5px] font-medium text-emerald-200">
                <span className="h-2 w-2 rounded-full bg-emerald-400 shadow-[0_0_10px_rgba(52,211,153,.5)]" />
                {t("会议仍在进行中")}
              </div>
              <div className="mt-1 truncate text-[10.5px] text-slate-400">
                {t(activeRoom.room_name)} · http://{activeRoom.host_ip}:{activeRoom.viewer_port}
              </div>
            </div>
            <button type="button" onClick={onResumeRoom}
              className="shrink-0 rounded-lg bg-emerald-500 px-3.5 py-2 text-[11px] font-semibold text-[#06291f] transition hover:bg-emerald-400">
              {t("返回会议")}
            </button>
          </div>
        )}

        {mode === "host" ? (
          <section className="home-host-grid mx-auto grid min-h-0 w-full max-w-[1120px] flex-1 grid-cols-2 gap-4 px-5 py-4">
            <div className="flex min-h-0 flex-col">
              <div className="mb-3 shrink-0">
                <h1 className="text-[23px] font-semibold tracking-[-0.035em] text-white">{t("准备共享你的屏幕")}</h1>
                <p className="mt-1.5 max-w-xl text-[11.5px] leading-4.5 text-slate-400">
                  {t("参会者通过浏览器地址加入。默认高清档兼顾文字清晰度与多人观看时的流畅度。")}
                </p>
              </div>

              <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-2xl border border-white/[0.08] bg-[#11151d] shadow-2xl shadow-black/25">
                <div className="flex shrink-0 items-center justify-between border-b border-white/[0.06] px-4 py-2.5">
                  <div className="flex items-center gap-2 text-[11.5px] text-slate-300">
                    <span className="flex h-6 w-6 items-center justify-center rounded-md bg-blue-500/10 text-blue-300"><ScreenIcon className="h-3.5 w-3.5" /></span>
                    <span className="max-w-[260px] truncate">{selectedSourceName ?? t("当前共享来源")}</span>
                  </div>
                  <button type="button" onClick={() => void openSourcePicker()} className="rounded-md border border-blue-400/20 bg-blue-500/10 px-2.5 py-1 text-[9.5px] font-medium text-blue-300 transition hover:bg-blue-500/20">{t("选择来源")}</button>
                </div>
                <div className="relative min-h-0 flex-1 bg-[#080a0e] p-4">
                  <div className="relative flex h-full items-center justify-center overflow-hidden rounded-xl border border-white/[0.08] bg-gradient-to-br from-[#192130] to-[#0e131c]">
                    {selectedSource?.thumbnail_data_url ? (
                      <img src={selectedSource.thumbnail_data_url} alt="" className="absolute inset-0 h-full w-full object-contain" draggable={false} />
                    ) : <div className="absolute inset-0 opacity-20 [background-image:linear-gradient(rgba(255,255,255,.08)_1px,transparent_1px),linear-gradient(90deg,rgba(255,255,255,.08)_1px,transparent_1px)] [background-size:28px_28px]" />}
                    <div className="relative flex flex-col items-center">
                      {!selectedSource?.thumbnail_data_url && <div className="flex h-12 w-12 items-center justify-center rounded-xl border border-blue-300/15 bg-blue-500/10 text-blue-200 shadow-xl shadow-blue-950/30"><ScreenIcon className="h-6 w-6" /></div>}
                      <div className={`${selectedSource?.thumbnail_data_url ? "rounded-lg bg-black/60 px-3 py-1.5 backdrop-blur" : "mt-3"} text-[12.5px] font-medium text-white`}>{selectedSourceName ?? t("当前主屏幕")}</div>
                      <div className="mt-1 text-[10.5px] text-slate-500">
                        {selectedSource ? `${selectedSource.width}×${selectedSource.height}` : t("完整屏幕")} · {t("自动适配分辨率")}
                      </div>
                    </div>
                  </div>
                </div>
                <div className="grid shrink-0 grid-cols-3 border-t border-white/[0.06]">
                  <Metric label={t("传输")} value={t("高清低延迟")} />
                  <Metric label={t("画质")} value={t(selectedQuality.label)} />
                  <Metric label={t("网络占用")} value={t(selectedQuality.network)} />
                </div>
              </div>

              <div className="mt-3 shrink-0 rounded-xl border border-blue-400/10 bg-blue-400/[0.045] px-3.5 py-2.5 text-[10px] leading-4 text-slate-400">
                <span className="font-medium text-blue-300">{t("多人观看提示：")}</span>
                {t("当前版本最多 {count} 位观看者。观看人数越多，主机网络占用越高，建议使用有线网络或稳定 Wi-Fi。", { count: meetingCapacity })}
              </div>
            </div>

            <div className="flex min-h-0 h-full flex-col rounded-2xl border border-white/[0.08] bg-[#11151d] p-4 shadow-2xl shadow-black/20">
              <div className="mb-3 shrink-0">
                <div className="text-[14px] font-semibold text-white">{t("会议设置")}</div>
                <div className="mt-1 text-[10.5px] text-slate-500">{t("配置加入方式和共享质量")}</div>
              </div>

              <div className="space-y-3">
                <Field label={t("会议名称")}>
                  <input className={inputClass} value={roomName} maxLength={48} onChange={(event) => setRoomName(event.target.value)} placeholder={t("例如：研发周会")} />
                </Field>

                <Field label={t("共享画质")} hint={t("推荐高清")}>
                  <div className="grid grid-cols-3 gap-2">
                    {qualityOptions.map((option) => {
                      const active = option.value === qualityTier;
                      return (
                        <button type="button" key={option.value} onClick={() => setQualityTier(option.value)}
                          className={`relative rounded-xl border px-3 py-2.5 text-left transition ${active
                            ? "border-blue-400/60 bg-blue-500/[0.12] shadow-[0_0_0_1px_rgba(96,165,250,.12)]"
                            : "border-white/[0.08] bg-white/[0.025] hover:border-white/[0.15] hover:bg-white/[0.045]"}`}>
                          {option.recommended && <span className="absolute right-2 top-2 h-1.5 w-1.5 rounded-full bg-blue-400" />}
                          <div className={`text-[11.5px] font-semibold ${active ? "text-blue-200" : "text-slate-200"}`}>{t(option.label)}</div>
                          <div className="mt-1 text-[9px] leading-3.5 text-slate-500">{t(option.meta)}</div>
                        </button>
                      );
                    })}
                  </div>
                  <div className="mt-2 text-[10px] text-slate-500">{t(selectedQuality.network)} · {t("最多 {count} 人，实际占用随画面变化", { count: meetingCapacity })}</div>
                </Field>

                <Field label={t("会议密码")} hint={t("留空表示无需密码")}>
                  <div className="relative">
                    <span className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-slate-500"><LockIcon className="h-4 w-4" /></span>
                    <input className={`${inputClass} pl-10`} type="password" value={hostPassword} maxLength={64}
                      onChange={(event) => setHostPassword(event.target.value)} placeholder={t("建议内部会议设置密码")} />
                  </div>
                </Field>

                <button
                  type="button"
                  role="switch"
                  aria-checked={shareSystemAudio}
                  onClick={() => setShareSystemAudio((enabled) => !enabled)}
                  className={`flex w-full items-center gap-3 rounded-xl border px-3 py-2.5 text-left transition ${
                    shareSystemAudio
                      ? "border-emerald-400/35 bg-emerald-500/[0.09]"
                      : "border-white/[0.07] bg-white/[0.02] hover:bg-white/[0.04]"
                  }`}
                >
                  <span className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-lg ${shareSystemAudio ? "bg-emerald-500/15 text-emerald-300" : "bg-white/[0.04] text-slate-500"}`}>
                    <SpeakerIcon className="h-4 w-4" />
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="block text-[10.5px] font-medium text-slate-200">{t("共享电脑声音")}</span>
                    <span className="mt-0.5 block truncate text-[8.5px] text-slate-500">{t("让观看者同时听到电脑正在播放的声音")}</span>
                  </span>
                  <span className={`rounded-full px-2 py-1 text-[9px] font-semibold ${shareSystemAudio ? "bg-emerald-400/15 text-emerald-300" : "bg-white/[0.04] text-slate-500"}`}>
                    {shareSystemAudio ? t("已开启") : t("已关闭")}
                  </span>
                </button>
              </div>

              <div className="mt-auto shrink-0">
              {hostError && <ErrorBox>{t(hostError)}</ErrorBox>}
              <button type="button" onClick={activeRoom ? onResumeRoom : () => void openSourcePicker()} disabled={hostLoading}
                className="mt-3 flex h-11 w-full items-center justify-center gap-2 rounded-xl bg-gradient-to-r from-blue-600 to-sky-500 text-[12px] font-semibold text-white shadow-lg shadow-blue-950/40 transition hover:brightness-110 disabled:cursor-wait disabled:opacity-60">
                {hostLoading ? <Spinner /> : <ScreenIcon className="h-4 w-4" />}{hostLoading ? t("正在创建会议…") : activeRoom ? t("返回进行中的会议") : t("开始共享屏幕")}
              </button>
              <div className="mt-2 text-center text-[9px] text-slate-600">{t("开始后会生成浏览器访问地址")}</div>
              </div>
            </div>
          </section>
        ) : (
          <section className="mx-auto flex min-h-0 w-full max-w-[1040px] flex-1 flex-col px-5 py-4">
            <div className="flex shrink-0 items-end justify-between">
              <div><h1 className="text-[26px] font-semibold tracking-[-0.035em] text-white">{t("加入会议")}</h1><p className="mt-2 text-[12.5px] text-slate-400">{t("自动发现同一局域网内的 OrbShare 会议。")}</p></div>
              <button type="button" onClick={handleScan} disabled={scanning}
                className="flex h-9 items-center gap-2 rounded-lg border border-white/[0.09] bg-white/[0.035] px-3 text-[11px] text-slate-300 transition hover:bg-white/[0.07] disabled:opacity-60">
                {scanning ? <Spinner /> : <RefreshIcon className="h-3.5 w-3.5" />}{scanning ? t("正在扫描") : t("刷新会议")}
              </button>
            </div>

            <div className="home-join-grid mt-4 grid min-h-0 flex-1 grid-cols-2 gap-4">
              <div className="flex min-h-0 flex-col">
                <div className="mb-3 flex items-center justify-between text-[10.5px] text-slate-500"><span>{t("附近会议")}</span><span>{t("{count} 个可用", { count: rooms.length })}</span></div>
                <div className="min-h-0 flex-1 space-y-2 overflow-hidden rounded-2xl border border-white/[0.07] bg-[#11151d] p-3">
                  {rooms.length === 0 ? (
                    <div className="flex h-full flex-col items-center justify-center text-center">
                      <div className="flex h-12 w-12 items-center justify-center rounded-2xl bg-white/[0.04] text-slate-500"><RadarIcon className="h-6 w-6" /></div>
                      <div className="mt-4 text-[12px] font-medium text-slate-300">{t("暂未发现会议")}</div>
                      <div className="mt-1 max-w-[250px] text-[10.5px] leading-4 text-slate-500">{t("确认两台设备连接同一局域网，或在右侧手动输入主机 IP。")}</div>
                    </div>
                  ) : rooms.map((room) => (
                    <button type="button" key={room.room_id} onClick={() => prepareRoomJoin(room)} disabled={joinLoading}
                      className="group flex w-full items-center gap-3 rounded-xl border border-transparent bg-white/[0.025] p-3 text-left transition hover:border-blue-400/20 hover:bg-blue-500/[0.07]">
                      <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-blue-500/10 text-blue-300"><ScreenIcon className="h-5 w-5" /></span>
                      <span className="min-w-0 flex-1"><span className="block truncate text-[12px] font-medium text-white">{t(room.room_name)}</span><span className="mt-1 block font-mono text-[9.5px] text-slate-500">{room.host_ip}:{room.signaling_port}</span></span>
                      <span className="rounded-full border border-white/[0.07] bg-white/[0.035] px-2 py-1 text-[9.5px] text-slate-400 group-hover:text-blue-200">{room.requires_password ? t("需密码") : t("加入")}</span>
                    </button>
                  ))}
                </div>
              </div>

              <div className="flex min-h-0 flex-col">
                <div className="mb-3 text-[10.5px] text-slate-500">{t("手动加入")}</div>
                <div className="flex min-h-0 flex-1 flex-col rounded-2xl border border-white/[0.07] bg-[#11151d] p-5">
                  <div className="text-[13px] font-semibold text-white">{t("主机地址")}</div>
                  <p className="mt-1 text-[10.5px] leading-4 text-slate-500">{t("输入发起者客户端显示的 IP 地址与连接端口。")}</p>
                  <div className="mt-5 space-y-4">
                    <Field label={t("主机 IP")}><input className={inputClass} value={manualIp} onChange={(event) => setManualIp(event.target.value)} placeholder="192.168.1.20" /></Field>
                    <Field label={t("连接端口")}><input className={inputClass} type="number" min="1" max="65535" value={manualPort} onChange={(event) => setManualPort(event.target.value)} /></Field>
                    <Field label={t("会议密码")} hint={t("如有")}><input className={inputClass} type="password" value={manualPassword} onChange={(event) => setManualPassword(event.target.value)} placeholder={t("请输入密码")} /></Field>
                  </div>
                  {joinError && <ErrorBox>{t(joinError)}</ErrorBox>}
                  <button type="button" onClick={() => void submitJoin()} disabled={joinLoading}
                    className="mt-auto flex h-11 w-full shrink-0 items-center justify-center gap-2 rounded-xl bg-blue-600 text-[12px] font-semibold text-white transition hover:bg-blue-500 disabled:cursor-wait disabled:opacity-60">
                    {joinLoading && <Spinner />}{joinLoading ? t("正在连接…") : t("加入会议")}
                  </button>
                </div>
                <div className="mt-3 flex shrink-0 items-center gap-2 rounded-xl border border-emerald-400/10 bg-emerald-400/[0.04] px-3 py-2.5 text-[10px] text-slate-500"><LockIcon className="h-3.5 w-3.5 text-emerald-400" />{t("会议画面和声音不会离开当前局域网")}</div>
              </div>
            </div>
          </section>
        )}
      </main>

      <SourcePicker
        open={showSourcePicker}
        sources={captureSources}
        selected={selectedSource}
        loading={sourcePickerLoading}
        switching={hostLoading}
        error={showSourcePicker ? hostError : ""}
        onSelect={(source) => {
          setSourceId(source.id);
          setSourceIsDisplay(source.is_display);
          setHostError("");
        }}
        onRefresh={() => void refreshCaptureSources(true)}
        onCancel={() => {
          if (!hostLoading) setShowSourcePicker(false);
        }}
        onConfirm={() => {
          if (!selectedSource) return;
          void handleCreateRoom(selectedSource);
        }}
      />

      {showPermDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/65 p-5 backdrop-blur-sm">
          <div className="w-full max-w-[420px] rounded-2xl border border-white/[0.09] bg-[#151922] p-6 shadow-2xl">
            <div className="flex h-11 w-11 items-center justify-center rounded-xl bg-amber-400/10 text-amber-300"><ScreenIcon className="h-5 w-5" /></div>
            <h2 className="mt-4 text-[17px] font-semibold text-white">{t("需要屏幕录制权限")}</h2>
            <p className="mt-2 text-[11.5px] leading-5 text-slate-400">{t("请在系统设置的“隐私与安全性 → 屏幕录制”中启用 OrbShare，然后完全退出并重新打开应用。")}</p>
            <div className="mt-5 grid grid-cols-2 gap-2">
              <button type="button" onClick={() => setShowPermDialog(false)} className="h-10 rounded-lg border border-white/[0.08] bg-white/[0.03] text-[11px] text-slate-300 hover:bg-white/[0.06]">{t("稍后处理")}</button>
              <button type="button" onClick={() => void openScreenRecordingSettings()} className="h-10 rounded-lg bg-blue-600 text-[11px] font-semibold text-white hover:bg-blue-500">{t("打开系统设置")}</button>
            </div>
            <button type="button" onClick={async () => {
              const granted = await checkScreenCapturePermission().catch(() => false);
              setHasPermission(granted);
              if (granted) setShowPermDialog(false);
            }} className="mt-3 w-full text-center text-[10.5px] text-blue-300 hover:text-blue-200">{t("我已授权，重新检测")}</button>
          </div>
        </div>
      )}
    </div>
  );
}

const inputClass = "h-10 w-full rounded-lg border border-white/[0.09] bg-[#0d1016] px-3 text-[11.5px] text-white outline-none transition placeholder:text-slate-600 focus:border-blue-400/70 focus:ring-2 focus:ring-blue-500/10 [color-scheme:dark]";

function OrbMark() {
  return <span className="relative flex h-9 w-9 items-center justify-center rounded-xl bg-gradient-to-br from-blue-600 to-cyan-500 shadow-lg shadow-blue-950/40"><span className="h-3.5 w-3.5 rounded-full border-2 border-white/90" /><span className="absolute h-0.5 w-7 -rotate-[24deg] rounded bg-white/90" /></span>;
}

function NavButton({ active, onClick, icon, children }: { active: boolean; onClick: () => void; icon: "share" | "join" | "license"; children: React.ReactNode }) {
  const iconNode = icon === "share" ? <ScreenIcon className="h-4 w-4" /> : icon === "join" ? <RadarIcon className="h-4 w-4" /> : <LicenseIcon className="h-4 w-4" />;
  return <button type="button" onClick={onClick} className={`flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-[11.5px] transition ${active ? "bg-blue-500/[0.12] text-blue-200" : "text-slate-400 hover:bg-white/[0.04] hover:text-slate-200"}`}>{iconNode}{children}</button>;
}

function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return <div><div className="mb-2 flex items-center justify-between"><label className="text-[10.5px] font-medium text-slate-400">{label}</label>{hint && <span className="text-[9.5px] text-slate-600">{hint}</span>}</div>{children}</div>;
}

function Metric({ label, value }: { label: string; value: string }) {
  return <div className="border-r border-white/[0.06] px-4 py-3 last:border-r-0"><div className="text-[9px] uppercase tracking-[0.12em] text-slate-600">{label}</div><div className="mt-1 text-[10.5px] font-medium text-slate-300">{value}</div></div>;
}

function ErrorBox({ children }: { children: React.ReactNode }) {
  return <div className="mt-4 rounded-lg border border-rose-400/15 bg-rose-400/[0.07] px-3 py-2.5 text-[10.5px] leading-4 text-rose-300">{children}</div>;
}

function Spinner() {
  return <span className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-current/25 border-t-current" />;
}

function ScreenIcon({ className }: { className?: string }) {
  return <svg viewBox="0 0 24 24" className={className} fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"><rect x="2" y="4" width="20" height="14" rx="3" /><path d="M8 21h8M12 18v3" /></svg>;
}

function LockIcon({ className }: { className?: string }) {
  return <svg viewBox="0 0 24 24" className={className} fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"><rect x="4" y="10" width="16" height="11" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg>;
}

function SpeakerIcon({ className }: { className?: string }) {
  return <svg viewBox="0 0 24 24" className={className} fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"><path d="M11 5 6 9H3v6h3l5 4V5Z" /><path d="M15.5 8.5a5 5 0 0 1 0 7M18 6a8.5 8.5 0 0 1 0 12" /></svg>;
}

function RadarIcon({ className }: { className?: string }) {
  return <svg viewBox="0 0 24 24" className={className} fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round"><path d="M5 12.5a10 10 0 0 1 14 0M2 9a15 15 0 0 1 20 0M8.5 16a5 5 0 0 1 7 0" /><circle cx="12" cy="20" r="1" fill="currentColor" stroke="none" /></svg>;
}

function RefreshIcon({ className }: { className?: string }) {
  return <svg viewBox="0 0 24 24" className={className} fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"><path d="M20 11a8 8 0 1 0 2 5M20 4v7h-7" /></svg>;
}

function LicenseIcon({ className }: { className?: string }) {
  return <svg viewBox="0 0 24 24" className={className} fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"><path d="M12 3 20 6v5c0 5-3.4 8.5-8 10-4.6-1.5-8-5-8-10V6l8-3Z" /><circle cx="11" cy="11" r="2" /><path d="m12.5 9.5 3-3M14 8l1.5 1.5" /></svg>;
}
