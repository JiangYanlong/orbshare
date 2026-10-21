import { useEffect, useMemo, useState } from "react";
import QRCode from "qrcode";
import type { RoomInfo } from "../lib/types";
import { useLanguage } from "../i18n";

interface MeetingInviteDialogProps {
  open: boolean;
  room: RoomInfo;
  password?: string;
  onClose: () => void;
}

type CopiedTarget = "link" | "password" | "invitation" | null;

export default function MeetingInviteDialog({ open, room, password = "", onClose }: MeetingInviteDialogProps) {
  const { t } = useLanguage();
  const [qrDataUrl, setQrDataUrl] = useState("");
  const [qrError, setQrError] = useState("");
  const [copiedTarget, setCopiedTarget] = useState<CopiedTarget>(null);
  const [passwordVisible, setPasswordVisible] = useState(false);
  const inviteUrl = useMemo(
    () => `http://${room.host_ip}:${room.viewer_port}/`,
    [room.host_ip, room.viewer_port],
  );
  const localizedRoomName = t(room.room_name);
  const invitationText = useMemo(() => [
    t("邀请你加入 OrbShare 局域网会议"),
    t("会议名称：{name}", { name: localizedRoomName }),
    t("加入链接：{url}", { url: inviteUrl }),
    room.requires_password
      ? password ? t("会议密码：{password}", { password }) : t("会议设有密码，请向主持人获取。")
      : t("无需会议密码。"),
    t("请确保设备与主持人连接到同一局域网。"),
  ].join("\n"), [inviteUrl, localizedRoomName, password, room.requires_password, t]);

  useEffect(() => {
    if (!open) {
      setPasswordVisible(false);
      return;
    }
    let cancelled = false;
    setQrDataUrl("");
    setQrError("");
    void QRCode.toDataURL(inviteUrl, {
      errorCorrectionLevel: "M",
      margin: 2,
      width: 384,
      color: { dark: "#08111f", light: "#ffffff" },
    }).then((dataUrl) => {
      if (!cancelled) setQrDataUrl(dataUrl);
    }).catch((error: unknown) => {
      if (!cancelled) setQrError(error instanceof Error ? error.message : String(error));
    });
    return () => {
      cancelled = true;
    };
  }, [inviteUrl, open]);

  useEffect(() => {
    if (!open) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [onClose, open]);

  useEffect(() => {
    if (!copiedTarget) return;
    const timer = window.setTimeout(() => setCopiedTarget(null), 1800);
    return () => window.clearTimeout(timer);
  }, [copiedTarget]);

  const copyText = async (text: string, target: Exclude<CopiedTarget, null>) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopiedTarget(target);
    } catch {
      window.prompt(t("请复制以下内容："), text);
    }
  };

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-[120] flex items-center justify-center bg-black/75 p-6 backdrop-blur-md"
      role="dialog"
      aria-modal="true"
      aria-labelledby="meeting-invite-title"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="w-[min(720px,calc(100vw-48px))] overflow-hidden rounded-2xl border border-white/[0.11] bg-[#10141c] shadow-2xl shadow-black/70">
        <div className="flex items-center justify-between border-b border-white/[0.07] px-5 py-4">
          <div>
            <h2 id="meeting-invite-title" className="text-[16px] font-semibold text-white">{t("邀请成员加入会议")}</h2>
            <p className="mt-1 text-[10.5px] text-slate-500">{t("手机或平板扫码即可通过浏览器加入，无需安装应用。")}</p>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label={t("关闭邀请窗口")}
            className="flex h-8 w-8 items-center justify-center rounded-lg border border-white/[0.07] text-slate-400 transition hover:bg-white/[0.06] hover:text-white"
          >
            <svg viewBox="0 0 24 24" className="h-4 w-4" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><path d="M18 6 6 18M6 6l12 12" /></svg>
          </button>
        </div>

        <div className="grid grid-cols-[250px_minmax(0,1fr)] gap-5 p-5">
          <div className="rounded-2xl bg-white p-3 shadow-[0_18px_50px_rgba(0,0,0,.28)]">
            <div className="aspect-square overflow-hidden rounded-xl bg-white">
              {qrDataUrl ? (
                <img src={qrDataUrl} alt={`${localizedRoomName} · ${t("加入二维码")}`} className="h-full w-full" draggable={false} />
              ) : qrError ? (
                <div className="flex h-full items-center justify-center px-5 text-center text-[11px] text-rose-500">{t("二维码生成失败")}<br />{t("请复制右侧链接加入")}</div>
              ) : (
                <div className="flex h-full items-center justify-center gap-2 text-[11px] text-slate-500">
                  <span className="h-4 w-4 animate-spin rounded-full border-2 border-slate-300 border-t-blue-500" />{t("正在生成二维码")}
                </div>
              )}
            </div>
            <div className="mt-2 text-center text-[10px] font-medium text-slate-700">{t("使用手机相机或浏览器扫码")}</div>
          </div>

          <div className="flex min-w-0 flex-col">
            <div className="rounded-xl border border-white/[0.07] bg-black/15 p-4">
              <div className="text-[9.5px] font-medium uppercase tracking-[0.12em] text-slate-600">{t("会议名称")}</div>
              <div className="mt-1.5 truncate text-[15px] font-semibold text-white" title={localizedRoomName}>{localizedRoomName}</div>
              <div className="mt-4 text-[9.5px] font-medium uppercase tracking-[0.12em] text-slate-600">{t("加入链接")}</div>
              <div className="mt-1.5 break-all rounded-lg border border-blue-400/15 bg-blue-500/[0.06] px-3 py-2 font-mono text-[11px] leading-4 text-blue-300">{inviteUrl}</div>
              <div className="mt-3 flex items-center gap-2 text-[10px] text-slate-500">
                <span className={`h-1.5 w-1.5 rounded-full ${room.requires_password ? "bg-amber-400" : "bg-emerald-400"}`} />
                {room.requires_password ? t("加入时需要输入会议密码") : t("当前会议无需密码")}
              </div>
              {room.requires_password && (
                <div className="mt-4 border-t border-white/[0.06] pt-3">
                  <div className="flex items-center justify-between gap-3">
                    <div className="min-w-0 flex-1">
                      <div className="text-[9.5px] font-medium uppercase tracking-[0.12em] text-slate-600">{t("会议密码")}</div>
                      <div className="mt-1.5 h-9 rounded-lg border border-amber-400/15 bg-amber-400/[0.05] px-3 font-mono text-[12px] leading-9 text-amber-200">
                        {password ? passwordVisible ? password : "••••••••" : t("暂时无法读取")}
                      </div>
                    </div>
                    <div className="flex shrink-0 gap-1.5 pt-5">
                      <button
                        type="button"
                        disabled={!password}
                        onClick={() => setPasswordVisible((visible) => !visible)}
                        className="h-9 rounded-lg border border-white/[0.08] bg-white/[0.035] px-2.5 text-[10px] text-slate-300 transition hover:bg-white/[0.07] disabled:cursor-not-allowed disabled:opacity-40"
                      >
                        {passwordVisible ? t("隐藏") : t("显示")}
                      </button>
                      <button
                        type="button"
                        disabled={!password}
                        onClick={() => void copyText(password, "password")}
                        className="h-9 rounded-lg border border-amber-400/15 bg-amber-400/[0.06] px-2.5 text-[10px] text-amber-200 transition hover:bg-amber-400/[0.12] disabled:cursor-not-allowed disabled:opacity-40"
                      >
                        {copiedTarget === "password" ? t("已复制 ✓") : t("复制")}
                      </button>
                    </div>
                  </div>
                </div>
              )}
            </div>

            <div className="mt-auto grid grid-cols-2 gap-2 pt-4">
              <button
                type="button"
                onClick={() => void copyText(inviteUrl, "link")}
                className="inline-flex h-10 items-center justify-center gap-2 rounded-xl border border-white/[0.09] bg-white/[0.04] text-[11px] font-medium text-slate-200 transition hover:bg-white/[0.08]"
              >
                <svg viewBox="0 0 24 24" className="h-3.5 w-3.5" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
                {copiedTarget === "link" ? t("链接已复制 ✓") : t("复制链接")}
              </button>
              <button
                type="button"
                onClick={() => void copyText(invitationText, "invitation")}
                className="inline-flex h-10 items-center justify-center gap-2 rounded-xl bg-gradient-to-r from-blue-600 to-cyan-500 text-[11px] font-semibold text-white shadow-lg shadow-blue-950/30 transition hover:brightness-110"
              >
                <svg viewBox="0 0 24 24" className="h-3.5 w-3.5" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M4 4h16v12H5.17L4 17.17V4Z" /><path d="m9 8 3 3 3-3" /></svg>
                {copiedTarget === "invitation" ? t("邀请信息已复制 ✓") : t("复制完整邀请")}
              </button>
            </div>
            <p className="mt-3 text-[9.5px] leading-4 text-slate-600">{t("二维码仅包含局域网加入地址，不包含会议密码；离开当前网络后无法访问。")}</p>
          </div>
        </div>
      </div>
    </div>
  );
}
