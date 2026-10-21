import type { CaptureSource } from "../lib/types";
import { useLanguage } from "../i18n";

interface SourcePickerProps {
  open: boolean;
  sources: CaptureSource[];
  selected: CaptureSource | null;
  loading: boolean;
  switching?: boolean;
  title?: string;
  confirmLabel?: string;
  error?: string;
  onSelect: (source: CaptureSource) => void;
  onRefresh: () => void;
  onCancel: () => void;
  onConfirm: () => void;
}

function SourcePlaceholder({ isDisplay }: { isDisplay: boolean }) {
  return (
    <div className="flex h-full w-full items-center justify-center bg-[radial-gradient(circle_at_50%_30%,rgba(59,130,246,.16),transparent_55%),#0a0d13] text-slate-500">
      {isDisplay ? (
        <svg aria-hidden="true" viewBox="0 0 24 24" className="h-9 w-9" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round"><rect x="2.5" y="4" width="19" height="13" rx="2" /><path d="M8 21h8M12 17v4" /></svg>
      ) : (
        <svg aria-hidden="true" viewBox="0 0 24 24" className="h-9 w-9" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round"><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M3 8h18M7 6h.01M10 6h.01" /></svg>
      )}
    </div>
  );
}

export default function SourcePicker({
  open,
  sources,
  selected,
  loading,
  switching = false,
  title,
  confirmLabel,
  error = "",
  onSelect,
  onRefresh,
  onCancel,
  onConfirm,
}: SourcePickerProps) {
  const { t } = useLanguage();
  if (!open) return null;
  const dialogTitle = title ?? t("选择共享内容");
  const dialogConfirmLabel = confirmLabel ?? t("开始共享");
  const desktops = sources.filter((source) => source.is_display);
  const windows = sources.filter((source) => !source.is_display);

  const renderSource = (source: CaptureSource) => {
    const isSelected = selected?.id === source.id
      && selected?.is_display === source.is_display;
    const sourceName = t(source.name);
    return (
      <button
        type="button"
        key={`${source.is_display ? "display" : "window"}-${source.id}`}
        onClick={() => onSelect(source)}
        className={`group min-w-0 overflow-hidden rounded-xl border text-left transition ${
          isSelected
            ? "border-blue-400 bg-blue-500/10 shadow-[0_0_0_2px_rgba(96,165,250,.15)]"
            : "border-white/[0.08] bg-[#10141c] hover:border-white/20 hover:bg-[#141923]"
        }`}
      >
        <div className="relative aspect-[16/10] overflow-hidden border-b border-white/[0.06] bg-black">
          {source.thumbnail_data_url ? (
            <img src={source.thumbnail_data_url} alt="" className="h-full w-full object-contain" draggable={false} />
          ) : <SourcePlaceholder isDisplay={source.is_display} />}
          <span className={`absolute left-2 top-2 rounded-md px-1.5 py-0.5 text-[9px] font-semibold backdrop-blur ${
            source.is_display ? "bg-blue-500/80 text-white" : "bg-black/65 text-slate-200"
          }`}>
            {source.is_display ? t("桌面") : t("窗口")}
          </span>
          {isSelected && (
            <span className="absolute right-2 top-2 flex h-5 w-5 items-center justify-center rounded-full bg-blue-500 text-white shadow-lg">
              <svg viewBox="0 0 24 24" className="h-3.5 w-3.5" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round"><path d="m5 12 4 4L19 6" /></svg>
            </span>
          )}
        </div>
        <div className="px-3 py-2.5">
          <div className="truncate text-[11.5px] font-medium text-slate-100" title={sourceName}>{sourceName}</div>
          <div className="mt-0.5 text-[9.5px] text-slate-500">{source.width}×{source.height}</div>
        </div>
      </button>
    );
  };

  return (
    <div className="fixed inset-0 z-[100] flex items-center justify-center bg-black/70 p-6 backdrop-blur-sm" role="dialog" aria-modal="true" aria-label={dialogTitle}>
      <div className="flex h-[min(650px,calc(100vh-48px))] w-[min(940px,calc(100vw-48px))] flex-col overflow-hidden rounded-2xl border border-white/[0.11] bg-[#0d1016] shadow-2xl shadow-black/70">
        <div className="flex shrink-0 items-center justify-between border-b border-white/[0.07] px-5 py-4">
          <div>
            <h2 className="text-[16px] font-semibold text-white">{dialogTitle}</h2>
            <p className="mt-1 text-[10.5px] text-slate-500">{t("共享桌面可看到所有切换内容；共享窗口只会显示所选软件。")}</p>
          </div>
          <button type="button" onClick={onRefresh} disabled={loading || switching} className="inline-flex items-center gap-1.5 rounded-lg border border-white/[0.08] bg-white/[0.035] px-3 py-1.5 text-[10.5px] text-slate-300 transition hover:bg-white/[0.07] disabled:opacity-50">
            <svg viewBox="0 0 24 24" className={`h-3.5 w-3.5 ${loading ? "animate-spin" : ""}`} fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><path d="M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7" /></svg>
            {t("刷新")}
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
          {loading ? (
            <div className="flex h-full items-center justify-center gap-2 text-[11.5px] text-slate-400"><span className="h-4 w-4 animate-spin rounded-full border-2 border-blue-400/25 border-t-blue-400" />{t("正在读取可共享内容…")}</div>
          ) : sources.length === 0 ? (
            <div className="flex h-full flex-col items-center justify-center text-center"><div className="text-[12px] text-slate-300">{t("没有找到可共享内容")}</div><div className="mt-1 text-[10px] text-slate-500">{t("请确认已授予“屏幕与系统录音”权限，然后刷新。")}</div></div>
          ) : (
            <div className="space-y-5">
              {desktops.length > 0 && <section><div className="mb-2.5 text-[10.5px] font-medium text-slate-400">{t("桌面")}</div><div className="grid grid-cols-3 gap-3">{desktops.map(renderSource)}</div></section>}
              {windows.length > 0 && <section><div className="mb-2.5 flex items-center justify-between"><span className="text-[10.5px] font-medium text-slate-400">{t("应用窗口")}</span><span className="text-[9.5px] text-slate-600">{t("共 {count} 个", { count: windows.length })}</span></div><div className="grid grid-cols-3 gap-3">{windows.map(renderSource)}</div></section>}
            </div>
          )}
        </div>

        <div className="flex shrink-0 items-center justify-between gap-4 border-t border-white/[0.07] bg-[#0a0d12] px-5 py-3.5">
          <div className="min-w-0 text-[10px] text-slate-500">
            {error ? <span className="text-rose-300">{t(error)}</span> : selected ? <span className="truncate">{t("已选择：")}<span className="text-slate-300">{t(selected.name)}</span></span> : t("请选择一个共享来源")}
          </div>
          <div className="flex shrink-0 gap-2">
            <button type="button" onClick={onCancel} disabled={switching} className="rounded-lg border border-white/[0.09] px-4 py-2 text-[11px] font-medium text-slate-300 transition hover:bg-white/[0.06] disabled:opacity-50">{t("取消")}</button>
            <button type="button" onClick={onConfirm} disabled={!selected || loading || switching} className="inline-flex min-w-[110px] items-center justify-center gap-2 rounded-lg bg-blue-500 px-4 py-2 text-[11px] font-semibold text-white transition hover:bg-blue-400 disabled:cursor-not-allowed disabled:opacity-45">
              {switching && <span className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-white/30 border-t-white" />}
              {switching ? t("正在切换…") : dialogConfirmLabel}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
