import { act, renderHook } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import {
  COMPACT_LAYOUT_QUERY,
  compactLayoutMatches,
  useCompactLayout,
  type CompactLayoutWindow,
  type MediaQueryListLike,
} from "./use-compact-layout";

/** A `matchMedia` stub whose query can be flipped and its listener fired. */
function mediaStub(initial: boolean) {
  const listeners = new Set<(event: { matches: boolean }) => void>();
  const state = { matches: initial };
  const query: MediaQueryListLike = {
    get matches() {
      return state.matches;
    },
    addEventListener: (_type, listener) => {
      listeners.add(listener);
    },
    removeEventListener: (_type, listener) => {
      listeners.delete(listener);
    },
  };
  const target: CompactLayoutWindow = {
    matchMedia: (media) => {
      expect(media).toBe(COMPACT_LAYOUT_QUERY);
      return query;
    },
  };
  return {
    target,
    fire: (next: boolean) => {
      state.matches = next;
      for (const listener of listeners) listener({ matches: next });
    },
    listenerCount: () => listeners.size,
  };
}

describe("compact layout breakpoint", () => {
  test("keeps the full column set when the window cannot be measured", () => {
    expect(compactLayoutMatches(undefined)).toBe(false);
    expect(compactLayoutMatches({})).toBe(false);
    const { result } = renderHook(() => useCompactLayout({}));
    expect(result.current).toBe(false);
  });

  test("follows the query across window resizes", () => {
    const stub = mediaStub(true);
    const { result } = renderHook(() => useCompactLayout(stub.target));
    expect(result.current).toBe(true);
    act(() => stub.fire(false));
    expect(result.current).toBe(false);
    expect(stub.listenerCount()).toBe(1);
  });
});
