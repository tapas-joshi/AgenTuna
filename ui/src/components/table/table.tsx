/**
 * The virtualized data table.
 *
 * TanStack Table v9 owns the models (sorting, header groups, cells), TanStack
 * Virtual owns the render window, and this module owns the AgenTuna look and
 * behaviour: grid rows, per-agent cells, selection, keyboard navigation, the
 * row menu slot and the empty/loading states.
 *
 * Layout contract: rows are absolutely positioned inside a spacer of
 * `virtualizer.getTotalSize()`, so only the rows in the viewport (plus
 * `overscan`) exist in the DOM. The rendered count is published as
 * `data-rendered-rows` on the scroll container and the total as
 * `data-total-rows`; the performance test asserts on those counters instead of
 * guessing from a timing measurement.
 */
import { useVirtualizer } from "@tanstack/react-virtual";
import type { VirtualItem } from "@tanstack/react-virtual";
import {
  FlexRender,
  flexRender,
  useTable,
  type Cell,
  type ColumnDef,
  type RowData,
  type SortingState,
} from "@tanstack/react-table";
import { useEffect, useMemo, useRef, useState } from "react";
import type { JSX, KeyboardEvent, MouseEvent, ReactNode } from "react";
import { Checkbox } from "../checkbox";
import { StateView } from "../states";
import { cn } from "../cn";
import { dataTableFeatures, type DataTableFeatures } from "./features";
import {
  bulkSelectionState,
  countSelected,
  selectionForClick,
  setDisplayedSelected,
  toggleSelection,
  type SelectionMode,
} from "./selection";

/** Default height of one data row, in pixels. */
export const TABLE_ROW_HEIGHT = 64;
/** Height of the sticky header row, in pixels. */
export const TABLE_HEADER_HEIGHT = 36;

const SELECTION_COLUMN_WIDTH = 40;
const ACTIONS_COLUMN_WIDTH = 40;

/** Clicks inside these elements select nothing; they belong to the widget. */
const ROW_INTERACTIVE_SELECTOR =
  "[data-row-interactive],button,a,input,select,textarea,[role='switch'],[role='checkbox'],[role='menu'],[role='menuitem']";

/** Props accepted by {@link DataTable}. */
export interface DataTableProps<TData extends RowData> {
  /** Accessible name for the table region, for example `Skills`. */
  readonly ariaLabel: string;
  /** Rows in display order; the screen filters before passing them in. */
  readonly data: ReadonlyArray<TData>;
  /**
   * Column definitions. Use {@link agentColumns} for the per-agent columns.
   * Pass a stable array (module scope or `useMemo`): a new array invalidates
   * every row model TanStack Table derived.
   */
  readonly columns: ReadonlyArray<ColumnDef<DataTableFeatures, TData>>;
  /** Stable row id; selections and React keys use it, never the index. */
  readonly getRowId: (row: TData) => string;
  /** Human-readable row name used in checkbox labels and row actions. */
  readonly getRowLabel: (row: TData) => string;
  /** `none` hides the checkbox column. Defaults to `multiple`. */
  readonly selectionMode?: SelectionMode;
  /** Controlled selection. Omit to let the table own it. */
  readonly selectedIds?: ReadonlySet<string>;
  /** Reports the whole next selection after a click, checkbox or key press. */
  readonly onSelectionChange?: (ids: ReadonlySet<string>) => void;
  /** Row height in pixels; also the virtualizer estimate. Defaults to 64. */
  readonly rowHeight?: number;
  /**
   * Fixed viewport height in pixels. Omit to fill a parent that constrains the
   * table (`flex-1 min-h-0` or an explicit height).
   */
  readonly viewportHeight?: number;
  /** Rows rendered above and below the viewport. Defaults to 8. */
  readonly overscan?: number;
  /** Renders the TU-021 loading state instead of rows. */
  readonly loading?: boolean;
  /** Title of the default empty state. */
  readonly emptyTitle?: string;
  /** Body copy of the default empty state. */
  readonly emptyBody?: ReactNode;
  /** Primary action of the default empty state. */
  readonly emptyAction?: { readonly label: string; readonly onClick: () => void };
  /** Replaces the default empty state entirely. */
  readonly emptyState?: ReactNode;
  /** Row actions slot, for example a {@link RowMenuTrigger}. */
  readonly rowActions?: (row: TData) => ReactNode;
  /** Tooltip for the whole row, for example the scope that shadows it. */
  readonly rowTooltip?: (row: TData) => string | undefined;
  /** `dimmed` renders shadowed rows at reduced emphasis. */
  readonly rowTone?: (row: TData) => "default" | "dimmed";
  /** Reports a double-click or Enter on a row. */
  readonly onRowActivate?: (row: TData) => void;
  /** Initial sort order. */
  readonly initialSorting?: SortingState;
  /** Extra grid columns for a cell; return 2 to merge two agent columns. */
  readonly getColumnSpan?: (row: TData, columnId: string) => number;
  readonly className?: string;
}

function SortIndicator({ direction }: { readonly direction: false | "asc" | "desc" }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 12 12"
      className={cn("size-12 shrink-0", direction === false ? "opacity-0" : "opacity-100")}
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      focusable="false"
    >
      {direction === "desc" ? <path d="m3 4.5 3 3 3-3" /> : <path d="m3 7.5 3-3 3 3" />}
    </svg>
  );
}

/**
 * Virtualized table with sortable headers, row selection and a row-actions
 * slot. Used by the Skills, MCP Servers and Effective config lists.
 */
export function DataTable<TData extends RowData>({
  ariaLabel,
  data,
  columns,
  getRowId,
  getRowLabel,
  selectionMode = "multiple",
  selectedIds,
  onSelectionChange,
  rowHeight = TABLE_ROW_HEIGHT,
  viewportHeight,
  overscan = 8,
  loading = false,
  emptyTitle = "Nothing to show",
  emptyBody,
  emptyAction,
  emptyState,
  rowActions,
  rowTooltip,
  rowTone,
  onRowActivate,
  initialSorting,
  getColumnSpan,
  className,
}: DataTableProps<TData>): JSX.Element {
  const [internalSelection, setInternalSelection] = useState<ReadonlySet<string>>(() => new Set<string>());
  const selection = selectedIds ?? internalSelection;
  const anchorRef = useRef<string | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const focusRequest = useRef<number | null>(null);
  const [activeIndex, setActiveIndex] = useState(0);

  const table = useTable({
    features: dataTableFeatures,
    data,
    columns,
    getRowId: (original) => getRowId(original),
    initialState: { sorting: initialSorting ?? [] },
  });

  const rows = table.getRowModel().rows;
  const rowIds = useMemo(() => rows.map((row) => row.id), [rows]);
  const leafColumns = table.getAllLeafColumns();

  const rowVirtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => rowHeight,
    overscan,
    getItemKey: (index) => rows[index]?.id ?? index,
    initialRect: viewportHeight === undefined ? undefined : { width: 0, height: viewportHeight },
  });
  const virtualRows = rowVirtualizer.getVirtualItems();
  const totalSize = rowVirtualizer.getTotalSize();

  useEffect(() => {
    setActiveIndex((current) => (current < rows.length ? current : Math.max(0, rows.length - 1)));
  }, [rows.length]);

  useEffect(() => {
    const index = focusRequest.current;
    if (index === null) {
      return;
    }
    const element = scrollRef.current?.querySelector<HTMLElement>(`[data-row-index="${index}"]`);
    if (element !== null && element !== undefined) {
      element.focus({ preventScroll: true });
      focusRequest.current = null;
    }
  });

  const commitSelection = (next: ReadonlySet<string>) => {
    if (selectedIds === undefined) {
      setInternalSelection(next);
    }
    onSelectionChange?.(next);
  };

  const focusRow = (index: number) => {
    if (rows.length === 0) {
      return;
    }
    const clamped = Math.max(0, Math.min(index, rows.length - 1));
    setActiveIndex(clamped);
    focusRequest.current = clamped;
    rowVirtualizer.scrollToIndex(clamped, { align: "auto" });
  };

  const scrollHeight = viewportHeight ?? rowHeight * 10;
  const pageStep = Math.max(1, Math.floor((scrollHeight - TABLE_HEADER_HEIGHT) / rowHeight) - 1);

  const bodyClass = "flex min-h-0 flex-col overflow-hidden rounded-6 border border-border bg-raised";

  if (loading || rows.length === 0) {
    return (
      <section className={cn(bodyClass, className)} aria-label={ariaLabel} aria-busy={loading || undefined}>
        {!loading && emptyState !== undefined ? (
          emptyState
        ) : (
          <StateView
            variant={loading ? "loading" : "empty"}
            title={loading ? "Loading" : emptyTitle}
            body={loading ? undefined : emptyBody}
            actions={loading || emptyAction === undefined ? undefined : [emptyAction]}
            skeletonRows={loading ? 6 : undefined}
            minHeight={viewportHeight}
          />
        )}
      </section>
    );
  }

  const gridTemplate = [
    ...(selectionMode === "none" ? [] : [`${SELECTION_COLUMN_WIDTH}px`]),
    ...leafColumns.map((column) =>
      column.columnDef.meta?.grow === true ? "minmax(0,1fr)" : `${column.getSize()}px`,
    ),
    ...(rowActions === undefined ? [] : [`${ACTIONS_COLUMN_WIDTH}px`]),
  ].join(" ");

  const columnCount =
    leafColumns.length + (selectionMode === "none" ? 0 : 1) + (rowActions === undefined ? 0 : 1);
  const bulkState = bulkSelectionState(rowIds, selection);
  const selectedCount = countSelected(rowIds, selection);
  const headers = table.getHeaderGroups()[0]?.headers ?? [];

  const headerSelection = (
    <div role="columnheader" className="flex items-center justify-center" aria-label="Selection">
      <Checkbox
        aria-label={`Select all ${rows.length} rows`}
        checked={bulkState === "all" ? true : bulkState === "some" ? "indeterminate" : false}
        onCheckedChange={(checked) => {
          commitSelection(setDisplayedSelected(rowIds, selection, checked === true));
        }}
      />
    </div>
  );

  return (
    <section className={cn(bodyClass, className)} aria-label={ariaLabel}>
      <div
        ref={scrollRef}
        role="table"
        aria-label={ariaLabel}
        aria-rowcount={rows.length + 1}
        aria-colcount={columnCount}
        data-total-rows={rows.length}
        data-rendered-rows={virtualRows.length}
        data-selected-rows={selectedCount}
        style={viewportHeight === undefined ? undefined : { height: viewportHeight }}
        className="min-h-0 flex-1 overflow-auto overscroll-contain"
      >
        <div
          role="rowgroup"
          style={{ height: TABLE_HEADER_HEIGHT }}
          className="sticky top-0 z-10 bg-raised"
        >
          <div
            role="row"
            aria-rowindex={1}
            style={{ gridTemplateColumns: gridTemplate, height: TABLE_HEADER_HEIGHT }}
            className="grid items-center border-b border-border px-8 text-11 font-medium tracking-wide text-text-muted uppercase"
          >
            {selectionMode === "none" ? null : headerSelection}
            {headers.map((header) => {
              const sort = header.column.getIsSorted();
              const canSort = header.column.getCanSort();
              const align = header.column.columnDef.meta?.align ?? "start";
              return (
                <div
                  key={header.id}
                  role="columnheader"
                  aria-sort={
                    canSort
                      ? sort === "asc"
                        ? "ascending"
                        : sort === "desc"
                          ? "descending"
                          : "none"
                      : undefined
                  }
                  className={cn("min-w-0 px-4", align === "center" ? "text-center" : undefined)}
                >
                  {header.isPlaceholder ? null : canSort ? (
                    <button
                      type="button"
                      onClick={(event) => header.column.toggleSorting(undefined, event.shiftKey)}
                      className="inline-flex max-w-full items-center gap-4 rounded-3 uppercase text-text-muted hover:text-text focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus"
                    >
                      <span className="truncate">
                        {flexRender(header.column.columnDef.header, header.getContext())}
                      </span>
                      <SortIndicator direction={sort} />
                    </button>
                  ) : (
                    <span className="block truncate">
                      {flexRender(header.column.columnDef.header, header.getContext())}
                    </span>
                  )}
                </div>
              );
            })}
            {rowActions === undefined ? null : (
              <div role="columnheader" aria-label="Row actions" className="px-4" />
            )}
          </div>
        </div>
        <div role="rowgroup" style={{ height: totalSize, position: "relative" }}>
          {virtualRows.map((item: VirtualItem) => {
            const row = rows[item.index];
            if (row === undefined) {
              return null;
            }
            const selected = selection.has(row.id);
            const tone = rowTone?.(row.original) ?? "default";
            const cells = row.getAllCells();
            const renderedCells: Array<{ cell: Cell<DataTableFeatures, TData, unknown>; span: number }> = [];
            for (let index = 0; index < cells.length; index += 1) {
              const cell = cells[index];
              if (cell === undefined) {
                continue;
              }
              const requested = getColumnSpan?.(row.original, cell.column.id) ?? 1;
              const span = Math.max(1, Math.min(requested, cells.length - index));
              renderedCells.push({ cell, span });
              index += span - 1;
            }

            return (
              <div
                key={row.id}
                role="row"
                aria-rowindex={item.index + 2}
                aria-selected={selectionMode === "none" ? undefined : selected}
                data-row-id={row.id}
                data-row-index={item.index}
                data-selected={selected || undefined}
                data-tone={tone}
                tabIndex={item.index === activeIndex ? 0 : -1}
                title={rowTooltip?.(row.original)}
                onClick={(event: MouseEvent<HTMLDivElement>) => {
                  const target = event.target;
                  if (target instanceof Element && target.closest(ROW_INTERACTIVE_SELECTOR) !== null) {
                    return;
                  }
                  if (event.shiftKey) {
                    event.preventDefault();
                  }
                  const result = selectionForClick({
                    order: rowIds,
                    selection,
                    anchor: anchorRef.current,
                    rowId: row.id,
                    mode: selectionMode,
                    modifiers: {
                      metaKey: event.metaKey,
                      ctrlKey: event.ctrlKey,
                      shiftKey: event.shiftKey,
                    },
                  });
                  anchorRef.current = result.anchor;
                  commitSelection(result.selection);
                }}
                onDoubleClick={() => onRowActivate?.(row.original)}
                onFocus={() => setActiveIndex(item.index)}
                onKeyDown={(event: KeyboardEvent<HTMLDivElement>) => {
                  switch (event.key) {
                    case "ArrowDown":
                      event.preventDefault();
                      focusRow(item.index + 1);
                      break;
                    case "ArrowUp":
                      event.preventDefault();
                      focusRow(item.index - 1);
                      break;
                    case "Home":
                      event.preventDefault();
                      focusRow(0);
                      break;
                    case "End":
                      event.preventDefault();
                      focusRow(rows.length - 1);
                      break;
                    case "PageDown":
                      event.preventDefault();
                      focusRow(item.index + pageStep);
                      break;
                    case "PageUp":
                      event.preventDefault();
                      focusRow(item.index - pageStep);
                      break;
                    case " ": {
                      event.preventDefault();
                      if (selectionMode === "none") {
                        break;
                      }
                      anchorRef.current = row.id;
                      commitSelection(toggleSelection(selection, row.id));
                      break;
                    }
                    case "Enter":
                      event.preventDefault();
                      onRowActivate?.(row.original);
                      break;
                    default:
                      break;
                  }
                }}
                style={{
                  gridTemplateColumns: gridTemplate,
                  height: rowHeight,
                  transform: `translateY(${item.start}px)`,
                }}
                className={cn(
                  "absolute top-0 left-0 grid w-full items-center border-b border-border/60 border-l-2 px-8 text-13 transition-colors",
                  selected ? "border-l-accent bg-accent/10" : "border-l-transparent",
                  tone === "dimmed" ? "opacity-60" : "hover:bg-overlay/60",
                  "focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-focus",
                )}
              >
                {selectionMode === "none" ? null : (
                  <div
                    role="cell"
                    className="flex items-center justify-center"
                    data-row-interactive=""
                  >
                    <Checkbox
                      aria-label={`Select ${getRowLabel(row.original)}`}
                      checked={selected}
                      onCheckedChange={(checked) => {
                        const next = new Set(selection);
                        if (checked === true) {
                          next.add(row.id);
                        } else {
                          next.delete(row.id);
                        }
                        anchorRef.current = row.id;
                        commitSelection(next);
                      }}
                    />
                  </div>
                )}
                {renderedCells.map(({ cell, span }) => {
                  const align = cell.column.columnDef.meta?.align ?? "start";
                  return (
                    <div
                      key={cell.id}
                      role="cell"
                      aria-colspan={span > 1 ? span : undefined}
                      style={span > 1 ? { gridColumn: `span ${span}` } : undefined}
                      className={cn(
                        "min-w-0 px-4",
                        align === "center" ? "text-center" : align === "end" ? "text-right" : undefined,
                      )}
                    >
                      <FlexRender cell={cell} />
                    </div>
                  );
                })}
                {rowActions === undefined ? null : (
                  <div
                    role="cell"
                    className="flex items-center justify-center"
                    data-row-interactive=""
                  >
                    {rowActions(row.original)}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    </section>
  );
}
