import type { JSX } from "react";
import { useMemo } from "react";
import type { CommandError } from "../../generated/CommandError";
import {
  ERROR_COPY,
  REMEDY_LABELS,
  errorDetailsText,
  toErrorReport,
  type ErrorClass,
  type ErrorClassCopy,
  type ErrorReportView,
  type RemedyId,
} from "./error-model";
import { StateView } from "./state-view";
import type { ClipboardPort, StateAction, StateHeadingLevel } from "./types";

/** Props accepted by {@link ErrorState}. */
export interface ErrorStateProps {
  /** The failure: a generated `CommandError` or a normalised report. */
  readonly error: ErrorReportView | CommandError;
  /**
   * Forces a category when the code alone is ambiguous, for example a
   * `not_found` report raised while detecting a missing agent.
   */
  readonly errorClass?: ErrorClass;
  /** Overrides the class heading. */
  readonly title?: string;
  /** Overrides the class explanation. */
  readonly body?: string;
  /** Retry callback; rendered as the primary button when the class is retryable. */
  readonly onRetry?: () => void;
  /** Keeps the error on screen and shows a spinner on the retry button. */
  readonly retrying?: boolean;
  /** Runs a remedy button, for example `reveal_in_finder` or `open_settings`. */
  readonly onRemedy?: (remedy: RemedyId) => void;
  /** Minimum height in pixels, matching the populated region. */
  readonly minHeight?: number;
  /** Heading element. Defaults to `h2`. */
  readonly headingLevel?: StateHeadingLevel;
  /** Clipboard port used by Copy details; defaults to the webview clipboard. */
  readonly clipboard?: ClipboardPort;
  /** Extra classes appended to the layout. */
  readonly className?: string;
}

interface ActionInputs {
  readonly report: ErrorReportView;
  readonly copy: ErrorClassCopy;
  readonly onRetry: ErrorStateProps["onRetry"];
  readonly retrying: ErrorStateProps["retrying"];
  readonly onRemedy: ErrorStateProps["onRemedy"];
}

function buildActions({ report, copy, onRetry, retrying, onRemedy }: ActionInputs): StateAction[] {
  const actions: StateAction[] = [];
  const remedies = report.remedies.filter((remedy) => remedy !== "copy_details");
  const retryOffered = remedies.includes("retry") || report.retryable;

  if (onRetry !== undefined && retryOffered) {
    actions.push({
      label: copy.retryLabel,
      onClick: onRetry,
      variant: "primary",
      loading: retrying === true,
    });
  }

  for (const remedy of remedies) {
    if (remedy === "retry" || onRemedy === undefined) {
      continue;
    }
    actions.push({
      label: REMEDY_LABELS[remedy],
      onClick: () => {
        onRemedy(remedy);
      },
      variant: remedy === "cancel" ? "ghost" : "secondary",
    });
  }

  return actions;
}

/**
 * The error variant of {@link StateView}.
 *
 * Shows human copy, every remedy the report offers as a button, a retry action
 * that can spin while the previous error stays visible, and a collapsed
 * Technical details drawer holding the raw path and the OS/parser error with
 * Copy details.
 *
 * @example
 * ```tsx
 * <ErrorState
 *   error={report}
 *   onRetry={retry}
 *   retrying={loading}
 *   onRemedy={runRemedy}
 * />
 * ```
 */
export function ErrorState({
  error,
  errorClass,
  title,
  body,
  onRetry,
  retrying,
  onRemedy,
  minHeight,
  headingLevel,
  clipboard,
  className,
}: ErrorStateProps): JSX.Element {
  const report = useMemo(() => toErrorReport(error), [error]);
  const resolvedClass = errorClass ?? report.class;
  const copy = ERROR_COPY[resolvedClass];
  const details = useMemo(
    () => ({
      summary: `Technical details (${report.code})`,
      text: errorDetailsText(report),
    }),
    [report],
  );
  const actions = buildActions({ report, copy, onRetry, retrying, onRemedy });

  return (
    <StateView
      variant={resolvedClass === "offline" ? "offline" : "error"}
      title={title ?? copy.title}
      body={body ?? copy.body}
      actions={actions}
      details={details}
      minHeight={minHeight}
      headingLevel={headingLevel}
      clipboard={clipboard}
      className={className}
    />
  );
}
