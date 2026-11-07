import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentCardView } from "../../generated/AgentCardView";
import type { SkillAgentPreviewView } from "../../generated/SkillAgentPreviewView";
import type { SkillAgentApplyView } from "../../generated/SkillAgentApplyView";
import { Toggle } from "../../components/toggle";
import { EnableSkillPopover } from "../../overlays/popovers";
import type { SkillCompareView } from "../../generated/SkillCompareView";
import type { SkillVersionCompareView } from "../../generated/SkillVersionCompareView";
import type { SkillKeepCopyPreviewView } from "../../generated/SkillKeepCopyPreviewView";
import type { SkillKeepCopyResultView } from "../../generated/SkillKeepCopyResultView";
import type { SkillDetailView } from "../../generated/SkillDetailView";
import type { SkillEditPreviewView } from "../../generated/SkillEditPreviewView";
import type { SkillUpdatePreviewView } from "../../generated/SkillUpdatePreviewView";
import { Button } from "../../components/button";
import { DiffViewer } from "../../components/diff";
import { MarkdownEditor } from "../../components/editor";
import { MarkdownPreview } from "../../components/editor/markdown-preview";
import { AsyncBoundary, useAsyncResource } from "../../components/states";
import { useAppSnapshot, useAppState } from "../../state/context";
import { ProjectSyncPanel } from "../overview/OverviewScreen";
import { applySkillEdit, applySkillUpdate, checkSkillSource, compareSkillCopies, compareSkillVersion, ignoreSkillUpdate, loadSkillDetail, previewSkillEdit, previewSkillUpdate, revealSkill, setSkillAutoUpdate } from "./commands";
import { publishRefusedOverwrite } from "../../overlays/dialogs/overwrite-commands";
import { Modal } from "../../overlays/dialogs/Modal";
import { SkillActionDialogs } from "../../overlays/dialogs/SkillActionDialogs";
import type { SkillActionDialogRequest } from "../../overlays/dialogs/SkillActionDialogs";

function detailId(screenState: Readonly<Record<string, unknown>>): string | null {
  const value = screenState["detail:itemId"];
  return typeof value === "string" ? value : null;
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
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(detail.markdown);
  const [comparison, setComparison] = useState<SkillCompareView | null>(null);
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
  const scopeLabel = detail.scope[0]?.toUpperCase() + detail.scope.slice(1);
  return <main aria-label="Skill detail" className="mx-auto flex w-full max-w-[1120px] flex-col gap-16 p-24">
    <header className="flex flex-wrap items-start justify-between gap-16">
      <div className="min-w-0"><button type="button" className="mb-8 border-0 bg-transparent p-0 text-12 text-text-secondary hover:text-text" onClick={() => app.navigate("skills")}>← Back to Skills</button><div className="flex flex-wrap items-center gap-8"><h1 className="m-0 text-26 font-semibold text-text">{detail.name}</h1><span className={`rounded-6 px-8 py-4 text-12 ${SCOPE_BADGE[detail.scope] ?? "bg-overlay text-text-secondary"}`}>{scopeLabel}</span><span className="rounded-6 bg-overlay px-8 py-4 text-12 text-text-secondary">{relativeUpdate(detail.updated_at)}</span>{detail.shadowed_by && <span className="rounded-6 bg-warning/15 px-8 py-4 text-12 text-warning">Shadowed by {detail.shadowed_by}</span>}</div><p className="mb-0 mt-8 break-all font-mono text-12 text-text-muted">{detail.path ?? "Path unavailable"}</p></div>
      <div className="flex flex-wrap justify-end gap-8">
        <Button size="sm" disabled={!detail.editable} onClick={() => { if (editing) { setDraft(detail.markdown); setEditPreview(null); } setEditing((value) => !value); }}>
          {editing ? "Cancel editing" : "Edit"}
        </Button>
        <Button size="sm" onClick={() => { void revealSkill(detail.item_id).catch(() => app.toasts.show({ kind: "error", title: "Could not reveal skill", message: "The system file manager could not reveal this package." })); }} disabled={!detail.path}>
          Reveal in Finder
        </Button>
        {detail.editable && !detail.missing && (
          <>
            <Button size="sm" variant="secondary" onClick={() => setActionDialog({ kind: "transfer", itemId: detail.item_id, transfer: "promote" })}>Promote</Button>
            <Button size="sm" variant="secondary" onClick={() => setActionDialog({ kind: "transfer", itemId: detail.item_id, transfer: "move" })}>Move</Button>
            <Button size="sm" variant="secondary" onClick={() => setActionDialog({ kind: "transfer", itemId: detail.item_id, transfer: "duplicate" })}>Duplicate</Button>
            <Button size="sm" variant="ghost" className="text-danger" onClick={() => setActionDialog({ kind: "delete", itemId: detail.item_id })}>Delete</Button>
            {snapshot.activeProject !== null && (
              <Button size="sm" variant="secondary" onClick={() => setActionDialog({ kind: "disable-in-repo", itemId: detail.item_id })}>
                Disable in this repo
              </Button>
            )}
          </>
        )}
      </div>
    </header>
    {detail.scope === "project" && snapshot.activeProject !== null && <ProjectSyncPanel key={`${snapshot.activeProject}:${detail.item_id}`} projectId={snapshot.activeProject} itemId={detail.item_id} />}
    {detail.source && <section className="flex flex-wrap items-center justify-between gap-8 rounded-8 border border-border bg-raised p-12" aria-label="Skill source updates"><span className="text-13 font-medium text-text" title={`Installed ${detail.source_commit_sha ?? "unknown"} · Upstream ${detail.source_remote_sha ?? "unknown"} · Source ${detail.source} · Last checked ${detail.source_last_checked_at ?? "unknown"}`}>{updateAvailable ? <span className="rounded-6 bg-warning/15 px-8 py-4 text-warning">Update available</span> : sourceStatus === "unknown" || sourceStatus === "source_gone" ? <span className="inline-flex items-center gap-6 text-text-muted" title={sourceStatus === "source_gone" ? "The source is no longer available; no update badge is shown." : "Source status is unknown or offline; no update badge is shown."}><span aria-hidden="true">●</span><span className="sr-only">{sourceStatus === "source_gone" ? "Source is gone" : "Source status unknown or offline"}</span></span> : "Source checked"}</span><div className="flex flex-wrap gap-8"><Button size="sm" variant="secondary" loading={sourceChecking} onClick={() => void checkSource()}>Check now</Button>{updateAvailable && <><Button size="sm" variant="secondary" disabled={sourceChecking} onClick={() => void reviewUpdate()}>Update now</Button><Button size="sm" variant="secondary" disabled={sourceChecking} onClick={() => void reviewUpdate()}>View changes</Button><Button size="sm" variant="secondary" onClick={() => void ignoreUpdate()}>Ignore this version</Button></>}</div></section>}
    {updatePreview && <section role="dialog" aria-modal="true" aria-labelledby="skill-update-title" className="rounded-8 border border-warning/40 bg-warning/5 p-12"><h2 id="skill-update-title" className="m-0 text-14 font-semibold text-text">Review skill update</h2><p className="mb-8 mt-4 text-12 text-text-secondary">Review every file before applying. Existing files will be backed up.</p><ul className="m-0 max-h-[320px] list-none overflow-auto p-0">{updatePreview.files.map((file) => <li key={file.path} className="border-t border-border py-8 first:border-0"><strong className="block font-mono text-11 text-text">{file.path}</strong><pre className="m-0 overflow-x-auto whitespace-pre-wrap text-11 text-text-secondary">{file.diff}</pre></li>)}</ul><div className="mt-12 flex justify-end gap-8"><Button size="sm" variant="secondary" disabled={updateApplying} onClick={() => setUpdatePreview(null)}>Cancel</Button><Button size="sm" variant="primary" loading={updateApplying} onClick={() => void applyUpdate()}>Apply update</Button></div></section>}
    {!detail.editable && <p className="m-0 text-12 text-text-muted">This native skill is read-only until it is adopted into AgenTuna.</p>}
    {detail.missing && <div role="alert" className="flex items-center justify-between gap-12 rounded-6 border border-warning/50 bg-warning/10 p-12 text-13 text-text"><span>This skill was removed or its package is unavailable outside AgenTuna.</span><Button onClick={onRefresh}>Refresh</Button></div>}
    {detail.shadow_chain.length > 0 && (
      <section className="rounded-6 border border-warning/40 bg-warning/5 p-12">
        <h2 className="m-0 text-13 font-semibold text-text">Scope shadow chain</h2>
        <p className="mb-0 mt-4 text-12 text-text-secondary">
          Effective winner: {detail.shadow_chain[0]}. Copies, highest precedence first: {detail.shadow_chain.join(" → ")}.
        </p>
        <Button
          className="mt-8"
          size="sm"
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
            className="ml-8 mt-8"
            size="sm"
            variant="secondary"
            disabled={keeping}
            onClick={() => void handleKeep(detail.scope, compareScope)}
          >
            Keep {detail.scope} copy only
          </Button>
        )}
        {compareError && <p role="alert" className="mb-0 mt-8 text-12 text-danger">{compareError}</p>}
      </section>
    )}
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
        {editing ? <div className="h-[520px]"><MarkdownEditor label={`Edit ${detail.name} SKILL.md`} value={draft} baseline={detail.markdown} onChange={(value) => { setDraft(value); setEditPreview(null); }} readOnly={detail.missing || !detail.editable} /></div> : <div className="max-h-[620px] min-h-[320px] overflow-auto p-16"><MarkdownPreview markdown={detail.markdown} /></div>}
        {editing && <div className="flex flex-wrap justify-end gap-8 border-t border-border p-12"><Button onClick={() => { setDraft(detail.markdown); setEditPreview(null); setEditing(false); }}>Discard</Button><Button variant="primary" disabled={!dirty || previewing} onClick={() => { setPreviewing(true); void previewSkillEdit(detail.item_id, draft).then(setEditPreview, () => app.toasts.show({ kind: "error", title: "Preview failed", message: "The skill could not be previewed. Reload before editing." })).finally(() => setPreviewing(false)); }}>{previewing ? "Preparing diff…" : "Preview changes"}</Button>{editPreview && <Button variant="primary" disabled={saving} onClick={() => { setSaving(true); void applySkillEdit(detail.item_id, draft, editPreview.expected_hash, editPreview.preview_id).then(() => { setEditing(false); setEditPreview(null); app.toasts.show({ kind: "success", title: "Skill updated", message: "The reviewed change was backed up and added to history." }); onRefresh(); }, (error: unknown) => { if (!publishRefusedOverwrite(error)) app.toasts.show({ kind: "error", title: "Save failed", message: "The skill changed or the safe write could not be completed. Reload and review the diff again." }); }).finally(() => setSaving(false)); }}>{saving ? "Saving…" : "Save reviewed changes"}</Button>}</div>}
        {editing && editPreview && <div className="border-t border-border p-12"><h3 className="mb-8 mt-0 text-13 font-semibold text-text">Reviewed changes</h3><DiffViewer diff={editPreview.diff} displayPath="SKILL.md" /></div>}
      </section>
      <aside className="flex flex-col gap-16">
        <section className="rounded-10 border border-border bg-raised p-16"><h2 className="mb-12 mt-0 text-14 font-semibold text-text">Details</h2><dl className="m-0 grid gap-12 text-12"><Meta label="Source" value={detail.source ?? "Created in AgenTuna"}/><Meta label="Hash" value={detail.hash ?? "Unavailable"} mono/><Meta label="Updated" value={detail.updated_at}/><Meta label="Storage" value={detail.mode ?? "Managed copy"}/></dl></section>
        {detail.source && <section className="rounded-10 border border-border bg-raised p-16"><h2 className="mb-8 mt-0 text-14 font-semibold text-text">Automatic updates</h2><label className="flex items-center justify-between gap-12 text-12 text-text">Per-skill preference<select aria-label="Per-skill automatic updates" value={autoUpdate} onChange={(event) => { const value = event.target.value; if (value === "off" || value === "inherit" || value === "on") void changeAutoUpdate(value); }} className="rounded-6 border border-border bg-surface px-8 py-6 text-text"><option value="inherit">Follow Preferences</option><option value="on">Always on</option><option value="off">Off</option></select></label>{updateAvailable && <div className="mt-8 text-11 text-text-muted">Installed {detail.source_commit_sha ?? "unknown"} · Upstream {detail.source_remote_sha ?? "unknown"} · Checked {detail.source_last_checked_at ?? "unknown"}</div>}</section>}
        <section className="rounded-10 border border-border bg-raised p-16"><div className="flex items-center justify-between gap-8"><h2 className="m-0 text-14 font-semibold text-text">Enabled for</h2><Button size="sm" disabled={!detail.editable || detail.missing || agentChoices.length === 0} onClick={() => setEnablePopoverOpen((open) => !open)}>Manage agents</Button></div>{agentsError && <p role="alert" className="mb-0 mt-8 text-12 text-danger">{agentsError}</p>}{detail.deployments.length === 0 ? <p className="m-0 mt-8 text-12 text-text-muted">No deployments recorded.</p> : <ul className="m-0 mt-8 list-none p-0">{detail.deployments.map((deployment) => <li key={deployment.agent_id} className="flex items-center justify-between gap-8 border-t border-border py-8 first:border-0"><span className="min-w-0"><strong className="block truncate text-12 text-text">{deployment.agent_name}</strong><span className="text-11 text-text-muted">{deployment.target_path ?? deployment.mode ?? "No target path"}</span></span><Toggle size="sm" label={`${deployment.agent_name} enabled`} checked={deployment.state === "linked"} disabled={agentBusy !== null || !detail.editable || detail.missing} onCheckedChange={(next) => { void toggleDeployment(deployment, next); }} /></li>)}</ul>}{enablePopoverOpen && <EnableSkillPopover itemId={detail.item_id} scope={detail.scope} projectId={detail.scope === "project" || detail.scope === "local" ? snapshot.activeProject : null} agents={agentChoices.map((agent) => { const deployment = detail.deployments.find((entry) => entry.agent_id === agent.id); return { id: agent.id, label: agent.name, enabled: deployment?.state === "linked", disabledReason: !agent.installed ? "Not installed" : !agent.managed ? "AgenTuna management is off" : undefined }; })} onClose={() => setEnablePopoverOpen(false)} onApplied={() => { setEnablePopoverOpen(false); onRefresh(); }} />}</section>
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
