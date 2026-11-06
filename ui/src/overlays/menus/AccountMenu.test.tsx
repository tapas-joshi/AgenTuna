import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AccountMenu } from "./AccountMenu";

afterEach(cleanup);

describe("AccountMenu", () => {
  it("shows the no-account message, version and routes each menu action", async () => {
    const user = userEvent.setup();
    const onAction = vi.fn();
    render(<AccountMenu version="v0.1.2 · local" onAction={onAction} />);
    await user.click(screen.getByRole("button", { name: "Open account menu" }));
    expect(screen.getByText("No account, no telemetry")).toBeTruthy();
    expect(screen.getByText("Local app · v0.1.2 · local")).toBeTruthy();
    const actions = ["Preferences", "Backups & restore", "Keychain secrets", "Agents on this Mac", "About AgenTuna"];
    for (const action of actions) {
      const item = screen.getByRole("menuitem", { name: action });
      await user.click(item);
      expect(onAction).toHaveBeenLastCalledWith(({ 
        Preferences: "preferences",
        "Backups & restore": "backups",
        "Keychain secrets": "secrets",
        "Agents on this Mac": "agents",
        "About AgenTuna": "about",
      } as const)[action as (typeof actions)[number]]);
      if (action !== "About AgenTuna") await user.click(screen.getByRole("button", { name: "Open account menu" }));
    }
  });

  it("opens with keyboard and closes after choosing an action", async () => {
    const user = userEvent.setup();
    const onAction = vi.fn();
    render(<AccountMenu version="v1 · local" onAction={onAction} />);
    const trigger = screen.getByRole("button", { name: "Open account menu" });
    trigger.focus();
    await user.keyboard("{Enter}");
    expect(screen.getByRole("menu")).toBeTruthy();
    await user.keyboard("{ArrowDown}{Enter}");
    expect(onAction).toHaveBeenCalledWith("backups");
  });
});
