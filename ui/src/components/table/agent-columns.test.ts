import { describe, expect, test } from "vitest";
import { agentFolderGroups, folderSpan } from "./agent-columns";
import type { AgentDescriptor } from "./types";

const agents: AgentDescriptor[] = [
  { id: "claude_code", label: "Claude Code" },
  { id: "codex", label: "Codex" },
  { id: "omp", label: "omp" },
];

describe("agentFolderGroups", () => {
  test("folds agents that read one folder into contiguous group columns", () => {
    const paths = new Map([
      ["claude_code", "/sandbox/.claude/skills/demo"],
      ["codex", "/sandbox/.agents/skills/demo"],
      ["omp", "/sandbox/.agents/skills/demo"],
    ]);
    const groups = agentFolderGroups(agents, (agent) => paths.get(agent.id));

    expect(groups.get("codex")?.agents.map((agent) => agent.id)).toEqual(["codex", "omp"]);
    expect(groups.get("omp")?.path).toBe("/sandbox/.agents/skills/demo");
    expect(groups.has("claude_code")).toBe(false);
    // Only the first member spans; the table skips the columns it covers.
    expect(folderSpan("agent:codex", groups, agents)).toBe(2);
    expect(folderSpan("agent:omp", groups, agents)).toBe(1);
    expect(folderSpan("agent:claude_code", groups, agents)).toBe(1);
    expect(folderSpan("scope", groups, agents)).toBe(1);
  });

  test("leaves agents with unrecorded or non-contiguous folders independent", () => {
    const paths = new Map([
      ["claude_code", "/sandbox/.agents/skills/demo"],
      ["codex", "/sandbox/.codex/skills/demo"],
      ["omp", "/sandbox/.agents/skills/demo"],
    ]);
    const groups = agentFolderGroups(agents, (agent) => paths.get(agent.id));

    // claude_code and omp share a folder but are not adjacent: merging them would
    // move the Codex cell into the block, so both stay separate columns.
    expect(groups.size).toBe(0);
    expect(folderSpan("agent:omp", groups, agents)).toBe(1);
  });
});
