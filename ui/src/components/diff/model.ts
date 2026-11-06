/**
 * Parsing and shaping for unified diffs.
 *
 * `at-fs::diff::unified` renders a single file (and a one-line note for binary
 * input) with `---`/`+++` headers and `@@` hunks. Write previews, migration
 * conflicts and drift screens all hand that string to {@link DiffViewer}, so
 * this module is the one place that turns it into line numbers, counts and
 * side-by-side rows.
 */

/** Bytes of unified diff text accepted before the viewer truncates it. */
export const MAX_DIFF_BYTES = 2 * 1024 * 1024;

/** Lines of unified diff text accepted before the viewer truncates it. */
export const MAX_DIFF_LINES = 20_000;

/** Which side of a diff a line belongs to. */
export type DiffLineKind = "context" | "add" | "remove";

/** One line of a hunk, with the line numbers it occupies on each side. */
export interface DiffLine {
  readonly kind: DiffLineKind;
  /** Content without the leading ` `/`+`/`-` marker. */
  readonly text: string;
  /** Old-file line number, or `null` for an added line. */
  readonly oldLine: number | null;
  /** New-file line number, or `null` for a removed line. */
  readonly newLine: number | null;
  /** True when a removed/added pair differs only in whitespace. */
  readonly whitespaceOnly: boolean;
}

/** One `@@` hunk. */
export interface DiffHunk {
  /** The literal `@@ -a,b +c,d @@` header text. */
  readonly header: string;
  readonly oldStart: number;
  readonly oldCount: number;
  readonly newStart: number;
  readonly newCount: number;
  readonly lines: readonly DiffLine[];
}

/** One file section of a unified diff. */
export interface DiffFile {
  /** Old path, or `null` when the file did not exist. */
  readonly oldPath: string | null;
  /** New path, or `null` when the file was deleted. */
  readonly newPath: string | null;
  readonly hunks: readonly DiffHunk[];
  /** True for at-fs's `Binary files … differ` note instead of hunks. */
  readonly binary: boolean;
}

/** A parsed unified diff. */
export interface DiffModel {
  readonly files: readonly DiffFile[];
  /** True when the input exceeded {@link MAX_DIFF_BYTES} or {@link MAX_DIFF_LINES}. */
  readonly truncated: boolean;
  /** Why the input was truncated, or `null` when it was shown in full. */
  readonly truncationReason: "bytes" | "lines" | null;
}

/** Additions and deletions after an optional whitespace filter. */
export interface DiffCounts {
  readonly additions: number;
  readonly deletions: number;
}

/** One row of a side-by-side hunk: old side on the left, new side on the right. */
export interface SideBySideRow {
  readonly left: DiffLine | null;
  readonly right: DiffLine | null;
}

export interface LimitedDiffText {
  readonly text: string;
  readonly truncated: boolean;
  readonly truncationReason: "bytes" | "lines" | null;
}

const HUNK_HEADER = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)$/;
const BINARY_NOTE = /^Binary files (.+) differ$/;

/** Cuts at `limit` code units without leaving half a surrogate pair behind. */
function clipAt(text: string, limit: number): string {
  let end = Math.min(text.length, limit);
  const last = text.charCodeAt(end - 1);
  if (last >= 0xd800 && last <= 0xdbff) {
    end -= 1;
  }
  return text.slice(0, end);
}

/**
 * Clips diff text to the viewer's size budget before parsing.
 *
 * The byte check runs first because it is cheap for absurd inputs: UTF-8 never
 * uses fewer bytes than the string has UTF-16 code units, so a string longer
 * than {@link MAX_DIFF_BYTES} is over budget without encoding all of it.
 */
export function limitDiffText(raw: string): LimitedDiffText {
  if (raw.length > MAX_DIFF_BYTES) {
    return { text: clipAt(raw, MAX_DIFF_BYTES), truncated: true, truncationReason: "bytes" };
  }
  const bytes = new TextEncoder().encode(raw).byteLength;
  if (bytes > MAX_DIFF_BYTES) {
    return { text: clipAt(raw, MAX_DIFF_BYTES), truncated: true, truncationReason: "bytes" };
  }
  const lines = raw.split("\n");
  const logicalLines = raw.endsWith("\n") ? lines.length - 1 : lines.length;
  if (logicalLines > MAX_DIFF_LINES) {
    return {
      text: lines.slice(0, MAX_DIFF_LINES).join("\n"),
      truncated: true,
      truncationReason: "lines",
    };
  }
  return { text: raw, truncated: false, truncationReason: null };
}

function parsePath(value: string): string | null {
  const path = value.trim();
  if (path === "/dev/null") {
    return null;
  }
  return path.replace(/^[ab]\//, "");
}

/** Whitespace-insensitive form of a line, used to spot formatting-only edits. */
function normalized(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

/**
 * Marks removed/added pairs that are equal once whitespace is normalised.
 *
 * Pairs are matched by position inside each contiguous change block, which is
 * how unified diffs present a replacement: removals first, then additions.
 */
function markWhitespaceOnly(lines: readonly DiffLine[]): DiffLine[] {
  interface MutableLine {
    kind: DiffLineKind;
    text: string;
    oldLine: number | null;
    newLine: number | null;
    whitespaceOnly: boolean;
  }
  const marked: MutableLine[] = lines.map((line) => ({ ...line }));
  let index = 0;
  while (index < marked.length) {
    if (marked[index]?.kind === "context") {
      index += 1;
      continue;
    }
    const block: MutableLine[] = [];
    while (index < marked.length && marked[index]?.kind !== "context") {
      const line = marked[index];
      if (line !== undefined) {
        block.push(line);
      }
      index += 1;
    }
    const removals = block.filter((line) => line.kind === "remove");
    const additions = block.filter((line) => line.kind === "add");
    const pairs = Math.min(removals.length, additions.length);
    for (let pair = 0; pair < pairs; pair += 1) {
      const removed = removals[pair];
      const added = additions[pair];
      if (removed === undefined || added === undefined) {
        continue;
      }
      if (normalized(removed.text) === normalized(added.text)) {
        removed.whitespaceOnly = true;
        added.whitespaceOnly = true;
      }
    }
  }
  return marked;
}

/**
 * Parses one or more unified file diffs.
 *
 * Unrecognised header lines (`diff --git`, `index …`) are skipped so a git-style
 * diff still yields its hunks. A `Binary files … differ` note produces a file
 * with `binary: true` and no hunks.
 */
export function parseUnifiedDiff(raw: string): DiffModel {
  const limited = limitDiffText(raw);
  const lines = limited.text.replace(/\r\n/g, "\n").split("\n");
  if (lines.at(-1) === "") {
    lines.pop();
  }
  const files: DiffFile[] = [];
  let index = 0;
  while (index < lines.length) {
    const line = lines[index] ?? "";
    const binary = BINARY_NOTE.exec(line);
    if (binary !== null) {
      const described = binary[1] ?? "";
      const parts = described.split(" and ");
      const pair =
        parts.length === 2 &&
        (parts[0]?.startsWith("a/") === true || parts[0] === "/dev/null") &&
        (parts[1]?.startsWith("b/") === true || parts[1] === "/dev/null");
      const path = parsePath(pair ? (parts[1] ?? described) : described);
      files.push({ oldPath: path, newPath: path, hunks: [], binary: true });
      index += 1;
      continue;
    }
    if (!line.startsWith("--- ") || !(lines[index + 1] ?? "").startsWith("+++ ")) {
      index += 1;
      continue;
    }
    const oldPath = parsePath(line.slice(4));
    const newPath = parsePath((lines[index + 1] ?? "").slice(4));
    index += 2;
    const hunks: DiffHunk[] = [];
    while (index < lines.length) {
      const headerLine = lines[index] ?? "";
      if (
        (headerLine.startsWith("--- ") && (lines[index + 1] ?? "").startsWith("+++ ")) ||
        BINARY_NOTE.test(headerLine)
      ) {
        break;
      }
      const header = HUNK_HEADER.exec(headerLine);
      if (header === null) {
        index += 1;
        continue;
      }
      const oldStart = Number(header[1] ?? "0");
      const oldCount = header[2] === undefined ? 1 : Number(header[2]);
      const newStart = Number(header[3] ?? "0");
      const newCount = header[4] === undefined ? 1 : Number(header[4]);
      index += 1;
      let oldLine = oldStart;
      let newLine = newStart;
      let oldRemaining = oldCount;
      let newRemaining = newCount;
      const body: DiffLine[] = [];
      while (index < lines.length && (oldRemaining > 0 || newRemaining > 0)) {
        const bodyLine = lines[index] ?? "";
        if (bodyLine.startsWith("\\")) {
          index += 1;
          continue;
        }
        if (bodyLine.startsWith("+")) {
          body.push({
            kind: "add",
            text: bodyLine.slice(1),
            oldLine: null,
            newLine,
            whitespaceOnly: false,
          });
          newLine += 1;
          newRemaining -= 1;
        } else if (bodyLine.startsWith("-")) {
          body.push({
            kind: "remove",
            text: bodyLine.slice(1),
            oldLine,
            newLine: null,
            whitespaceOnly: false,
          });
          oldLine += 1;
          oldRemaining -= 1;
        } else if (bodyLine.startsWith(" ") || bodyLine === "") {
          body.push({
            kind: "context",
            text: bodyLine.slice(1),
            oldLine,
            newLine,
            whitespaceOnly: false,
          });
          oldLine += 1;
          newLine += 1;
          oldRemaining -= 1;
          newRemaining -= 1;
        } else {
          break;
        }
        index += 1;
      }
      hunks.push({ header: headerLine, oldStart, oldCount, newStart, newCount, lines: markWhitespaceOnly(body) });
    }
    files.push({ oldPath, newPath, hunks, binary: false });
  }
  return { files, truncated: limited.truncated, truncationReason: limited.truncationReason };
}

/** Counts changed lines, excluding whitespace-only pairs when asked. */
export function countDiff(model: DiffModel, ignoreWhitespace = false): DiffCounts {
  let additions = 0;
  let deletions = 0;
  for (const file of model.files) {
    for (const hunk of file.hunks) {
      for (const line of hunk.lines) {
        if (ignoreWhitespace && line.whitespaceOnly) {
          continue;
        }
        if (line.kind === "add") {
          additions += 1;
        } else if (line.kind === "remove") {
          deletions += 1;
        }
      }
    }
  }
  return { additions, deletions };
}

/** True when at least one hunk contains a whitespace-only pair. */
export function hasWhitespaceOnlyChanges(model: DiffModel): boolean {
  return model.files.some((file) =>
    file.hunks.some((hunk) => hunk.lines.some((line) => line.whitespaceOnly)),
  );
}

/** Lines of a hunk after the appearance filter. */
export function visibleLines(
  hunk: DiffHunk,
  ignoreWhitespace: boolean,
): readonly DiffLine[] {
  return ignoreWhitespace ? hunk.lines.filter((line) => !line.whitespaceOnly) : hunk.lines;
}

/**
 * Aligns a hunk into side-by-side rows.
 *
 * Removed and added runs inside one change block are paired by position; the
 * longer run leaves blank cells on the other side. Context lines appear on
 * both sides with their old and new numbers.
 */
export function sideBySideRows(
  hunk: DiffHunk,
  ignoreWhitespace: boolean,
): readonly SideBySideRow[] {
  const rows: SideBySideRow[] = [];
  const lines = visibleLines(hunk, ignoreWhitespace);
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    if (line === undefined) {
      break;
    }
    if (line.kind === "context") {
      rows.push({ left: line, right: line });
      index += 1;
      continue;
    }
    const block: DiffLine[] = [];
    while (index < lines.length && lines[index]?.kind !== "context") {
      const changed = lines[index];
      if (changed !== undefined) {
        block.push(changed);
      }
      index += 1;
    }
    const removals = block.filter((entry) => entry.kind === "remove");
    const additions = block.filter((entry) => entry.kind === "add");
    const rowCount = Math.max(removals.length, additions.length);
    for (let row = 0; row < rowCount; row += 1) {
      rows.push({ left: removals[row] ?? null, right: additions[row] ?? null });
    }
  }
  return rows;
}
