/** Public surface of the diff viewer and its parsed model. */

export { DiffViewer } from "./diff-viewer";
export type { DiffMode, DiffViewerProps } from "./diff-viewer";
export { writeClipboard } from "./clipboard";
export type { CopyText } from "./clipboard";
export {
  MAX_DIFF_BYTES,
  MAX_DIFF_LINES,
  countDiff,
  hasWhitespaceOnlyChanges,
  limitDiffText,
  parseUnifiedDiff,
  sideBySideRows,
  visibleLines,
} from "./model";
export type {
  DiffCounts,
  DiffFile,
  DiffHunk,
  DiffLine,
  DiffLineKind,
  DiffModel,
  LimitedDiffText,
  SideBySideRow,
} from "./model";
