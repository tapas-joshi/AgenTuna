import type { JSX, ReactNode } from "react";
import { StateView } from "./state-view";
import type { StateHeadingLevel, StateViewProps } from "./types";

/** Props accepted by {@link LoadingState}. */
export interface LoadingStateProps {
  /** Visible label; also the accessible name of the spinner. Defaults to `Loading…`. */
  readonly title?: string;
  /** Optional supporting sentence under the label. */
  readonly body?: ReactNode;
  /**
   * Number of skeleton rows to draw. When set, the skeleton replaces the
   * spinner, which is what table bodies and lists use.
   */
  readonly skeletonRows?: number;
  /** Minimum height in pixels, matching the populated region. */
  readonly minHeight?: number;
  /** Heading element. Defaults to `h2`. */
  readonly headingLevel?: StateHeadingLevel;
  /**
   * Live-region politeness. Defaults to `polite`; set `off` when the screen has
   * its own announcement channel for the same event.
   */
  readonly ariaLive?: StateViewProps["ariaLive"];
  /** Extra classes appended to the layout. */
  readonly className?: string;
}

/** The loading variant of {@link StateView}: a spinner with a label, or skeleton rows. */
export function LoadingState({
  title = "Loading…",
  body,
  skeletonRows,
  minHeight,
  headingLevel,
  ariaLive,
  className,
}: LoadingStateProps): JSX.Element {
  return (
    <StateView
      variant="loading"
      title={title}
      body={body}
      skeletonRows={skeletonRows}
      minHeight={minHeight}
      headingLevel={headingLevel}
      ariaLive={ariaLive}
      className={className}
    />
  );
}
