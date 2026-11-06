/**
 * Public surface of the table module.
 *
 * `DataTable` is the virtualized list used by the Skills, MCP Servers and
 * Effective config screens. Pair it with `agentColumns` for the per-agent
 * columns and `useRowMenu` for the row menu.
 */

export { AGENT_COLUMN_WIDTH, agentColumns } from "./agent-columns";
export type { AgentColumnOptions } from "./agent-columns";
export { AgentCell } from "./agent-cell";
export type { AgentCellProps } from "./agent-cell";
export { dataTableFeatures } from "./features";
export type { DataTableColumnMeta, DataTableFeatures } from "./features";
export { RowMenuTrigger, useRowMenu } from "./row-menu";
export type { RowMenuController, RowMenuTriggerProps } from "./row-menu";
export {
  bulkSelectionState,
  countSelected,
  selectionForClick,
  setDisplayedSelected,
  toggleSelection,
} from "./selection";
export type {
  BulkSelectionState,
  SelectionClick,
  SelectionMode,
  SelectionModifiers,
  SelectionResult,
} from "./selection";
export { DataTable, TABLE_HEADER_HEIGHT, TABLE_ROW_HEIGHT } from "./table";
export type { DataTableProps } from "./table";
export type { AgentCellState, AgentDescriptor } from "./types";
