import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react";

export type AppLanguage = "zh-CN" | "en-US";
type Variables = Record<string, string | number>;

const STORAGE_KEY = "orbshare-language";

const en: Record<string, string> = {
  "局域网内部会议": "Private LAN meetings",
  "发起会议": "Host meeting",
  "加入会议": "Join meeting",
  "发起内部会议": "Host a private meeting",
  "加入内部会议": "Join a private meeting",
  "共享屏幕给同一网络内的设备": "Share your screen with devices on the same network",
  "扫描会议或输入主机地址": "Find a meeting or enter the host address",
  "正在检测权限": "Checking permission",
  "屏幕采集已就绪": "Screen sharing is ready",
  "需要屏幕录制权限": "Screen recording permission required",
  "高清低延迟共享": "Clear, low-latency sharing",
  "局域网内直接连接": "Direct connection on your LAN",
  "仅局域网": "LAN only",
  "会议仍在进行中": "The meeting is still running",
  "返回会议": "Return to meeting",
  "准备共享你的屏幕": "Get ready to share your screen",
  "参会者通过浏览器地址加入。默认高清档兼顾文字清晰度与多人观看时的流畅度。": "Participants join from a browser. The default HD mode balances clear text and smooth viewing for multiple people.",
  "当前共享来源": "Current share source",
  "选择来源": "Choose source",
  "当前主屏幕": "Primary display",
  "完整屏幕": "Entire screen",
  "自动适配分辨率": "Resolution adjusts automatically",
  "传输": "Connection",
  "高清低延迟": "Clear and responsive",
  "画质": "Quality",
  "网络占用": "Network usage",
  "多人观看提示：": "Multiple viewers: ",
  "当前版本最多 {count} 位观看者。观看人数越多，主机网络占用越高，建议使用有线网络或稳定 Wi-Fi。": "This edition supports up to {count} viewers. More viewers use more host bandwidth; use Ethernet or stable Wi-Fi when possible.",
  "会议设置": "Meeting settings",
  "配置加入方式和共享质量": "Set how people join and the sharing quality",
  "会议名称": "Meeting name",
  "例如：研发周会": "For example: Weekly team meeting",
  "共享画质": "Sharing quality",
  "推荐高清": "HD recommended",
  "最多 {count} 人，实际占用随画面变化": "Up to {count} people; usage changes with the picture",
  "会议密码": "Meeting password",
  "留空表示无需密码": "Leave blank for no password",
  "建议内部会议设置密码": "A password is recommended for private meetings",
  "共享电脑声音": "Share computer audio",
  "让观看者同时听到电脑正在播放的声音": "Let viewers hear audio playing on this computer",
  "已开启": "On",
  "已关闭": "Off",
  "正在创建会议…": "Creating meeting…",
  "返回进行中的会议": "Return to current meeting",
  "开始共享屏幕": "Start sharing",
  "开始后会生成浏览器访问地址": "A browser link will be created after sharing starts",
  "自动发现同一局域网内的 OrbShare 会议。": "Automatically find OrbShare meetings on the same LAN.",
  "正在扫描": "Scanning",
  "刷新会议": "Refresh",
  "附近会议": "Nearby meetings",
  "{count} 个可用": "{count} available",
  "暂未发现会议": "No meetings found",
  "确认两台设备连接同一局域网，或在右侧手动输入主机 IP。": "Make sure both devices are on the same LAN, or enter the host IP on the right.",
  "需密码": "Password required",
  "加入": "Join",
  "手动加入": "Join manually",
  "主机地址": "Host address",
  "输入发起者客户端显示的 IP 地址与连接端口。": "Enter the IP address and connection port shown on the host.",
  "主机 IP": "Host IP",
  "连接端口": "Connection port",
  "如有": "If required",
  "请输入密码": "Enter password",
  "正在连接…": "Connecting…",
  "会议画面和声音不会离开当前局域网": "Meeting video and audio stay on this LAN",
  "请在系统设置的“隐私与安全性 → 屏幕录制”中启用 OrbShare，然后完全退出并重新打开应用。": "Enable OrbShare in System Settings → Privacy & Security → Screen Recording, then quit and reopen the app.",
  "稍后处理": "Not now",
  "打开系统设置": "Open System Settings",
  "我已授权，重新检测": "Permission granted, check again",
  "已有会议正在进行，请返回当前会议后再操作。": "A meeting is already running. Return to it before continuing.",
  "请输入会议名称": "Enter a meeting name",
  "请输入有效的主机 IP 和端口": "Enter a valid host IP and port",
  "流畅": "Smooth",
  "网络较慢时使用": "For slower networks",
  "节省网络": "Low network usage",
  "高清": "HD",
  "日常演示推荐": "Recommended for presentations",
  "清晰流畅": "Clear and smooth",
  "文字优先": "Text clarity",
  "代码与文档更清楚": "Sharper code and documents",
  "占用网络较多": "Higher network usage",
  "内部屏幕会议": "Internal screen meeting",
  "桌面 {number}": "Desktop {number}",
  "桌面 {number}（主屏幕）": "Desktop {number} (Primary)",

  "选择共享内容": "Choose what to share",
  "开始共享": "Start sharing",
  "共享桌面可看到所有切换内容；共享窗口只会显示所选软件。": "Sharing a desktop shows everything you switch to. Sharing a window shows only that app.",
  "刷新": "Refresh",
  "正在读取可共享内容…": "Loading shareable content…",
  "没有找到可共享内容": "No shareable content found",
  "请确认已授予“屏幕与系统录音”权限，然后刷新。": "Make sure Screen & System Audio Recording permission is granted, then refresh.",
  "桌面": "Desktop",
  "窗口": "Window",
  "应用窗口": "App windows",
  "共 {count} 个": "{count} total",
  "已选择：": "Selected: ",
  "请选择一个共享来源": "Choose a share source",
  "取消": "Cancel",
  "正在切换…": "Switching…",
  "切换共享": "Switch",
  "切换共享内容": "Switch shared content",

  "邀请你加入 OrbShare 局域网会议": "You're invited to an OrbShare LAN meeting",
  "会议名称：{name}": "Meeting: {name}",
  "加入链接：{url}": "Join link: {url}",
  "会议密码：{password}": "Meeting password: {password}",
  "会议设有密码，请向主持人获取。": "This meeting requires a password. Ask the host for it.",
  "无需会议密码。": "No meeting password is required.",
  "请确保设备与主持人连接到同一局域网。": "Make sure your device is on the same LAN as the host.",
  "请复制以下内容：": "Copy the following text:",
  "邀请成员加入会议": "Invite people to the meeting",
  "手机或平板扫码即可通过浏览器加入，无需安装应用。": "Scan with a phone or tablet to join in a browser. No app is required.",
  "关闭邀请窗口": "Close invite window",
  "加入二维码": "Join QR code",
  "二维码生成失败": "Could not create QR code",
  "请复制右侧链接加入": "Copy the link on the right to join",
  "正在生成二维码": "Creating QR code",
  "使用手机相机或浏览器扫码": "Scan with your phone camera or browser",
  "加入链接": "Join link",
  "加入时需要输入会议密码": "A meeting password is required",
  "当前会议无需密码": "No password required",
  "暂时无法读取": "Password unavailable",
  "隐藏": "Hide",
  "显示": "Show",
  "复制": "Copy",
  "已复制 ✓": "Copied ✓",
  "链接已复制 ✓": "Link copied ✓",
  "复制链接": "Copy link",
  "邀请信息已复制 ✓": "Invitation copied ✓",
  "复制完整邀请": "Copy full invitation",
  "二维码仅包含局域网加入地址，不包含会议密码；离开当前网络后无法访问。": "The QR code contains only the LAN address, not the password. It cannot be reached outside this network.",

  "返回主页（会议继续）": "Back to Home (meeting continues)",
  "断开并返回": "Disconnect and return",
  "屏幕共享": "Screen sharing",
  "本地": "Local",
  "邀请参会": "Invite",
  "正在共享当前屏幕的系统声音": "Computer audio is being shared",
  "系统声音": "Computer audio",
  "正在录制": "Recording",
  "主持人已将你禁言": "The host muted you",
  "关闭麦克风": "Turn off microphone",
  "开启麦克风": "Turn on microphone",
  "已禁言": "Muted by host",
  "麦克风已开": "Mic on",
  "麦克风已关": "Mic off",
  "放下手": "Lower hand",
  "向主持人举手": "Raise hand",
  "处理中": "Working",
  "已举手": "Hand raised",
  "举手": "Raise hand",
  "关闭弹幕": "Turn off bullet comments",
  "开启弹幕": "Turn on bullet comments",
  "弹幕已开": "Comments",
  "弹幕已关": "Comments off",
  "关闭激光笔": "Turn off laser pointer",
  "开启激光笔": "Turn on laser pointer",
  "激光笔已开": "Laser on",
  "激光笔": "Laser",
  "聊天": "Chat",
  "展开成员列表": "Open participant list",
  "{count} 人举手 · {total} 人": "{count} raised · {total} people",
  "{count} 位观看者": "{count} viewers",
  "等待接入": "Waiting",
  "正在观看 · 高清画面": "Watching · Clear picture",
  "已结束": "Ended",
  "等待视频帧": "Waiting for picture",
  "切换桌面或应用窗口，会议和观看者保持连接": "Switch desktop or app window without disconnecting viewers",
  "正在切换": "Switching",
  "正在准备录制画面": "Preparing recording",
  "停止并保存录制": "Stop and save recording",
  "开始本地录制": "Start local recording",
  "准备录制": "Preparing",
  "正在保存": "Saving",
  "录制": "Record",
  "录制 {duration}": "Recording {duration}",
  "画面": "Video",
  "停止共享": "Stop",
  "断开": "Disconnect",
  "正在切换共享内容…": "Switching shared content…",
  "录制已保存 · {size}": "Recording saved · {size}",
  "录制已自动保存": "Recording saved automatically",
  "打开所在文件夹": "Show in folder",
  "正在共享屏幕": "Sharing screen",
  "等待局域网设备接入…": "Waiting for devices on this LAN…",
  "显示二维码": "Show QR code",
  "局域网内任意设备浏览器直接打开即可观看": "Open the link in any browser on this LAN",
  "正在等待视频流…": "Waiting for the shared picture…",
  "返回": "Back",
  "收起成员与聊天": "Close participants and chat",
  "展开成员与聊天": "Open participants and chat",
  "成员 {count}": "Participants {count}",
  "聊天 {count}": "Chat {count}",
  "会内聊天": "Meeting chat",
  "仅当前局域网会议可见": "Visible only in this LAN meeting",
  "收起面板": "Close panel",
  "本场 {count} 人 · 重新加入 {rejoins} 次": "{count} people · {rejoins} rejoins",
  "举手 {count} 人": "{count} raised",
  "暂无观看者": "No viewers",
  "设备加入后会显示名称和接入方式": "Names and connection types appear after people join",
  "{platform} · {time} 加入": "{platform} · Joined at {time}",
  "在线 {duration}": "Online {duration}",
  "重进 {count} 次": "Rejoined {count} times",
  "主持人已禁言": "Muted by host",
  "麦克风已开启": "Microphone on",
  "麦克风关闭": "Microphone off",
  "{time} 举手": "Raised at {time}",
  "允许发言": "Allow speaking",
  "禁言": "Mute",
  "移除中": "Removing",
  "移除": "Remove",
  "实时共享状态": "Live sharing status",
  "画面流畅度": "Picture smoothness",
  "当前网络占用": "Current network usage",
  "画质会根据当前画面和网络情况自动调整": "Quality adjusts automatically to the picture and network",
  "开始会内交流": "Start the conversation",
  "消息仅在本次局域网会议中可见": "Messages are visible only in this LAN meeting",
  "支持文字和常用表情": "Text and common emoji are supported",
  "主": "H",
  "访": "G",
  "主持人": "Host",
  "常用表情": "Common emoji",
  "收起": "Collapse",
  "插入表情 {emoji}": "Insert emoji {emoji}",
  "输入消息，Enter 发送…": "Type a message, press Enter to send…",
  "选择常用表情": "Choose an emoji",
  "Shift + Enter 换行": "Shift + Enter for a new line",
  "发送中": "Sending",
  "发送": "Send",
  "OrbShare · 局域网安全直连": "OrbShare · Secure LAN connection",
  "观看者": "Viewer",
  "已连接": "Connected",
  "连接中": "Connecting",
  "正在恢复": "Reconnecting",
  "连接失败": "Connection failed",
  "已断开": "Disconnected",
  "准备中": "Preparing",
  "等待统计": "Waiting for data",
  "一般": "Fair",
  "较慢": "Slow",
  "较低": "Low",
  "适中": "Moderate",
  "较高": "High",
  "{count}秒": "{count}s",
  "{count}分钟": "{count}m",
  "{hours}小时{minutes}分": "{hours}h {minutes}m",
  "{seconds}秒": "{seconds}s",
  "{minutes}分钟": "{minutes}m",
  "放下": "Lower",
  "局域网安全直连": "Secure direct LAN connection",
  "双击最大化或还原窗口": "Double-click to maximize or restore",
  "投屏已结束": "Screen sharing ended",
  "房主已停止共享，当前连接已经关闭。": "The host stopped sharing and the connection is closed.",
  "已被移出会议": "Removed from meeting",
  "主持人已结束你的观看权限，本次会议中无法自动重新加入。": "The host removed you from this meeting. You cannot rejoin automatically.",
  "视频连接已建立": "Video connected",
  "视频连接异常": "Video connection error",
  "视频连接已关闭": "Video connection closed",
  "高清视频已连接": "Clear video connected",
  "消息发送失败": "Message could not be sent",
  "音频通道正在连接，请稍后再试": "Audio is still connecting. Try again shortly.",
  "没有检测到可用麦克风": "No microphone was found",
  "无法使用麦克风，请在系统设置或浏览器站点设置中允许麦克风访问后重试。": "Microphone access is blocked. Allow it in System Settings or the browser's site settings, then try again.",
  "麦克风正在被其他应用占用，请关闭占用后重试。": "The microphone is being used by another app. Close that app and try again.",
  "麦克风启动失败，请稍后重试。": "The microphone could not start. Try again shortly.",
  "无法开启麦克风，请检查权限后重试。": "Could not turn on the microphone. Check its permission and try again.",
  "录制画面还在准备，请稍后再试。": "The recording picture is still preparing. Try again shortly.",
  "系统录制组件发生错误。": "The system recording component encountered an error.",
  "录制正在切换状态，请稍后再停止共享。": "Recording is changing state. Wait before stopping sharing.",
  "录制画面仍在准备，请稍后再关闭应用。": "The recording picture is still being prepared. Wait before closing the app.",
  "请先停止录制并等待文件保存完成，再返回主页。": "Stop recording and wait for the file to finish saving before returning home.",
  "复制以下链接到其他设备浏览器打开：": "Copy this link and open it in a browser on another device:",
  "控制通道正在连接，请稍后再举手。": "Meeting controls are still connecting. Try raising your hand again shortly.",
  "聊天通道正在连接，请稍后再试": "Chat is still connecting. Try again shortly.",
  "读取可共享内容超时，请关闭弹窗后重试。": "Loading shareable content timed out. Close this window and try again.",
  "切换共享内容超时。原共享内容仍在继续，请刷新来源列表后重试。": "Switching shared content timed out. The previous source is still being shared; refresh the source list and try again.",
  "已有会议正在进行，请先返回会议或停止当前共享。": "A meeting is already running. Return to it or stop the current share first.",
  "当前没有正在进行的会议": "There is no active meeting.",
  "观看者已离开会议": "The viewer has left the meeting.",
  "不能移除主持人": "The host cannot be removed.",
  "不能禁言主持人": "The host cannot be muted.",
  "不能修改主持人的举手状态": "The host's hand status cannot be changed.",
  "当前没有正在进行的屏幕共享。": "There is no active screen share.",
  "当前会议已经在录制。": "This meeting is already being recorded.",
  "当前没有正在进行的录制。": "There is no active recording.",
  "录制没有生成有效内容，空文件已自动清理，请重新录制。": "The recording contained no valid media. The empty file was removed; please record again.",
  "无法读取录制分片。": "Could not read the recording segment.",
  "录制分片格式无效。": "The recording segment format is invalid.",
  "只能打开 OrbShare 录制目录中的视频文件。": "Only video files inside the OrbShare recordings folder can be shown.",
  "无法读取系统影片目录：{reason}": "Could not access the system Movies folder: {reason}",
  "无法读取录制目录：{reason}": "Could not access the recordings folder: {reason}",
  "录制文件不存在：{reason}": "The recording file does not exist: {reason}",
  "无法在 Finder 中显示录制文件：{reason}": "Could not show the recording in Finder: {reason}",
  "无法在文件资源管理器中显示录制文件：{reason}": "Could not show the recording in File Explorer: {reason}",
  "无法打开录制目录：{reason}": "Could not open the recordings folder: {reason}",
  "无法生成唯一的录制文件名，请稍后重试。": "A unique recording file name could not be created. Try again shortly.",
  "单个录制分片过大，已停止写入以保护应用内存。": "A recording segment was too large, so writing stopped to protect app memory.",
  "当前系统 WebView 不支持应用内 MP4/WebM 录制，请升级 macOS 后重试。": "This system WebView does not support in-app MP4/WebM recording. Upgrade macOS and try again.",
  "部分录制分片写入失败，已保留可恢复文件：{reason}": "Some recording segments could not be written. A recoverable file was kept: {reason}",
  "{reason}: 首帧超时 (8s)": "{reason}: First picture timed out (8s)",
  "无法切换共享内容：{reason}": "Could not switch shared content: {reason}",
  "生成共享来源预览失败：{reason}": "Could not create a share-source preview: {reason}",
};

const dynamicEnglishPatterns: Array<{
  pattern: RegExp;
  template: string;
  variable: string;
}> = [
  { pattern: /^(.*): 首帧超时 \(8s\)$/, template: "{reason}: 首帧超时 (8s)", variable: "reason" },
  { pattern: /^部分录制分片写入失败，已保留可恢复文件：(.*)$/, template: "部分录制分片写入失败，已保留可恢复文件：{reason}", variable: "reason" },
  { pattern: /^无法读取系统影片目录：(.*)$/, template: "无法读取系统影片目录：{reason}", variable: "reason" },
  { pattern: /^无法读取录制目录：(.*)$/, template: "无法读取录制目录：{reason}", variable: "reason" },
  { pattern: /^录制文件不存在：(.*)$/, template: "录制文件不存在：{reason}", variable: "reason" },
  { pattern: /^无法在 Finder 中显示录制文件：(.*)$/, template: "无法在 Finder 中显示录制文件：{reason}", variable: "reason" },
  { pattern: /^无法在文件资源管理器中显示录制文件：(.*)$/, template: "无法在文件资源管理器中显示录制文件：{reason}", variable: "reason" },
  { pattern: /^无法打开录制目录：(.*)$/, template: "无法打开录制目录：{reason}", variable: "reason" },
  { pattern: /^无法切换共享内容：(.*)$/, template: "无法切换共享内容：{reason}", variable: "reason" },
  { pattern: /^生成共享来源预览失败：(.*)$/, template: "生成共享来源预览失败：{reason}", variable: "reason" },
  { pattern: /^桌面 (\d+)(?:（主屏幕）| \(主屏幕\))$/, template: "桌面 {number}（主屏幕）", variable: "number" },
  { pattern: /^桌面 (\d+)$/, template: "桌面 {number}", variable: "number" },
];

const chineseAliases: Record<string, string> = {
  "Internal screen meeting": "内部屏幕会议",
};

function translateEnglish(source: string): string {
  const exact = en[source];
  if (exact) return exact;
  for (const { pattern, template, variable } of dynamicEnglishPatterns) {
    const match = source.match(pattern);
    if (match) return interpolate(en[template] ?? template, { [variable]: match[1] });
  }
  return source;
}

export function describeMicrophoneError(error: unknown): string {
  const name = error instanceof DOMException
    ? error.name
    : typeof error === "object" && error && "name" in error
      ? String((error as { name?: unknown }).name ?? "")
      : "";
  const message = error instanceof Error ? error.message : String(error ?? "");
  if (name === "NotAllowedError" || name === "SecurityError" || /denied permission|not allowed|denied/i.test(message)) {
    return "无法使用麦克风，请在系统设置或浏览器站点设置中允许麦克风访问后重试。";
  }
  if (name === "NotFoundError" || name === "DevicesNotFoundError") return "没有检测到可用麦克风";
  if (name === "NotReadableError" || name === "TrackStartError") return "麦克风正在被其他应用占用，请关闭占用后重试。";
  if (name === "AbortError") return "麦克风启动失败，请稍后重试。";
  return "无法开启麦克风，请检查权限后重试。";
}

function detectLanguage(): AppLanguage {
  try {
    const saved = window.localStorage.getItem(STORAGE_KEY);
    if (saved === "zh-CN" || saved === "en-US") return saved;
  } catch {
    // Storage can be disabled; fall back to the system language.
  }
  const languages = typeof navigator === "undefined"
    ? []
    : [...(navigator.languages ?? []), navigator.language].filter(Boolean);
  return languages.some((language) => language.toLowerCase().startsWith("zh"))
    ? "zh-CN"
    : "en-US";
}

function interpolate(value: string, variables?: Variables): string {
  if (!variables) return value;
  return value.replace(/\{(\w+)\}/g, (match, key: string) => (
    Object.prototype.hasOwnProperty.call(variables, key) ? String(variables[key]) : match
  ));
}

interface LanguageContextValue {
  language: AppLanguage;
  setLanguage: (language: AppLanguage) => void;
  t: (source: string, variables?: Variables) => string;
}

const LanguageContext = createContext<LanguageContextValue | null>(null);

export function LanguageProvider({ children }: { children: ReactNode }) {
  const [language, setLanguageState] = useState<AppLanguage>(detectLanguage);

  const setLanguage = (nextLanguage: AppLanguage) => {
    setLanguageState(nextLanguage);
    try {
      window.localStorage.setItem(STORAGE_KEY, nextLanguage);
    } catch {
      // The selection still applies for this process when storage is disabled.
    }
  };

  useEffect(() => {
    document.documentElement.lang = language;
  }, [language]);

  const value = useMemo<LanguageContextValue>(() => ({
    language,
    setLanguage,
    t: (source, variables) => {
      const normalizedSource = chineseAliases[source] ?? source;
      return interpolate(language === "en-US" ? translateEnglish(normalizedSource) : normalizedSource, variables);
    },
  }), [language]);

  return <LanguageContext.Provider value={value}>{children}</LanguageContext.Provider>;
}

export function useLanguage(): LanguageContextValue {
  const context = useContext(LanguageContext);
  if (!context) throw new Error("useLanguage must be used within LanguageProvider");
  return context;
}

export function LanguageSwitch({ compact = false }: { compact?: boolean }) {
  const { language, setLanguage } = useLanguage();
  const next = language === "zh-CN" ? "en-US" : "zh-CN";
  return (
    <button
      type="button"
      onClick={() => setLanguage(next)}
      title={language === "zh-CN" ? "Switch to English" : "切换为中文"}
      aria-label={language === "zh-CN" ? "Switch to English" : "切换为中文"}
      className={`inline-flex items-center justify-center rounded-lg border border-white/[0.08] bg-white/[0.035] font-medium text-slate-400 transition hover:bg-white/[0.07] hover:text-white ${
        compact ? "h-7 min-w-8 px-2 text-[10px]" : "h-8 min-w-10 px-2.5 text-[10.5px]"
      }`}
    >
      {language === "zh-CN" ? "中文" : "EN"}
    </button>
  );
}
