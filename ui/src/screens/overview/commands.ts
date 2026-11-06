import { invoke } from "@tauri-apps/api/core";
import type { OverviewView } from "../../generated/OverviewView";
import type { ProjectSyncApplyView } from "../../generated/ProjectSyncApplyView";
import type { ProjectSyncItemView } from "../../generated/ProjectSyncItemView";
import type { ProjectSyncPreviewView } from "../../generated/ProjectSyncPreviewView";

/** Loads the database-backed Overview projections for the currently selected scope. */
export function loadOverview(activeScope: string): Promise<OverviewView> {
  return invoke<OverviewView>("overview_load", { activeScope });
}

/** Requests one-shot, user-approved preparation of the Global store for writes. */
export function enableGlobalWrites(): Promise<void> {
  return invoke<void>("overview_enable_global_writes");
}

/** Loads Project skill labels from the existing TA-005 sync projection. */
export function loadProjectSyncStatus(projectId: string): Promise<ProjectSyncItemView[]> {
  return invoke<ProjectSyncItemView[]>("project_sync_status", { projectId });
}

/** Previews the exact files and diff for a Project/User sync. */
export function previewProjectSync(projectId: string, itemId: string, direction: "push" | "pull"): Promise<ProjectSyncPreviewView> {
  return invoke<ProjectSyncPreviewView>("project_sync_preview", { projectId, itemId, direction });
}

/** Applies a reviewed sync after explicit confirmation. */
export function applyProjectSync(previewId: string): Promise<ProjectSyncApplyView> {
  return invoke<ProjectSyncApplyView>("project_sync_apply", { previewId, confirmed: true });
}
