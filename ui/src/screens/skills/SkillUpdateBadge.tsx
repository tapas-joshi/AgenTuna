/**
 * Update state control for one skills-list row (TS-040).
 *
 * The badge appears only when the source advertises an unignored update. Its
 * menu names the installed and upstream commits plus the last check time, and
 * offers the two user actions the story requires: review an update and ignore
 * this upstream version. Offline or deleted sources render a muted dot with an
 * explanation instead, never an update badge.
 */

import { useEffect, useRef, useState } from "react";
import { Badge } from "../../components/badge";
import type { SkillRowView } from "../../generated/SkillRowView";

/** Derived update state of one row. */
export type SkillSourceState = "update_available" | "current" | "source_gone" | "unknown";

/** Derives the badge state from the persisted source columns. */
export function skillSourceState(row: Pick<SkillRowView, "update_available" | "source" | "source_remote_sha" | "source_last_checked_at">): SkillSourceState {
  if (row.update_available) return "update_available";
  if (row.source === null) return "current";
  if (row.source_remote_sha !== null) return "current";
  return row.source_last_checked_at !== null ? "source_gone" : "unknown";
}

/** Explains why no update badge is shown. */
export function sourceStateExplanation(state: SkillSourceState): string {
  if (state === "source_gone") {
    return "The source was deleted or its branch no longer exists; no update badge is shown.";
  }
  if (state === "unknown") {
    return "The source has not been checked or could not be reached offline; no update badge is shown.";
  }
  return "The installed skill matches its checked source.";
}

/** Tooltip listing both commits and the last check time. */
export function updateTooltip(row: Pick<SkillRowView, "source_commit_sha" | "source_remote_sha" | "source_last_checked_at">): string {
  return [
    `Installed ${row.source_commit_sha ?? "unknown"}`,
    `Upstream ${row.source_remote_sha ?? "unknown"}`,
    `Last checked ${row.source_last_checked_at ?? "never"}`,
  ].join(" · ");
}

/** Props accepted by {@link SkillUpdateBadge}. */
export interface SkillUpdateBadgeProps {
  readonly row: SkillRowView;
  /** Opens the reviewed update diff for this row. */
  readonly onUpdate: (row: SkillRowView) => void;
  /** Rejects the currently checked upstream commit for this row. */
  readonly onIgnore: (row: SkillRowView) => void;
  /** Disables both actions while an update operation runs. */
  readonly busy?: boolean;
}

/** Renders the update badge menu or the muted non-badge source state. */
export function SkillUpdateBadge({ row, onUpdate, onIgnore, busy = false }: SkillUpdateBadgeProps) {
  const [open, setOpen] = useState(false);
  const container = useRef<HTMLSpanElement | null>(null);
  const state = skillSourceState(row);

  useEffect(() => {
    if (!open) return;
    function close(event: MouseEvent): void {
      if (!container.current?.contains(event.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  if (state === "update_available") {
    return (
      <span ref={container} data-slot="skill-update-badge" className="relative inline-flex min-w-0 items-center">
        <button
          type="button"
          aria-label={`Update available for ${row.name}`}
          aria-haspopup="menu"
          aria-expanded={open}
          title={updateTooltip(row)}
          className="rounded-4 border-0 bg-transparent p-0"
          onClick={(event) => {
            event.stopPropagation();
            setOpen((value) => !value);
          }}
        >
          <Badge tone="info" dot className="cursor-pointer">
            Update available
          </Badge>
        </button>
        {open && (
          <div
            role="menu"
            aria-label={`Update actions for ${row.name}`}
            className="absolute left-0 top-full z-50 mt-4 min-w-[220px] rounded-6 border border-border bg-raised p-4 text-12 text-text shadow-lg"
            onClick={(event) => event.stopPropagation()}
          >
            <p className="m-0 px-8 py-6 font-mono text-11 leading-4 text-text-muted">{updateTooltip(row)}</p>
            <button
              type="button"
              role="menuitem"
              disabled={busy}
              className="w-full rounded-4 px-8 py-4 text-left hover:bg-surface disabled:opacity-50"
              onClick={() => {
                setOpen(false);
                onUpdate(row);
              }}
            >
              Update now
            </button>
            <button
              type="button"
              role="menuitem"
              disabled={busy}
              className="w-full rounded-4 px-8 py-4 text-left hover:bg-surface disabled:opacity-50"
              onClick={() => {
                setOpen(false);
                onIgnore(row);
              }}
            >
              Ignore this version
            </button>
          </div>
        )}
      </span>
    );
  }

  if (state === "source_gone" || state === "unknown") {
    return (
      <span
        data-slot="skill-source-state"
        data-state={state}
        title={sourceStateExplanation(state)}
        className="inline-flex items-center gap-4 text-11 text-text-muted"
      >
        <span aria-hidden="true" className="size-6 shrink-0 rounded-full bg-text-muted/60" />
        {state === "source_gone" ? "Source unavailable" : "Source unchecked"}
      </span>
    );
  }

  return null;
}
