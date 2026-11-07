/**
 * Builds the per-agent column definitions.
 *
 * Screens pass their detected agents and a state lookup; every column stays
 * identical in width, sorting and accessibility, which is what keeps the
 * Skills, MCP Servers and Effective config lists aligned. A screen that knows
 * several agents read one discovery folder passes `getFolderGroups`: the first
 * column of the group renders one folder block and spans the following ones,
 * so a shared root never turns into independent per-agent switches.
 */
import type { ColumnDef, RowData } from "@tanstack/react-table";
import { AgentCell, SharedFolderCell } from "./agent-cell";
import type { DataTableFeatures } from "./features";
import type { AgentCellState, AgentDescriptor } from "./types";

/** Default width of one agent column, in pixels. Wide enough for `shared with <agent>`. */
export const AGENT_COLUMN_WIDTH = 112;

/** One discovery folder that several agents read. */
export interface AgentFolderGroup {
  /** Agents that read the folder, in column order. */
  readonly agents: readonly AgentDescriptor[];
  /** Absolute folder path they share. */
  readonly path: string;
}

/**
 * Groups agents whose cells point at the same folder.
 *
 * Only contiguous columns become a group: the table merges cells with a grid
 * span, and a span across an unrelated column would move that cell into the
 * block visually. Agents without a recorded folder stay independent.
 */
export function agentFolderGroups(
  agents: readonly AgentDescriptor[],
  pathOf: (agent: AgentDescriptor) => string | null | undefined,
): ReadonlyMap<string, AgentFolderGroup> {
  const byPath = new Map<string, AgentDescriptor[]>();
  for (const agent of agents) {
    const path = pathOf(agent);
    if (path === null || path === undefined || path === "") continue;
    const members = byPath.get(path);
    if (members === undefined) byPath.set(path, [agent]);
    else members.push(agent);
  }
  const groups = new Map<string, AgentFolderGroup>();
  for (const [path, members] of byPath) {
    if (members.length < 2) continue;
    const indexes = members.map((agent) => agents.findIndex((entry) => entry.id === agent.id));
    const contiguous = indexes.every(
      (value, index) => index === 0 || value === (indexes[index - 1] ?? -2) + 1,
    );
    if (!contiguous) continue;
    const group: AgentFolderGroup = { agents: members, path };
    for (const agent of members) groups.set(agent.id, group);
  }
  return groups;
}

/** How many agent columns a shared folder cell spans, starting at its first agent. */
export function folderSpan(
  columnId: string,
  groups: ReadonlyMap<string, AgentFolderGroup>,
  agents: readonly AgentDescriptor[],
): number {
  const prefix = "agent:";
  if (!columnId.startsWith(prefix)) return 1;
  const group = groups.get(columnId.slice(prefix.length));
  if (group === undefined) return 1;
  const index = agents.findIndex((agent) => `agent:${agent.id}` === columnId);
  if (index < 0 || agents[index]?.id !== group.agents[0]?.id) return 1;
  return group.agents.length;
}

/** Options accepted by {@link agentColumns}. */
export interface AgentColumnOptions<TData extends RowData> {
  /** Agents in display order; one column each. */
  readonly agents: readonly AgentDescriptor[];
  /** Reads the state of one item in one agent. */
  readonly getState: (row: TData, agent: AgentDescriptor) => AgentCellState;
  /**
   * Reads the discovery folders shared by several agents for this row.
   * Returning nothing keeps every agent column separate.
   */
  readonly getFolderGroups?: (row: TData) => ReadonlyMap<string, AgentFolderGroup>;
  /** Applies a switch change. Without it the switches render disabled. */
  readonly onToggle?: (row: TData, agent: AgentDescriptor, next: boolean) => void;
  /** Column width in pixels; defaults to {@link AGENT_COLUMN_WIDTH}. */
  readonly width?: number;
}

/**
 * One column per agent, each rendering {@link AgentCell} or, for a shared
 * discovery root, one {@link SharedFolderCell} spanning the coupled columns.
 *
 * Agent columns are not sortable: they contain a switch or a path, not a value
 * the store can order, and a sort arrow on a toggle would be a lie.
 */
export function agentColumns<TData extends RowData>(
  options: AgentColumnOptions<TData>,
): ReadonlyArray<ColumnDef<DataTableFeatures, TData>> {
  return options.agents.map((agent) => ({
    id: `agent:${agent.id}`,
    header: agent.label,
    size: options.width ?? AGENT_COLUMN_WIDTH,
    enableSorting: false,
    meta: { align: "center" as const },
    cell: ({ row }) => {
      const group = options.getFolderGroups?.(row.original).get(agent.id);
      const state = options.getState(row.original, agent);
      if (group !== undefined && group.agents[0]?.id === agent.id) {
        const first = group.agents[0] ?? agent;
        return (
          <SharedFolderCell
            agents={group.agents}
            path={group.path}
            state={options.getState(row.original, first)}
            onToggle={
              options.onToggle === undefined
                ? undefined
                : (next) => options.onToggle?.(row.original, first, next)
            }
          />
        );
      }
      return (
        <AgentCell
          agent={agent}
          state={state}
          onToggle={
            options.onToggle === undefined
              ? undefined
              : (next) => options.onToggle?.(row.original, agent, next)
          }
        />
      );
    },
  }));
}
