import { invoke } from "@tauri-apps/api/core";
import type { OverwriteMoveAsideView } from "../../generated/OverwriteMoveAsideView";
import type { OverwriteRemedyResult } from "./OverwriteRefusalDialog";

/** Invokes the backend's path-guarded at-fs move-aside command. */
export async function moveRefusedItemAside(path: string): Promise<OverwriteRemedyResult> {
  const result = await invoke<OverwriteMoveAsideView>("overwrite_move_aside", { path });
  return {
    summary: "The unmanaged item was moved aside without replacing its contents.",
    movedTo: result.moved_to,
  };
}

/** Publishes only structured RefusedOverwrite failures for the global dialog host. */
export function publishRefusedOverwrite(error: unknown): boolean {
  if (error === null || typeof error !== "object") return false;
  const value = error as Record<string, unknown>;
  const refusal = value["refusal"];
  if ((value["code"] !== "refused_overwrite" && value["code"] !== "E_REFUSED_OVERWRITE") || refusal === null || typeof refusal !== "object") return false;
  window.dispatchEvent(new CustomEvent("agentuna:refused-overwrite", { detail: error }));
  return true;
}
