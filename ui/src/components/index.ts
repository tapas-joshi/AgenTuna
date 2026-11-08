/**
 * Component library barrel.
 *
 * Import widgets from `@/components` style paths; every widget is controlled by
 * props and reports interactions through typed callbacks.
 *
 * Heavy widgets — the table (`./table`), diff viewer (`./diff`) and markdown
 * editor/preview (`./editor`) — are NOT re-exported here: the shell and its
 * overlays import this barrel eagerly, and a re-export here would pull the
 * CodeMirror/markdown/react-table stacks into the startup bundle (TF-035).
 * Import them from their subpaths, e.g. `../../components/table`.
 */

export { Badge } from "./badge";
export type { BadgeProps, BadgeTone } from "./badge";
export { Button } from "./button";
export type { ButtonProps, ButtonSize, ButtonVariant } from "./button";
export { Checkbox } from "./checkbox";
export type { CheckboxProps } from "./checkbox";
export { Chip } from "./chip";
export type { ChipDotTone, ChipProps } from "./chip";
export { cn } from "./cn";
export { Input } from "./input";
export type { InputProps, InputSize } from "./input";
export { RadioCardGroup } from "./radio-card";
export type { RadioCardGroupProps, RadioCardOption } from "./radio-card";
export { SegmentedControl } from "./segmented-control";
export type { SegmentedControlProps, SegmentedOption } from "./segmented-control";
export {
  ITEM_STATUSES,
  STATUS_META,
  STATUS_TONE_CLASSES,
  StatusGlyph,
  statusMeta,
} from "./status";
export type {
  ItemStatus,
  StatusGlyphProps,
  StatusMeta,
  StatusTone,
  StatusToneClasses,
} from "./status";
export { StatusDot } from "./status-dot";
export type { StatusDotProps } from "./status-dot";
export { Toggle } from "./toggle";
export type { ToggleLabelPlacement, ToggleProps, ToggleSize } from "./toggle";
export {
  AsyncBoundary,
  DEFAULT_MIN_LOADING_MS,
  EmptyState,
  ErrorState,
  ERROR_CLASSES,
  ERROR_COPY,
  LoadingState,
  REMEDY_IDS,
  REMEDY_LABELS,
  SCREEN_STATES,
  SAMPLE_ERROR,
  Skeleton,
  Spinner,
  STATE_CHROME_IDS,
  STATE_ROUTE_IDS,
  STATE_SURFACE_IDS,
  STATE_VARIANTS,
  STATE_VARIANT_LABEL,
  StateView,
  SURFACE_STATE_FIXTURES,
  classifyErrorCode,
  createBrowserClipboard,
  errorDetailsText,
  toErrorReport,
  useAsyncResource,
  validateSurfaceFixtures,
} from "./states";
export type {
  AsyncBoundaryEmptyProps,
  AsyncBoundaryProps,
  AsyncResource,
  AsyncResourceOptions,
  AsyncStatus,
  ClipboardPort,
  CopyState,
  EmptyStateProps,
  ErrorClass,
  ErrorClassCopy,
  ErrorReportView,
  ErrorStateProps,
  LoadingStateProps,
  PopulatedFixture,
  RemedyId,
  ScreenState,
  SkeletonProps,
  SpinnerProps,
  StateAction,
  StateChromeId,
  StateDetails,
  StateFixture,
  StateHeadingLevel,
  StateRouteId,
  StateSurfaceId,
  StateVariant,
  StateViewProps,
  SurfaceStateFixture,
} from "./states";
export { ScopeSwitcher } from "./scope_switcher/ScopeSwitcher";
export type { ScopeSelected, ScopeSwitcherProps } from "./scope_switcher/ScopeSwitcher";
export { NativeBanner } from "./native/NativeBanner";
export type { NativeBannerProps } from "./native/NativeBanner";
