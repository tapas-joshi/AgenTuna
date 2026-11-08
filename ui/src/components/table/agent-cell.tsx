/**
 * Per-agent cell renderer.
 *
 * One cell answers "what does this agent have?" and uses the product's status
 * vocabulary for every state: the words come from `STATUS_META`, the colours
 * from the theme tokens, and a widget that matches the state. A state is never
 * colour-only: every branch renders a visible word, and the tooltip explains
 * where the limitation comes from (path, lock owner, winning scope).
 */
import type { JSX } from "react";
import { Badge } from "../badge";
import { STATUS_META, StatusGlyph } from "../status";
import { StatusDot } from "../status-dot";
import { Toggle } from "../toggle";
import { cn } from "../cn";
import type { AgentCellState, AgentDescriptor } from "./types";

/** Props accepted by {@link AgentCell}. */
export interface AgentCellProps {
  /** The agent this cell belongs to; its label names the switch. */
  readonly agent: AgentDescriptor;
  /** Human-readable item name, included in the switch's accessible name. */
  readonly itemLabel?: string;
  /** One of the six states. */
  readonly state: AgentCellState;
  /**
   * Reports the requested switch state. Without it, `linked` and `off` render
   * a disabled switch: a cell that cannot write must not look writable.
   */
  readonly onToggle?: (next: boolean) => void;
  /** Disables the switch independently of the state, for example while saving. */
  readonly disabled?: boolean;
  readonly className?: string;
}

function switchable(state: AgentCellState): state is Extract<AgentCellState, { kind: "linked" | "off" }> {
  return state.kind === "linked" || state.kind === "off";
}

/** The discovery root of a skill path, for example `.agents/skills`. */
export function discoveryRootLabel(path: string): string {
  const segments = path.replace(/[\\/]+$/, "").split(/[\\/]/);
  const skills = segments.lastIndexOf("skills");
  if (skills >= 1) return segments.slice(skills - 1, skills + 1).join("/");
  return segments.slice(-2).join("/");
}

/** Props accepted by {@link SharedFolderCell}. */
export interface SharedFolderCellProps {
  /** Agents that read the one folder, in column order. */
  readonly agents: readonly AgentDescriptor[];
  /** Human-readable item name, included in the switch's accessible name. */
  readonly itemLabel?: string;
  /** Absolute folder all of them read. */
  readonly path: string;
  /** State of the folder; the first member's state names the control. */
  readonly state: AgentCellState;
  /** Reports the requested state. Without it a managed folder renders read-only. */
  readonly onToggle?: (next: boolean) => void;
  /** Disables the control independently of the state, for example while saving. */
  readonly disabled?: boolean;
  readonly className?: string;
}

/**
 * One folder block for agents that share a discovery root.
 *
 * Codex and omp both read `<project>/.agents/skills`, so the folder is the unit
 * that changes, not the agent: a single control writes the one folder, the
 * visible line says `1 folder · N agents`, and the tooltip names every reader.
 * Native folders render read-only because AgenTuna does not own them yet.
 */
export function SharedFolderCell({ agents, itemLabel = "item", path, state, onToggle, disabled = false, className }: SharedFolderCellProps): JSX.Element {
  const labels = agents.map((agent) => agent.label).join(" + ");
  const root = discoveryRootLabel(path);
  const readOnly = state.kind === "native" || state.kind === "shadowed";
  const pending = state.kind === "linked" || state.kind === "off" ? state.pending === true : false;

  let control: JSX.Element;
  if (switchable(state)) {
    control = (
      <span className="flex items-center gap-6">
        <Toggle
          size="sm"
          label={`${itemLabel}, ${labels}, ${state.kind === "linked" ? "enabled" : "disabled"}, one shared folder`}
          labelPlacement="hidden"
          checked={state.kind === "linked"}
          disabled={
            disabled ||
            state.readOnly === true ||
            pending ||
            onToggle === undefined ||
            Boolean(state.disabledReason)
          }
          onCheckedChange={(next) => onToggle?.(next)}
        />
        <span aria-hidden="true" className="text-11 text-text-muted">
          {STATUS_META[state.kind].label}
        </span>
      </span>
    );
  } else if (state.kind === "native") {
    control = (
      <span className="flex flex-wrap items-center gap-4">
        <Badge tone="muted" mono>
          native
        </Badge>
        <Badge tone="muted">read-only</Badge>
      </span>
    );
  } else if (state.kind === "shadowed") {
    control = <StatusDot status="shadowed" title={`Shadowed by ${state.shadowedBy}`} />;
  } else {
    control = (
      <span className="text-11 text-text-muted" aria-hidden="true">
        —
      </span>
    );
  }

  return (
    <span
      role="group"
      aria-label={`${labels}: one shared folder`}
      aria-busy={pending || undefined}
      data-slot="shared-folder"
      data-state={state.kind}
      data-read-only={readOnly || undefined}
      title={`${path} · read by ${labels} · one folder on disk`}
      className={cn("flex min-w-0 flex-col items-start gap-2", className)}
    >
      {control}
      <span data-slot="agent-path" className="max-w-full truncate font-mono text-11 text-text-muted">
        {root}
      </span>
      <span data-slot="shared-root" className="max-w-full truncate text-11 text-scope-user">
        1 folder · {agents.length} agents
      </span>
    </span>
  );
}

/**
 * Renders the state of one item in one agent.
 *
 * The visible word under the switch is `aria-hidden`; the switch itself is
 * named from the status vocabulary so screen readers hear "Linked", not a bare
 * toggle. The cell is wrapped in a group named after the agent, so the column
 * context survives navigation.
 */
export function AgentCell({ agent, itemLabel = "item", state, onToggle, disabled = false, className }: AgentCellProps): JSX.Element {
  const readOnly = state.kind === "linked" || state.kind === "off" ? state.readOnly === true : false;
  const pending = state.kind === "linked" || state.kind === "off" ? state.pending === true : false;

  let content: JSX.Element;

  if (switchable(state)) {
    const checked = state.kind === "linked";
    content = (
      <span className="flex flex-col items-center gap-2">
        <Toggle
          size="sm"
          label={`${itemLabel}, ${agent.label}, ${checked ? "enabled" : "disabled"}${state.disabledReason ? `, ${state.disabledReason}` : ""}`}
          labelPlacement="hidden"
          checked={checked}
          disabled={disabled || readOnly || pending || onToggle === undefined || Boolean(state.disabledReason)}
          onCheckedChange={(next) => onToggle?.(next)}
        />
        <span data-slot="agent-label" aria-hidden="true" className="text-11 text-text-muted">
          {STATUS_META[state.kind].label}
        </span>
      </span>
    );
  } else if (state.kind === "locked") {
    content = (
      <span className="flex flex-col items-center gap-2">
        <span className="flex items-center gap-4">
          <Toggle
            size="sm"
            label={`${itemLabel}, ${agent.label}, ${state.enabled === false ? "disabled" : "enabled"}, locked by ${state.lockedBy}`}
            labelPlacement="hidden"
            checked={state.enabled !== false}
            disabled
            onCheckedChange={() => undefined}
          />
          <StatusGlyph status="locked" className="text-scope-global" />
        </span>
        <span data-slot="agent-label" aria-hidden="true" className="text-11 text-text-muted">
          {STATUS_META.locked.label}
        </span>
      </span>
    );
  } else if (state.kind === "native") {
    const shared = state.sharedWith !== undefined && state.sharedWith.length > 0;
    content = (
      <span className="flex min-w-0 w-full flex-col items-start gap-2">
        <span className="flex min-w-0 flex-wrap items-center gap-4">
          <Badge tone="muted" mono>
            native
          </Badge>
          {state.readOnly === true && shared ? <Badge tone="muted">read-only</Badge> : null}
        </span>
        <span
          data-slot="agent-path"
          title={state.path}
          className="w-full truncate font-mono text-11 text-text-muted"
        >
          {discoveryRootLabel(state.path)}
        </span>
        {shared ? (
          <span data-slot="agent-shared" className="max-w-full truncate text-11 text-scope-user">
            shared with {state.sharedWith?.join(" + ")}
          </span>
        ) : null}
      </span>
    );
  } else if (state.kind === "shadowed") {
    content = <StatusDot status="shadowed" title={`Shadowed by ${state.shadowedBy}`} />;
  } else {
    content = (
      <span className="inline-flex items-center justify-center text-text-muted" title="Not present in this agent">
        <span aria-hidden="true">—</span>
        <span className="sr-only">Not present</span>
      </span>
    );
  }

  const tooltip =
    state.kind === "native"
      ? [
          state.path,
          ...(state.readOnly === true ? ["read-only"] : []),
          ...(state.sharedWith !== undefined && state.sharedWith.length > 0
            ? [`shared by ${state.sharedWith.join(" + ")}`]
            : []),
        ].join(" · ")
      : state.kind === "locked"
        ? `Locked by ${state.lockedBy}`
        : state.kind === "shadowed"
          ? `Shadowed by ${state.shadowedBy}`
          : state.kind === "none"
            ? "Not present in this agent"
            : (state.disabledReason ?? STATUS_META[state.kind].description);

  return (
    <span
      role="group"
      aria-label={agent.label}
      aria-busy={pending || undefined}
      data-slot="agent-cell"
      data-state={state.kind}
      title={tooltip}
      className={cn(
        "flex min-w-0",
        switchable(state) || state.kind === "locked"
          ? "items-center justify-center"
          : state.kind === "native"
            ? "items-start justify-start"
            : "items-center justify-center",
        className,
      )}
    >
      {content}
    </span>
  );
}
