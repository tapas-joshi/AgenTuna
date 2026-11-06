import "./project-picker.css";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useMemo, useRef, useState } from "react";
import type { JSX } from "react";
import { Modal } from "../dialogs/Modal";
import { Button } from "../../components/button";
import { SearchIcon } from "../../shell/icons";

/** Project row shown by the project picker. */
export interface ProjectPickerProject {
  readonly id: string;
  readonly name: string;
  readonly path: string;
  readonly skillCount: number;
  readonly serverCount: number;
  readonly lastUsed: string | null;
}

/** Formats a stored UTC last-used timestamp for the compact project row. */
export function relativeLastUsed(timestamp: string | null, now = Date.now()): string {
  if (timestamp === null) return "Never used";
  const time = Date.parse(timestamp);
  if (!Number.isFinite(time)) return "Unknown";
  const elapsed = Math.max(0, now - time);
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  if (elapsed < 60_000) return formatter.format(-Math.floor(elapsed / 1000), "second");
  if (elapsed < 3_600_000) return formatter.format(-Math.floor(elapsed / 60_000), "minute");
  if (elapsed < 86_400_000) return formatter.format(-Math.floor(elapsed / 3_600_000), "hour");
  return formatter.format(-Math.floor(elapsed / 86_400_000), "day");
}

/** Props for the controlled project picker. */
export interface ProjectPickerProps {
  readonly open: boolean;
  readonly currentProjectId?: string | null;
  readonly projects: readonly ProjectPickerProject[];
  readonly loading?: boolean;
  readonly scanning?: boolean;
  readonly error?: string | null;
  readonly onSelect: (project: ProjectPickerProject) => void;
  readonly onClose: () => void;
  readonly onAddFolder: () => void;
  readonly onScan: () => void;
  readonly onCancelScan?: () => void;
  readonly onReveal?: (project: ProjectPickerProject) => void;
}

function matchedSpans(text: string, query: string): readonly [number, number][] {
  const normalized = query.trim().toLocaleLowerCase();
  if (!normalized) return [];
  const lowerText = text.toLocaleLowerCase();
  const spans: [number, number][] = [];
  let cursor = 0;
  while (cursor < lowerText.length) {
    const index = lowerText.indexOf(normalized, cursor);
    if (index < 0) break;
    spans.push([index, index + normalized.length]);
    cursor = index + Math.max(1, normalized.length);
  }
  return spans;
}

function Highlight({ text, query }: { readonly text: string; readonly query: string }): JSX.Element {
  const spans = matchedSpans(text, query);
  if (!spans.length) return <>{text}</>;
  const parts: JSX.Element[] = [];
  let offset = 0;
  spans.forEach(([start, end], index) => {
    if (offset < start) parts.push(<span key={`text-${index}`}>{text.slice(offset, start)}</span>);
    parts.push(<mark key={`match-${index}`} className="rounded-sm bg-accent/30 text-text">{text.slice(start, end)}</mark>);
    offset = end;
  });
  if (offset < text.length) parts.push(<span key="tail">{text.slice(offset)}</span>);
  return <>{parts}</>;
}

/** Filters projects by case-insensitive substring across name and full path. */
export function filterProjects(projects: readonly ProjectPickerProject[], query: string): readonly ProjectPickerProject[] {
  const normalized = query.trim().toLocaleLowerCase();
  return projects.filter((project) => `${project.name} ${project.path}`.toLocaleLowerCase().includes(normalized));
}

/** Searchable project picker; data loading and native folder actions are injected. */
export function ProjectPicker({ open, projects, currentProjectId = null, loading = false, scanning = false, error = null, onSelect, onClose, onAddFolder, onScan, onCancelScan, onReveal }: ProjectPickerProps): JSX.Element | null {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const filtered = useMemo(() => filterProjects(projects, query), [projects, query]);
  const virtualizer = useVirtualizer({ count: filtered.length, getScrollElement: () => listRef.current, estimateSize: () => 72, overscan: 6 });
  const virtualItems = virtualizer.getVirtualItems();
  const renderedItems = virtualItems.length ? virtualItems : filtered.slice(0, 12).map((_, index) => ({ index, start: index * 72 }));
  useEffect(() => {
    if (open && filtered.length > 0) virtualizer.scrollToIndex(active, { align: "auto" });
  }, [active, filtered.length, open, virtualizer]);
  if (!open) return null;
  const choose = (project: ProjectPickerProject) => { onSelect(project); onClose(); };
  return <Modal open={open} onOpenChange={(next) => { if (!next) onClose(); }} title="Switch project" description="Search and select a registered project." size="lg" className="at-project-picker">
    <div className="flex min-h-0 flex-col gap-12">
      <div className="flex h-[44px] items-center gap-8 rounded-8 border border-border bg-surface px-12 focus-within:ring-2 focus-within:ring-accent">
        <SearchIcon />
        <input autoFocus role="combobox" aria-label="Search projects" aria-autocomplete="list" aria-expanded="true" aria-controls="project-picker-results" aria-activedescendant={filtered[active] === undefined ? undefined : `project-option-${encodeURIComponent(filtered[active].id)}`} placeholder={`Search ${projects.length} projects`} value={query} onChange={(event) => { setQuery(event.target.value); setActive(0); }} onKeyDown={(event) => {
        if ((event.metaKey || event.ctrlKey) && event.key === "Enter" && filtered[active]) { event.preventDefault(); onReveal?.(filtered[active]); return; }
        if (event.key === "ArrowDown") { event.preventDefault(); setActive((value) => Math.min(value + 1, filtered.length - 1)); }
        if (event.key === "ArrowUp") { event.preventDefault(); setActive((value) => Math.max(0, value - 1)); }
        if (event.key === "Enter" && filtered[active]) { event.preventDefault(); choose(filtered[active]); }
      }} className="min-w-0 flex-1 appearance-none border-0 bg-transparent text-text outline-none" />
        <kbd aria-hidden="true" className="font-mono text-11 text-muted">⌘P</kbd>
      </div>
      <div className="px-8 text-12 font-semibold uppercase text-muted" aria-live="polite">{query ? `${filtered.length} matches` : `All projects · ${projects.length}`}</div>
      <div id="project-picker-results" ref={listRef} role="listbox" aria-label="Projects" className="max-h-[360px] min-h-[160px] overflow-y-auto">
        {loading ? <div className="flex flex-col gap-8 p-12" role="status" aria-live="polite" aria-label="Loading projects">{[0,1,2].map((n) => <div key={n} className="h-[48px] animate-pulse rounded-8 bg-surface-raised" />)}</div> : <>
          {error && <div role="alert" className="p-12 text-danger">{error} <button onClick={onScan} className="underline">Retry</button></div>}
          {scanning && <div className="flex items-center justify-between p-12" role="status">Scanning projects…{onCancelScan && <button onClick={onCancelScan}>Cancel</button>}</div>}
          {filtered.length > 0 && <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>{renderedItems.map((virtualRow) => {
            const project = filtered[virtualRow.index];
            if (!project) return null;
            const isCurrent = project.id === currentProjectId;
            return <button id={`project-option-${encodeURIComponent(project.id)}`} key={project.id} ref={virtualizer.measureElement} data-index={virtualRow.index} role="option" aria-selected={virtualRow.index === active} aria-current={isCurrent ? "true" : undefined} onMouseEnter={() => setActive(virtualRow.index)} onClick={() => choose(project)} className={`absolute left-0 top-0 flex w-full flex-col gap-4 rounded-8 p-12 text-left ${virtualRow.index === active ? "bg-surface-raised" : ""}`} style={{ transform: `translateY(${virtualRow.start}px)` }}>
              <span className="flex items-center justify-between gap-8"><span className="font-medium text-text"><Highlight text={project.name} query={query} /></span>{isCurrent && <span className="rounded-4 bg-success/15 px-6 py-2 text-10 font-semibold uppercase text-success">Current</span>}</span><span className="truncate text-12 text-muted"><Highlight text={project.path} query={query} /></span><span className="text-12 text-muted">{project.skillCount} skills · {project.serverCount} servers · {project.lastUsed ?? "Never used"}</span>
            </button>;
          })}</div>}
          {!loading && filtered.length === 0 && projects.length === 0 && query.trim() === "" && <div className="p-20 text-center text-muted">No projects registered yet. Add a project folder or scan ~/Projects.</div>}
          {!loading && filtered.length === 0 && (projects.length > 0 || query.trim() !== "") && <div className="p-20 text-center text-muted">No projects match <button className="text-accent underline" onClick={() => setQuery("")}>Clear</button></div>}
        </>}
      </div>
      <div className="flex justify-between border-t border-border pt-12"><Button variant="secondary" onClick={onAddFolder}>Add project folder</Button><Button variant="secondary" onClick={onScan} disabled={scanning}>Scan ~/Projects</Button></div>
    </div>
  </Modal>;
}
