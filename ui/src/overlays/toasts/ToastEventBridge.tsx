import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import type { DriftNoticeView } from "../../generated/DriftNoticeView";
import { useAppState } from "../../state/context";
import { showDriftDetectedToast } from "./catalogue";
import { useToast } from "./ToastProvider";

const DRIFT_TOAST_DEDUPE_MS = 10_000;

/** Bridges only new watcher detections into the app-wide toast service. */
export function ToastEventBridge() {
  const app = useAppState();
  const toast = useToast();
  const lastShownByItem = useRef(new Map<string, number>());

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<DriftNoticeView[]>("agentuna:drift-detected", ({ payload }) => {
      const now = Date.now();
      for (const [key, timestamp] of lastShownByItem.current) {
        if (now - timestamp >= DRIFT_TOAST_DEDUPE_MS) lastShownByItem.current.delete(key);
      }
      for (const notice of payload) {
        if (!notice.toast_eligible) continue;
        const identity = notice.item_id ?? `${notice.agent}:${notice.path}`;
        const previous = lastShownByItem.current.get(identity);
        if (previous !== undefined && now - previous < DRIFT_TOAST_DEDUPE_MS) continue;
        lastShownByItem.current.set(identity, now);
        const pathParts = notice.path.split(/[\\/]/).filter(Boolean);
        const filename = pathParts[pathParts.length - 1];
        const itemName = filename?.toLowerCase() === "skill.md" && pathParts.length > 1
          ? pathParts[pathParts.length - 2] ?? filename
          : filename || "A file";
        showDriftDetectedToast(toast, itemName, () => {
          app.setSelection("drift", [String(notice.event_id)]);
          app.navigate("drift");
        });
      }
    }).then((dispose) => {
      if (disposed) dispose();
      else unlisten = dispose;
    }, () => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [app, toast]);

  return null;
}
