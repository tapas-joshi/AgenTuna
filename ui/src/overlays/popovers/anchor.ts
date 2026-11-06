/**
 * Popover anchors (TU-004).
 *
 * A popover can anchor to a live element or to a rectangle captured earlier —
 * for example the widget rect of a list row when a click opened the panel, or
 * the caret of a text field. Rectangles become a `virtualRef` anchor, which
 * floating-ui reads exactly like an element without needing a DOM node.
 */

/** A widget rectangle; `DOMRect` satisfies it. */
export interface AnchorRect {
  readonly top: number;
  readonly left: number;
  readonly width: number;
  readonly height: number;
}

/** What a virtual anchor has to provide to floating-ui. */
export interface VirtualAnchor {
  getBoundingClientRect(): DOMRect;
  getClientRects(): DOMRect[];
}

/** What Radix reads from an anchor (`Measurable` in `@radix-ui/react-popper`). */
export interface MeasurableAnchor {
  getBoundingClientRect(): DOMRect;
}

/** Narrows an anchor target to a captured rectangle. */
export function isAnchorRect(target: Element | AnchorRect): target is AnchorRect {
  // A live element measures itself; a captured rectangle carries plain numbers.
  if (typeof (target as Partial<MeasurableAnchor>).getBoundingClientRect === "function") {
    return false;
  }
  const candidate = target as AnchorRect;
  return (
    typeof candidate.top === "number" &&
    typeof candidate.left === "number" &&
    typeof candidate.width === "number" &&
    typeof candidate.height === "number"
  );
}

/**
 * Wraps a captured rectangle in the minimum floating-ui anchor shape.
 *
 * The rect is recomputed on every read so a caller that moves the anchor (row
 * scrolled away, window resized) gets the new coordinates without re-creating
 * the popover.
 */
export function virtualAnchorFromRect(rect: AnchorRect): VirtualAnchor {
  const toDomRect = (): DOMRect =>
    ({
      top: rect.top,
      left: rect.left,
      width: rect.width,
      height: rect.height,
      right: rect.left + rect.width,
      bottom: rect.top + rect.height,
      x: rect.left,
      y: rect.top,
      toJSON: () => ({}),
    }) as DOMRect;

  return {
    getBoundingClientRect: toDomRect,
    getClientRects: () => [toDomRect()],
  };
}
