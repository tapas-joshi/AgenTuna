import { useMemo, useState } from "react";
import type { JSX } from "react";
import { Modal } from "../dialogs/Modal";
import { searchPalette, idlePaletteEntries } from "./index";
import type { PaletteEntry } from "./index";

/** Props for the command palette view. */
export interface PaletteProps {
  readonly open: boolean;
  readonly index: readonly PaletteEntry[];
  readonly recentIds?: readonly string[];
  readonly onClose: () => void;
  readonly onRun: (entry: PaletteEntry) => void;
}

/** Search and dispatch the indexed actions without implementing action behavior here. */
export function Palette({ open, index, recentIds = [], onClose, onRun }: PaletteProps): JSX.Element | null {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const entries = useMemo(() => query.trim() ? searchPalette(index, query) : idlePaletteEntries(index, recentIds), [index, query, recentIds]);
  if (!open) return null;
  const run = (entry: PaletteEntry | undefined) => {
    if (entry === undefined || entry.disabledReason !== undefined) return;
    onRun(entry);
    onClose();
  };
  const groups = new Map<string, PaletteEntry[]>();
  for (const entry of entries) groups.set(entry.kind, [...(groups.get(entry.kind) ?? []), entry]);
  return <Modal open={open} onOpenChange={(next) => { if (!next) onClose(); }} title="Command palette" description="Search commands, skills, servers and rules." size="lg" className="!p-0">
    <div className="flex flex-col">
      <input autoFocus role="combobox" aria-label="Search commands" aria-autocomplete="list" aria-expanded="true" aria-controls="palette-results" aria-activedescendant={entries[active] === undefined ? undefined : `palette-option-${encodeURIComponent(entries[active].id)}`} placeholder="Search commands, skills, servers…" value={query} onChange={(event) => { setQuery(event.target.value); setActive(0); }} onKeyDown={(event) => {
        if (event.key === "ArrowDown") { event.preventDefault(); setActive((n) => Math.min(n + 1, entries.length - 1)); }
        if (event.key === "ArrowUp") { event.preventDefault(); setActive((n) => Math.max(0, n - 1)); }
        if (event.key === "Enter") { event.preventDefault(); run(entries[active]); }
      }} className="h-48 border-b border-border bg-surface px-16 text-16 text-text outline-none focus-visible:ring-2 focus-visible:ring-accent" />
      <div id="palette-results" role="listbox" aria-label="Command palette results" className="max-h-[420px] min-h-[120px] overflow-y-auto p-8">
        {entries.length === 0 ? <div className="p-20 text-center text-muted" role="status">No matches</div> : [...groups].map(([kind, grouped]) => <section key={kind} role="group" aria-label={kind} className="mb-8"><h3 className="px-8 py-4 text-11 font-semibold uppercase text-muted">{kind}</h3>{grouped.map((entry) => {
          const indexOfEntry = entries.indexOf(entry);
          return <button id={`palette-option-${encodeURIComponent(entry.id)}`} key={entry.id} role="option" aria-selected={active === indexOfEntry} aria-disabled={entry.disabledReason !== undefined || undefined} disabled={entry.disabledReason !== undefined} onMouseEnter={() => setActive(indexOfEntry)} onClick={() => run(entry)} className={`flex w-full items-center justify-between rounded-6 px-8 py-8 text-left disabled:cursor-not-allowed disabled:opacity-60 ${active === indexOfEntry ? "bg-surface-raised" : ""}`}><span><span className="block text-13 text-text">{entry.title}</span><span className="block text-11 text-muted">{entry.subtitle}</span>{entry.disabledReason !== undefined && <span className="block text-11 text-warning">{entry.disabledReason}</span>}</span><span aria-hidden="true" className="shrink-0 text-11 text-muted">{entry.shortcut ?? "↵"}</span></button>;
        })}</section>)}
      </div>
    </div>
  </Modal>;
}
