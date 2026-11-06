import { EditorState } from "@codemirror/state";
import { describe, expect, test } from "vitest";
import { MAX_HISTORY_DEPTH, boundedHistory } from "./bounded-history";

function typed(state: EditorState, insert: string): EditorState {
  return state.update({
    changes: { from: state.doc.length, insert },
    userEvent: "input.type",
  }).state;
}

function pasted(state: EditorState, insert: string): EditorState {
  return state.update({
    changes: { from: state.doc.length, insert },
    userEvent: "input.paste",
  }).state;
}

describe("bounded history", () => {
  test("groups adjacent typing into one undo step", () => {
    let state = EditorState.create({ doc: "", extensions: [boundedHistory] });
    state = typed(state, "a");
    state = typed(state, "b");
    state = typed(state, "c");

    expect(state.doc.toString()).toBe("abc");
    expect(state.field(boundedHistory).entries).toHaveLength(2);
  });

  test("starts a new step after a paste and after a programmatic edit", () => {
    let state = EditorState.create({ doc: "", extensions: [boundedHistory] });
    state = typed(state, "a");
    state = pasted(state, "PASTE");
    state = state.update({ changes: { from: state.doc.length, insert: "-programmatic" } }).state;

    expect(state.field(boundedHistory).entries).toHaveLength(4);
  });

  test("keeps at most a bounded number of snapshots", () => {
    let state = EditorState.create({ doc: "", extensions: [boundedHistory] });
    for (let index = 0; index < MAX_HISTORY_DEPTH + 7; index += 1) {
      state = state.update({ changes: { from: state.doc.length, insert: "x" } }).state;
    }
    const history = state.field(boundedHistory);

    expect(history.entries).toHaveLength(MAX_HISTORY_DEPTH + 1);
    expect(history.index).toBe(MAX_HISTORY_DEPTH);
    expect(history.entries[0]?.doc.toString()).toBe("x".repeat(7));
  });
});
