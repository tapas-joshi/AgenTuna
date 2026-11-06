import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import { afterEach, expect, test, vi } from "vitest";
import { ScopeSwitcher } from "./ScopeSwitcher";

afterEach(cleanup);

function renderSwitcher(overrides: Partial<ComponentProps<typeof ScopeSwitcher>> = {}) {
  const onScopeSelected = vi.fn();
  render(
    <ScopeSwitcher
      scope="user"
      localEnabled={false}
      hasProject
      onScopeSelected={onScopeSelected}
      {...overrides}
    />,
  );
  return onScopeSelected;
}

test("renders Global, User and Project and hides Local when disabled", () => {
  const onScopeSelected = renderSwitcher();
  expect(within(screen.getByRole("radiogroup", { name: "Scope" })).getAllByRole("radio")).toHaveLength(3);
  expect(screen.queryByRole("radio", { name: "Local (only you, this repo)" })).toBeNull();
  expect(screen.getByRole("radio", { name: "User" }).getAttribute("aria-checked")).toBe("true");
  expect(onScopeSelected).not.toHaveBeenCalled();
});

test("shows Local with the specified label and emits exactly one selection", async () => {
  const user = userEvent.setup();
  const onScopeSelected = renderSwitcher({ localEnabled: true });
  await user.click(screen.getByRole("radio", { name: "Local (only you, this repo)" }));
  expect(onScopeSelected).toHaveBeenCalledExactlyOnceWith("local");
});

test("disables project scopes with an accessible reason when no project is selected", () => {
  renderSwitcher({ hasProject: false, localEnabled: true });
  expect(screen.getByRole("radio", { name: "Project" }).getAttribute("aria-disabled")).toBe("true");
  expect(screen.getByRole("radio", { name: "Local (only you, this repo)" }).getAttribute("aria-describedby")).toBe("scope-project-required");
  expect(screen.getByText("Choose a project first").className).toContain("sr-only");
});

test("renders locked Global without selecting it", async () => {
  const user = userEvent.setup();
  const onScopeSelected = renderSwitcher({ globalLocked: true });
  expect(screen.getByRole("radio", { name: "Global" }).getAttribute("aria-disabled")).toBe("true");
  await user.click(screen.getByRole("radio", { name: "Global" }));
  expect(onScopeSelected).not.toHaveBeenCalled();
});

test("arrow navigation selects one enabled option", async () => {
  const user = userEvent.setup();
  const onScopeSelected = renderSwitcher();
  screen.getByRole("radio", { name: "User" }).focus();
  await user.keyboard("{ArrowRight}");
  expect(document.activeElement).toBe(screen.getByRole("radio", { name: "Project" }));
  expect(onScopeSelected).toHaveBeenCalledExactlyOnceWith("project");
});

test("marks dark and light rendering variants", () => {
  const { rerender } = render(
    <ScopeSwitcher scope="global" localEnabled={false} hasProject onScopeSelected={() => undefined} theme="dark" />,
  );
  const group = screen.getByRole("radiogroup", { name: "Scope" });
  expect(group.getAttribute("data-theme")).toBe("dark");
  rerender(<ScopeSwitcher scope="global" localEnabled={false} hasProject onScopeSelected={() => undefined} theme="light" />);
  expect(group.getAttribute("data-theme")).toBe("light");
});
