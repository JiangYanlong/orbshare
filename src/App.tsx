import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import Home from "./pages/Home";
import Viewer from "./pages/Viewer";
import { getActiveRoom, revealRecording } from "./lib/tauri-commands";
import type { RecordingResult, RoomInfo } from "./lib/types";
import { useLanguage } from "./i18n";

type Screen =
  | { mode: "home" }
  | { mode: "hosting" }
  | { mode: "viewing"; address: string; port: number };

function App() {
  const { t } = useLanguage();
  const [screen, setScreen] = useState<Screen>({ mode: "home" });
  const [activeRoom, setActiveRoom] = useState<RoomInfo | null>(null);
  const [lastRecording, setLastRecording] = useState<RecordingResult | null>(null);
  const [recordingRevealError, setRecordingRevealError] = useState("");

  useEffect(() => {
    getActiveRoom().then(setActiveRoom).catch(() => setActiveRoom(null));
  }, []);

  return (
    <div className="relative flex h-dvh min-h-0 flex-col overflow-hidden bg-[#090b0f]">
      <div
        data-tauri-drag-region
        onDoubleClick={() => void getCurrentWindow().toggleMaximize()}
        className="h-7 shrink-0 select-none border-b border-white/[0.04] bg-[#090b0f]"
        title={t("双击最大化或还原窗口")}
      />
      <div className={screen.mode === "home" ? "min-h-0 flex-1" : "hidden"}>
        <Home
          activeRoom={activeRoom}
          onResumeRoom={() => setScreen({ mode: "hosting" })}
          onHost={(room) => {
            setActiveRoom(room);
            setScreen({ mode: "hosting" });
          }}
          onJoin={(address, port) => setScreen({ mode: "viewing", address, port })}
        />
      </div>

      {activeRoom && (
        <div className={screen.mode === "hosting" ? "min-h-0 flex-1" : "hidden"}>
          <Viewer
            key={activeRoom.room_id}
            room={activeRoom}
            isHost
            onBack={() => setScreen({ mode: "home" })}
            onRecordingSaved={(result) => {
              setLastRecording(result);
              setRecordingRevealError("");
            }}
            onStopped={() => {
              setActiveRoom(null);
              setScreen({ mode: "home" });
            }}
          />
        </div>
      )}

      {screen.mode === "viewing" && (
        <div className="min-h-0 flex-1">
          <Viewer
            address={screen.address}
            port={screen.port}
            onBack={() => setScreen({ mode: "home" })}
          />
        </div>
      )}

      {lastRecording && (
        <div className="absolute bottom-5 right-5 z-[100] w-[360px] rounded-xl border border-emerald-300/20 bg-[#10201b]/95 p-3 text-emerald-50 shadow-2xl backdrop-blur-xl">
          <div className="flex items-start gap-2.5">
            <span className="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-emerald-400/20 text-[11px] text-emerald-300">✓</span>
            <div className="min-w-0 flex-1">
              <div className="text-[12px] font-semibold">{t("录制已自动保存")}</div>
              <div className="mt-1 truncate font-mono text-[9.5px] text-emerald-100/55" title={lastRecording.path}>{lastRecording.path}</div>
              {recordingRevealError && <div className="mt-1.5 text-[10px] text-rose-300">{t(recordingRevealError)}</div>}
              <button
                type="button"
                onClick={() => void revealRecording(lastRecording.path)
                  .then(() => setRecordingRevealError(""))
                  .catch((error) => setRecordingRevealError(error instanceof Error ? error.message : String(error)))}
                className="mt-2 rounded-md border border-emerald-300/20 bg-emerald-300/10 px-2.5 py-1.5 text-[10.5px] font-medium text-emerald-100 transition hover:bg-emerald-300/20"
              >
                {t("打开所在文件夹")}
              </button>
            </div>
            <button type="button" onClick={() => setLastRecording(null)} className="text-emerald-100/45 transition hover:text-white">×</button>
          </div>
        </div>
      )}
    </div>
  );
}

export default App;
