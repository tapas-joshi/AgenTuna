/**
 * Shared types for the loading, empty and error state pattern (TU-021).
 *
 * A `StateView` is plain data plus a few typed callbacks: screens decide which
 * variant to show and own the navigation, so the component never fetches,
 * writes or keeps app state.
 */

import type { ButtonVariant } from "../button";
import type { ReactNode } from "react";

/** The four states every screen can be in. */
export const STATE_VARIANTS = ["loading", "empty", "error", "offline"] as const;

/** One of the four screen states. */
export type StateVariant = (typeof STATE_VARIANTS)[number];

/** Visible label announced for a variant when the live region fires. */
export const STATE_VARIANT_LABEL: Readonly<Record<StateVariant, string>> = {
  loading: "Loading",
  empty: "Nothing here yet",
  error: "Error",
  offline: "Offline",
};

/** Heading element used for the state title; defaults to `h2` under a page title. */
export type StateHeadingLevel = "h1" | "h2" | "h3" | "h4";

/** One button rendered under the state copy. */
export interface StateAction {
  /** Visible button text; also the accessible name. */
  readonly label: string;
  /** Runs when the button is pressed. */
  readonly onClick: () => void;
  /**
   * Button weight. Defaults to `primary` for the first action in a list and
   * `secondary` for the rest.
   */
  readonly variant?: ButtonVariant;
  /** Shows a spinner and disables the button while work runs. */
  readonly loading?: boolean;
  /** Disables the button without a spinner. */
  readonly disabled?: boolean;
}

/** Expandable technical payload: raw paths, OS errors and operation ids. */
export interface StateDetails {
  /** Button text of the disclosure; defaults to `Technical details`. */
  readonly summary?: string;
  /**
   * Pre-formatted plain text. The Copy details button writes exactly this to
   * the clipboard, so build it with `errorDetailsText` for errors instead of
   * hand-assembling a string.
   */
  readonly text: string;
}

/**
 * Clipboard seam so the webview API is never called directly from a component.
 *
 * Structurally identical to the toast service port, so one app-level
 * implementation (or test double) satisfies both.
 */
export interface ClipboardPort {
  /** Copies `text` to the system clipboard. */
  writeText(text: string): Promise<void>;
}

/** Result of one copy attempt, reflected in the Copy details button label. */
export type CopyState = "idle" | "copied" | "failed";

/** Props accepted by `StateView`. */
export interface StateViewProps {
  /** Which state this is. Selects the icon, tone and default live region. */
  readonly variant: StateVariant;
  /** Short heading in plain language, for example "No skills yet". */
  readonly title: string;
  /** Optional supporting sentence or node under the title. */
  readonly body?: ReactNode;
  /** Decorative icon; the variant supplies a glyph when omitted. */
  readonly icon?: ReactNode;
  /** Buttons under the copy, in reading order. */
  readonly actions?: readonly StateAction[];
  /** Expandable technical details, rendered collapsed by default. */
  readonly details?: StateDetails;
  /**
   * Minimum height in pixels, so swapping states in a table or panel never
   * moves the surrounding layout.
   */
  readonly minHeight?: number;
  /** With the `loading` variant, draws this many skeleton rows instead of a spinner. */
  readonly skeletonRows?: number;
  /**
   * Live-region politeness. Defaults to `assertive` for `error` and `polite`
   * otherwise; `off` renders no live region (for states that are pure decoration).
   */
  readonly ariaLive?: "polite" | "assertive" | "off";
  /** Heading element for the title. Defaults to `h2`. */
  readonly headingLevel?: StateHeadingLevel;
  /** Clipboard port used by Copy details; defaults to the webview clipboard. */
  readonly clipboard?: ClipboardPort;
  /** Extra classes appended after the layout classes. */
  readonly className?: string;
}
