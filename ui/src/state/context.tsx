import { createContext, useContext, useSyncExternalStore, type ReactNode } from "react";
import type { AppState, AppSnapshot } from "./AppState";

const AppStateContext = createContext<AppState | null>(null);

/** Provides one app-level state instance to all routed screens. */
export function AppStateProvider({ state, children }: { readonly state: AppState; readonly children: ReactNode }) {
  return <AppStateContext.Provider value={state}>{children}</AppStateContext.Provider>;
}

/** Reads the current immutable presentation snapshot. */
export function useAppSnapshot(): AppSnapshot {
  const state = useAppState();
  return useSyncExternalStore(state.subscribe, state.getSnapshot, state.getSnapshot);
}

/** Reads the shared app state service. */
export function useAppState(): AppState {
  const state = useContext(AppStateContext);
  if (state === null) throw new Error("useAppState must be used inside <AppStateProvider>");
  return state;
}
