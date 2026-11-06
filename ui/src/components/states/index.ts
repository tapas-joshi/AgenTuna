/**
 * Public surface of the loading, empty and error state pattern (TU-021).
 *
 * Screens import `StateView` for a controlled state block, or `useAsyncResource`
 * plus `AsyncBoundary` to get the fetch lifecycle and the four states with no
 * flash. `fixtures` carries the TU-010 route matrix used by tests.
 */

export { AsyncBoundary } from "./async-boundary";
export type { AsyncBoundaryEmptyProps, AsyncBoundaryProps } from "./async-boundary";
export { createBrowserClipboard } from "./clipboard";
export { EmptyState } from "./empty-state";
export type { EmptyStateProps } from "./empty-state";
export {
  classifyErrorCode,
  ERROR_CLASSES,
  ERROR_COPY,
  errorDetailsText,
  REMEDY_IDS,
  REMEDY_LABELS,
  toErrorReport,
} from "./error-model";
export type { ErrorClass, ErrorClassCopy, ErrorReportView, RemedyId } from "./error-model";
export { ErrorState } from "./error-state";
export type { ErrorStateProps } from "./error-state";
export {
  SCREEN_STATES,
  SAMPLE_ERROR,
  STATE_CHROME_IDS,
  STATE_ROUTE_IDS,
  STATE_SURFACE_IDS,
  SURFACE_STATE_FIXTURES,
  validateSurfaceFixtures,
} from "./fixtures";
export type {
  PopulatedFixture,
  ScreenState,
  StateChromeId,
  StateFixture,
  StateRouteId,
  StateSurfaceId,
  SurfaceStateFixture,
} from "./fixtures";
export { LoadingState } from "./loading-state";
export { Skeleton } from "./skeleton";
export type { SkeletonProps } from "./skeleton";
export { Spinner } from "./spinner";
export type { SpinnerProps } from "./spinner";
export type { LoadingStateProps } from "./loading-state";
export { StateView } from "./state-view";
export type {
  ClipboardPort,
  CopyState,
  StateAction,
  StateDetails,
  StateHeadingLevel,
  StateVariant,
  StateViewProps,
} from "./types";
export { STATE_VARIANTS, STATE_VARIANT_LABEL } from "./types";
export { DEFAULT_MIN_LOADING_MS, useAsyncResource } from "./use-async-resource";
export type { AsyncResource, AsyncResourceOptions, AsyncStatus } from "./use-async-resource";
