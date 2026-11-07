import { describe, expect, test } from "vitest";
import type { SkillRowView } from "../../generated/SkillRowView";
import { buildSkillLookup, skillStatus } from "./SkillsScreen";

/**
 * The per-cell cross-row scans in the STATUS column were replaced by one
 * snapshot index (3,000 rows × every rendered cell was millions of comparisons
 * per render). These tests pin the lookup semantics the scans implemented.
 */

function row(name: string, scope: string, extra: Partial<SkillRowView> = {}): SkillRowView {
  return {
    item_id: `itm_${name}_${scope}`,
    name,
    scope,
    state: "active",
    shadowed_by: null,
    description: null,
    native: false,
    native_path: null,
    update_available: false,
    source: null,
    source_commit_sha: null,
    source_remote_sha: null,
    source_last_checked_at: null,
    updated_at: "2026-11-06T12:00:00Z",
    agents: [],
    locked: false,
    lock_scope: null,
    lock_reason: null,
    ...extra,
  };
}

function nativeRow(name: string, path: string, agents: readonly string[] = ["codex"]): SkillRowView {
  return row(name, "user", {
    item_id: "",
    native: true,
    state: "native",
    native_path: path,
    agents: agents.map((agent) => ({
      agent,
      state: "native",
      enabled: true,
      config_path: path,
    })),
  });
}

// Stands in for the screen's agent-descriptor lookup, which renames Claude.
const label = (id: string): string => (id === "claude_code" ? "Claude Code" : id);

describe("buildSkillLookup", () => {
  test("names the copy overridden earliest in snapshot order", () => {
    const project = row("Shared", "project");
    const userFirst = buildSkillLookup([row("shared", "user"), row("shared", "global"), project]);
    expect(userFirst.overriddenBy(project)?.scope).toBe("user");

    const globalFirst = buildSkillLookup([row("shared", "global"), row("shared", "user"), project]);
    expect(globalFirst.overriddenBy(project)?.scope).toBe("global");
  });

  test("a row only overrides strictly lower scopes", () => {
    const user = row("solo", "user");
    const lookup = buildSkillLookup([user, row("solo", "project")]);
    expect(lookup.overriddenBy(user)).toBeUndefined();

    const global = row("solo", "global");
    const withGlobal = buildSkillLookup([global, row("solo", "user")]);
    expect(withGlobal.overriddenBy(global)).toBeUndefined();
    expect(withGlobal.overriddenBy(row("solo", "user"))?.scope).toBe("global");
  });

  test("native copies group by the exact display name", () => {
    const lookup = buildSkillLookup([
      nativeRow("Alpha", "/claude/alpha", ["claude_code"]),
      nativeRow("Alpha", "/shared/alpha", ["codex", "omp"]),
      nativeRow("alpha", "/other/alpha"),
    ]);

    expect(lookup.nativeCopies(row("Alpha", "project"))).toHaveLength(2);
    expect(lookup.nativeCopies(row("alpha", "project"))).toHaveLength(1);
    expect(lookup.nativeCopies(row("missing", "project"))).toHaveLength(0);

    const status = skillStatus(
      row("Alpha", "project", { native: true, native_path: "/claude/alpha" }),
      lookup,
      label,
    );
    expect(status.label).toBe("2 copies · 1 shared root");
  });

  test("firstAtScope ignores native rows and matches any case", () => {
    const winner = row("Code-Review", "project");
    const lookup = buildSkillLookup([
      nativeRow("code-review", "/agents/code-review"),
      winner,
      row("code-review", "project", { item_id: "itm_second" }),
    ]);

    expect(lookup.firstAtScope(row("code-review", "user"), "project")).toBe(winner);
    expect(lookup.firstAtScope(row("code-review", "user"), "user")).toBeUndefined();
  });

  test("STATUS keeps its precedence: lock, shadow, native folder, override, active", () => {
    const locked = row("locked", "user", {
      locked: true,
      lock_scope: "global",
      lock_reason: "Pinned by policy",
    });
    const lookup = buildSkillLookup([locked, row("locked", "project")]);

    expect(skillStatus(locked, lookup, label)).toEqual({
      label: "Locked by Global",
      tone: "locked",
      title: "Pinned by policy",
    });
    expect(
      skillStatus(row("locked", "project", { shadowed_by: "user" }), lookup, label).label,
    ).toBe("Shadowed by User");
    const solo = nativeRow("solo", "/claude/solo", ["claude_code"]);
    expect(skillStatus(solo, buildSkillLookup([solo]), label).label).toBe("Claude only");
    expect(skillStatus(row("locked", "project"), lookup, label).label).toBe(
      "Overrides User copy",
    );
    expect(skillStatus(row("unique", "user"), lookup, label).label).toBe("Active");
  });
});
