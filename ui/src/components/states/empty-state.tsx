import type { JSX, ReactNode } from "react";
import { StateView } from "./state-view";
import type { StateAction, StateHeadingLevel } from "./types";

/** Props accepted by {@link EmptyState}. */
export interface EmptyStateProps {
  /** Heading that names what is missing, for example "No MCP servers yet". */
  readonly title: string;
  /** One sentence that explains how to get started. */
  readonly hint?: ReactNode;
  /** Decorative icon; the tray glyph is used when omitted. */
  readonly icon?: ReactNode;
  /** The one action that resolves the emptiness; rendered as a primary button. */
  readonly action?: StateAction;
  /** Optional secondary action, rendered after the primary one. */
  readonly secondaryAction?: StateAction;
  /** Reserves space so swapping a table for the empty state does not shift layout. */
  readonly minHeight?: number;
  /** Heading element. Defaults to `h2`. */
  readonly headingLevel?: StateHeadingLevel;
  /** Extra classes appended to the layout. */
  readonly className?: string;
}

/**
 * The empty variant of {@link StateView}: one sentence and one action.
 *
 * Never render stale rows beside it; screens replace the region entirely.
 */
export function EmptyState({
  title,
  hint,
  icon,
  action,
  secondaryAction,
  minHeight,
  headingLevel,
  className,
}: EmptyStateProps): JSX.Element {
  return (
    <StateView
      variant="empty"
      title={title}
      body={hint}
      icon={icon}
      actions={[action, secondaryAction].filter((entry): entry is StateAction => entry !== undefined)}
      minHeight={minHeight}
      headingLevel={headingLevel}
      className={className}
    />
  );
}
