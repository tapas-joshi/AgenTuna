import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentCardView } from "../../generated/AgentCardView";
import type { SkillAgentPreviewView } from "../../generated/SkillAgentPreviewView";
import type { SkillAgentApplyView } from "../../generated/SkillAgentApplyView";
import { Toggle } from "../../components/toggle";
import { EnableSkillPopover } from "../../overlays/popovers";
import type { AgentChoice } from "../../overlays/popovers";
import type { SkillCompareView } from "../../generated/SkillCompareView";
import type { SkillVersionCompareView } from "../../generated/SkillVersionCompareView";
import type { SkillKeepCopyPreviewView } from "../../generated/SkillKeepCopyPreviewView";
import type { SkillKeepCopyResultView } from "../../generated/SkillKeepCopyResultView";
import type { SkillDetailView } from "../../generated/SkillDetailView";
import type { SkillEditPreviewView } from "../../generated/SkillEditPreviewView";
import type { SkillDeploymentView } from "../../generated/SkillDeploymentView";
import type { SkillUpdatePreviewView } from "../../generated/SkillUpdatePreviewView";
import { Button } from "../../components/button";
import { DiffViewer } from "../../components/diff";
import { MarkdownEditor } from "../../components/editor";
import { MarkdownPreview } from "../../components/editor/markdown-preview";
import { AsyncBoundary, useAsyncResource } from "../../components/states";
import { useRowMenu, useRowMenuPlacement } from "../../components/table";
import { useAppSnapshot, useAppState } from "../../state/context";
import { ProjectSyncPanel } from "../overview/OverviewScreen";
import { applySkillEdit, applySkillUpdate, checkSkillSource, compareSkillCopies, compareSkillVersion, ignoreSkillUpdate, loadSkillDetail, previewSkillEdit, previewSkillUpdate, revealSkill, setSkillAutoUpdate } from "./commands";
import { publishRefusedOverwrite } from "../../overlays/dialogs/overwrite-commands";
import { Modal } from "../../overlays/dialogs/Modal";
import { SkillActionDialogs } from "../../overlays/dialogs/SkillActionDialogs";
import type { SkillActionDialogRequest } from "../../overlays/dialogs/SkillActionDialogs";
import { NativeSkillDetail } from "./NativeSkillDetail";

function detailId(screenState: Readonly<Record<string, unknown>>): string | null {
  const value = screenState["detail:itemId"];
  return typeof value === "string" && value !== "" ? value : null;
}

/** Native folder opened from the Skills list, when the user is looking at an unmanaged skill. */
function nativePath(screenState: Readonly<Record<string, unknown>>): string | null {
  const value = screenState["detail:nativePath"];
  return typeof value === "string" && value !== "" ? value : null;
}

function relativeUpdate(timestamp: string): string {
  const elapsed = Date.now() - Date.parse(timestamp);
  if (!Number.isFinite(elapsed) || elapsed < 0) return `Updated ${timestamp}`;
  const hours = Math.floor(elapsed / 3_600_000);
  if (hours < 1) return "Updated just now";
  if (hours < 24) return `Updated ${hours} hr ago`;
  const days = Math.floor(hours / 24);
  return `Updated ${days} day${days === 1 ? "" : "s"} ago`;
}

const SCOPE_BADGE: Readonly<Record<string, string>> = {
  global: "bg-scope-global/15 text-scope-global",
  user: "bg-scope-user/15 text-scope-user",
  project: "bg-scope-project/15 text-scope-project",
};

/** Product names for built-in agent ids, used when a stored row still holds the id. */
const AGENT_LABELS: Readonly<Record<string, string>> = {
  claude_code: "Claude Code",
  codex: "Codex",
  omp: "omp",
  cursor: "Cursor",
};

/** Shared styling for the header action menu items. */
const MENU_ITEM =
  "w-full rounded-4 px-8 py-4 text-left hover:bg-surface focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-focus disabled:cursor-not-allowed disabled:opacity-50";

/** Display name for a deployment; a stored row that still holds the id gets the product name. */
function deploymentAgentName(deployment: SkillDeploymentView): string {
  if (deployment.agent_name !== deployment.agent_id) return deployment.agent_name;
  return AGENT_LABELS[deployment.agent_id] ?? deployment.agent_id;
}

/** `2 hr ago` style age for a copy's `SKILL.md`, matching the shadow card copy. */
function relativeAge(timestamp: string): string {
  const elapsed = Date.now() - Date.parse(timestamp);
  if (!Number.isFinite(elapsed) || elapsed < 0) return timestamp;
  const minutes = Math.floor(elapsed / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} hr ago`;
  const days = Math.floor(hours / 24);
  return `${days} day${days === 1 ? "" : "s"} ago`;
}

/** One YAML frontmatter block at the start of a SKILL.md document. */
interface SkillFrontmatter {
  /** Fields in source order, for example `name: tdd`. */
  readonly fields: ReadonlyArray<{ readonly key: string; readonly value: string }>;
  /** Document body with the block removed. */
  readonly body: string;
}

/**
 * Splits a leading `---` YAML block from a SKILL.md document.
 *
 * The block is rendered as its own field list. Rendering it as Markdown flattened
 * `name` and `description` into one paragraph, which is metadata, not prose.
 */
export function splitFrontmatter(markdown: string): SkillFrontmatter {
  const match = /^---\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n|$)/.exec(markdown);
  if (match === null) return { fields: [], body: markdown };
  const fields: Array<{ key: string; value: string }> = [];
  for (const line of (match[1] ?? "").split(/\r?\n/)) {
    if (line.trim() === "") continue;
    const entry = /^([A-Za-z0-9_.-]+):\s*(.*)$/.exec(line);
    if (entry !== null) {
      fields.push({ key: entry[1] ?? "", value: entry[2] ?? "" });
      continue;
    }
    // An indented continuation line belongs to the field above it.
    const previous = fields[fields.length - 1];
    if (previous !== undefined) {
      previous.value = `${previous.value} ${line.trim()}`.trim();
    }
  }
  return { fields, body: markdown.slice(match[0].length) };
}
interface SkillDetailContentProps {
  readonly detail: SkillDetailView;
  readonly onRefresh: () => void;
  /** Reports whether the editor holds content the user has not saved. */
  readonly onDirtyChange?: (dirty: boolean) => void;
  /** Bumped by the parent when the user agrees to discard edits for a refresh. */
  readonly discardToken?: number;
}

function SkillDetailContent({ detail, onRefresh, onDirtyChange, discardToken = 0 }: SkillDetailContentProps) {
  const snapshot = useAppSnapshot();
  const app = useAppState();
  const moreMenu = useRowMenu();
  const moreMenuPlacement = useRowMenuPlacement(moreMenu.anchor, moreMenu.openRowId !== null);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(detail.markdown);
  const [comparison, setComparison] = useState<SkillCompareView | null>(null);
  const [shadowPreview, setShadowPreview] = useState<SkillCompareView | null>(null);
  const [versionComparison, setVersionComparison] = useState<SkillVersionCompareView | null>(null);
  const [versionLoading, setVersionLoading] = useState<string | null>(null);
  const [versionError, setVersionError] = useState<string | null>(null);
  const [compareError, setCompareError] = useState<string | null>(null);
  const [comparing, setComparing] = useState(false);
  const [keeping, setKeeping] = useState(false);
  const [editPreview, setEditPreview] = useState<SkillEditPreviewView | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [autoUpdate, setAutoUpdate] = useState(detail.auto_update);
  const [sourceChecking, setSourceChecking] = useState(false);
  const [agentBusy, setAgentBusy] = useState<string | null>(null);
  const [agentChoices, setAgentChoices] = useState<AgentCardView[]>([]);
  const [agentsError, setAgentsError] = useState<string | null>(null);
  const [enablePopoverOpen, setEnablePopoverOpen] = useState(false);
  const [actionDialog, setActionDialog] = useState<SkillActionDialogRequest | null>(null);
  useEffect(() => {
    let active = true;
    void invoke<AgentCardView[]>("agents_list").then(
      (agents) => { if (active && Array.isArray(agents)) setAgentChoices(agents); },
      () => { if (active) setAgentsError("Could not load agent availability."); },
    );
    return () => { active = false; };
  }, []);
  const [updatePreview, setUpdatePreview] = useState<SkillUpdatePreviewView | null>(null);
  const [updateApplying, setUpdateApplying] = useState(false);
  const statusFromDetail = detail.source_remote_sha === null
    ? detail.source_last_checked_at === null ? "unknown" : "source_gone"
    : detail.update_available ? "update_available" : "current";
  const [sourceStatus, setSourceStatus] = useState(statusFromDetail);
  useEffect(() => setSourceStatus(statusFromDetail), [statusFromDetail]);
  const updateAvailable = detail.update_available && sourceStatus === "update_available";
  const changeAutoUpdate = async (mode: "off" | "inherit" | "on") => {
    try {
      await setSkillAutoUpdate(detail.item_id, mode);
      setAutoUpdate(mode);
    } catch {
      app.toasts.show({ kind: "error", title: "Update preference not saved", message: "The per-skill update setting could not be stored." });
    }
  };
  const ignoreUpdate = async () => {
    try {
      await ignoreSkillUpdate(detail.item_id);
      onRefresh();
      app.toasts.show({ kind: "success", title: "Version ignored", message: "This upstream version will stay hidden until a newer one appears." });
    } catch {
      app.toasts.show({ kind: "error", title: "Version could not be ignored", message: "No checked upstream commit could be ignored." });
    }
  };
  const checkSource = async () => {
    setSourceChecking(true);
    try {
      const result = await checkSkillSource(detail.item_id);
      setSourceStatus(result.status);
      onRefresh();
      const message = result.status === "update_available"
        ? "Review the upstream diff before applying."
        : result.status === "unknown"
          ? "Source status is unknown or offline; the update badge is hidden."
          : result.status === "source_gone"
            ? "The source is no longer available; the update badge is hidden."
            : "The installed skill matches its source.";
      app.toasts.show({ kind: result.status === "update_available" ? "success" : "info", title: result.status === "update_available" ? "Update available" : "Source checked", message });
    } catch {
      setSourceStatus("unknown");
      app.toasts.show({ kind: "error", title: "Source check refused", message: "Network policy blocked the request or the source could not be reached." });
    }
    finally { setSourceChecking(false); }
  };
  const reviewUpdate = async () => {
    setSourceChecking(true);
    try { setUpdatePreview(await previewSkillUpdate(detail.item_id)); }
    catch { app.toasts.show({ kind: "error", title: "Update preview failed", message: "The update is unsafe, offline, or has local edits. No files were changed." }); }
    finally { setSourceChecking(false); }
  };
  const applyUpdate = async () => {
    if (!updatePreview) return;
    setUpdateApplying(true);
    try {
      const result = await applySkillUpdate(updatePreview.preview_id);
      setUpdatePreview(null);
      onRefresh();
      app.toasts.show({ kind: "success", title: "Skill updated", message: `${result.files_changed} files were backed up and updated.` });
    } catch {
      setUpdatePreview(null);
      app.toasts.show({ kind: "error", title: "Update refused", message: "Files changed after review or the update could not be applied. Check the source and review again." });
    } finally { setUpdateApplying(false); }
  };
  async function handleKeep(keepScope: string, dropScope: string): Promise<void> {
    setKeeping(true);
    try {
      const preview = await invoke<SkillKeepCopyPreviewView>("skill_keep_copy_preview", {
        request: {
          item_id: detail.item_id,
          keep_scope: keepScope,
          drop_scope: dropScope,
        },
      });
      await invoke<SkillKeepCopyResultView>("skill_keep_copy_apply", {
        previewId: preview.preview_id,
      });
      app.toasts.show({
        kind: "success",
        title: `Kept ${keepScope} copy`,
        message: `Overwrote ${dropScope} copy with backup created.`,
      });
      setComparison(null);
      onRefresh();
    } catch (cause) {
      app.toasts.show({
        kind: "error",
        title: "Could not keep copy",
        message: String(cause),
      });
    } finally {
      setKeeping(false);
    }
  }
  const dirty = draft !== detail.markdown;
  const deploymentProjectId = detail.scope === "project" || detail.scope === "local" ? snapshot.activeProject : null;
  useEffect(() => {
    onDirtyChange?.(editing && dirty);
  }, [dirty, editing, onDirtyChange]);
  useEffect(() => {
    if (!editing) setDraft(detail.markdown);
  }, [detail.markdown, editing]);
  useEffect(() => {
    if (discardToken === 0) return;
    setDraft(detail.markdown);
    setEditPreview(null);
    setEditing(false);
  }, [discardToken, detail.markdown]);
  async function toggleDeployment(deployment: SkillDetailView["deployments"][number], enabled: boolean): Promise<void> {
    setAgentBusy(deployment.agent_id);
    try {
      const preview = await invoke<SkillAgentPreviewView>("skill_agent_preview", {
        request: {
          item_id: detail.item_id,
          scope: detail.scope,
          agent_id: deployment.agent_id,
          enabled,
          project_id: deploymentProjectId,
        },
      });
      await invoke<SkillAgentApplyView>("skill_agent_apply", { previewId: preview.preview_id });
      app.toasts.show({
        kind: "success",
        title: enabled ? "Enabled for agent" : "Disabled for agent",
        message: `${detail.name} is ${enabled ? "enabled" : "disabled"} for ${deployment.agent_name}.`,
      });
      onRefresh();
    } catch (cause) {
      app.toasts.show({
        kind: "error",
        title: "Deployment not changed",
        message: String(cause),
      });
      onRefresh();
    } finally {
      setAgentBusy(null);
    }
  }
  const compareScope = detail.shadow_chain.find((scope) => scope !== detail.scope) ?? detail.shadowed_by;
  useEffect(() => {
    if (compareScope === null) {
      setShadowPreview(null);
      return;
    }
    let active = true;
    // Silent read for the card metrics: the diff modal still loads on demand.
    void compareSkillCopies(detail.item_id, compareScope).then(
      (view) => {
        if (active) setShadowPreview(view);
      },
      () => {
        if (active) setShadowPreview(null);
      },
    );
    return () => {
      active = false;
    };
  }, [compareScope, detail.item_id]);
  const scopeLabel = detail.scope[0]?.toUpperCase() + detail.scope.slice(1);
  const frontmatter = splitFrontmatter(detail.markdown);
  /** Deployments folded by the folder they write: one folder means one control. */
  const deploymentFolders = (() => {
    const folders = new Map<string, { path: string | null; deployments: typeof detail.deployments }>();
    for (const deployment of detail.deployments) {
      const path = deployment.target_path;
      const key = path !== null ? `path:${path}` : `agent:${deployment.agent_id}`;
      const entry = folders.get(key);
      if (entry === undefined) {
        folders.set(key, { path, deployments: [deployment] });
      } else {
        entry.deployments = [...entry.deployments, deployment];
      }
    }
    return [...folders.values()];
  })();
  /**
   * One choice per folder. A shared folder is one checkbox whose label names
   * every reader; the popover never renders a second switch for it.
   */
  const popoverAgents: AgentChoice[] = (() => {
    const choices: AgentChoice[] = [];
    const covered = new Set<string>();
    for (const folder of deploymentFolders) {
      const primary = folder.deployments[0];
      if (primary === undefined) continue;
      const known = agentChoices.find((agent) => agent.id === primary.agent_id);
      for (const deployment of folder.deployments) covered.add(deployment.agent_id);
      choices.push({
        id: primary.agent_id,
        label: folder.deployments.map((entry) => deploymentAgentName(entry)).join(" + "),
        enabled: folder.deployments.some((entry) => entry.state === "linked"),
        disabledReason:
          known === undefined
            ? undefined
            : !known.installed
              ? "Not installed"
              : !known.managed
                ? "AgenTuna management is off"
                : undefined,
        coupledWith: folder.deployments.slice(1).map((entry) => entry.agent_id),
        sharedPath: folder.deployments.length > 1 ? folder.path : null,
      });
    }
    for (const agent of agentChoices) {
      if (covered.has(agent.id)) continue;
      choices.push({
        id: agent.id,
        label: agent.name === agent.id ? (AGENT_LABELS[agent.id] ?? agent.id) : agent.name,
        enabled: false,
        disabledReason: !agent.installed
          ? "Not installed"
          : !agent.managed
            ? "AgenTuna management is off"
            : undefined,
        coupledWith: [],
        sharedPath: null,
      });
    }
    return choices;
  })();
  return <main aria-label="Skill detail" className="mx-auto flex w-full max-w-[1120px] flex-col gap-16 p-24">
    <header className="flex flex-wrap items-start justify-between gap-16">
      <div className="min-w-0">
        <nav aria-label="Breadcrumb" className="mb-8 flex items-center gap-6 text-12 text-text-secondary">
          <button type="button" className="cursor-pointer border-0 bg-transparent p-0 text-text-secondary hover:text-text" onClick={() => app.navigate("skills")}>Skills</button>
          <span aria-hidden="true">/</span>
          <span className="text-text">{detail.name}</span>
        </nav>
        <div className="flex flex-wrap items-center gap-8">
          <h1 className="m-0 text-26 font-semibold text-text">{detail.name}</h1>
          <span className={`rounded-6 px-8 py-4 text-12 ${SCOPE_BADGE[detail.scope] ?? "bg-overlay text-text-secondary"}`}>{scopeLabel}</span>
          <span className="text-12 text-text-muted">{relativeUpdate(detail.updated_at)}</span>
          {detail.shadowed_by && <span className="rounded-6 bg-warning/15 px-8 py-4 text-12 text-warning">Shadowed by {detail.shadowed_by}</span>}
        </div>
        <p className="mb-0 mt-8 text-13 text-text-secondary">{detail.description ?? "No description"}</p>
        <p className="mb-0 mt-4 break-all font-mono text-12 text-text-muted">{detail.path ?? "Path unavailable"}</p>
      </div>
      <div className="flex flex-wrap items-center justify-end gap-8">
        {detail.editable && !detail.missing && (
          <Button size="sm" variant="destructive" onClick={() => setActionDialog({ kind: "delete", itemId: detail.item_id })}>Delete</Button>
        )}
        {detail.editable && !detail.missing && (
          <Button size="sm" variant="secondary" onClick={() => setActionDialog({ kind: "transfer", itemId: detail.item_id, transfer: "promote" })}>Promote…</Button>
        )}
        <Button size="sm" variant="primary" disabled={!detail.editable} onClick={() => { if (editing) { setDraft(detail.markdown); setEditPreview(null); } setEditing((value) => !value); }}>
          {editing ? "Cancel editing" : "Edit"}
        </Button>
        <div className="relative" data-row-menu>
          <Button size="sm" variant="ghost" aria-label="More actions" aria-haspopup="menu" aria-expanded={moreMenu.isOpen(detail.item_id)} onClick={(event) => moreMenu.toggle(detail.item_id, event.currentTarget)}>⋯</Button>
          {moreMenu.isOpen(detail.item_id) && (
            <div
              role="menu"
              aria-label="More actions"
              className={`absolute right-0 z-50 min-w-[200px] rounded-6 border border-border bg-raised p-4 text-12 text-text shadow-lg overflow-y-auto overscroll-contain ${
                moreMenuPlacement.side === "above" ? "bottom-full mb-4" : "top-full mt-4"
              }`}
              style={{ maxHeight: moreMenuPlacement.maxHeight }}
              onKeyDown={(event) => {
                const items = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'));
                if (event.key === "Escape") {
                  event.preventDefault();
                  moreMenu.close();
                  event.currentTarget.parentElement?.querySelector<HTMLButtonElement>('[aria-label="More actions"]')?.focus();
                  return;
                }
                const current = items.indexOf(document.activeElement as HTMLButtonElement);
                const target = event.key === "ArrowDown" ? (current + 1) % items.length : event.key === "ArrowUp" ? (current - 1 + items.length) % items.length : null;
                if (target !== null) {
                  event.preventDefault();
                  items[target]?.focus();
                }
              }}
              onBlur={(event) => {
                const next = event.relatedTarget;
                if (!(next instanceof Node) || !event.currentTarget.contains(next)) moreMenu.close();
              }}
            >
              <button type="button" role="menuitem" tabIndex={-1} className={MENU_ITEM} disabled={!detail.path} onClick={() => { moreMenu.close(); void revealSkill(detail.item_id).catch(() => app.toasts.show({ kind: "error", title: "Could not reveal skill", message: "The system file manager could not reveal this package." })); }}>Reveal in Finder</button>
              {detail.editable && !detail.missing && (
                <>
                  <button type="button" role="menuitem" tabIndex={-1} className={MENU_ITEM} onClick={() => { moreMenu.close(); setActionDialog({ kind: "transfer", itemId: detail.item_id, transfer: "move" }); }}>Move to scope…</button>
                  <button type="button" role="menuitem" tabIndex={-1} className={MENU_ITEM} onClick={() => { moreMenu.close(); setActionDialog({ kind: "transfer", itemId: detail.item_id, transfer: "duplicate" }); }}>Duplicate…</button>
                  {snapshot.activeProject !== null && (
                    <button type="button" role="menuitem" tabIndex={-1} className={MENU_ITEM} onClick={() => { moreMenu.close(); setActionDialog({ kind: "disable-in-repo", itemId: detail.item_id }); }}>Disable in this repo</button>
                  )}
                </>
              )}
            </div>
          )}
        </div>
      </div>
    </header>
    {detail.scope === "project" && snapshot.activeProject !== null && <ProjectSyncPanel key={`${snapshot.activeProject}:${detail.item_id}`} projectId={snapshot.activeProject} itemId={detail.item_id} />}
    {detail.source && <section className="flex flex-wrap items-center justify-between gap-8 rounded-8 border border-border bg-raised p-12" aria-label="Skill source updates"><span className="text-13 font-medium text-text" title={`Installed ${detail.source_commit_sha ?? "unknown"} · Upstream ${detail.source_remote_sha ?? "unknown"} · Source ${detail.source} · Last checked ${detail.source_last_checked_at ?? "unknown"}`}>{updateAvailable ? <span className="rounded-6 bg-warning/15 px-8 py-4 text-warning">Update available</span> : sourceStatus === "unknown" || sourceStatus === "source_gone" ? <span className="inline-flex items-center gap-6 text-text-muted" title={sourceStatus === "source_gone" ? "The source is no longer available; no update badge is shown." : "Source status is unknown or offline; no update badge is shown."}><span aria-hidden="true">●</span><span className="sr-only">{sourceStatus === "source_gone" ? "Source is gone" : "Source status unknown or offline"}</span></span> : "Source checked"}</span><div className="flex flex-wrap gap-8"><Button size="sm" variant="secondary" loading={sourceChecking} onClick={() => void checkSource()}>Check now</Button>{updateAvailable && <><Button size="sm" variant="secondary" disabled={sourceChecking} onClick={() => void reviewUpdate()}>Update now</Button><Button size="sm" variant="secondary" disabled={sourceChecking} onClick={() => void reviewUpdate()}>View changes</Button><Button size="sm" variant="secondary" onClick={() => void ignoreUpdate()}>Ignore this version</Button></>}</div></section>}
    {updatePreview && <section role="dialog" aria-modal="true" aria-labelledby="skill-update-title" className="rounded-8 border border-warning/40 bg-warning/5 p-12"><h2 id="skill-update-title" className="m-0 text-14 font-semibold text-text">Review skill update</h2><p className="mb-8 mt-4 text-12 text-text-secondary">Review every file before applying. Existing files will be backed up.</p><ul className="m-0 max-h-[320px] list-none overflow-auto p-0">{updatePreview.files.map((file) => <li key={file.path} className="border-t border-border py-8 first:border-0"><strong className="block font-mono text-11 text-text">{file.path}</strong><pre className="m-0 overflow-x-auto whitespace-pre-wrap text-11 text-text-secondary">{file.diff}</pre></li>)}</ul><div className="mt-12 flex justify-end gap-8"><Button size="sm" variant="secondary" disabled={updateApplying} onClick={() => setUpdatePreview(null)}>Cancel</Button><Button size="sm" variant="primary" loading={updateApplying} onClick={() => void applyUpdate()}>Apply update</Button></div></section>}
    {!detail.editable && <p className="m-0 text-12 text-text-muted">This native skill is read-only until it is adopted into AgenTuna.</p>}
    {detail.missing && <div role="alert" className="flex items-center justify-between gap-12 rounded-6 border border-warning/50 bg-warning/10 p-12 text-13 text-text"><span>This skill was removed or its package is unavailable outside AgenTuna.</span><Button onClick={onRefresh}>Refresh</Button></div>}
    <Modal
      open={comparison !== null}
      title="Compare copies"
      description={comparison ? `${comparison.shadow.scope} compared with winning ${comparison.winner.scope}${comparison.identical ? " · identical copies" : ""}` : undefined}
      onOpenChange={(open) => { if (!open) setComparison(null); }}
      footer={
        <>
          <Button variant="ghost" onClick={() => setComparison(null)}>
            Close
          </Button>
          {comparison && (
            <>
              <Button
                variant="secondary"
                disabled={comparison.identical || keeping}
                onClick={() => void handleKeep(comparison.winner.scope, comparison.shadow.scope)}
              >
                Keep {comparison.winner.scope}
              </Button>
              <Button
                variant="secondary"
                disabled={comparison.identical || keeping}
                onClick={() => void handleKeep(comparison.shadow.scope, comparison.winner.scope)}
              >
                Keep {comparison.shadow.scope}
              </Button>
            </>
          )}
        </>
      }
    >
      {comparison && (
        <div className="grid gap-12 md:grid-cols-2">
          <div className="rounded-6 border border-border p-8">
            <h3 className="m-0 text-12 font-semibold capitalize">{comparison.winner.scope} (winner)</h3>
            <code className="mt-4 block break-all font-mono text-11 text-text-muted">{comparison.winner.hash}</code>
          </div>
          <div className="rounded-6 border border-border p-8">
            <h3 className="m-0 text-12 font-semibold capitalize">{comparison.shadow.scope} (shadow)</h3>
            <code className="mt-4 block break-all font-mono text-11 text-text-muted">{comparison.shadow.hash}</code>
          </div>
          <div className="md:col-span-2">
            {comparison.identical ? (
              <p className="m-0 py-16 text-center text-13 text-text-secondary" role="status">
                No differences — both copies are identical.
              </p>
            ) : (
              <DiffViewer diff={comparison.diff} displayPath="SKILL.md" />
            )}
          </div>
        </div>
      )}
    </Modal>
    <div className="grid gap-16 lg:grid-cols-[minmax(0,2fr)_minmax(260px,1fr)]">
      <section className="min-h-[420px] overflow-hidden rounded-10 border border-border bg-raised" aria-label="Skill document">
        <div className="flex items-center justify-between border-b border-border px-16 py-12"><div><h2 className="m-0 text-14 font-semibold text-text">SKILL.md</h2><p className="mb-0 mt-4 text-11 text-text-muted">{detail.description ?? "No description"}</p></div>{editing && dirty && <span className="text-12 text-warning">Unsaved changes</span>}</div>
        {editing ? <div className="h-[520px]"><MarkdownEditor label={`Edit ${detail.name} SKILL.md`} value={draft} baseline={detail.markdown} onChange={(value) => { setDraft(value); setEditPreview(null); }} readOnly={detail.missing || !detail.editable} /></div> : (
          <div className="max-h-[620px] min-h-[320px] overflow-auto p-16">
            {frontmatter.fields.length > 0 && (
              <div data-slot="skill-frontmatter" role="group" aria-label="SKILL.md frontmatter" className="grid gap-4 rounded-6 border border-border bg-overlay/60 p-12 font-mono text-12 text-text-secondary">
                <span aria-hidden="true" className="text-text-muted">---</span>
                {frontmatter.fields.map((field) => (
                  <div key={field.key} className="grid grid-cols-[auto_minmax(0,1fr)] gap-8">
                    <span className="text-scope-user">{field.key}:</span>
                    <span className="break-words text-text">{field.value}</span>
                  </div>
                ))}
                <span aria-hidden="true" className="text-text-muted">---</span>
              </div>
            )}
            <MarkdownPreview markdown={frontmatter.body} />
          </div>
        )}
        {editing && <div className="flex flex-wrap justify-end gap-8 border-t border-border p-12"><Button onClick={() => { setDraft(detail.markdown); setEditPreview(null); setEditing(false); }}>Discard</Button><Button variant="primary" disabled={!dirty || previewing} onClick={() => { setPreviewing(true); void previewSkillEdit(detail.item_id, draft).then(setEditPreview, () => app.toasts.show({ kind: "error", title: "Preview failed", message: "The skill could not be previewed. Reload before editing." })).finally(() => setPreviewing(false)); }}>{previewing ? "Preparing diff…" : "Preview changes"}</Button>{editPreview && <Button variant="primary" disabled={saving} onClick={() => { setSaving(true); void applySkillEdit(detail.item_id, draft, editPreview.expected_hash, editPreview.preview_id).then(() => { setEditing(false); setEditPreview(null); app.toasts.show({ kind: "success", title: "Skill updated", message: "The reviewed change was backed up and added to history." }); onRefresh(); }, (error: unknown) => { if (!publishRefusedOverwrite(error)) app.toasts.show({ kind: "error", title: "Save failed", message: "The skill changed or the safe write could not be completed. Reload and review the diff again." }); }).finally(() => setSaving(false)); }}>{saving ? "Saving…" : "Save reviewed changes"}</Button>}</div>}
        {editing && editPreview && <div className="border-t border-border p-12"><h3 className="mb-8 mt-0 text-13 font-semibold text-text">Reviewed changes</h3><DiffViewer diff={editPreview.diff} displayPath="SKILL.md" /></div>}
      </section>
      <aside className="flex flex-col gap-16">
        <section className="rounded-10 border border-border bg-raised p-16"><h2 className="mb-12 mt-0 text-14 font-semibold text-text">Details</h2><dl className="m-0 grid gap-12 text-12"><Meta label="Source" value={detail.source ?? "Created in AgenTuna"}/><Meta label="Hash" value={detail.hash ?? "Unavailable"} mono/><Meta label="Updated" value={detail.updated_at}/><Meta label="Storage" value={detail.mode ?? "Managed copy"}/></dl></section>
        {detail.source && <section className="rounded-10 border border-border bg-raised p-16"><h2 className="mb-8 mt-0 text-14 font-semibold text-text">Automatic updates</h2><label className="flex items-center justify-between gap-12 text-12 text-text">Per-skill preference<select aria-label="Per-skill automatic updates" value={autoUpdate} onChange={(event) => { const value = event.target.value; if (value === "off" || value === "inherit" || value === "on") void changeAutoUpdate(value); }} className="rounded-6 border border-border bg-surface px-8 py-6 text-text"><option value="inherit">Follow Preferences</option><option value="on">Always on</option><option value="off">Off</option></select></label>{updateAvailable && <div className="mt-8 text-11 text-text-muted">Installed {detail.source_commit_sha ?? "unknown"} · Upstream {detail.source_remote_sha ?? "unknown"} · Checked {detail.source_last_checked_at ?? "unknown"}</div>}</section>}
        <section className="rounded-10 border border-border bg-raised p-16" aria-label="Enabled for">
          <div className="flex items-center justify-between gap-8">
            <h2 className="m-0 text-14 font-semibold text-text">Enabled for</h2>
            <Button size="sm" disabled={!detail.editable || detail.missing || agentChoices.length === 0} onClick={() => setEnablePopoverOpen((open) => !open)}>Manage agents</Button>
          </div>
          {agentsError && <p role="alert" className="mb-0 mt-8 text-12 text-danger">{agentsError}</p>}
          {detail.deployments.length === 0 ? (
            <p className="m-0 mt-8 text-12 text-text-muted">No deployments recorded.</p>
          ) : (
            <>
              {deploymentFolders.some((folder) => folder.deployments.length > 1) && (
                <div role="note" className="mt-8 rounded-6 border border-warning/40 bg-warning/10 p-8 text-11 text-text">
                  <strong className="block text-12">Shared discovery root</strong>
                  {deploymentFolders
                    .filter((folder) => folder.deployments.length > 1)
                    .map((folder) => `${folder.deployments.map((entry) => deploymentAgentName(entry)).join(" + ")} read ${folder.path ?? "one folder"} — one folder on disk, so this switch changes both agents. AgenTuna never offers a separate switch for it.`)
                    .join(" ")}
                </div>
              )}
              <ul className="m-0 mt-8 list-none p-0">
                {deploymentFolders.map((folder) => {
                  const primary = folder.deployments[0];
                  if (primary === undefined) return null;
                  const labels = folder.deployments.map((entry) => deploymentAgentName(entry)).join(" + ");
                  const shared = folder.deployments.length > 1;
                  const enabled = folder.deployments.every((entry) => entry.state === "linked");
                  return (
                    <li key={folder.path ?? primary.agent_id} className="flex items-center justify-between gap-8 border-t border-border py-8 first:border-0">
                      <span className="min-w-0">
                        <strong className="block truncate text-12 text-text">{labels}</strong>
                        <span className="block break-all text-11 text-text-muted">{folder.path ?? primary.mode ?? "No target path"}</span>
                        {shared && <span className="block text-11 text-scope-user">1 folder · {folder.deployments.length} agents · one switch</span>}
                      </span>
                      <Toggle
                        size="sm"
                        label={`${labels} enabled`}
                        labelPlacement="hidden"
                        checked={enabled}
                        disabled={agentBusy !== null || !detail.editable || detail.missing}
                        onCheckedChange={(next) => { void toggleDeployment(primary, next); }}
                      />
                    </li>
                  );
                })}
              </ul>
            </>
          )}
          {enablePopoverOpen && (
            <EnableSkillPopover
              itemId={detail.item_id}
              scope={detail.scope}
              projectId={detail.scope === "project" || detail.scope === "local" ? snapshot.activeProject : null}
              agents={popoverAgents}
              onClose={() => setEnablePopoverOpen(false)}
              onApplied={() => { setEnablePopoverOpen(false); onRefresh(); }}
            />
          )}
        </section>
        {detail.shadow_chain.length > 0 && (
          <section aria-label="Shadowed copies" className="rounded-10 border border-warning/40 bg-warning/5 p-16">
            <h2 className="m-0 text-14 font-semibold text-warning">
              {detail.shadowed_by
                ? `Shadowed by ${detail.shadowed_by[0]?.toUpperCase() ?? ""}${detail.shadowed_by.slice(1)}`
                : `Shadows a ${compareScope === null ? "lower" : `${compareScope[0]?.toUpperCase() ?? ""}${compareScope.slice(1)}`} copy`}
            </h2>
            <p className="mb-0 mt-6 text-12 text-text-secondary">
              {detail.shadow_chain.length > 1
                ? `This skill exists in ${detail.shadow_chain.length} scopes (${detail.shadow_chain.join(" → ")}). The nearest scope wins; every copy still loads where the nearest one is absent.`
                : `${detail.name} also exists at ${compareScope ?? "another scope"}. The nearest scope wins here, and the other copy still loads where this one is absent.`}
            </p>
            {shadowPreview !== null && (
              <dl className="mb-0 mt-10 grid gap-4 font-mono text-12 text-warning">
                {[shadowPreview.winner, shadowPreview.shadow].map((copy) => (
                  <div key={copy.scope} className="flex gap-8">
                    <dt className="w-[92px] shrink-0 capitalize">{copy.scope}:</dt>
                    <dd className="m-0">
                      {copy.markdown.split("\n").length} lines · edited {relativeAge(copy.mtime)}
                    </dd>
                  </div>
                ))}
              </dl>
            )}
            <div className="mt-12 flex flex-wrap items-center gap-8">
              <Button
                size="sm"
                variant="secondary"
                disabled={comparing || compareScope === null}
                onClick={() => {
                  if (!compareScope) return;
                  setComparing(true);
                  setCompareError(null);
                  void compareSkillCopies(detail.item_id, compareScope)
                    .then(setComparison, () => setCompareError("Could not read both skill copies."))
                    .finally(() => setComparing(false));
                }}
              >
                {comparing ? "Comparing…" : "Compare copies"}
              </Button>
              {compareScope !== null && (
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={keeping || shadowPreview?.identical === true}
                  onClick={() =>
                    detail.shadowed_by !== null
                      ? void handleKeep(detail.scope, compareScope)
                      : void handleKeep(compareScope, detail.scope)
                  }
                >
                  Keep{" "}
                  {detail.shadowed_by !== null
                    ? scopeLabel
                    : `${compareScope[0]?.toUpperCase() ?? ""}${compareScope.slice(1)}`}{" "}
                  copy only
                </Button>
              )}
            </div>
            {compareError && <p role="alert" className="mb-0 mt-8 text-12 text-danger">{compareError}</p>}
          </section>
        )}
        <section className="rounded-10 border border-border bg-raised p-16"><h2 className="mb-12 mt-0 text-14 font-semibold text-text">History</h2>{detail.history.length === 0 ? <p className="m-0 text-12 text-text-muted">No captured versions yet.</p> : <ol className="m-0 list-none p-0">{detail.history.map((entry) => <li key={`${entry.version_id}-${entry.captured_at}`} className="flex items-center justify-between gap-8 border-t border-border py-8 first:border-0"><span className="min-w-0"><strong className="block text-12 text-text">{entry.operation_kind ?? "Captured version"}</strong><span className="text-11 text-text-muted">{entry.captured_at} · {entry.content_hash.slice(0, 10)}</span></span><Button size="sm" disabled={versionLoading !== null} onClick={() => { setVersionLoading(entry.version_id); setVersionError(null); void compareSkillVersion(detail.item_id, entry.version_id).then(setVersionComparison, () => setVersionError("The stored version or its backup is no longer available.")).finally(() => setVersionLoading(null)); }}>{versionLoading === entry.version_id ? "Comparing…" : "Compare"}</Button></li>)}</ol>}{versionError && <p role="alert" className="mt-8 text-12 text-danger">{versionError}</p>}{versionComparison && <div className="mt-12 border-t border-border pt-12"><div className="mb-8 flex items-center justify-between gap-8"><span className="text-11 text-text-muted">Historical {versionComparison.version_hash.slice(0, 10)} → current {versionComparison.current_hash.slice(0, 10)}</span><Button size="sm" onClick={() => setVersionComparison(null)}>Close diff</Button></div><DiffViewer diff={versionComparison.diff} displayPath="SKILL.md" /></div>}</section>
      </aside>
    </div>
    <SkillActionDialogs
      action={actionDialog}
      projectId={snapshot.activeProject}
      onClose={() => setActionDialog(null)}
      onTransferSuccess={(result) => {
        setActionDialog(null);
        app.toasts.show({
          kind: "success",
          title: "Skill transfer complete",
          message: `Backup location: ${result.backup_location}`,
        });
        onRefresh();
      }}
      onDeleteSuccess={(result) => {
        setActionDialog(null);
        app.toasts.show({
          kind: "success",
          title: "Skill deleted",
          message: `Backup location: ${result.backup_folder}`,
          action: {
            label: "Undo",
            run: () => {
              void invoke("operation_undo", { operationId: result.operation_id, force: false })
                .then(() => {
                  app.toasts.show({ kind: "success", title: "Delete undone", message: "The skill was restored." });
                  onRefresh();
                })
                .catch(() => app.toasts.show({ kind: "error", title: "Undo failed", message: "The skill backup could not be restored." }));
            },
          },
        });
        onRefresh();
      }}
      onDisableSuccess={(result) => {
        setActionDialog(null);
        app.toasts.show({
          kind: "success",
          title: "Disabled in this repo",
          message: `${result.agents_disabled} agent link(s) removed after backup. Backup location: ${result.backup_location}`,
        });
        onRefresh();
      }}
    />
  </main>;
}

function Meta({ label, value, mono = false }: { readonly label: string; readonly value: string; readonly mono?: boolean }) {
  return <div><dt className="text-11 text-text-muted">{label}</dt><dd className={`m-0 mt-2 break-all text-text-secondary ${mono ? "font-mono text-11" : "text-12"}`}>{value}</dd></div>;
}

/** Displays an indexed skill detail, including stable loading, empty, error and deleted states. */
export function SkillDetailScreen() {
  const { screenState, externalRefreshVersion } = useAppSnapshot();
  const itemId = detailId(screenState);
  const app = useAppState();
  const [refreshToken, setRefreshToken] = useState(0);
  const [dirtyEditor, setDirtyEditor] = useState(false);
  const [pendingDirtyRefresh, setPendingDirtyRefresh] = useState(false);
  const [discardToken, setDiscardToken] = useState(0);
  const dirtyRef = useRef(false);
  const seenRefresh = useRef(externalRefreshVersion);
  dirtyRef.current = dirtyEditor;
  const resource = useAsyncResource(
    () => (itemId === null ? Promise.resolve(null) : loadSkillDetail(itemId)),
    { deps: [itemId, refreshToken], isEmpty: (data) => data === null }
  );
  useEffect(() => {
    if (externalRefreshVersion === seenRefresh.current) return;
    seenRefresh.current = externalRefreshVersion;
    if (dirtyRef.current) {
      setPendingDirtyRefresh(true);
      return;
    }
    setRefreshToken((value) => value + 1);
  }, [externalRefreshVersion]);
  if (itemId === null && nativePath(screenState) !== null) {
    const opened = nativePath(screenState) ?? "";
    return (
      <NativeSkillDetail
        key={opened}
        path={opened}
        onClose={() => {
          app.setScreenState("detail:nativePath", null);
          app.navigate("skills");
        }}
      />
    );
  }
  if (itemId === null) return <main aria-label="Skill detail" className="p-24"><h1 className="m-0 text-22 font-semibold text-text">Skill detail</h1><p className="text-13 text-text-secondary">Select a skill from Skills to view its details.</p></main>;
  return <>
    {pendingDirtyRefresh && (
      <div role="alert" className="mx-auto mt-16 flex w-full max-w-[1120px] flex-wrap items-center justify-between gap-12 rounded-8 border border-warning/50 bg-warning/10 p-12 text-13 text-text">
        <span>External changes were detected, but the editor has unsaved content. Save or discard it before the detail refreshes.</span>
        <span className="flex flex-wrap gap-8">
          <Button
            size="sm"
            onClick={() => {
              setDiscardToken((value) => value + 1);
              setDirtyEditor(false);
              dirtyRef.current = false;
              setPendingDirtyRefresh(false);
              setRefreshToken((value) => value + 1);
            }}
          >
            Discard edits and refresh
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setPendingDirtyRefresh(false)}>Keep editing</Button>
        </span>
      </div>
    )}
    <AsyncBoundary state={resource} loadingTitle="Loading skill" loadingBody="Reading the skill package and deployment index" skeletonRows={3} errorTitle="Skill could not load" minHeight={360} empty={{ title: "Skill no longer found", body: "This skill was removed from the index. Return to Skills or refresh the list.", action: { label: "Back to Skills", onClick: () => app.navigate("skills") }, secondaryAction: { label: "Refresh", onClick: () => setRefreshToken((value) => value + 1) } }}>
      {(data) => data && <SkillDetailContent key={data.item_id} detail={data} onRefresh={resource.retry} onDirtyChange={setDirtyEditor} discardToken={discardToken} />}
    </AsyncBoundary>
  </>;
}
