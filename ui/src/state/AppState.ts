import type { ToastStore } from "../overlays/toasts/store";
import { createToastStore } from "../overlays/toasts/store";
import type { ScopeToken } from "../theme";
import type { StateRouteId } from "../components/states";

/** One dirty editor buffer owned by the app state rather than by a screen. */
export interface EditorBuffer {
  readonly route: StateRouteId;
  readonly value: string;
  readonly savedValue: string;
}

/** Presentation-only snapshot for the whole webview. */
export interface AppSnapshot {
  readonly activeProject: string | null;
  readonly scope: ScopeToken;
  readonly route: StateRouteId;
  readonly routeStack: readonly StateRouteId[];
  readonly screenState: Readonly<Record<string, unknown>>;
  readonly selections: Readonly<Record<string, readonly string[]>>;
  readonly editors: Readonly<Record<string, EditorBuffer>>;
  readonly externalChanges: Readonly<Record<string, { readonly contentHash: string; readonly kept: boolean }>>;
  readonly overlay: string | null;
  readonly backgroundJobs: readonly string[];
  readonly revision: number;
}

/** Explicit effects which the app bridge consumes outside rendering. */
export type AppEffect =
  | { readonly type: "reload-route"; readonly route: StateRouteId; readonly reason: "project" | "scope" | "external" }
  | { readonly type: "persist-last-project"; readonly projectId: string }
  | { readonly type: "dirty-navigation"; readonly from: StateRouteId; readonly to: StateRouteId };

/** Effect sink; injected so tests never call Tauri or the filesystem. */
export interface AppStatePorts {
  readonly effects?: (effect: AppEffect) => void;
  readonly toasts?: ToastStore;
}

/** Dirty-navigation choice. */
export type DirtyDecision = "save" | "discard" | "cancel";

/** App state and typed user/event actions for UI presentation. */
export interface AppState {
  readonly toasts: ToastStore;
  getSnapshot(): AppSnapshot;
  subscribe(listener: () => void): () => void;
  /** Project-selection event clears project-bound caches and schedules one route reload. */
  selectProject(projectId: string | null): Promise<void>;
  /** Scope-authority event from TA-053 schedules exactly one route reload. */
  receiveScope(scope: ScopeToken): void;
  /** Route change with an explicit dirty-buffer decision. */
  navigate(route: StateRouteId, decision?: DirtyDecision): boolean;
  /** CLI/agent watcher events coalesce throughout the debounce interval. */
  requestExternalReload(): void;
  /** Marks a dirty screen's file as changed externally. */
  markExternalChange(route: StateRouteId, contentHash: string): void;
  /** Resolves the dirty-file banner without silently replacing the editor. */
  resolveExternalChange(route: StateRouteId, choice: "reload" | "keep", content?: string): void;
  setEditorBuffer(route: StateRouteId, buffer: EditorBuffer): void;
  setSelection(key: string, ids: readonly string[]): void;
  setScreenState(key: string, value: unknown): void;
  setOverlay(overlay: string | null): void;
  setJob(jobId: string, active: boolean): void;
}

/** Creates one isolated app state instance. */
export function createAppState(ports: AppStatePorts = {}): AppState {
  const listeners = new Set<() => void>();
  const effects = ports.effects ?? (() => undefined);
  let snapshot: AppSnapshot = {
    activeProject: null,
    scope: "user",
    route: "overview",
    routeStack: ["overview"],
    screenState: {},
    selections: {},
    editors: {},
    externalChanges: {},
    backgroundJobs: [],
    overlay: null,
    revision: 0,
  };
  let externalReloadTimer: number | undefined;

  function publish(next: Omit<AppSnapshot, "revision">): void {
    snapshot = { ...next, revision: snapshot.revision + 1 };
    for (const listener of [...listeners]) listener();
  }

  function update(patch: Partial<Omit<AppSnapshot, "revision">>): void {
    publish({ ...snapshot, ...patch });
  }

  function reload(reason: "project" | "scope" | "external"): void {
    effects({ type: "reload-route", route: snapshot.route, reason });
  }

  return {
    toasts: ports.toasts ?? createToastStore(),
    getSnapshot: () => snapshot,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    async selectProject(projectId) {
      if (projectId === snapshot.activeProject) return;
      update({
        activeProject: projectId,
        screenState: {},
        selections: {},
        editors: {},
        externalChanges: {},
        overlay: null,
      });
      if (projectId !== null) effects({ type: "persist-last-project", projectId });
      reload("project");
    },
    receiveScope(scope) {
      if (scope === snapshot.scope) return;
      update({ scope });
      reload("scope");
    },
    navigate(route, decision = "cancel") {
      const buffer = snapshot.editors[snapshot.route];
      const dirty = buffer !== undefined && buffer.value !== buffer.savedValue;
      if (dirty && decision === "cancel") {
        effects({ type: "dirty-navigation", from: snapshot.route, to: route });
        return false;
      }
      const editors = { ...snapshot.editors };
      if (dirty && decision === "discard") delete editors[snapshot.route];
      if (dirty && decision === "save") {
        editors[snapshot.route] = { ...buffer, savedValue: buffer.value };
      }
      if (route === snapshot.route) return true;
      update({ route, routeStack: [...snapshot.routeStack, route].slice(-50), editors, overlay: null });
      return true;
    },
    requestExternalReload() {
      window.clearTimeout(externalReloadTimer);
      externalReloadTimer = window.setTimeout(() => {
        externalReloadTimer = undefined;
        reload("external");
      }, 100);
    },
    markExternalChange(route, contentHash) {
      const editor = snapshot.editors[route];
      if (editor !== undefined && editor.value !== editor.savedValue) {
        update({ externalChanges: { ...snapshot.externalChanges, [route]: { contentHash, kept: false } } });
      } else {
        reload("external");
      }
    },
    resolveExternalChange(route, choice, content) {
      if (choice === "reload") {
        if (content === undefined) return;
        const editor = snapshot.editors[route];
        if (editor !== undefined) {
          update({
            editors: { ...snapshot.editors, [route]: { ...editor, value: content, savedValue: content } },
            externalChanges: Object.fromEntries(Object.entries(snapshot.externalChanges).filter(([key]) => key !== route)),
          });
        }
        return;
      }
      const change = snapshot.externalChanges[route];
      if (change !== undefined) {
        update({ externalChanges: { ...snapshot.externalChanges, [route]: { ...change, kept: true } } });
      }
    },
    setEditorBuffer(route, buffer) {
      update({ editors: { ...snapshot.editors, [route]: buffer } });
    },
    setSelection(key, ids) {
      update({ selections: { ...snapshot.selections, [key]: [...ids] } });
    },
    setScreenState(key, value) {
      update({ screenState: { ...snapshot.screenState, [key]: value } });
    },
    setOverlay(overlay) {
      update({ overlay });
    },
    setJob(jobId, active) {
      const jobs = new Set(snapshot.backgroundJobs);
      if (active) jobs.add(jobId);
      else jobs.delete(jobId);
      update({ backgroundJobs: [...jobs] });
    },
  };
}
