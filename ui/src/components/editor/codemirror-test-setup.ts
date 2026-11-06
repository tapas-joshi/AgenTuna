/**
 * jsdom fills in no layout geometry, and CodeMirror measures text ranges while
 * mounting. These stubs make those reads return empty geometry instead of
 * throwing; they do not fake layout, so tests assert structure and state, not
 * pixel positions.
 */
if (typeof Range !== "undefined") {
  Range.prototype.getBoundingClientRect = () => new DOMRect(0, 0, 0, 0);
  Range.prototype.getClientRects = () =>
    ({
      length: 0,
      item: () => null,
      [Symbol.iterator]: [][Symbol.iterator],
    }) as unknown as DOMRectList;
}

if (typeof globalThis.ResizeObserver === "undefined") {
  class ResizeObserverStub {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  }
  globalThis.ResizeObserver = ResizeObserverStub as unknown as typeof ResizeObserver;
}
