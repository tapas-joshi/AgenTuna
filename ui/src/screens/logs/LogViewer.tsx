import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef, useState } from "react";
import { StateView } from "../../components/states";

export type LogLine = { readonly level: string; readonly target: string; readonly text: string };
export type LogTail = { readonly path: string; readonly lines: readonly LogLine[] };
const LEVELS = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"] as const;

/** Read-only, bounded log viewer. Backend redaction is required before any line reaches this view. */
export function LogViewer() {
  const [result, setResult] = useState<LogTail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [level, setLevel] = useState<string>("ALL");
  const [module, setModule] = useState("");
  const [query, setQuery] = useState("");
  const [tailFollow, setTailFollow] = useState(true);
  const [scrollTop, setScrollTop] = useState(0);
  const [working, setWorking] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionNotice, setActionNotice] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const reload = () => {
    setWorking(true); setError(null);
    void invoke<LogTail>("logs_tail", { limit: 1000 }).then(setResult, (reason: unknown) => setError(String(reason))).finally(() => setWorking(false));
  };
  useEffect(reload, []);
  useEffect(() => {
    if (tailFollow && result !== null && scrollRef.current !== null) scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [result, tailFollow]);
  const visibleLines = useMemo(() => (result?.lines ?? []).filter(line =>
    (level === "ALL" || line.level.toUpperCase() === level) &&
    (module.trim() === "" || line.target.includes(module.trim())) &&
    (query.trim() === "" || line.text.toLowerCase().includes(query.trim().toLowerCase()))), [result, level, module, query]);
  if (result === null && working) return <StateView variant="loading" title="Loading logs" />;
  if (result === null && error !== null) return <StateView variant="error" title="Couldn't read the current log" body={error} details={{ text: error }} actions={[{ label: "Retry", onClick: reload }]} />;
  if (result === null || result.lines.length === 0) return <StateView variant="empty" title="No log entries yet" body={result ? `Log file: ${result.path}` : "No log directory or current log file was found."} actions={result ? [] : [{ label: "Retry", onClick: reload }]} />;
  const rowHeight = 28;
  const start = Math.max(0, Math.floor(scrollTop / rowHeight) - 5);
  const shown = visibleLines.slice(start, start + 55);
  return <main className="mx-auto flex h-full max-w-[1100px] flex-col gap-16 p-24" data-testid="log-viewer">
    <header className="flex items-start justify-between gap-16">
      <div><h1 className="m-0 text-22 font-semibold text-text">Logs & diagnostics</h1><p className="mt-4 break-all font-mono text-11 text-muted">{result.path}</p></div>
      <div className="flex gap-8">
        <button type="button" className="rounded-6 border border-border px-10 py-8 text-12 text-text" onClick={() => { setActionError(null); void invoke("logs_reveal").catch((reason: unknown) => setActionError(String(reason))); }}>Reveal in Finder</button>
        <button type="button" className="rounded-6 bg-accent px-10 py-8 text-12 text-surface" onClick={() => { setActionError(null); setActionNotice(null); void invoke<unknown>("diagnostics_export").then(outcome => { if (outcome !== "cancelled") setActionNotice("Diagnostics exported"); }, (reason: unknown) => setActionError(String(reason))); }}>Export diagnostics</button>
      </div>
    </header>
    {actionError && <div role="alert" className="text-13 text-danger">{actionError}</div>}{actionNotice && <div role="status" className="text-13 text-success">{actionNotice}</div>}
    <div className="flex flex-wrap items-end gap-12 rounded-10 border border-border bg-surface p-12">
      <label className="text-12 text-secondary">Level<select aria-label="Level filter" value={level} onChange={event => setLevel(event.target.value)} className="mt-4 block rounded-6 border border-border bg-raised px-8 py-6 text-text"><option>ALL</option>{LEVELS.map(item => <option key={item}>{item}</option>)}</select></label>
      <label className="text-12 text-secondary">Module<input aria-label="Module filter" value={module} onChange={event => setModule(event.target.value)} placeholder="e.g. svc=fs" className="mt-4 block rounded-6 border border-border bg-raised px-8 py-6 text-text" /></label>
      <label className="text-12 text-secondary">Search<input aria-label="Search log text" value={query} onChange={event => setQuery(event.target.value)} placeholder="Find in logs" className="mt-4 block rounded-6 border border-border bg-raised px-8 py-6 text-text" /></label>
      <label className="flex items-center gap-6 text-12 text-secondary"><input type="checkbox" checked={tailFollow} onChange={event => setTailFollow(event.target.checked)} />Follow tail</label>
      <span role="status" className="ml-auto text-12 text-muted">{visibleLines.length} shown · {result.lines.length - visibleLines.length} hidden</span>
    </div>
    <div role="region" aria-label="Application log entries" className="min-h-0 flex-1 overflow-auto rounded-10 border border-border bg-raised font-mono text-11" onScroll={event => setScrollTop(event.currentTarget.scrollTop)}>
      <div style={{ height: start * rowHeight }} aria-hidden="true" />
      {shown.map((line, index) => <div key={`${start + index}-${line.text}`} className="grid min-h-[28px] grid-cols-[60px_180px_1fr] items-center gap-10 border-b border-border/50 px-10 text-text"><span className={line.level === "ERROR" ? "text-danger" : line.level === "WARN" ? "text-warning" : "text-muted"}>{line.level}</span><span className="truncate text-secondary">{line.target}</span><span className="break-all">{line.text}</span></div>)}
      <div style={{ height: Math.max(0, visibleLines.length - start - shown.length) * rowHeight }} aria-hidden="true" />
    </div>
  </main>;
}
