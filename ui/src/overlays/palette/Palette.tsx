import { useMemo, useState, type JSX, type ReactNode } from "react";
import { Modal } from "../dialogs/Modal";
import { searchPalette, idlePaletteEntries } from "./index";
import type { PaletteEntry } from "./index";

/** Highlight matched substrings or subsequences with bold text spans. */
export function highlightMatch(text: string, query: string): ReactNode {
  const trimmed = query.trim();
  if (!trimmed) return text;
  const lowerText = text.toLowerCase();
  const lowerQuery = trimmed.toLowerCase();
  const subIndex = lowerText.indexOf(lowerQuery);
  if (subIndex >= 0) {
    const before = text.slice(0, subIndex);
    const match = text.slice(subIndex, subIndex + trimmed.length);
    const after = text.slice(subIndex + trimmed.length);
    return (
      <>
        {before}
        <strong className="font-bold text-text">{match}</strong>
        {after}
      </>
    );
  }
  const elements: ReactNode[] = [];
  let queryPos = 0;
  let lastIndex = 0;
  for (let i = 0; i < text.length && queryPos < lowerQuery.length; i++) {
    if (text[i]?.toLowerCase() === lowerQuery[queryPos]) {
      if (i > lastIndex) elements.push(text.slice(lastIndex, i));
      elements.push(<strong key={i} className="font-bold text-text">{text[i]}</strong>);
      lastIndex = i + 1;
      queryPos++;
    }
  }
  if (lastIndex < text.length) elements.push(text.slice(lastIndex));
  return elements.length > 0 ? <>{elements}</> : text;
}

/** Props for the command palette view. */
export interface PaletteProps {
  readonly open: boolean;
  readonly index: readonly PaletteEntry[];
  readonly recentIds?: readonly string[];
  readonly onRetry?: () => void;
  readonly loading?: boolean;
  readonly error?: string | null;
  readonly onClose: () => void;
  readonly onRun: (entry: PaletteEntry, options?: { background?: boolean }) => void;
  readonly onRecordAction?: (entry: PaletteEntry) => void;
}

/** Search and dispatch the indexed actions without implementing action behavior here. */
export function Palette({ open, index, recentIds = [], loading = false, error = null, onRetry, onClose, onRun, onRecordAction }: PaletteProps): JSX.Element | null {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const entries = useMemo(() => query.trim() ? searchPalette(index, query) : idlePaletteEntries(index, recentIds), [index, query, recentIds]);
  const suggestedActions = useMemo(() => {
    if (entries.length > 0 || !query.trim()) return [];
    const actions = index.filter(e => e.kind === "action");
    return actions.slice(0, 3);
  }, [entries.length, index, query]);
  if (!open) return null;
  const totalNavigable = entries.length > 0 ? entries.length : suggestedActions.length;
  const run = (entry: PaletteEntry | undefined, options?: { background?: boolean }) => {
    if (entry === undefined || entry.disabledReason !== undefined) return;
    onRecordAction?.(entry);
    if (options?.background) {
      onRun(entry, options);
    } else {
      onRun(entry);
      onClose();
    }
  };
  const groups = new Map<string, PaletteEntry[]>();
  for (const entry of entries) groups.set(entry.kind, [...(groups.get(entry.kind) ?? []), entry]);
  const activeOptionId = entries[active] !== undefined
    ? `palette-option-${encodeURIComponent(entries[active].id)}`
    : suggestedActions[active] !== undefined
    ? `palette-suggested-${encodeURIComponent(suggestedActions[active].id)}`
    : undefined;

  return <Modal open={open} onOpenChange={(next) => { if (!next) onClose(); }} title="Command palette" description="Search commands, skills, servers and rules." size="lg" className="!p-0">
    <div className="flex flex-col">
      <input autoFocus role="combobox" aria-label="Search commands" aria-autocomplete="list" aria-expanded="true" aria-controls="palette-results" aria-activedescendant={activeOptionId} placeholder="Search commands, skills, servers…" value={query} onChange={(event) => { setQuery(event.target.value); setActive(0); }} onKeyDown={(event) => {
        if (event.key === "ArrowDown") { event.preventDefault(); setActive((n) => Math.min(n + 1, Math.max(0, totalNavigable - 1))); }
        if (event.key === "ArrowUp") { event.preventDefault(); setActive((n) => Math.max(0, n - 1)); }
        if (event.key === "Home") { event.preventDefault(); setActive(0); }
        if (event.key === "End") { event.preventDefault(); setActive(Math.max(0, totalNavigable - 1)); }
        if (event.key === "Enter") {
          event.preventDefault();
          const isBackground = event.metaKey || event.ctrlKey;
          const target = entries.length > 0 ? entries[active] : suggestedActions[active];
          run(target, { background: isBackground });
        }
      }} className="h-48 border-b border-border bg-surface px-16 text-16 text-text outline-none focus-visible:ring-2 focus-visible:ring-accent" />
      <div id="palette-results" role="listbox" aria-label="Command palette results" className="max-h-[420px] min-h-[120px] overflow-y-auto p-8">
        {loading && <div className="p-12 text-center text-muted" role="status">Loading current items…</div>}
        {error !== null && <div className="m-8 rounded-6 border border-danger/40 bg-danger/10 p-10 text-12 text-danger" role="alert">{error}{onRetry && <button type="button" className="ml-8 underline" onClick={onRetry}>Retry</button>}</div>}
        {entries.length === 0 ? (
          loading ? (
            <div className="p-20 text-center text-muted" role="status">Loading commands…</div>
          ) : (
            <div className="flex flex-col gap-12 p-12">
              <div className="p-8 text-center text-muted" role="status">No matches</div>
              {suggestedActions.length > 0 && (
                <div className="flex flex-col gap-4">
                  <div className="px-8 text-11 font-semibold uppercase text-muted">Suggested actions</div>
                  {suggestedActions.map((entry, idx) => (
                    <button
                      id={`palette-suggested-${encodeURIComponent(entry.id)}`}
                      key={entry.id}
                      role="option"
                      aria-selected={active === idx}
                      aria-disabled={entry.disabledReason !== undefined || undefined}
                      disabled={entry.disabledReason !== undefined}
                      onMouseEnter={() => setActive(idx)}
                      onClick={(e) => run(entry, { background: e.metaKey || e.ctrlKey })}
                      className={`flex w-full items-center justify-between rounded-6 px-8 py-8 text-left disabled:cursor-not-allowed disabled:opacity-60 ${active === idx ? "bg-surface-raised" : ""}`}
                    >
                      <span>
                        <span className="block text-13 text-text">{highlightMatch(entry.title, query)}</span>
                        <span className="block text-11 text-muted">{highlightMatch(entry.subtitle, query)}</span>
                      </span>
                      <span aria-hidden="true" className="shrink-0 text-11 text-muted">{entry.shortcut ?? "↵"}</span>
                    </button>
                  ))}
                </div>
              )}
            </div>
          )
        ) : [...groups].map(([kind, grouped]) => <section key={kind} role="group" aria-label={kind} className="mb-8"><h3 className="px-8 py-4 text-11 font-semibold uppercase text-muted">{kind}</h3>{grouped.map((entry) => {
          const indexOfEntry = entries.indexOf(entry);
          return <button id={`palette-option-${encodeURIComponent(entry.id)}`} key={entry.id} role="option" aria-selected={active === indexOfEntry} aria-disabled={entry.disabledReason !== undefined || undefined} disabled={entry.disabledReason !== undefined} onMouseEnter={() => setActive(indexOfEntry)} onClick={(e) => run(entry, { background: e.metaKey || e.ctrlKey })} className={`flex w-full items-center justify-between rounded-6 px-8 py-8 text-left disabled:cursor-not-allowed disabled:opacity-60 ${active === indexOfEntry ? "bg-surface-raised" : ""}`}><span><span className="block text-13 text-text">{highlightMatch(entry.title, query)}</span><span className="block text-11 text-muted">{highlightMatch(entry.subtitle, query)}</span>{entry.disabledReason !== undefined && <span className="block text-11 text-warning">{entry.disabledReason}</span>}</span><span aria-hidden="true" className="shrink-0 text-11 text-muted">{entry.shortcut ?? "↵"}</span></button>;
        })}</section>)}
      </div>
    </div>
  </Modal>;
}
