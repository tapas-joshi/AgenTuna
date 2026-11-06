import { cleanup, render, screen } from "@testing-library/react";
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
