import type { CaptureSource } from "./types";
import { getCaptureSourceThumbnail } from "./tauri-commands";

/**
 * Fill source previews after metadata has already rendered. Limiting the worker
 * count prevents a desktop with many windows from creating a CPU/JPEG spike.
 */
export async function hydrateCaptureSourceThumbnails(
  sources: CaptureSource[],
  onThumbnail: (source: CaptureSource, thumbnailDataUrl: string) => void,
): Promise<void> {
  let cursor = 0;
  const worker = async () => {
    while (cursor < sources.length) {
      const source = sources[cursor++];
      try {
        const thumbnail = await getCaptureSourceThumbnail(source.id, source.is_display);
        if (thumbnail) onThumbnail(source, thumbnail);
      } catch {
        // A window can close while the picker is open. Keep its placeholder;
        // the next metadata refresh will remove it.
      }
    }
  };

  await Promise.all(Array.from({ length: Math.min(2, sources.length) }, () => worker()));
}
