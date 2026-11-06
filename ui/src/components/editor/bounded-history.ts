import { EditorSelection, StateEffect, StateField } from "@codemirror/state";
import type { Extension, Text, Transaction } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";

/**
 * Maximum number of undo steps kept in memory.
 *
 * CodeMirror's own history grows without a hard cap (`minDepth` is a floor,
 * not a ceiling), so this editor stores snapshots in a bounded ring instead.
 * Text values share structure in CodeMirror, so a snapshot costs little beyond
 * its change chain.
 */
export const MAX_HISTORY_DEPTH = 100;

/** Adjacent typing ticks within this window collapse into one undo step. */
export const HISTORY_GROUP_DELAY_MS = 500;

interface Snapshot {
  readonly doc: Text;
  readonly selection: EditorSelection;
}

interface BoundedHistoryState {
  readonly entries: readonly Snapshot[];
  /** Index of the snapshot matching the current document. */
  readonly index: number;
  /** Timestamp of the last recorded change, used to group typing. */
  readonly lastChangeAt: number;
}

const historyEffect = StateEffect.define<"undo" | "redo">();

function snapshotOf(tr: Transaction): Snapshot {
  return { doc: tr.newDoc, selection: tr.newSelection };
}

/**
 * A bounded undo/redo history.
 *
 * Typing and deleting group within {@link HISTORY_GROUP_DELAY_MS}; pastes and
 * programmatic replacements each become their own step, so undoing typed text
 * does not require one keystroke per undo. A new change after an undo discards
 * the redo branch. The oldest snapshot is dropped once the bound is exceeded.
 */
export const boundedHistory = StateField.define<BoundedHistoryState>({
  create(state) {
    return {
      entries: [{ doc: state.doc, selection: state.selection }],
      index: 0,
      lastChangeAt: 0,
    };
  },
  update(value, tr) {
    for (const effect of tr.effects) {
      if (!effect.is(historyEffect)) {
        continue;
      }
      if (effect.value === "undo" && value.index > 0) {
        return { entries: value.entries, index: value.index - 1, lastChangeAt: 0 };
      }
      if (effect.value === "redo" && value.index < value.entries.length - 1) {
        return { entries: value.entries, index: value.index + 1, lastChangeAt: 0 };
      }
      return value;
    }
    if (!tr.docChanged) {
      return value;
    }
    const snapshot = snapshotOf(tr);
    const typing = tr.isUserEvent("input.type") || tr.isUserEvent("delete");
    const now = Date.now();
    const canGroup =
      typing &&
      value.index > 0 &&
      value.index === value.entries.length - 1 &&
      now - value.lastChangeAt <= HISTORY_GROUP_DELAY_MS;
    if (canGroup) {
      const entries = value.entries.slice();
      entries[value.index] = snapshot;
      return { entries, index: value.index, lastChangeAt: now };
    }
    const entries = value.entries.slice(0, value.index + 1);
    entries.push(snapshot);
    if (entries.length > MAX_HISTORY_DEPTH + 1) {
      entries.splice(0, entries.length - (MAX_HISTORY_DEPTH + 1));
    }
    return { entries, index: entries.length - 1, lastChangeAt: now };
  },
});

/** Undoes one entry; returns `false` when the history has no earlier state. */
export function undoHistory(view: EditorView): boolean {
  if (view.state.readOnly) {
    return false;
  }
  const history = view.state.field(boundedHistory);
  if (history.index === 0) {
    return false;
  }
  const target = history.entries[history.index - 1];
  if (target === undefined) {
    return false;
  }
  view.dispatch({
    changes: { from: 0, to: view.state.doc.length, insert: target.doc },
    selection: target.selection,
    effects: historyEffect.of("undo"),
    scrollIntoView: true,
  });
  return true;
}

/** Redoes one entry; returns `false` when there is no redo branch. */
export function redoHistory(view: EditorView): boolean {
  if (view.state.readOnly) {
    return false;
  }
  const history = view.state.field(boundedHistory);
  if (history.index >= history.entries.length - 1) {
    return false;
  }
  const target = history.entries[history.index + 1];
  if (target === undefined) {
    return false;
  }
  view.dispatch({
    changes: { from: 0, to: view.state.doc.length, insert: target.doc },
    selection: target.selection,
    effects: historyEffect.of("redo"),
    scrollIntoView: true,
  });
  return true;
}

/**
 * Undo/redo key bindings.
 *
 * Explicit `Meta` and `Ctrl` entries sit beside `Mod` so macOS Cmd+Z and
 * non-macOS Ctrl+Z both work regardless of CodeMirror's platform detection,
 * which is not testable from the browser bundle.
 */
export function boundedHistoryExtension(): Extension {
  return [
    boundedHistory,
    keymap.of([
      { key: "Mod-z", run: undoHistory },
      { key: "Meta-z", run: undoHistory },
      { key: "Ctrl-z", run: undoHistory },
      { key: "Mod-Shift-z", run: redoHistory },
      { key: "Meta-Shift-z", run: redoHistory },
      { key: "Ctrl-Shift-z", run: redoHistory },
      { key: "Mod-y", run: redoHistory },
      { key: "Meta-y", run: redoHistory },
      { key: "Ctrl-y", run: redoHistory },
    ]),
  ];
}
