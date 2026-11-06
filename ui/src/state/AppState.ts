import type { ToastStore } from "../overlays/toasts/store";
import { createToastStore } from "../overlays/toasts/store";
import type { ScopeToken } from "../theme";
import type { StateRouteId } from "../components/states";
import { routeRequiresProject } from "../shell/routes";

/** One dirty editor buffer owned by the app state rather than by a screen. */
export interface EditorBuffer {
  readonly route: StateRouteId;
  readonly value: string;
  readonly savedValue: string;
  /** Saves this buffer through its owning screen command. */
  readonly onSave?: () => void | Promise<void>;
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
  | { readonly type: "dirty-navigation"; readonly from: StateRouteId; readonly to: StateRouteId }
  | { readonly type: "project-picker-required"; readonly route: StateRouteId }
  | { readonly type: "persist-last-scope"; readonly projectId: string; readonly scope: ScopeToken }
  | { readonly type: "restore-project-scope"; readonly projectId: string };

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
  /** Project selection clears project-bound caches and schedules one reload. */
  selectProject(projectId: string | null): Promise<void>;
  /** Hydrates durable startup selections before the shell is shown. */
  restore(projectId: string | null, scope: ScopeToken): void;
  /** Scope-authority event from TA-053 schedules exactly one route reload. */
  receiveScope(scope: ScopeToken): void;
  /** Route change with an explicit dirty-buffer decision. */
  navigate(route: StateRouteId, decision?: DirtyDecision): boolean;
  /** Navigates back to the previous route without adding a history entry. */
  back(decision?: DirtyDecision): boolean;
  /** Handles an item deep link after the backend has resolved its id. */
  openItem(itemId: string, item: { readonly kind: string; readonly scope: string; readonly project_id: string | null } | null): void;
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
  const toasts = ports.toasts ?? createToastStore();
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
    toasts,
    getSnapshot: () => snapshot,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    restore(projectId, scope) {
      update({ activeProject: projectId, scope });
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
      if (projectId !== null) effects({ type: "restore-project-scope", projectId });
      reload("project");
    },
    receiveScope(scope) {
      if (scope === "local" && snapshot.activeProject === null) return;
      if (scope === snapshot.scope) return;
      if (snapshot.activeProject !== null) {
        effects({ type: "persist-last-scope", projectId: snapshot.activeProject, scope });
      }
      const route = scope === "local" ? "effective" : snapshot.route;
      update({ scope, route, routeStack: route === snapshot.route ? snapshot.routeStack : [...snapshot.routeStack, route].slice(-50) });
      reload("scope");
    },
    navigate(route, decision) {
      if (routeRequiresProject(route) && snapshot.activeProject === null) {
        effects({ type: "project-picker-required", route });
        return false;
      }
      const buffer = snapshot.editors[snapshot.route];
      const dirty = buffer !== undefined && buffer.value !== buffer.savedValue;
      if (dirty && (decision === undefined || decision === "cancel")) {
        effects({ type: "dirty-navigation", from: snapshot.route, to: route });
        return false;
      }
      const editors = { ...snapshot.editors };
      if (dirty && decision === "discard") delete editors[snapshot.route];
      if (dirty && decision === "save" && buffer.onSave !== undefined) {
        void Promise.resolve(buffer.onSave()).then(() => {
          if (snapshot.route !== buffer.route) return;
          const editors = { ...snapshot.editors, [buffer.route]: { ...buffer, savedValue: buffer.value } };
          if (route === buffer.route) {
            update({ editors });
          } else {
            update({ route, routeStack: [...snapshot.routeStack, route].slice(-50), editors, overlay: null });
          }
        }).catch(() => {
          toasts.show({ kind: "error", title: "Save failed", message: "Changes were not saved." });
        });
        return false;
      }
      if (dirty && decision === "save" && buffer.onSave === undefined) {
        toasts.show({ kind: "error", title: "Save unavailable", message: "This screen has no save handler." });
        return false;
      }
      if (route === snapshot.route) return true;
      update({ route, routeStack: [...snapshot.routeStack, route].slice(-50), editors, overlay: null });
      return true;
    },
    back(decision) {
      if (snapshot.routeStack.length < 2) return false;
      const target = snapshot.routeStack[snapshot.routeStack.length - 2];
      if (target === undefined) return false;
      const current = snapshot.route;
      const buffer = snapshot.editors[current];
      const dirty = buffer !== undefined && buffer.value !== buffer.savedValue;
      if (dirty && decision === undefined) {
        effects({ type: "dirty-navigation", from: current, to: target });
        return false;
      }
      if (dirty && decision === "cancel") return false;
      if (dirty && decision === "save") {
        if (buffer?.onSave === undefined) {
          toasts.show({ kind: "error", title: "Save unavailable", message: "This screen has no save handler." });
          return false;
        }
        void Promise.resolve(buffer.onSave()).then(() => {
          if (snapshot.route !== current) return;
          const editors = { ...snapshot.editors, [current]: { ...buffer, savedValue: buffer.value } };
          update({ route: target, routeStack: snapshot.routeStack.slice(0, -1), editors, overlay: null });
        }).catch(() => toasts.show({ kind: "error", title: "Save failed", message: "Changes were not saved." }));
        return false;
      }
      const editors = { ...snapshot.editors };
      if (dirty && decision === "discard") delete editors[current];
      update({ route: target, routeStack: snapshot.routeStack.slice(0, -1), editors, overlay: null });
      return true;
    },
    openItem(itemId, item) {
      if (item === null) {
        toasts.show({ kind: "warning", title: "Item not found", message: "Item not found" });
        return;
      }
      const target: StateRouteId = item.kind === "mcp_server" ? "mcp-detail" : item.kind === "rule" ? "instructions" : "skill-detail";
      if ((item.scope === "project" || item.scope === "local") && item.project_id === null) {
        effects({ type: "project-picker-required", route: target });
        return;
      }
      const projectId = item.project_id ?? snapshot.activeProject;
      const scope = item.scope === "global" || item.scope === "user" || item.scope === "project" || item.scope === "local"
        ? item.scope : snapshot.scope;
      if (projectId !== null) effects({ type: "persist-last-scope", projectId, scope });
      update({
        activeProject: projectId,
        scope,
        route: target,
        routeStack: [...snapshot.routeStack, target].slice(-50),
        screenState: { ...snapshot.screenState, "detail:itemId": itemId },
      });
      effects({ type: "reload-route", route: target, reason: "project" });
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
