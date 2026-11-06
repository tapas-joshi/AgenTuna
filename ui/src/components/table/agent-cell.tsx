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

/**
 * Renders the state of one item in one agent.
 *
 * The visible word under the switch is `aria-hidden`; the switch itself is
 * named from the status vocabulary so screen readers hear "Linked", not a bare
 * toggle. The cell is wrapped in a group named after the agent, so the column
 * context survives navigation.
 */
export function AgentCell({ agent, state, onToggle, disabled = false, className }: AgentCellProps): JSX.Element {
  const readOnly = state.kind === "linked" || state.kind === "off" ? state.readOnly === true : false;
  const pending = state.kind === "linked" || state.kind === "off" ? state.pending === true : false;

  let content: JSX.Element;

  if (switchable(state)) {
    const checked = state.kind === "linked";
    content = (
      <span className="flex flex-col items-center gap-2">
        <Toggle
          size="sm"
          label={`${agent.label}: ${STATUS_META[state.kind].label}`}
          labelPlacement="hidden"
          checked={checked}
          disabled={disabled || readOnly || pending || onToggle === undefined}
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
            label={`${agent.label}: ${STATUS_META.locked.label}`}
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
      <span className="flex min-w-0 flex-col items-start gap-2">
        <span className="flex min-w-0 flex-wrap items-center gap-4">
          <Badge tone="muted" mono>
            native
          </Badge>
          {state.readOnly === true && shared ? <Badge tone="muted">read-only</Badge> : null}
        </span>
        <span data-slot="agent-path" className="max-w-full truncate font-mono text-11 text-text-muted">
          {state.path}
        </span>
        {shared ? (
          <span className="text-11 text-scope-user">shared with {state.sharedWith?.join(" + ")}</span>
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
            : STATUS_META[state.kind].description;

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
