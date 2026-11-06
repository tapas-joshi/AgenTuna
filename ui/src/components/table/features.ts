/**
 * Feature set shared by every AgenTuna table.
 *
 * TanStack Table v9 is headless and feature-registered: a table only exposes
 * the APIs whose features were added to this object. Registering them once at
 * module scope keeps the object reference stable, which is what the table
 * memoisation relies on (a new `features` object invalidates every row model).
 *
 * Sorting is registered together with the sorted row model so a header click
 * re-orders rows. That row model tie-breaks equal values by the row's original
 * index, so sorting is stable and a selection keyed by row id survives it.
 */
import { columnSizingFeature, createSortedRowModel, rowSortingFeature, tableFeatures } from "@tanstack/react-table";

/**
 * Column metadata the table renderer understands.
 *
 * Declared through the `columnMeta` slot so `columnDef.meta` is typed in the
 * screens that build their own columns.
 */
export interface DataTableColumnMeta {
  /** Stretch this column to fill the leftover width instead of its `size`. */
  readonly grow?: boolean;
  /** Horizontal alignment of the cell content. Defaults to `start`. */
  readonly align?: "start" | "center" | "end";
}

/** The exact feature object type used by {@link DataTable}. */
export const dataTableFeatures = tableFeatures({
  rowSortingFeature,
  columnSizingFeature,
  sortedRowModel: createSortedRowModel(),
  columnMeta: {} as DataTableColumnMeta,
});

/** Type-level view of {@link dataTableFeatures}, for `ColumnDef` generics. */
export type DataTableFeatures = typeof dataTableFeatures;
