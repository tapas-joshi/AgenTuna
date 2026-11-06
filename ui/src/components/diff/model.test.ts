import { describe, expect, test } from "vitest";
import {
  MAX_DIFF_BYTES,
  MAX_DIFF_LINES,
  countDiff,
  hasWhitespaceOnlyChanges,
  limitDiffText,
  parseUnifiedDiff,
  sideBySideRows,
  visibleLines,
} from "./model";

const SAMPLE = [
  "--- a/skills/demo/SKILL.md",
  "+++ b/skills/demo/SKILL.md",
  "@@ -1,4 +1,5 @@",
  " # Demo",
  "-old line",
  "+new line",
  "-same   ",
  "+same",
  "+extra",
  " context",
].join("\n");

describe("parseUnifiedDiff", () => {
  test("parses paths, hunk numbers and both line-number columns", () => {
    const model = parseUnifiedDiff(SAMPLE);

    expect(model.files).toHaveLength(1);
    const file = model.files[0];
    expect(file?.oldPath).toBe("skills/demo/SKILL.md");
    expect(file?.newPath).toBe("skills/demo/SKILL.md");
    expect(file?.binary).toBe(false);
    const hunk = file?.hunks[0];
    expect(hunk?.oldStart).toBe(1);
    expect(hunk?.oldCount).toBe(4);
    expect(hunk?.newStart).toBe(1);
    expect(hunk?.newCount).toBe(5);
    expect(hunk?.lines.map((line) => [line.kind, line.oldLine, line.newLine])).toEqual([
      ["context", 1, 1],
      ["remove", 2, null],
      ["add", null, 2],
      ["remove", 3, null],
      ["add", null, 3],
      ["add", null, 4],
      ["context", 4, 5],
    ]);
  });

  test("does not mistake content that looks like file headers for a new file", () => {
    const model = parseUnifiedDiff(
      [
        "--- a/x",
        "+++ b/x",
        "@@ -1,2 +1,2 @@",
        "--- old header-ish",
        "+++ new header-ish",
        " context",
      ].join("\n"),
    );

    expect(model.files).toHaveLength(1);
    expect(model.files[0]?.hunks[0]?.lines.map((line) => line.text)).toEqual([
      "-- old header-ish",
      "++ new header-ish",
      "context",
    ]);
  });

  test("skips the no-newline marker and supports /dev/null paths", () => {
    const model = parseUnifiedDiff(
      ["--- /dev/null", "+++ b/new.txt", "@@ -0,0 +1,1 @@", "+hello", "\\ No newline at end of file"].join(
        "\n",
      ),
    );
    const file = model.files[0];

    expect(file?.oldPath).toBeNull();
    expect(file?.newPath).toBe("new.txt");
    expect(file?.hunks[0]?.lines).toHaveLength(1);
  });

  test("parses multiple file sections and binary notes", () => {
    const model = parseUnifiedDiff(
      [
        "--- a/one",
        "+++ b/one",
        "@@ -1 +1 @@",
        "-a",
        "+b",
        "--- a/two",
        "+++ b/two",
        "@@ -1 +1 @@",
        "-c",
        "+d",
        "Binary files a/logo.png and b/logo.png differ",
      ].join("\n"),
    );

    expect(model.files.map((file) => file.newPath)).toEqual(["one", "two", "logo.png"]);
    expect(model.files[2]?.binary).toBe(true);
  });

  test("returns an empty model for empty or unrelated text", () => {
    expect(parseUnifiedDiff("")).toEqual({
      files: [],
      truncated: false,
      truncationReason: null,
    });
    expect(parseUnifiedDiff("just a note").files).toEqual([]);
  });
});

describe("whitespace-only changes", () => {
  test("marks paired lines that differ only in whitespace", () => {
    const hunk = parseUnifiedDiff(SAMPLE).files[0]?.hunks[0];
    const marked = hunk?.lines.filter((line) => line.whitespaceOnly);

    expect(marked?.map((line) => [line.kind, line.text])).toEqual([
      ["remove", "same   "],
      ["add", "same"],
    ]);
    expect(hasWhitespaceOnlyChanges(parseUnifiedDiff(SAMPLE))).toBe(true);
  });

  test("updates counts when whitespace-only pairs are hidden", () => {
    const model = parseUnifiedDiff(SAMPLE);

    expect(countDiff(model)).toEqual({ additions: 3, deletions: 2 });
    expect(countDiff(model, true)).toEqual({ additions: 2, deletions: 1 });
    expect(visibleLines(model.files[0]!.hunks[0]!, true).map((line) => line.text)).toEqual([
      " # Demo".slice(1),
      "old line",
      "new line",
      "extra",
      "context",
    ]);
  });
});

describe("side-by-side shaping", () => {
  test("pairs removals with additions and leaves the longer side blank", () => {
    const hunk = parseUnifiedDiff(
      ["--- a/x", "+++ b/x", "@@ -1,2 +1,3 @@", " keep", "-old", "+new", "+extra"].join("\n"),
    ).files[0]!.hunks[0]!;
    const rows = sideBySideRows(hunk, false);

    expect(rows).toHaveLength(3);
    expect([rows[0]?.left?.text, rows[0]?.right?.text]).toEqual(["keep", "keep"]);
    expect([rows[1]?.left?.text, rows[1]?.right?.text]).toEqual(["old", "new"]);
    expect(rows[2]?.left).toBeNull();
    expect(rows[2]?.right?.text).toBe("extra");
    expect(rows[2]?.left === null).toBe(true);
  });

  test("drops hidden whitespace rows from both sides", () => {
    const hunk = parseUnifiedDiff(SAMPLE).files[0]!.hunks[0]!;
    const rows = sideBySideRows(hunk, true);

    expect(rows.some((row) => row.left?.text === "same   " || row.right?.text === "same")).toBe(false);
    expect(rows).toHaveLength(4);
  });
});

describe("size limits", () => {
  test("truncates input over the byte budget without encoding all of it", () => {
    const limited = limitDiffText("x".repeat(MAX_DIFF_BYTES + 1));

    expect(limited.truncated).toBe(true);
    expect(limited.truncationReason).toBe("bytes");
    expect(limited.text).toHaveLength(MAX_DIFF_BYTES);
  });

  test("truncates input over the line budget and parses only the kept lines", () => {
    const lines = Array.from({ length: MAX_DIFF_LINES + 5 }, (_, index) => ` line ${index}`);
    const raw = ["--- a/x", "+++ b/x", "@@ -1,20005 +1,20005 @@", ...lines].join("\n");
    const model = parseUnifiedDiff(raw);

    expect(model.truncated).toBe(true);
    expect(model.truncationReason).toBe("lines");
    expect(model.files[0]?.hunks[0]?.lines.length).toBe(MAX_DIFF_LINES - 3);
  });

  test("leaves a diff under both budgets untouched", () => {
    expect(limitDiffText(SAMPLE)).toEqual({ text: SAMPLE, truncated: false, truncationReason: null });
  });

  test("treats exactly the line budget as shown in full, including a trailing newline", () => {
    const lines = Array.from({ length: MAX_DIFF_LINES }, (_, index) => `line ${index}`);
    const raw = `${lines.join("\n")}\n`;

    expect(limitDiffText(raw)).toEqual({ text: raw, truncated: false, truncationReason: null });
  });
});
