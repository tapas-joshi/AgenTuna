import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";
import { ComponentGallery } from "./gallery";
import { ITEM_STATUSES, STATUS_META } from "./status";

afterEach(cleanup);

/**
 * The gallery is the "window" the story asks for: one page that renders every
 * widget variant and state. Rendering it here also proves each state exists.
 */
describe("component gallery", () => {
  test("renders every widget group", () => {
    render(<ComponentGallery />);
    for (const title of [
      "Button",
      "Badge",
      "Chip",
      "Toggle",
      "Checkbox",
      "Input",
      "Segmented control",
      "Radio card",
      "Status dot",
      "Table",
    ]) {
      expect(screen.getByRole("heading", { level: 2, name: title })).toBeTruthy();
    }
  });

  test("shows all four button variants plus disabled and destructive samples", () => {
    render(<ComponentGallery />);
    const buttons = screen.getAllByRole("button");
    const variants = new Set(buttons.map((button) => button.dataset["variant"]).filter(Boolean));

    expect(variants).toEqual(new Set(["primary", "secondary", "ghost", "destructive"]));
    expect(buttons.some((button) => button.hasAttribute("disabled"))).toBe(true);
    expect(screen.getByRole("button", { name: /Remove/ }).className).toContain("text-danger");
  });

  test("renders every item state as both a badge and a status dot", () => {
    render(<ComponentGallery />);
    for (const status of ITEM_STATUSES) {
      const badge = document.querySelector<HTMLElement>(
        `[data-slot="badge"][data-status="${status}"]`,
      );
      const dot = document.querySelector<HTMLElement>(
        `[data-slot="status-dot"][data-status="${status}"]`,
      );

      expect(badge?.textContent).toBe(STATUS_META[status].label);
      expect(dot?.textContent).toBe(STATUS_META[status].label);
      expect(badge?.querySelector(`[data-glyph="${status}"]`)).not.toBeNull();
      expect(document.querySelectorAll(`[data-dot="${status}"]`).length).toBeGreaterThanOrEqual(1);
    }
  });

  test("renders form controls in checked, mixed, disabled and error states", () => {
    render(<ComponentGallery />);
    const checkboxes = screen.getAllByRole("checkbox");

    expect(checkboxes.some((box) => box.getAttribute("aria-checked") === "mixed")).toBe(true);
    expect(checkboxes.some((box) => box.hasAttribute("disabled"))).toBe(true);
    expect(screen.getByText("Use ${VAR} references, never a secret value.")).toBeTruthy();
    expect(screen.getAllByRole("switch").some((toggle) => toggle.hasAttribute("disabled"))).toBe(true);
  });

  test("renders the segmented control and radio cards as selectable groups", () => {
    render(<ComponentGallery />);
    const scopeGroup = screen.getByRole("radiogroup", { name: "Scope" });
    expect(within(scopeGroup).getAllByRole("radio")).toHaveLength(3);

    const radioGroup = screen.getByRole("radiogroup", { name: "Migration placement" });
    expect(within(radioGroup).getAllByRole("radio")).toHaveLength(3);
  });
});
