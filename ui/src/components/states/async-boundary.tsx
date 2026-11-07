import type { JSX, ReactNode } from "react";
import { EmptyState } from "./empty-state";
import type { ErrorClass, RemedyId } from "./error-model";
import { ErrorState } from "./error-state";
import { LoadingState } from "./loading-state";
import type { ClipboardPort, StateAction, StateViewProps } from "./types";
import type { AsyncResource } from "./use-async-resource";

/** Copy for the empty state; every field is optional. */
export interface AsyncBoundaryEmptyProps {
  /** Heading; defaults to `Nothing here yet`. */
  readonly title?: string;
  /** One sentence that explains how to populate the screen. */
  readonly body?: ReactNode;
  /** Decorative icon. */
  readonly icon?: ReactNode;
  /** The action that resolves the emptiness. */
  readonly action?: StateAction;
  /** Optional second action, for states that offer an alternative route. */
  readonly secondaryAction?: StateAction;
}

/** Props accepted by {@link AsyncBoundary}. */
export interface AsyncBoundaryProps<T> {
  /** State returned by `useAsyncResource`. */
  readonly state: AsyncResource<T>;
  /** Renders the populated screen from loaded data. */
  readonly children: (data: T) => ReactNode;
  /** Label shown with the spinner; defaults to `Loading…`. */
  readonly loadingTitle?: string;
  /** Optional supporting sentence under the loading label. */
  readonly loadingBody?: ReactNode;
  /** Skeleton row count; replaces the spinner, for table bodies. */
  readonly skeletonRows?: number;
  /** Copy for the empty state. */
  readonly empty?: AsyncBoundaryEmptyProps;
  /** Overrides the error heading. */
  readonly errorTitle?: string;
  /** Forces an error category when the code is ambiguous. */
  readonly errorClass?: ErrorClass;
  /** Runs a remedy button from the error state. */
  readonly onRemedy?: (remedy: RemedyId) => void;
  /** Retry callback; defaults to `state.retry`. */
  readonly onRetry?: () => void;
  /** Height reserved by every state, so switching never shifts the layout. */
  readonly minHeight?: number;
  /** Live-region politeness forwarded to the loading state. */
  readonly ariaLive?: StateViewProps["ariaLive"];
  /** Clipboard port used by Copy details. */
  readonly clipboard?: ClipboardPort;
  /** Extra classes on the populated wrapper. */
  readonly className?: string;
}

/**
 * Renders exactly one of loading, error, empty or populated, never a flash of
 * the wrong one.
 *
 * Rules, in order:
 * 1. A visible error wins, even while a retry runs; the retry button spins and
 *    the previous error stays readable.
 * 2. Before any data arrives the loading state shows, so an empty state never
 *    appears during the first fetch.
 * 3. A reload with data already on screen keeps rendering that data and marks
 *    the region `aria-busy`.
 * 4. Empty data renders the empty state with its one action.
 *
 * @example
 * ```tsx
 * const resource = useAsyncResource(loadSkills, { deps: [scope] });
 * return (
 *   <AsyncBoundary
 *     state={resource}
 *     skeletonRows={8}
 *     minHeight={360}
 *     empty={{
 *       title: "No skills in this scope",
 *       body: "Import a skill or create one from scratch.",
 *       action: { label: "Add skill", onClick: openAddSkill },
 *     }}
 *   >
 *     {(skills) => <SkillTable rows={skills} />}
 *   </AsyncBoundary>
 * );
 * ```
 */
export function AsyncBoundary<T>({
  state,
  children,
  loadingTitle,
  loadingBody,
  skeletonRows,
  empty,
  errorTitle,
  errorClass,
  onRemedy,
  onRetry,
  minHeight,
  ariaLive,
  clipboard,
  className,
}: AsyncBoundaryProps<T>): JSX.Element {
  const retry = onRetry ?? state.retry;

  if (state.error !== undefined) {
    return (
      <ErrorState
        error={state.error}
        errorClass={errorClass}
        title={errorTitle}
        retrying={state.loading}
        onRetry={retry}
        onRemedy={onRemedy}
        minHeight={minHeight}
        clipboard={clipboard}
      />
    );
  }

  if (state.status === "loading" && (state.data === undefined || state.isEmpty)) {
    return (
      <LoadingState
        title={loadingTitle}
        body={loadingBody}
        skeletonRows={skeletonRows}
        minHeight={minHeight}
        ariaLive={ariaLive}
      />
    );
  }

  if (state.data === undefined || state.isEmpty) {
    return (
      <EmptyState
        title={empty?.title ?? "Nothing here yet"}
        hint={empty?.body}
        icon={empty?.icon}
        action={empty?.action}
        secondaryAction={empty?.secondaryAction}
        minHeight={minHeight}
      />
    );
  }

  return (
    <div
      data-slot="async-boundary"
      aria-busy={state.loading ? true : undefined}
      className={className}
    >
      {children(state.data)}
    </div>
  );
}
