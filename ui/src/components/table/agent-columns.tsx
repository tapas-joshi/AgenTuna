/**
 * Builds the per-agent column definitions.
 *
 * Screens pass their detected agents and a state lookup; every column stays
 * identical in width, sorting and accessibility, which is what keeps the
 * Skills, MCP Servers and Effective config lists aligned.
 */
import type { ColumnDef, RowData } from "@tanstack/react-table";
import { AgentCell } from "./agent-cell";
import type { DataTableFeatures } from "./features";
import type { AgentCellState, AgentDescriptor } from "./types";

/** Default width of one agent column, in pixels. */
export const AGENT_COLUMN_WIDTH = 96;

/** Options accepted by {@link agentColumns}. */
export interface AgentColumnOptions<TData extends RowData> {
  /** Agents in display order; one column each. */
  readonly agents: readonly AgentDescriptor[];
  /** Reads the state of one item in one agent. */
  readonly getState: (row: TData, agent: AgentDescriptor) => AgentCellState;
  /** Applies a switch change. Without it the switches render disabled. */
  readonly onToggle?: (row: TData, agent: AgentDescriptor, next: boolean) => void;
  /** Column width in pixels; defaults to {@link AGENT_COLUMN_WIDTH}. */
  readonly width?: number;
}

/**
 * One column per agent, each rendering {@link AgentCell}.
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
    cell: ({ row }) => (
      <AgentCell
        agent={agent}
        state={options.getState(row.original, agent)}
        onToggle={
          options.onToggle === undefined
            ? undefined
            : (next) => options.onToggle?.(row.original, agent, next)
        }
      />
    ),
  }));
}
