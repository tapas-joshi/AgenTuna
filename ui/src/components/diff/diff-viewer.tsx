import { useEffect, useMemo, useState } from "react";
import { Button } from "../button";
import { cn } from "../cn";
import { SegmentedControl } from "../segmented-control";
import { Toggle } from "../toggle";
import { writeClipboard } from "./clipboard";
import type { CopyText } from "./clipboard";
import { countDiff, hasWhitespaceOnlyChanges, parseUnifiedDiff, sideBySideRows, visibleLines } from "./model";
import type { DiffFile, DiffHunk, DiffLine, DiffLineKind, DiffModel } from "./model";

/** Layout of the diff body. */
export type DiffMode = "unified" | "side-by-side";

/** Props accepted by {@link DiffViewer}. */
export interface DiffViewerProps {
  /** Unified diff text as produced by `at-fs::diff::unified`. */
  readonly diff: string;
  /** Overrides the file name in the header; defaults to the parsed path. */
  readonly displayPath?: string;
  /** Controlled layout; omit to let the viewer keep it. */
  readonly mode?: DiffMode;
  readonly onModeChange?: (mode: DiffMode) => void;
  /** Controlled whitespace filter; omit to let the viewer keep it. */
  readonly hideWhitespace?: boolean;
  readonly onHideWhitespaceChange?: (hidden: boolean) => void;
  /** Clipboard writer; defaults to {@link writeClipboard}. */
  readonly copy?: CopyText;
  /** Extra classes for the bordered container. */
  readonly className?: string;
}

const MODE_OPTIONS = [
  { value: "unified", label: "Unified" },
  { value: "side-by-side", label: "Side by side" },
] as const;

const LINE_CLASSES: Readonly<Record<DiffLineKind, string>> = {
  context: "bg-transparent",
  add: "bg-success/10",
  remove: "bg-danger/10",
};

const GUTTER_CLASSES: Readonly<Record<DiffLineKind, string>> = {
  context: "bg-transparent",
  add: "bg-success/15",
  remove: "bg-danger/15",
};

const MARKER_CLASSES: Readonly<Record<DiffLineKind, string>> = {
  context: "text-text-muted",
  add: "text-success",
  remove: "text-danger",
};

const MARKERS: Readonly<Record<DiffLineKind, string>> = { context: " ", add: "+", remove: "-" };

const HUNK_HEADER = "sticky top-0 z-10 border-y border-border bg-overlay px-8 py-4 text-text-muted";

function fileLabel(model: DiffModel, displayPath: string | undefined): string {
  if (displayPath !== undefined) {
    return displayPath;
  }
  if (model.files.length === 1) {
    const file = model.files[0];
    return file?.newPath ?? file?.oldPath ?? "Changes";
  }
  return model.files.length === 0 ? "Changes" : `${model.files.length} files`;
}

function FileHeading({ file }: { readonly file: DiffFile }) {
  if (file.binary) {
    return (
      <p className="border-b border-border bg-raised px-8 py-4 text-warning" data-slot="diff-binary">
        Binary file{file.newPath === null ? "" : ` ${file.newPath}`}: no textual diff to show.
      </p>
    );
  }
  return null;
}

function LineNumber({
  value,
  className,
}: {
  readonly value: number | null;
  readonly className?: string;
}) {
  return (
    <span
      className={cn("min-w-24 select-none px-4 text-right text-text-muted tabular-nums", className)}
    >
      {value ?? ""}
    </span>
  );
}

function UnifiedHunk({
  hunk,
  ignoreWhitespace,
}: {
  readonly hunk: DiffHunk;
  readonly ignoreWhitespace: boolean;
}) {
  return (
    <div data-slot="diff-hunk" className="grid grid-cols-[auto_auto_auto_minmax(0,1fr)]">
      <div className={cn(HUNK_HEADER, "col-span-4")}>{hunk.header}</div>
      {visibleLines(hunk, ignoreWhitespace).map((line, index) => (
        <div
          key={`${hunk.header}:${index}`}
          data-slot="diff-line"
          data-kind={line.kind}
          className="contents"
        >
          <LineNumber value={line.oldLine} />
          <LineNumber value={line.newLine} />
          <span
            aria-hidden="true"
            className={cn("px-4 select-none", MARKER_CLASSES[line.kind], GUTTER_CLASSES[line.kind])}
          >
            {MARKERS[line.kind]}
          </span>
          <span
            data-kind={line.kind}
            className={cn("whitespace-pre px-8 text-text", LINE_CLASSES[line.kind])}
          >
            {line.text.length === 0 ? "\u00a0" : line.text}
          </span>
        </div>
      ))}
    </div>
  );
}

function Side({ line, side }: { readonly line: DiffLine | null; readonly side: "old" | "new" }) {
  const divider = side === "new" ? "border-l border-border" : undefined;
  if (line === null) {
    return (
      <>
        <LineNumber value={null} className={divider} />
        <span aria-hidden="true" className="px-4 select-none" />
        <span className="px-8" data-empty="true" />
      </>
    );
  }
  const number = side === "old" ? line.oldLine : line.newLine;
  return (
    <>
      <LineNumber value={number} className={divider} />
      <span
        aria-hidden="true"
        className={cn("px-4 select-none", MARKER_CLASSES[line.kind], GUTTER_CLASSES[line.kind])}
      >
        {MARKERS[line.kind]}
      </span>
      <span
        data-kind={line.kind}
        className={cn("whitespace-pre px-8 text-text", LINE_CLASSES[line.kind])}
      >
        {line.text.length === 0 ? "\u00a0" : line.text}
      </span>
    </>
  );
}

function SideBySideHunk({
  hunk,
  ignoreWhitespace,
}: {
  readonly hunk: DiffHunk;
  readonly ignoreWhitespace: boolean;
}) {
  return (
    <div
      data-slot="diff-hunk"
      className="grid grid-cols-[auto_auto_minmax(0,1fr)_auto_auto_minmax(0,1fr)]"
    >
      <div className={cn(HUNK_HEADER, "col-span-6")}>{hunk.header}</div>
      {sideBySideRows(hunk, ignoreWhitespace).map((row, index) => (
        <div
          key={`${hunk.header}:${index}`}
          data-slot="diff-row"
          className="contents"
        >
          <Side line={row.left} side="old" />
          <Side line={row.right} side="new" />
        </div>
      ))}
    </div>
  );
}

/**
 * Renders unified and side-by-side diffs from the same parsed model.
 *
 * The viewer parses the string once per `diff` change; mode and whitespace
 * filters only reshape the model, and the header counts come from the same
 * lines that are rendered. Inputs over 2 MB or 20,000 lines are clipped before
 * parsing, with a notice; Copy always copies the full string the caller passed.
 *
 * The viewer owns no model data: it reports mode and whitespace changes through
 * the optional controlled props and copies through the injected `copy` writer.
 */
export function DiffViewer({
  diff,
  displayPath,
  mode: controlledMode,
  onModeChange,
  hideWhitespace: controlledHideWhitespace,
  onHideWhitespaceChange,
  copy = writeClipboard,
  className,
}: DiffViewerProps) {
  const [internalMode, setInternalMode] = useState<DiffMode>("unified");
  const [internalHideWhitespace, setInternalHideWhitespace] = useState(false);
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle");
  const mode = controlledMode ?? internalMode;
  const hideWhitespace = controlledHideWhitespace ?? internalHideWhitespace;

  const model = useMemo(() => parseUnifiedDiff(diff), [diff]);
  const counts = useMemo(() => countDiff(model, hideWhitespace), [model, hideWhitespace]);
  const whitespaceOnly = useMemo(() => hasWhitespaceOnlyChanges(model), [model]);
  const label = fileLabel(model, displayPath);

  useEffect(() => {
    if (copyState === "idle") {
      return;
    }
    const timer = window.setTimeout(() => setCopyState("idle"), 2_000);
    return () => window.clearTimeout(timer);
  }, [copyState]);

  const changeMode = (next: DiffMode) => {
    if (controlledMode === undefined) {
      setInternalMode(next);
    }
    onModeChange?.(next);
  };

  const changeWhitespace = (hidden: boolean) => {
    if (controlledHideWhitespace === undefined) {
      setInternalHideWhitespace(hidden);
    }
    onHideWhitespaceChange?.(hidden);
  };

  const copyDiff = () => {
    void Promise.resolve()
      .then(() => copy(diff))
      .then(
        () => setCopyState("copied"),
        () => setCopyState("failed"),
      );
  };

  const copyLabel = copyState === "copied" ? "Copied" : copyState === "failed" ? "Copy failed" : "Copy diff";

  return (
    <section
      data-slot="diff-viewer"
      aria-label={`Diff of ${label}`}
      className={cn("flex min-w-0 flex-col overflow-hidden rounded-6 border border-border bg-surface", className)}
    >
      <header className="flex min-w-0 flex-wrap items-center gap-8 border-b border-border bg-raised px-8 py-4">
        <span className="min-w-0 truncate font-mono text-12 text-text-secondary" title={label}>
          {label}
        </span>
        <span
          role="img"
          className="flex shrink-0 items-center gap-4 font-mono text-12"
          aria-label={`${counts.additions} additions, ${counts.deletions} deletions`}
        >
          <span aria-hidden="true" className="text-success">
            +{counts.additions}
          </span>
          <span aria-hidden="true" className="text-danger">
            -{counts.deletions}
          </span>
        </span>
        <div className="ml-auto flex min-w-0 flex-wrap items-center gap-8">
          <Toggle
            size="sm"
            label="Hide whitespace"
            checked={hideWhitespace}
            onCheckedChange={changeWhitespace}
            disabled={!whitespaceOnly}
          />
          <SegmentedControl<DiffMode>
            label="Diff layout"
            value={mode}
            onValueChange={changeMode}
            options={MODE_OPTIONS}
          />
          <Button size="sm" variant="secondary" onClick={copyDiff} aria-live="polite">
            {copyLabel}
          </Button>
        </div>
      </header>

      {model.truncated ? (
        <p
          role="status"
          data-slot="diff-truncated"
          className="border-b border-border bg-raised px-8 py-4 text-12 text-warning"
        >
          Diff truncated:{" "}
          {model.truncationReason === "bytes"
            ? "the file is larger than 2 MB"
            : "the file has more than 20,000 lines"}
          . Copy diff still copies the full patch.
        </p>
      ) : null}

      {model.files.length === 0 ? (
        <p className="px-16 py-24 text-13 text-text-muted">No changes.</p>
      ) : (
        <div className="min-w-fit overflow-auto font-mono text-13" data-slot="diff-body">
          {model.files.map((file, fileIndex) => (
            <div key={`${file.oldPath ?? "absent"}:${file.newPath ?? "absent"}:${fileIndex}`}>
              {model.files.length > 1 && !file.binary ? (
                <div className="border-b border-border bg-surface px-8 py-4 text-12 font-semibold text-text-secondary">
                  {file.newPath ?? file.oldPath ?? "Changes"}
                </div>
              ) : null}
              <FileHeading file={file} />
              {mode === "unified"
                ? file.hunks.map((hunk, hunkIndex) => (
                    <UnifiedHunk
                      key={`${hunk.header}:${hunkIndex}`}
                      hunk={hunk}
                      ignoreWhitespace={hideWhitespace}
                    />
                  ))
                : file.hunks.map((hunk, hunkIndex) => (
                    <SideBySideHunk
                      key={`${hunk.header}:${hunkIndex}`}
                      hunk={hunk}
                      ignoreWhitespace={hideWhitespace}
                    />
                  ))}
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
