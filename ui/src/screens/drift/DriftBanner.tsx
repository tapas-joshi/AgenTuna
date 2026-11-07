import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { DriftSnapshotView } from "../../generated/DriftSnapshotView";
import { useAppState } from "../../state/context";

/** Compact link shown above the Skills and MCP inventories when drift is open. */
export function DriftBanner() {
  const app = useAppState();
  const [count, setCount] = useState(0);

  useEffect(() => {
    let active = true;
    void invoke<DriftSnapshotView>("drift_snapshot")
      .then((snapshot) => { if (active) setCount(snapshot.rows.length); })
      .catch(() => { if (active) setCount(0); });
    return () => { active = false; };
  }, []);

  if (count === 0) return null;
  return <aside aria-label="External changes need review" className="mb-12 flex flex-wrap items-center justify-between gap-8 rounded-8 border border-warning/30 bg-warning/10 px-12 py-8 text-12 text-text">
    <span><strong>{count} external change{count === 1 ? "" : "s"}</strong> need review before you sync.</span>
    <button type="button" onClick={() => app.navigate("drift")} className="rounded-6 px-8 py-5 font-medium text-accent hover:bg-overlay focus-visible:outline-2 focus-visible:outline-focus">Review drift</button>
  </aside>;
}
