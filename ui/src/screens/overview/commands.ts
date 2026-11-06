import { invoke } from "@tauri-apps/api/core";
import type { OverviewView } from "../../generated/OverviewView";

/** Loads the database-backed Overview projections for the currently selected scope. */
export function loadOverview(activeScope: string): Promise<OverviewView> {
  return invoke<OverviewView>("overview_load", { activeScope });
}
