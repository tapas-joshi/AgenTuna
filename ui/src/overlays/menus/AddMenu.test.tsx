import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AddMenu } from "./AddMenu";

afterEach(cleanup);

describe("AddMenu", () => {
  it("dispatches the selected typed action with the active scope", async () => {
    const onAction = vi.fn();
    render(<AddMenu scope="project" onAction={onAction} />);
    await userEvent.click(screen.getByRole("button", { name: "Add" }));
    await userEvent.click(screen.getByRole("menuitem", { name: /Add MCP server/ }));
    expect(onAction).toHaveBeenCalledWith({ id: "add.server", scope: "project" });
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("shows the reason when scope does not permit adding", async () => {
    render(<AddMenu scope="global" disabledReason="Global scope is locked" onAction={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Add" }));
    expect(screen.getByRole("menuitem", { name: /New skill/ }).getAttribute("aria-disabled")).toBe("true");
  });
});
  it("opens, selects and closes using only the keyboard", async () => {
    const onAction = vi.fn();
    const user = userEvent.setup();
    render(<AddMenu scope="user" onAction={onAction} />);
    const trigger = screen.getByRole("button", { name: "Add" });
    trigger.focus();
    await user.keyboard("{Enter}");
    await user.keyboard("{ArrowDown}");
    await user.keyboard("{Enter}");
    expect(onAction).toHaveBeenCalledWith({ id: "add.server", scope: "user" });
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });
  it("closes on route change and restores focus to Add", async () => {
    const user = userEvent.setup();
    const onAction = vi.fn();
    const props = { scope: "user" as const, onAction, routeKey: "skills" };
    const view = render(<AddMenu {...props} />);
    const trigger = screen.getByRole("button", { name: "Add" });
    await user.click(trigger);
    expect(screen.getByRole("menu")).toBeTruthy();
    view.rerender(<AddMenu {...props} routeKey="mcp-form" />);
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    expect(document.activeElement).toBe(trigger);
    expect(onAction).not.toHaveBeenCalled();
  });
  it("routes the New rule file selection to the dispatcher with active scope", async () => {
    const user = userEvent.setup();
    const onAction = vi.fn();
    render(<AddMenu scope="project" onAction={onAction} />);
    screen.getByRole("button", { name: "Add" }).focus();
    await user.keyboard("{Enter}");
    await user.keyboard("{ArrowDown}");
    await user.keyboard("{ArrowDown}");
    await user.keyboard("{Enter}");
    expect(onAction).toHaveBeenCalledWith({ id: "add.rule", scope: "project" });
    expect(screen.queryByRole("menu")).toBeNull();
  });
  it("disables only actions that need a missing agent and exposes the reason", async () => {
    render(<AddMenu scope="user" disabledReasons={{ "add.skill": "Install an agent first", "add.server": "Install an agent first" }} onAction={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Add" }));
    expect(screen.getByRole("menuitem", { name: /New skill/ }).getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByRole("menuitem", { name: /Add MCP server/ }).getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByRole("menuitem", { name: /New rule file/ }).getAttribute("aria-disabled")).not.toBe("true");
    expect(screen.getAllByText("Unavailable: Install an agent first")).toHaveLength(2);
  });
