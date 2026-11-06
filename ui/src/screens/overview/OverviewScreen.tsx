import { useEffect, useMemo, useState } from "react";
import { Button } from "../../components/button";
import { AsyncBoundary } from "../../components/states";
import { useAsyncResource } from "../../components/states";
import { useAppSnapshot, useAppState } from "../../state/context";
import type { OverviewView } from "../../generated/OverviewView";
import type { OverviewScopeView } from "../../generated/OverviewScopeView";
import type { ProjectSyncItemView } from "../../generated/ProjectSyncItemView";
import type { ProjectSyncPreviewView } from "../../generated/ProjectSyncPreviewView";
import type { SkillCompareView } from "../../generated/SkillCompareView";
import { compareSkillCopies } from "../skill_detail/commands";
import { enableGlobalWrites, loadOverview, loadProjectSyncStatus, previewProjectSync, applyProjectSync } from "./commands";
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

function ScopeCard({ card, path, selected, onSetUp, onCopy }: { readonly card: OverviewScopeView; readonly path: string; readonly selected: boolean; readonly onSetUp: () => void; readonly onCopy: () => void }) {
  const skills = Number(card.skills);
  const mcp = Number(card.mcp);
  const rules = Number(card.rules);
  const locks = Number(card.locks);
  const disabled = Number(card.disabled);
  const empty = skills + mcp + rules + locks + disabled === 0 && card.last_edited === null;
  const scopeColor = card.scope === "global" || card.scope === "user" || card.scope === "project" || card.scope === "local" ? `var(--at-color-scope-${card.scope})` : "var(--at-color-border)";
  return (
    <article className={`flex h-[214px] flex-col gap-8 rounded-10 border p-12 ${selected ? "bg-raised shadow-[0_0_0_1px_var(--at-color-border)]" : "border-border bg-raised"}`} style={selected ? { borderColor: scopeColor, boxShadow: `0 0 0 1px ${scopeColor}` } : undefined} aria-current={selected ? "true" : undefined}>
      <div className="flex items-center justify-between">
        <span className={`rounded-6 px-8 py-4 text-12 font-semibold ${SCOPE_COLORS[card.scope] ?? "bg-overlay"}`}>{SCOPE_NAMES[card.scope] ?? card.scope}</span>
        {locks > 0 && <span className="inline-flex items-center gap-4 text-12 text-warning" aria-label={`${locks} locked items`} title="Global locks protect these items"><svg aria-hidden="true" className="h-12 w-12 fill-none stroke-current" viewBox="0 0 16 16"><rect x="3" y="7" width="10" height="8" rx="1" /><path d="M5 7V5a3 3 0 0 1 6 0v2" /></svg>{locks} locked</span>}
      </div>
      {empty ? <div className="flex flex-1 flex-col justify-center gap-8"><p className="m-0 flex min-w-0 items-center gap-6 font-mono text-12 text-text-muted"><span className="min-w-0 flex-1 truncate" title={path}>{path}</span>{card.scope === "user" && <button type="button" className="shrink-0 rounded-4 px-6 py-3 font-sans text-11 text-text-secondary hover:bg-overlay focus-visible:outline-2 focus-visible:outline-focus" onClick={onCopy}>Copy path</button>}</p><p className="m-0 text-13 text-text-secondary">This scope has no AgenTuna store yet.</p><Button size="sm" className="self-start" onClick={onSetUp}>Set up this scope</Button></div> : <>
        <p className="m-0 flex min-w-0 items-center gap-6 font-mono text-12 text-text-muted"><span className="min-w-0 flex-1 truncate" title={path}>{path}</span>{card.scope === "user" && <button type="button" className="shrink-0 rounded-4 px-6 py-3 font-sans text-11 text-text-secondary hover:bg-overlay focus-visible:outline-2 focus-visible:outline-focus" onClick={onCopy}>Copy path</button>}</p>
        <div className="grid grid-cols-3 gap-8">
          <Count value={skills} label="skills" />
          <Count value={mcp} label="MCP servers" />
          <Count value={rules} label="instruction files" />
        </div>
        <div className="mt-auto flex justify-between border-t border-border pt-8 text-11 text-text-muted"><span>{disabled > 0 ? `${disabled} disabled` : "No disabled items"}</span><span>{relativeTime(card.last_edited)}</span></div>
      </>}
    </article>
  );
}

function Count({ value, label }: { readonly value: number; readonly label: string }) {
  return <div><strong className="block font-mono text-20 text-text">{value}</strong><span className="text-11 text-text-muted">{label}</span></div>;
}

function AttentionMarker({ row }: { readonly row: OverviewView["attention"][number] }) {
  if (row.kind.includes("locked")) return <span aria-label="Locked item" title={row.subtitle ?? "Global lock"} className="mt-2 inline-flex h-12 w-12 shrink-0 items-center justify-center text-warning"><svg aria-hidden="true" className="h-12 w-12 fill-none stroke-current" viewBox="0 0 16 16"><rect x="3" y="7" width="10" height="8" rx="1" /><path d="M5 7V5a3 3 0 0 1 6 0v2" /></svg></span>;
  return <span aria-hidden="true" className={`mt-4 h-8 w-8 shrink-0 rounded-full ${row.kind.includes("health") ? "bg-danger" : row.kind.includes("drift") ? "bg-warning" : "bg-scope-user"}`} />;
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
    {entries.length === 0 ? <p className="m-0 py-24 text-13 text-text-muted">Everything looks clear.</p> : <ul className="m-0 list-none p-0">{visible.map((row, index) => <li key={`${row.kind}-${row.item_id ?? index}`} className="border-t border-border first:border-0"><button title={row.kind.includes("locked") ? row.subtitle ?? "Global lock" : undefined} className="flex min-h-[48px] w-full items-center justify-between gap-12 bg-transparent py-8 text-left text-text hover:bg-overlay focus-visible:outline-2 focus-visible:outline-focus" onClick={() => onActivate(row)}><span className="flex items-start gap-8"><AttentionMarker row={row}/><span><strong className="block text-13">{row.title}</strong><span className="text-11 text-text-muted">{row.subtitle ?? row.kind}</span></span></span><span aria-hidden="true" className="text-text-muted">›</span></button></li>)}</ul>}
    {!expanded && total > visible.length && <Button size="sm" variant="secondary" className="mt-8" onClick={() => setExpanded(true)}>Show all {total}</Button>}
  </>;
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
      return <li key={row.id} className="relative mb-12 last:mb-0"><span aria-hidden="true" className="absolute -left-21 top-4 h-8 w-8 rounded-full bg-scope-project ring-2 ring-raised"/><button type="button" aria-expanded={isExpanded} className="w-full bg-transparent p-0 text-left focus-visible:outline-2 focus-visible:outline-focus" onClick={() => setExpanded((current) => { const next = new Set(current); if (next.has(row.id)) next.delete(row.id); else next.add(row.id); return next; })}><strong className="block text-13 text-text">{row.summary ?? row.kind}</strong><span className="text-11 text-text-muted">{relativeTime(row.created_at)} · {row.file_count} files{row.scope ? ` · ${SCOPE_NAMES[row.scope] ?? row.scope}` : ""}</span></button>{isExpanded && <ul className="mt-6 list-none space-y-4 p-0 text-11 text-text-muted">{row.files.length === 0 ? <li>No file paths recorded</li> : row.files.map((file, index) => <li key={`${file.path}-${index}`} className="flex flex-wrap items-center gap-6"><code className="break-all">{file.path}</code>{file.scope && <span className={`rounded-4 px-4 py-2 ${SCOPE_COLORS[file.scope] ?? "bg-overlay"}`}>{SCOPE_NAMES[file.scope] ?? file.scope}</span>}</li>)}</ul>}</li>;
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

function OverviewContent({ data, activeScope, projectId }: { readonly data: OverviewView; readonly activeScope: string; readonly projectId: string | null }) {
  const total = data.scopes.reduce((sum, scope) => sum + Number(scope.skills) + Number(scope.mcp) + Number(scope.rules), 0);
  const percentages = useMemo(() => data.scopes.map((scope) => ({ ...scope, value: total === 0 ? 0 : (Number(scope.skills) + Number(scope.mcp) + Number(scope.rules)) / total * 100 })), [data.scopes, total]);
  const app = useAppState();
  const [globalWritable, setGlobalWritable] = useState(data.global_writable);
  const [enablingGlobal, setEnablingGlobal] = useState(false);
  const cards = data.scopes.filter((scope) => scope.scope !== "local");
  const scopePath = (scope: OverviewScopeView) => scope.scope === "project" ? "./.agentuna" : scope.scope === "user" ? data.user_path : scope.scope === "global" ? data.global_path : "Local store";
  const copyUserPath = () => { void navigator.clipboard.writeText(data.user_path).catch(() => undefined); };
  const requestGlobalWrites = () => {
    setEnablingGlobal(true);
    void enableGlobalWrites().then(() => { setGlobalWritable(true); app.toasts.show({ kind: "success", title: "Global writes enabled", message: "The Global store is ready for reviewed changes." }); }, () => app.toasts.show({ kind: "error", title: "Global writes unavailable", message: "Authorization was declined or the store is still not writable." })).finally(() => setEnablingGlobal(false));
  };
  return <div className="mx-auto flex w-full max-w-[1240px] flex-col gap-24 p-24">
    <header className="flex flex-wrap items-start justify-between gap-16"><div><h1 className="m-0 text-26 font-semibold text-text">Overview</h1><p className="mb-0 mt-8 text-13 text-text-secondary">What your agents load here. Nearest scope wins: Project, then User, then Global.</p></div><Button variant="primary" onClick={() => app.navigate("add-skill")}>Add to this repo</Button></header>
    {activeScope === "global" && !globalWritable && <section role="status" className="flex flex-wrap items-center justify-between gap-12 rounded-8 border border-warning/40 bg-warning/10 p-12"><span className="text-13 text-text">Global store is read-only{data.global_reason ? `: ${data.global_reason}` : "."}</span><Button size="sm" variant="secondary" loading={enablingGlobal} onClick={requestGlobalWrites}>Enable global writes</Button></section>}
    <section aria-label="Scope stores" className="grid grid-cols-1 gap-16 md:grid-cols-3">{cards.map((card) => <ScopeCard key={card.scope} card={card} path={scopePath(card)} selected={activeScope === card.scope} onSetUp={() => app.navigate("add-skill")} onCopy={copyUserPath} />)}</section>
    <section className="grid gap-16 rounded-10 border border-border bg-raised p-16 md:grid-cols-2" aria-label="Effective configuration summary">
      <div><h2 className="m-0 text-14 font-semibold text-text">Effective in this repo</h2><p className="mb-0 mt-8 font-mono text-16 font-semibold text-text">{total} indexed items</p><p className="mb-0 mt-4 text-12 text-text-muted">Skills, servers and instruction files across scopes</p></div>
      <div><div className="mb-8 flex justify-between text-12 text-text-secondary"><span>Where they come from</span><span>{total} items</span></div><div className="flex h-8 overflow-hidden rounded-full bg-overlay" role="img" aria-label="Items by scope">{percentages.map((scope) => scope.value > 0 && <span key={scope.scope} className={SCOPE_COLORS[scope.scope] ?? "bg-overlay"} style={{ width: `${scope.value}%` }} />)}</div><div className="mt-8 flex flex-wrap gap-12 text-11 text-text-muted">{percentages.map((scope) => <span key={scope.scope} className="inline-flex items-center gap-4"><i aria-hidden="true" className={`h-8 w-8 rounded-full ${SCOPE_COLORS[scope.scope] ?? "bg-overlay"}`} />{SCOPE_NAMES[scope.scope] ?? scope.scope} {Math.round(scope.value)}%</span>)}</div></div>
    </section>
    <div className="grid gap-16 lg:grid-cols-[1.6fr_1fr]">
      <section className="rounded-10 border border-border bg-raised p-16"><div className="mb-12 flex items-center justify-between"><h2 className="m-0 text-15 font-semibold text-text">Needs attention</h2><span className="text-12 text-text-muted">{Number(data.attention_total)} items</span></div><AttentionList entries={data.attention} total={Number(data.attention_total)} onActivate={(row) => { const route = row.screen === "mcp" ? "mcp-list" : row.screen === "drift" ? "drift" : row.screen === "agents" ? "agents" : "skills"; if (row.row_id) app.setSelection(route, [row.row_id]); app.navigate(route); }} /></section>
      <section className="rounded-10 border border-border bg-raised p-16"><h2 className="mb-12 mt-0 text-15 font-semibold text-text">Recent changes</h2><ChangesTimeline entries={data.changes} /></section>
    </div>
    {projectId !== null && activeScope === "project" && <ProjectSyncPanel key={projectId} projectId={projectId} />}
  </div>;
}

/** Overview uses only the typed Rust query; errors and loading are never hidden by sample data. */
export function OverviewScreen() {
  const { scope, activeProject } = useAppSnapshot();
  const activeScope = scope;
  const resource = useAsyncResource(() => loadOverview(activeScope), { deps: [activeScope], isEmpty: (data) => data.scopes.length === 0 });
  return <AsyncBoundary state={resource} loadingTitle="Loading overview" loadingBody="Reading scope and change summaries" skeletonRows={3} errorTitle="Overview could not load" minHeight={360} empty={{ title: "No scope data", body: "The overview query returned no scope summaries." }}>
    {(data) => <OverviewContent data={data} activeScope={activeScope} projectId={activeProject} />}
  </AsyncBoundary>;
}
