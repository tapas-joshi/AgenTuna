import { describe, expect, test } from "vitest";
import {
  classifyErrorCode,
  ERROR_COPY,
  errorDetailsText,
  REMEDY_LABELS,
  toErrorReport,
} from "./error-model";

describe("classifyErrorCode", () => {
  test.each([
    ["E_PERMISSION_DENIED", "permission_denied"],
    ["E_INVALID_FORMAT", "invalid_format"],
    ["E_NOT_FOUND", "not_found"],
    ["E_CONFLICT", "conflict"],
    ["E_CONCURRENT_MODIFICATION", "conflict"],
    ["E_REFUSED_OVERWRITE", "conflict"],
    ["E_NETWORK", "offline"],
    ["E_NETWORK_DENIED", "network_denied"],
    ["E_INTERNAL", "unknown"],
    ["invalid_argument", "invalid_input"],
    ["not_found", "not_found"],
    ["bridge", "unavailable"],
    ["cli_install", "cli_install"],
    ["internal", "unknown"],
    ["E_SOMETHING_NEW", "unknown"],
  ])("%s becomes %s", (code, expected) => {
    expect(classifyErrorCode(code)).toBe(expected);
  });
});

describe("toErrorReport", () => {
  test("reads the serialised at-model report", () => {
    const report = toErrorReport({
      code: "E_READONLY",
      message: "read-only file system",
      user_message: "Cannot write because the folder is read-only.",
      path: "/Users/ana/.claude/settings.json",
      operation_id: "op_01",
      source_chain: ["open failed", "read-only file system"],
      remedy: ["reveal_in_finder", "copy_details"],
      retryable: false,
    });

    expect(report.code).toBe("E_READONLY");
    expect(report.class).toBe("unknown");
    expect(report.userMessage).toBe("Cannot write because the folder is read-only.");
    expect(report.path).toBe("/Users/ana/.claude/settings.json");
    expect(report.operationId).toBe("op_01");
    expect(report.sourceChain).toEqual(["open failed", "read-only file system"]);
    expect(report.remedies).toEqual(["reveal_in_finder", "copy_details"]);
  });

  test("normalising twice is lossless", () => {
    const once = toErrorReport({
      code: "E_PERMISSION_DENIED",
      message: "Permission denied (os error 13)",
      path: "/tmp/a.md",
      remedy: ["reveal_in_finder", "copy_details"],
      retryable: false,
    });
    const twice = toErrorReport(once);

    expect(twice).toEqual(once);
    expect(errorDetailsText(twice)).toBe(errorDetailsText(once));
  });

  test("filters unknown remedy ids and falls back to the class defaults", () => {
    const filtered = toErrorReport({
      code: "E_NETWORK",
      message: "offline",
      remedy: ["retry", "not_a_remedy"],
    });
    expect(filtered.remedies).toEqual(["retry"]);

    const fallback = toErrorReport({ code: "E_NETWORK", message: "offline", remedy: ["nope"] });
    expect(fallback.remedies).toEqual(ERROR_COPY.offline.remedies);
  });

  test("accepts a plain Error, a string and anything else", () => {
    expect(toErrorReport(new Error("boom")).message).toBe("boom");
    expect(toErrorReport("backend unavailable").message).toBe("backend unavailable");
    expect(toErrorReport(undefined).message).toBe("An unexpected failure occurred.");
    expect(toErrorReport(42).message).toBe("42");
    expect(toErrorReport(42).class).toBe("unknown");
  });
});

describe("errorDetailsText", () => {
  test("keeps the raw path, the OS error and the source chain", () => {
    const report = toErrorReport({
      code: "E_PERMISSION_DENIED",
      message: "Permission denied (os error 13)",
      path: "/Users/ana/.claude/skills/tdd/SKILL.md",
      operation_id: "op_01",
      source_chain: ["open failed"],
      remedy: ["reveal_in_finder", "copy_details"],
      retryable: false,
    });

    expect(errorDetailsText(report)).toBe(
      [
        "code: E_PERMISSION_DENIED",
        "class: permission_denied",
        "path: /Users/ana/.claude/skills/tdd/SKILL.md",
        "operation: op_01",
        "message: Permission denied (os error 13)",
        "caused by:",
        "  open failed",
        "remedy: reveal_in_finder, copy_details",
        "retryable: no",
      ].join("\n"),
    );
  });
});

describe("human copy", () => {
  test("covers the six domain failures a screen must explain", () => {
    for (const errorClass of [
      "permission_denied",
      "invalid_format",
      "missing_agent",
      "conflict",
      "offline",
      "unknown",
    ] as const) {
      const copy = ERROR_COPY[errorClass];
      expect(copy.title.length).toBeGreaterThan(0);
      expect(copy.body.length).toBeGreaterThan(0);
    }
  });

  test("every remedy the engine can send has a button label", () => {
    expect(REMEDY_LABELS.reveal_in_finder).toBe("Reveal in Finder");
    expect(REMEDY_LABELS.move_aside).toBe("Move aside");
    expect(REMEDY_LABELS.install_agent).toBe("Install agent");
    expect(REMEDY_LABELS.copy_details).toBe("Copy details");
    expect(REMEDY_LABELS.cancel).toBe("Cancel");
  });
});
