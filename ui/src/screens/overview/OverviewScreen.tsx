import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Badge } from "../../components/badge";
import { Button } from "../../components/button";
import { AsyncBoundary } from "../../components/states";
import { useAsyncResource } from "../../components/states";
import { Modal } from "../../overlays/dialogs";
import { useAppSnapshot, useAppState } from "../../state/context";
import { ACTION_IDS } from "../../shell/actions";
import type { OverviewView } from "../../generated/OverviewView";
import type { OverviewScopeView } from "../../generated/OverviewScopeView";
import type { ProjectSyncItemView } from "../../generated/ProjectSyncItemView";
import type { ProjectSyncPreviewView } from "../../generated/ProjectSyncPreviewView";
import type { ScopeSetupPreviewView } from "../../generated/ScopeSetupPreviewView";
import type { SkillCompareView } from "../../generated/SkillCompareView";
import { compareSkillCopies, loadSkillDetail } from "../skill_detail/commands";
import { applyScopeSetup, enableGlobalWrites, loadOverview, loadProjectSyncStatus, previewProjectSync, applyProjectSync, previewScopeSetup } from "./commands";
const SYNC_LABELS: Readonly<Record<string, string>> = { in_sync: "In sync", project_only: "Project only", library_newer: "Library newer", project_newer: "Project newer", library_only: "Library only", conflict: "Conflict", drifted: "Drifted" };
const SYNC_LABEL_COLORS: Readonly<Record<string, string>> = {
  in_sync: "bg-success/10 text-success",
  project_only: "bg-scope-project/15 text-scope-project",
  library_newer: "bg-scope-user/15 text-scope-user",
  project_newer: "bg-scope-project/15 text-scope-project",
  library_only: "bg-scope-user/15 text-scope-user",
  conflict: "bg-warning/15 text-warning",
  drifted: "bg-warning/15 text-warning",
};

const SCOPE_NAMES: Readonly<Record<string, string>> = { global: "Global", user: "User", project: "Project", local: "Local" };
const SCOPE_COLORS: Readonly<Record<string, string>> = { global: "bg-scope-global", user: "bg-scope-user", project: "bg-scope-project", local: "bg-scope-local" };

/** Visible word for every needs-attention kind; colour is never the only signal. */
const ATTENTION_STATUS: Readonly<Record<string, { readonly label: string; readonly tone: "danger" | "warning" | "info" | "muted" | "locked" }>> = {
  failing_health_check: { label: "Failing", tone: "danger" },
  drift_event: { label: "Drift", tone: "warning" },
  native_deployment: { label: "Not in AgenTuna", tone: "info" },
  missing_agent: { label: "Missing", tone: "muted" },
  shadowed_skill: { label: "Shadowed", tone: "warning" },
  locked_item_not_found: { label: "Locked", tone: "locked" },
};

/** Direct action offered on each attention row. */
const ATTENTION_ACTION: Readonly<Record<string, string>> = {
  failing_health_check: "Fix token",
  drift_event: "Review diff",
  native_deployment: "Review",
  missing_agent: "Set path",
  shadowed_skill: "Compare",
  locked_item_not_found: "Open",
};

/** One item resolved by `item_lookup`; the same shape `openItem` accepts. */
interface LookupItem {
  readonly kind: string;
  readonly scope: string;
  readonly project_id: string | null;
}

function relativeTime(timestamp: string | null): string {
  if (!timestamp) return "Not set up";
  const time = Date.parse(timestamp);
  if (!Number.isFinite(time)) return timestamp;
  const minutes = Math.max(0, Math.floor((Date.now() - time) / 60_000));
  if (minutes < 1) return "Just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} hr ago`;
  const days = Math.floor(hours / 24);
  return `${days} day${days === 1 ? "" : "s"} ago`;
}

const ACTOR_LABELS: Readonly<Record<string, string>> = {
  ui: "You",
  cli: "CLI",
  migration: "Migration",
  watcher: "File watcher",
  updater: "Updater",
};

/** Narrower scopes are dimmed when the selected scope is broader than they are. */
const SCOPE_DEPTH: Readonly<Record<string, number>> = { global: 0, user: 1, project: 2, local: 3 };

function ScopeCard({ card, selected, dimmed, setupBusy, onSetUp, onCopy }: { readonly card: OverviewScopeView; readonly selected: boolean; readonly dimmed: boolean; readonly setupBusy: boolean; readonly onSetUp: () => void; readonly onCopy: () => void }) {
  const skills = Number(card.skills);
  const mcp = Number(card.mcp);
  const rules = Number(card.rules);
  const locks = Number(card.locks);
  const empty = !card.store_ready;
  const nativeRoots = card.native_roots ?? [];
  const storeRoot = card.store_root === "" ? "" : card.store_root;
  const git = card.git_uncommitted;
  const editedBy = card.last_edited_by === null || card.last_edited_by === undefined ? null : ACTOR_LABELS[card.last_edited_by] ?? card.last_edited_by;
  const edited = card.last_edited === null ? "Not edited yet" : relativeTime(card.last_edited);
  const footerLeft = card.scope === "project"
    ? git === null || git === undefined ? "git: no repository" : git === 0 ? "git: clean" : `git: ${git} uncommitted`
    : `${locks} locked`;
  return (
    <article className={`flex min-h-[196px] flex-col gap-12 rounded-10 border border-border p-16 ${selected ? "bg-raised" : "bg-surface"} ${dimmed ? "opacity-65" : ""}`} style={selected ? { borderColor: "var(--at-color-scope-project)", boxShadow: "0 0 0 1px var(--at-color-scope-project)" } : undefined} aria-current={selected ? "true" : undefined} aria-label={`${SCOPE_NAMES[card.scope] ?? card.scope} scope`}>
      <div className="flex items-center justify-between gap-8">
        <span className="inline-flex items-center gap-6 rounded-6 bg-overlay px-8 py-4 text-12 font-semibold text-text">
          <i aria-hidden="true" className={`size-8 shrink-0 rounded-full ${SCOPE_COLORS[card.scope] ?? "bg-overlay"}`} />
          {SCOPE_NAMES[card.scope] ?? card.scope}
        </span>
        {selected && <span className="text-11 font-semibold text-text-secondary">Selected</span>}
      </div>
      {empty ? <div className="flex flex-1 flex-col justify-center gap-8"><p className="m-0 flex min-w-0 items-center gap-6 font-mono text-12 text-text-muted"><span className="min-w-0 flex-1 truncate" title={storeRoot}>{storeRoot}</span>{card.scope === "user" && <button type="button" className="shrink-0 rounded-4 px-6 py-3 font-sans text-11 text-text-secondary hover:bg-overlay focus-visible:outline-2 focus-visible:outline-focus" onClick={onCopy}>Copy path</button>}</p><p className="m-0 text-13 text-text-secondary">This scope has no AgenTuna store yet.</p><Button size="sm" className="self-start" loading={setupBusy} aria-label={`Set up the ${SCOPE_NAMES[card.scope] ?? card.scope} scope`} onClick={onSetUp}>Set up this scope</Button></div> : <>
        <div className="min-w-0">
          <div className="flex min-w-0 items-center gap-6">
            <p className="m-0 min-w-0 flex-1 truncate font-mono text-12 text-text" title={nativeRoots.length > 0 ? nativeRoots.join(" · ") : storeRoot}>
              {nativeRoots.length > 0 ? nativeRoots.join(" · ") : storeRoot}
            </p>
            {card.scope === "user" && nativeRoots.length === 0 && <button type="button" className="shrink-0 rounded-4 px-6 py-2 font-sans text-11 text-text-secondary hover:bg-overlay focus-visible:outline-2 focus-visible:outline-focus" onClick={onCopy}>Copy path</button>}
          </div>
          {nativeRoots.length > 0 && <p className="m-0 mt-2 flex min-w-0 items-center gap-6 text-11 text-text-muted"><span className="min-w-0 flex-1 truncate font-mono" title={storeRoot}>Store: {storeRoot}</span>{card.scope === "user" && <button type="button" className="shrink-0 rounded-4 px-6 py-2 font-sans text-11 text-text-secondary hover:bg-overlay focus-visible:outline-2 focus-visible:outline-focus" onClick={onCopy}>Copy path</button>}</p>}
        </div>
        <div className="grid grid-cols-3 gap-8">
          <Count value={skills} label="skills" />
          <Count value={mcp} label="MCP servers" />
          <Count value={rules} label="instruction files" />
        </div>
        <div className="mt-auto flex justify-between gap-8 border-t border-border pt-8 text-11 text-text-muted">
          <span className="inline-flex items-center gap-4">{card.scope !== "project" && locks > 0 && <svg aria-hidden="true" className="h-10 w-10 fill-none stroke-current text-warning" viewBox="0 0 16 16"><rect x="3" y="7" width="10" height="8" rx="1" /><path d="M5 7V5a3 3 0 0 1 6 0v2" /></svg>}{footerLeft}</span>
          <span title={card.last_edited ?? undefined}>{editedBy === null ? edited : `${editedBy} · ${edited}`}</span>
        </div>
      </>}
    </article>
  );
}

function Count({ value, label }: { readonly value: number; readonly label: string }) {
  return <div><strong className="block font-mono text-22 text-text">{value}</strong><span className="text-11 text-text-muted">{label}</span></div>;
}

function AttentionStatusBadge({ row }: { readonly row: OverviewView["attention"][number] }) {
  const meta = ATTENTION_STATUS[row.kind] ?? { label: "Issue", tone: "muted" as const };
  return <Badge tone={meta.tone} dot title={row.subtitle ?? undefined}>{meta.label}</Badge>;
}
function AttentionList({ entries, total, onActivate }: { readonly entries: OverviewView["attention"]; readonly total: number; readonly onActivate: (entry: OverviewView["attention"][number]) => void }) {
  const [expanded, setExpanded] = useState(false);
  const counts = new Map<string, number>();
  const visible = expanded ? entries : entries.filter((row) => {
    const severity = row.kind.includes("health") ? "health" : row.kind.includes("drift") ? "drift" : "other";
    const count = counts.get(severity) ?? 0;
    if (count >= 5) return false;
    counts.set(severity, count + 1);
    return true;
  });
  return <>
    {entries.length === 0 ? <p className="m-0 py-24 text-13 text-text-muted">Everything looks clear.</p> : (
      <ul className="m-0 list-none p-0">
        {visible.map((row, index) => {
          const action = ATTENTION_ACTION[row.kind];
          return (
            <li key={`${row.kind}-${row.item_id ?? index}`} className="border-t border-border first:border-0">
              <div className="flex min-h-[48px] items-center justify-between gap-12 py-8">
                <button
                  type="button"
                  title={row.kind.includes("locked") ? row.subtitle ?? "Global lock" : undefined}
                  className="flex min-w-0 flex-1 items-center gap-12 bg-transparent text-left text-text hover:bg-overlay focus-visible:outline-2 focus-visible:outline-focus"
                  onClick={() => onActivate(row)}
                >
                  <AttentionStatusBadge row={row} />
                  <span className="min-w-0">
                    <strong className="block truncate text-13">{row.title}</strong>
                    <span className="block truncate text-11 text-text-muted">{row.subtitle ?? row.kind}</span>
                  </span>
                </button>
                {action !== undefined && (
                  <Button
                    size="sm"
                    variant="secondary"
                    aria-label={`${action} for ${row.title}`}
                    title={row.subtitle ?? undefined}
                    onClick={() => onActivate(row)}
                  >
                    {action}
                  </Button>
                )}
              </div>
            </li>
          );
        })}
      </ul>
    )}
    {!expanded && total > visible.length && <Button size="sm" variant="secondary" className="mt-8" onClick={() => setExpanded(true)}>Show all {total}</Button>}
  </>;
}

/** Human verb for any operation kind that still carries a planner-style summary. */
const CHANGE_VERBS: Readonly<Record<string, string>> = {
  edit_mcp: "Updated MCP server",
  edit_skill: "Updated skill",
  edit_rule: "Updated instruction file",
  deploy: "Deployed",
  undeploy: "Removed deployment of",
  migrate: "Migrated",
  adopt: "Adopted",
  restore: "Restored",
  backup: "Backed up",
  update: "Updated",
  drift_adopt: "Adopted drift for",
  drift_restore: "Restored",
  delete: "Deleted",
  promote: "Promoted",
  move: "Moved",
  preset_apply: "Applied preset",
  preset_unapply: "Unapplied preset",
  lock: "Locked",
  unlock: "Unlocked",
  global_write: "Enabled Global writes for",
};

/** The item name a touched path implies, for legacy rows with no readable summary. */
function itemNameFromPath(path: string | undefined): string | null {
  if (!path) return null;
  const parts = path.split("/").filter(Boolean);
  const skills = parts.lastIndexOf("skills");
  const skillName = skills >= 0 ? parts[skills + 1] : undefined;
  if (skillName) return skillName;
  const base = parts.at(-1);
  if (!base) return null;
  return base.replace(/\.(md|json|toml|ya?ml)$/i, "");
}

/** One timeline line. Stored summaries win; planner-era rows map to the same wording. */
function changeLine(row: OverviewView["changes"][number]): string {
  const summary = row.summary?.trim() ?? "";
  if (summary && !/^Plan /i.test(summary) && !/\(one operation\)/i.test(summary)) return summary;
  const name = itemNameFromPath(row.files[0]?.path);
  const verb = CHANGE_VERBS[row.kind];
  if (verb) return name ? `${verb} ${name}` : verb;
  return summary && !/^Plan /i.test(summary) ? summary : name ? `Changed ${name}` : "Changed configuration";
}

/** Shortens a touched-file path to its last two components for the timeline line. */
function shortChangePath(path: string | undefined): string | null {
  if (!path) return null;
  const parts = path.split("/").filter(Boolean);
  if (parts.length <= 2) return parts.join("/");
  return `…/${parts.slice(-2).join("/")}`;
}

function ChangesTimeline({ entries }: { readonly entries: OverviewView["changes"] }) {
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  if (entries.length === 0) return <p className="m-0 py-24 text-13 text-text-muted">No changes yet.</p>;
  const now = new Date();
  const startToday = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const startYesterday = startToday - 86_400_000;
  const groups = new Map<string, typeof entries>();
  for (const row of entries) {
    const time = Date.parse(row.created_at);
    const group = time >= startToday ? "Today" : time >= startYesterday ? "Yesterday" : "Earlier";
    groups.set(group, [...(groups.get(group) ?? []), row]);
  }
  return <div className="space-y-12">{(["Today", "Yesterday", "Earlier"] as const).map((group) => {
    const rows = groups.get(group);
    return rows?.length ? <section key={group} aria-label={group}><h3 className="mb-8 mt-0 text-11 font-semibold uppercase tracking-wide text-text-muted">{group}</h3><ol className="m-0 list-none border-l border-border p-0 pl-16">{rows.map((row) => {
      const isExpanded = expanded.has(row.id);
      const file = shortChangePath(row.files[0]?.path);
      const scopeName = row.scope === null ? null : SCOPE_NAMES[row.scope] ?? row.scope;
      return <li key={row.id} className="relative mb-12 last:mb-0"><span aria-hidden="true" title={scopeName ?? undefined} className={`absolute -left-[20px] top-4 h-8 w-8 rounded-full ring-2 ring-raised ${SCOPE_COLORS[row.scope ?? ""] ?? "bg-scope-project"}`}/><button type="button" aria-expanded={isExpanded} title={row.files[0]?.path} className="w-full bg-transparent p-0 text-left focus-visible:outline-2 focus-visible:outline-focus" onClick={() => setExpanded((current) => { const next = new Set(current); if (next.has(row.id)) next.delete(row.id); else next.add(row.id); return next; })}><strong className="block text-13 text-text">{changeLine(row)}</strong><span className="text-11 text-text-muted">{relativeTime(row.created_at)}{file === null ? "" : ` · ${file}`}{scopeName === null ? null : <span className="sr-only"> · {scopeName} scope</span>}</span></button>{isExpanded && <ul className="mt-6 list-none space-y-4 p-0 text-11 text-text-muted">{row.files.length === 0 ? <li>No file paths recorded</li> : row.files.map((entry, index) => <li key={`${entry.path}-${index}`} className="flex flex-wrap items-center gap-6"><code className="break-all">{entry.path}</code>{entry.scope && <span className="shrink-0 whitespace-nowrap rounded-4 bg-overlay px-4 py-2 text-text-secondary">{SCOPE_NAMES[entry.scope] ?? entry.scope}</span>}</li>)}</ul>}</li>;
    })}</ol></section> : null;
  })}</div>;
}

export function ProjectSyncPanel({ projectId, itemId }: { readonly projectId: string; readonly itemId?: string }) {
  const app = useAppState();
  const [items, setItems] = useState<readonly ProjectSyncItemView[]>([]);
  const [preview, setPreview] = useState<ProjectSyncPreviewView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [comparison, setComparison] = useState<SkillCompareView | null>(null);
  const [stale, setStale] = useState(false);
  const [busy, setBusy] = useState(false);
  const visibleItems = itemId === undefined ? items : items.filter((item) => item.item_id === itemId);
  const refresh = () => loadProjectSyncStatus(projectId).then(setItems, () => setError("Project sync status could not be loaded."));
  useEffect(() => { void refresh(); }, [projectId]);
  const compare = async (item: ProjectSyncItemView) => {
    setBusy(true);
    setError(null);
    try { setComparison(await compareSkillCopies(item.item_id, item.scope === "user" ? "project" : "user")); }
    catch { setError("Both skill copies could not be read for comparison."); }
    finally { setBusy(false); }
  };
  const review = async (reviewItemId: string, direction: "push" | "pull") => {
    setBusy(true);
    setError(null);
    setStale(false);
    try { setPreview(await previewProjectSync(projectId, reviewItemId, direction)); }
    catch { setError("The copies cannot be synchronized safely. Compare them before retrying."); }
    finally { setBusy(false); }
  };
  const apply = async () => {
    if (!preview) return;
    setBusy(true);
    try {
      const result = await applyProjectSync(preview.preview_id);
      setPreview(null);
      setStale(false);
      app.toasts.show({ kind: "success", title: "Project sync complete", message: `${result.files_changed} files were backed up and synchronized.` });
      await refresh();
    } catch {
      setStale(true);
      setError("A target changed after review. Re-diff both copies before applying again.");
    } finally { setBusy(false); }
  };
  return <section className="rounded-10 border border-border bg-raised p-16" aria-label="Project and User sync">
    <div className="mb-12"><h2 className="m-0 text-15 font-semibold text-text">Project sync</h2><p className="mb-0 mt-4 text-12 text-text-muted">Compare Project skills with your User library. Every push or pull is previewed and backed up.</p></div>
    {visibleItems.length === 0 ? <p className="m-0 text-12 text-text-muted">No project skills have a sync status.</p> : <ul className="m-0 list-none p-0">{visibleItems.map((item) => <li key={item.item_id} className="flex flex-wrap items-center justify-between gap-8 border-t border-border py-8 first:border-0"><span className="min-w-0"><strong className="block truncate text-12 text-text">{item.name}</strong><span className={`inline-flex items-center gap-4 rounded-6 px-6 py-3 text-11 ${SYNC_LABEL_COLORS[item.label ?? ""] ?? "bg-overlay text-text-secondary"}`}><span aria-hidden="true">{item.action === "compare" ? "⚠" : item.action === "none" ? "↔" : item.action === "untracked" ? "○" : "●"}</span>{item.label === null ? "Untracked" : SYNC_LABELS[item.label] ?? item.label}</span></span>{item.action === "push" || item.action === "pull" ? <Button size="sm" variant="secondary" disabled={busy} onClick={() => void review(item.item_id, item.action as "push" | "pull")}>{item.action === "push" ? "Push to User library" : "Pull to Project"}</Button> : item.action === "compare" ? <Button size="sm" variant="secondary" disabled={busy} onClick={() => void compare(item)}>Compare copies</Button> : null}</li>)}</ul>}
    {error && <p role="alert" className="mb-0 mt-8 text-12 text-danger">{error}</p>}
    {preview && <div role="dialog" aria-modal="true" aria-labelledby="sync-review-title" className="mt-12 rounded-8 border border-warning/40 bg-warning/5 p-12"><h3 id="sync-review-title" className="m-0 text-14 font-semibold text-text">Review {preview.direction} for {preview.name}</h3><p className="mb-8 mt-4 text-12 text-text-secondary">{preview.files.length} file changes will be backed up before applying.</p><ul className="m-0 max-h-[240px] list-none overflow-auto p-0">{preview.files.map((file) => <li key={file.path} className="border-t border-border py-8 first:border-0"><strong className="block font-mono text-11 text-text">{file.path}</strong><pre className="m-0 overflow-x-auto whitespace-pre-wrap text-11 text-text-secondary">{file.diff}</pre></li>)}</ul>{preview.missing_source_paths.length > 0 && <p className="mb-0 mt-8 text-11 text-warning">Destination-only files are retained: {preview.missing_source_paths.join(", ")}</p>}{stale && <p role="alert" className="mb-0 mt-8 text-12 text-danger">The files changed after this preview. Build a fresh diff before continuing.</p>}<div className="mt-12 flex justify-end gap-8"><Button size="sm" variant="secondary" disabled={busy} onClick={() => { setPreview(null); setStale(false); }}>Cancel</Button>{stale ? <Button size="sm" variant="secondary" disabled={busy} onClick={() => void review(preview.item_id, preview.direction as "push" | "pull")}>Re-diff and review</Button> : <Button size="sm" variant="primary" disabled={busy} onClick={() => void apply()}>{busy ? "Applying…" : "Confirm sync"}</Button>}</div></div>}
    {comparison && <div role="dialog" aria-modal="true" aria-labelledby="sync-compare-title" className="mt-12 rounded-8 border border-border bg-surface p-12"><h3 id="sync-compare-title" className="m-0 text-14 font-semibold text-text">Compare skill copies</h3><p className="mb-8 mt-4 text-12 text-text-secondary">{comparison.winner.scope} and {comparison.shadow.scope} copies {comparison.identical ? "are identical." : "differ."}</p>{comparison.identical ? <p className="m-0 text-12 text-text-muted">No content differences.</p> : <pre className="m-0 max-h-[320px] overflow-auto whitespace-pre-wrap text-11 text-text-secondary">{comparison.diff}</pre>}<div className="mt-12 flex justify-end"><Button size="sm" variant="secondary" onClick={() => setComparison(null)}>Close</Button></div></div>}
  </section>;
}

/** Title, subtitle, CTA and summary heading for each scope the top bar selects. */
function overviewCopy(scope: string, projectName: string | null): { readonly title: string; readonly subtitle: string; readonly cta: string; readonly section: string } {
  if (scope === "user") {
    return {
      title: "Overview · User scope",
      subtitle: "What loads for you in every repo. A repo's Project files can still override these.",
      cta: "+ Add to User",
      section: "Effective for you, every repo",
    };
  }
  if (scope === "global") {
    return {
      title: "Overview · Global scope",
      subtitle: "What every user and agent on this Mac gets. Locked values can't be overridden below.",
      cta: "+ Add to Global",
      section: "Effective for everyone on this Mac",
    };
  }
  const where = projectName === null || projectName === "" ? "this repo" : projectName;
  return {
    title: "Overview",
    subtitle: `What your agents load in ${where}. Nearest scope wins: Project, then User, then Global.`,
    cta: "+ Add to this repo",
    section: "Effective in this repo",
  };
}

function OverviewContent({ data, activeScope, projectId, projectName, onReload }: { readonly data: OverviewView; readonly activeScope: string; readonly projectId: string | null; readonly projectName: string | null; readonly onReload: () => void }) {
  const app = useAppState();
  const cards = data.scopes.filter((scope) => scope.scope !== "local");
  const effective = data.effective ?? { skills: 0, mcp: 0, rules: 0, shadowed_skills: 0, disabled: 0 };
  const total = Number(effective.skills) + Number(effective.mcp) + Number(effective.rules);
  const scopeTotals = data.scopes.map((scope) => ({ ...scope, value: Number(scope.skills) + Number(scope.mcp) + Number(scope.rules) }));
  const scopeCopy = overviewCopy(activeScope, projectName);
  const activeDepth = SCOPE_DEPTH[activeScope] ?? SCOPE_DEPTH.project ?? 2;
  const [globalStatus, setGlobalStatus] = useState(data.global_store.status);
  const [enablingGlobal, setEnablingGlobal] = useState(false);
  const copyUserPath = () => { void navigator.clipboard.writeText(data.user_path).catch(() => undefined); };
  const requestGlobalWrites = () => {
    setEnablingGlobal(true);
    void enableGlobalWrites().then(() => { setGlobalStatus("available"); app.toasts.show({ kind: "success", title: "Global writes enabled", message: "The Global store is ready for reviewed changes." }); }, () => app.toasts.show({ kind: "error", title: "Global writes unavailable", message: "Authorization was declined or the store is still not writable." })).finally(() => setEnablingGlobal(false));
  };
  const [setupCard, setSetupCard] = useState<OverviewScopeView | null>(null);
  const [setupPreview, setSetupPreview] = useState<ScopeSetupPreviewView | null>(null);
  const [setupBusy, setSetupBusy] = useState(false);
  const [setupError, setSetupError] = useState<string | null>(null);
  const [comparisonPreview, setComparisonPreview] = useState<{ readonly name: string; readonly view: SkillCompareView } | null>(null);
  const [attentionBusy, setAttentionBusy] = useState(false);
  const [attentionError, setAttentionError] = useState<string | null>(null);
  const startSetup = async (card: OverviewScopeView) => {
    setSetupCard(card);
    setSetupPreview(null);
    setSetupError(null);
    setSetupBusy(true);
    try {
      const preview = await previewScopeSetup(card.scope, projectId);
      if (preview.already_ready) {
        setSetupCard(null);
        app.toasts.show({ kind: "success", title: "Store already ready", message: "This scope already has an AgenTuna store." });
        onReload();
        return;
      }
      setSetupPreview(preview);
    } catch {
      setSetupError(`The ${SCOPE_NAMES[card.scope] ?? card.scope} store could not be prepared.`);
    } finally {
      setSetupBusy(false);
    }
  };
  const confirmSetup = async () => {
    const previewId = setupPreview?.preview_id;
    if (previewId === null || previewId === undefined) return;
    setSetupBusy(true);
    setSetupError(null);
    try {
      const result = await applyScopeSetup(previewId);
      setSetupPreview(null);
      setSetupCard(null);
      app.toasts.show({ kind: "success", title: "Scope store created", message: `${SCOPE_NAMES[result.scope] ?? result.scope} is ready at ${result.path}.` });
      onReload();
    } catch {
      setSetupPreview(null);
      setSetupCard(null);
      setSetupError("The store changed after review; start the setup again.");
    } finally {
      setSetupBusy(false);
    }
  };

  /**
   * Runs one attention row's direct action through the same typed commands
   * the destination screens use, so the button and the screen cannot diverge.
   */
  const activateAttention = (row: OverviewView["attention"][number]) => {
    const openFallback = () => {
      const route = row.screen === "mcp" ? "mcp-list" : row.screen === "drift" ? "drift" : row.screen === "agents" ? "agents" : row.screen === "effective" ? "effective" : "skills";
      if (row.row_id !== null) app.setSelection(route, [row.row_id]);
      app.navigate(route);
    };
    if (row.kind === "drift_event" && row.row_id !== null) {
      app.setSelection("drift", [row.row_id]);
      window.dispatchEvent(new CustomEvent("agentuna:menu-action", { detail: ACTION_IDS.drift }));
      return;
    }
    if (row.kind === "missing_agent" && row.row_id !== null) {
      app.setSelection("agents", [row.row_id]);
      window.dispatchEvent(new CustomEvent("agentuna:menu-action", { detail: ACTION_IDS.agents }));
      return;
    }
    if (row.kind === "locked_item_not_found" && row.row_id !== null) {
      app.setSelection("effective", [row.row_id]);
      window.dispatchEvent(new CustomEvent("agentuna:menu-action", { detail: ACTION_IDS.effective }));
      return;
    }
    if (row.item_id === null) {
      openFallback();
      return;
    }
    const itemId = row.item_id;
    if (row.kind === "shadowed_skill") {
      setAttentionBusy(true);
      setAttentionError(null);
      void loadSkillDetail(itemId)
        .then(async (detail) => {
          const other = detail.shadow_chain.find((scope) => scope !== detail.scope) ?? detail.shadowed_by;
          if (other === null) {
            const item = await invoke<LookupItem | null>("item_lookup", { itemId });
            if (item === null) openFallback();
            else app.openItem(itemId, item);
            return;
          }
          setComparisonPreview({ name: detail.name, view: await compareSkillCopies(itemId, other) });
        })
        .catch(() => setAttentionError("The two skill copies could not be compared."))
        .finally(() => setAttentionBusy(false));
      return;
    }
    if (row.kind === "failing_health_check") {
      // The MCP detail screen opens its token repair dialog when this flag is set.
      app.setScreenState("mcp-detail:fix-token", true);
    }
    void invoke<LookupItem | null>("item_lookup", { itemId }).then(
      (item) => {
        if (item === null) openFallback();
        else app.openItem(itemId, item);
      },
      () => openFallback(),
    );
  };
  const disabledLabel = activeScope === "project" ? "in this repo" : activeScope === "user" ? "for you" : activeScope === "global" ? "for everyone" : "in this scope";
  const shadowed = Number(effective.shadowed_skills);
  const disabled = Number(effective.disabled);
  const effectiveParts: string[] = [];
  if (shadowed > 0) effectiveParts.push(`${shadowed} skill${shadowed === 1 ? "" : "s"} shadowed by a nearer scope`);
  if (disabled > 0) effectiveParts.push(`${disabled} item${disabled === 1 ? "" : "s"} disabled ${disabledLabel}`);
  const effectiveLine = effectiveParts.length > 0 ? effectiveParts.join(" · ") : "No shadowed or disabled items.";
  return <div className="flex flex-col gap-24">
    <header className="flex flex-wrap items-start justify-between gap-16"><div><h1 className="m-0 text-26 font-semibold text-text">{scopeCopy.title}</h1><p className="mb-0 mt-8 text-13 text-text-secondary">{scopeCopy.subtitle}</p></div><Button variant="primary" onClick={() => app.navigate("add-skill")}>{scopeCopy.cta}</Button></header>
    {(data.startup_warnings ?? []).map((warning) => <section key={`${warning.kind}-${warning.path ?? ""}`} role="status" aria-label="Startup warning" className="rounded-8 border border-warning/40 bg-warning/10 p-12"><p className="m-0 text-13 text-text">{warning.message}</p>{warning.path && <p className="mb-0 mt-4 break-words font-mono text-11 text-text-muted">{warning.path}</p>}</section>)}
    {activeScope === "global" && globalStatus !== "available" && <section role="status" className="flex flex-wrap items-center justify-between gap-12 rounded-8 border border-warning/40 bg-warning/10 p-12"><span className="text-13 text-text">Global store is read-only{data.global_store.reason ? `: ${data.global_store.reason}` : "."}</span><Button size="sm" variant="secondary" loading={enablingGlobal} onClick={requestGlobalWrites}>Enable global writes</Button></section>}
    <section aria-label="Scope stores" className="grid grid-cols-1 gap-16 md:grid-cols-3">{cards.map((card) => <ScopeCard key={card.scope} card={card} selected={activeScope === card.scope} dimmed={(SCOPE_DEPTH[card.scope] ?? 0) > activeDepth} setupBusy={setupBusy && setupCard?.scope === card.scope} onSetUp={() => void startSetup(card)} onCopy={copyUserPath} />)}</section>
    {setupError && <p role="alert" className="m-0 rounded-8 border border-danger/40 bg-danger/10 p-12 text-13 text-text">{setupError}</p>}
    <section className="grid gap-16 rounded-10 border border-border bg-surface p-16 md:grid-cols-[minmax(0,1.2fr)_minmax(0,1fr)]" aria-label="Effective configuration summary">
      <div>
        <h2 className="m-0 text-14 font-semibold text-text">{scopeCopy.section}</h2>
        <p className="mb-0 mt-10 text-18 font-semibold text-text">{Number(effective.skills)} skills · {Number(effective.mcp)} MCP servers · {Number(effective.rules)} instruction file{Number(effective.rules) === 1 ? "" : "s"}</p>
        <p className="mb-0 mt-6 text-12 text-text-muted">{effectiveLine}</p>
      </div>
      <div>
        <div className="mb-8 flex justify-between text-12 text-text-secondary"><span>Where they come from</span><span>{total} items</span></div>
        <div className="flex h-10 overflow-hidden rounded-full bg-overlay" role="img" aria-label={`Items by scope: ${scopeTotals.map((scope) => `${SCOPE_NAMES[scope.scope] ?? scope.scope} ${scope.value}`).join(", ")}`}>{scopeTotals.map((scope) => scope.value > 0 && <span key={scope.scope} className={SCOPE_COLORS[scope.scope] ?? "bg-overlay"} style={{ width: `${total === 0 ? 0 : scope.value / total * 100}%` }} />)}</div>
        <div className="mt-10 flex flex-wrap gap-x-16 gap-y-6 text-12 text-text-muted">{scopeTotals.map((scope) => <span key={scope.scope} aria-label={`${SCOPE_NAMES[scope.scope] ?? scope.scope} ${scope.value} items`} className="inline-flex items-center gap-6"><i aria-hidden="true" className={`size-8 rounded-full ${SCOPE_COLORS[scope.scope] ?? "bg-overlay"}`} />{SCOPE_NAMES[scope.scope] ?? scope.scope} <strong className="font-mono font-medium text-text-secondary">{scope.value}</strong></span>)}</div>
      </div>
    </section>
    <div className="grid gap-16 lg:grid-cols-[minmax(0,1.6fr)_minmax(0,1fr)]">
      <section className="rounded-10 border border-border bg-raised p-16">
        <div className="mb-12 flex items-center justify-between"><h2 className="m-0 text-15 font-semibold text-text">Needs attention</h2><span className="text-12 text-text-muted">{Number(data.attention_total)} items</span></div>
        <AttentionList entries={data.attention} total={Number(data.attention_total)} onActivate={activateAttention} />
        {attentionBusy && <p role="status" className="m-0 mt-8 text-12 text-text-muted">Comparing the two skill copies…</p>}
        {attentionError !== null && <p role="alert" className="m-0 mt-8 text-12 text-danger">{attentionError}</p>}
      </section>
      <section className="rounded-10 border border-border bg-raised p-16"><h2 className="mb-12 mt-0 text-15 font-semibold text-text">Recent changes</h2><ChangesTimeline entries={data.changes} /></section>
    </div>
    {projectId !== null && activeScope === "project" && <ProjectSyncPanel key={projectId} projectId={projectId} />}
    <Modal
      open={setupPreview !== null}
      onOpenChange={(open) => { if (!open && !setupBusy) { setSetupPreview(null); setSetupCard(null); } }}
      title={`Set up the ${setupCard ? SCOPE_NAMES[setupCard.scope] ?? setupCard.scope : ""} store`}
      description={setupPreview?.path}
      footer={<><Button variant="secondary" onClick={() => { setSetupPreview(null); setSetupCard(null); }}>Cancel</Button><Button variant="primary" loading={setupBusy} onClick={() => void confirmSetup()}>Create store</Button></>}
    >
      <p className="m-0 text-13 text-text-secondary">These entries will be created. Existing and foreign entries are left untouched.</p>
      <ul className="mb-0 mt-12 max-h-[260px] list-none overflow-auto p-0" aria-label="Planned store entries">
        {setupPreview?.files.map((file) => <li key={file.path} className="flex items-center justify-between gap-12 border-t border-border py-6 first:border-0"><span className="font-mono text-11 text-text">{file.path}</span><span className="text-11 text-text-muted">{file.action === "mkdir" ? "folder" : "file"}</span></li>)}
      </ul>
      {setupPreview !== null && setupPreview.foreign.length > 0 && <p className="mb-0 mt-12 text-11 text-warning">Left untouched: {setupPreview.foreign.join(", ")}</p>}
    </Modal>
    <Modal
      open={comparisonPreview !== null}
      onOpenChange={(open) => { if (!open) setComparisonPreview(null); }}
      title={`Compare ${comparisonPreview?.name ?? "skill"} copies`}
      description={comparisonPreview === null
        ? undefined
        : `${comparisonPreview.view.winner.scope} and ${comparisonPreview.view.shadow.scope} copies ${comparisonPreview.view.identical ? "are identical." : "differ."}`}
      footer={<Button variant="secondary" onClick={() => setComparisonPreview(null)}>Close</Button>}
    >
      {comparisonPreview !== null && (comparisonPreview.view.identical
        ? <p className="m-0 text-12 text-text-muted">No content differences.</p>
        : <pre className="m-0 max-h-[420px] overflow-auto whitespace-pre-wrap text-11 text-text-secondary">{comparisonPreview.view.diff}</pre>)}
    </Modal>
  </div>;
}

export const scopeScrollPositions = new Map<string, number>();

/** Overview uses only the typed Rust query; errors and loading are never hidden by sample data. */
export function OverviewScreen({ projectName = null }: { readonly projectName?: string | null } = {}) {
  const { scope, activeProject } = useAppSnapshot();
  const activeScope = scope;
  const resource = useAsyncResource(() => loadOverview(activeScope), { deps: [activeScope], isEmpty: (data) => !data || !Array.isArray(data.scopes) || data.scopes.length === 0 });

  useEffect(() => {
    const target = document.getElementById("shell-main");
    const readScroll = () => target ? target.scrollTop : (typeof window !== "undefined" ? window.scrollY : 0);
    const applyScroll = (y: number) => {
      if (target) target.scrollTop = y;
      else if (typeof window !== "undefined" && typeof window.scrollTo === "function") window.scrollTo(0, y);
    };

    applyScroll(scopeScrollPositions.get(activeScope) ?? 0);

    const onScroll = () => {
      scopeScrollPositions.set(activeScope, readScroll());
    };

    const listenerTarget = target ?? (typeof window !== "undefined" ? window : null);
    listenerTarget?.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      scopeScrollPositions.set(activeScope, readScroll());
      listenerTarget?.removeEventListener("scroll", onScroll);
    };
  }, [activeScope]);

  return (
    <main aria-label="Overview" className="mx-auto flex w-full max-w-[1240px] flex-col gap-24 p-24">
      <AsyncBoundary state={resource} loadingTitle="Loading overview" loadingBody="Reading scope and change summaries" skeletonRows={3} errorTitle="Overview could not load" minHeight={360} empty={{ title: "No scope data", body: "The overview query returned no scope summaries." }}>
        {(data) => <OverviewContent key={activeScope} data={data} activeScope={activeScope} projectId={activeProject} projectName={projectName} onReload={resource.retry} />}
      </AsyncBoundary>
    </main>
  );
}
