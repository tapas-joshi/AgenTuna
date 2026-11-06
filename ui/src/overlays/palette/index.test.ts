import { describe, expect, it } from "vitest";
import { buildPaletteIndex, idlePaletteEntries, searchPalette } from "./index";

const index = buildPaletteIndex({
  skills: [{ id: "skill:alpha", title: "McToolkit", subtitle: "Skill", keywords: ["MCP"] }],
  servers: [{ id: "server:db", title: "Postgres", subtitle: "MCP server", keywords: ["database"] }],
  rules: [{ id: "rule:one", title: "Agents.md", subtitle: "Rule file", keywords: ["instructions"] }],
  agents: [{ id: "agent:claude", title: "Claude Code", subtitle: "Installed", keywords: ["claude"] }],
  actions: [{ id: "action:mcp", title: "MCP Servers", subtitle: "Navigate", keywords: ["servers"] }, { id: "action:skills", title: "Skills", subtitle: "Navigate", keywords: ["skills"] }],
});

describe("palette index", () => {
  it("creates stable IDs for each inventory kind and ranks MCP matches", () => {
    expect(index.map(({ id }) => id)).toEqual(["skill:alpha", "server:db", "rule:one", "agent:claude", "action:mcp", "action:skills"]);
    expect(searchPalette(index, "mc").slice(0, 3).map(({ id }) => id)).toEqual(["action:mcp", "skill:alpha", "server:db"]);
  });

  it("ranks exact prefix above word-start and subsequence matches", () => {
    const results = searchPalette([
      { id: "prefix", kind: "action", title: "MCP Servers", subtitle: "", keywords: [] },
      { id: "boundary", kind: "action", title: "Open MCP Servers", subtitle: "", keywords: [] },
      { id: "subsequence", kind: "action", title: "macrophage", subtitle: "", keywords: [] },
    ], "mcp");
    expect(results.map(({ id }) => id)).toEqual(["prefix", "boundary", "subsequence"]);
  });

  it("returns recent entries and action shortcuts for an empty query", () => {
    expect(idlePaletteEntries(index, ["server:db", "missing"]).map(({ id }) => id)).toEqual(["server:db", "action:mcp", "action:skills"]);
  });
});
