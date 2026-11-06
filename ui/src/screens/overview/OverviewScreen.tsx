import { useMemo } from "react";
import { Button } from "../../components/button";
import { AsyncBoundary } from "../../components/states";
import { useAsyncResource } from "../../components/states";
import { useAppSnapshot, useAppState } from "../../state/context";
import type { OverviewView } from "../../generated/OverviewView";
import type { OverviewScopeView } from "../../generated/OverviewScopeView";
import { loadOverview } from "./commands";

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

function ScopeCard({ card, selected, onSetUp }: { readonly card: OverviewScopeView; readonly selected: boolean; readonly onSetUp: () => void }) {
  const skills = Number(card.skills);
  const mcp = Number(card.mcp);
  const rules = Number(card.rules);
  const locks = Number(card.locks);
  const disabled = Number(card.disabled);
  const empty = skills + mcp + rules + locks + disabled === 0 && card.last_edited === null;
  const scopeColor = card.scope === "global" || card.scope === "user" || card.scope === "project" || card.scope === "local" ? `var(--at-color-scope-${card.scope})` : "var(--at-color-border)";
  return (
    <article className={`flex min-h-176 flex-col gap-16 rounded-10 border p-16 ${selected ? "bg-raised shadow-[0_0_0_1px_var(--at-color-border)]" : "border-border bg-raised"}`} style={selected ? { borderColor: scopeColor, boxShadow: `0 0 0 1px ${scopeColor}` } : undefined} aria-current={selected ? "true" : undefined}>
      <div className="flex items-center justify-between">
        <span className={`rounded-6 px-8 py-4 text-12 font-semibold ${SCOPE_COLORS[card.scope] ?? "bg-overlay"}`}>{SCOPE_NAMES[card.scope] ?? card.scope}</span>
        {locks > 0 && <span className="text-12 text-warning">{locks} locked</span>}
      </div>
      {empty ? <div className="flex flex-1 flex-col justify-center gap-8"><p className="m-0 text-13 text-text-secondary">This scope has no AgenTuna store yet.</p><Button size="sm" className="self-start" onClick={onSetUp}>Set up this scope</Button></div> : <>
        <p className="m-0 break-all font-mono text-12 text-text-muted">{card.scope === "project" ? "./.agentuna" : card.scope === "user" ? "~/.agentuna" : card.scope === "global" ? "Global store" : "Local store"}</p>
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
  return <div><strong className="block font-mono text-22 text-text">{value}</strong><span className="text-11 text-text-muted">{label}</span></div>;
}

function OverviewContent({ data, activeScope }: { readonly data: OverviewView; readonly activeScope: string }) {
  const total = data.scopes.reduce((sum, scope) => sum + Number(scope.skills) + Number(scope.mcp) + Number(scope.rules), 0);
  const percentages = useMemo(() => data.scopes.map((scope) => ({ ...scope, value: total === 0 ? 0 : (Number(scope.skills) + Number(scope.mcp) + Number(scope.rules)) / total * 100 })), [data.scopes, total]);
  const app = useAppState();
  const cards = data.scopes.filter((scope) => scope.scope !== "local");
  return <div className="mx-auto flex w-full max-w-[1240px] flex-col gap-24 p-24">
    <header className="flex flex-wrap items-start justify-between gap-16"><div><h1 className="m-0 text-26 font-semibold text-text">Overview</h1><p className="mb-0 mt-8 text-13 text-text-secondary">What your agents load here. Nearest scope wins: Project, then User, then Global.</p></div><Button variant="primary" onClick={() => app.navigate("add-skill")}>Add to this repo</Button></header>
    <section aria-label="Scope stores" className="grid grid-cols-1 gap-16 md:grid-cols-3">{cards.map((card) => <ScopeCard key={card.scope} card={card} selected={activeScope === card.scope} onSetUp={() => app.navigate("add-skill")} />)}</section>
    <section className="grid gap-16 rounded-10 border border-border bg-raised p-16 md:grid-cols-2" aria-label="Effective configuration summary">
      <div><h2 className="m-0 text-14 font-semibold text-text">Effective in this repo</h2><p className="mb-0 mt-8 font-mono text-16 font-semibold text-text">{total} indexed items</p><p className="mb-0 mt-4 text-12 text-text-muted">Skills, servers and instruction files across scopes</p></div>
      <div><div className="mb-8 flex justify-between text-12 text-text-secondary"><span>Where they come from</span><span>{total} items</span></div><div className="flex h-8 overflow-hidden rounded-full bg-overlay" role="img" aria-label="Items by scope">{percentages.map((scope) => scope.value > 0 && <span key={scope.scope} className={SCOPE_COLORS[scope.scope] ?? "bg-overlay"} style={{ width: `${scope.value}%` }} />)}</div><div className="mt-8 flex flex-wrap gap-12 text-11 text-text-muted">{percentages.map((scope) => <span key={scope.scope} className="inline-flex items-center gap-4"><i aria-hidden="true" className={`h-8 w-8 rounded-full ${SCOPE_COLORS[scope.scope] ?? "bg-overlay"}`} />{SCOPE_NAMES[scope.scope] ?? scope.scope} {Math.round(scope.value)}%</span>)}</div></div>
    </section>
    <div className="grid gap-16 lg:grid-cols-[1.6fr_1fr]">
      <section className="rounded-10 border border-border bg-raised p-16"><div className="mb-12 flex items-center justify-between"><h2 className="m-0 text-15 font-semibold text-text">Needs attention</h2><span className="text-12 text-text-muted">{Number(data.attention_total)} items</span></div>{data.attention.length === 0 ? <p className="m-0 py-24 text-13 text-text-muted">Everything looks clear.</p> : <ul className="m-0 list-none p-0">{data.attention.map((row, index) => <li key={`${row.kind}-${row.item_id ?? index}`} className="border-t border-border first:border-0"><button className="flex min-h-48 w-full items-center justify-between gap-12 bg-transparent py-8 text-left text-text hover:bg-overlay focus-visible:outline-2 focus-visible:outline-focus" onClick={() => { const route = row.screen === "mcp" ? "mcp-list" : row.screen === "drift" ? "drift" : row.screen === "agents" ? "agents" : "skills"; if (row.row_id) app.setSelection(route, [row.row_id]); app.navigate(route); }}><span><strong className="block text-13">{row.title}</strong><span className="text-11 text-text-muted">{row.subtitle ?? row.kind}</span></span><span aria-hidden="true" className="text-text-muted">›</span></button></li>)}</ul>}</section>
      <section className="rounded-10 border border-border bg-raised p-16"><h2 className="mb-12 mt-0 text-15 font-semibold text-text">Recent changes</h2>{data.changes.length === 0 ? <p className="m-0 py-24 text-13 text-text-muted">No recent changes yet.</p> : <ol className="m-0 list-none border-l border-border p-0 pl-16">{data.changes.map((row) => <li key={row.id} className="relative mb-16 last:mb-0"><span aria-hidden="true" className="absolute -left-21 top-4 h-8 w-8 rounded-full bg-scope-project ring-2 ring-raised"/><strong className="block text-13 text-text">{row.summary ?? row.kind}</strong><span className="text-11 text-text-muted">{relativeTime(row.created_at)} · {row.file_count} files{row.scope ? ` · ${SCOPE_NAMES[row.scope] ?? row.scope}` : ""}</span></li>)}</ol>}</section>
    </div>
  </div>;
}

/** Overview uses only the typed Rust query; errors and loading are never hidden by sample data. */
export function OverviewScreen() {
  const { scope } = useAppSnapshot();
  const activeScope = scope;
  const resource = useAsyncResource(() => loadOverview(activeScope), { deps: [activeScope], isEmpty: (data) => data.scopes.length === 0 });
  return <AsyncBoundary state={resource} loadingTitle="Loading overview" loadingBody="Reading scope and change summaries" skeletonRows={3} errorTitle="Overview could not load" minHeight={360} empty={{ title: "No scope data", body: "The overview query returned no scope summaries." }}>
    {(data) => <OverviewContent data={data} activeScope={activeScope} />}
  </AsyncBoundary>;
}
