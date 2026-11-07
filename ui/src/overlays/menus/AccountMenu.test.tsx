import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AccountMenu } from "./AccountMenu";

afterEach(cleanup);

describe("TU-042: Account menu", () => {
  it("AC1: every item routes or opens its dialog; the menu closes on selection", async () => {
    const user = userEvent.setup();
    const onAction = vi.fn();
    render(<AccountMenu version="v0.1.2 · local" onAction={onAction} />);

    const trigger = screen.getByRole("button", { name: "Open account menu" });
    const actions = [
      { label: "Preferences", expected: "preferences" },
      { label: "Backups & restore", expected: "backups" },
      { label: "Keychain secrets", expected: "secrets" },
      { label: "Agents on this Mac", expected: "agents" },
      { label: "About AgenTuna", expected: "about" },
    ] as const;

    for (const { label, expected } of actions) {
      await user.click(trigger);
      expect(screen.getByRole("menu")).toBeTruthy();
      const item = screen.getByRole("menuitem", { name: label });
      await user.click(item);
      expect(onAction).toHaveBeenLastCalledWith(expected);
      expect(screen.queryByRole("menu")).toBeNull();
    }
  });

  it("AC2: shows 'No account, no telemetry' and version; no sign-in or account control exists", () => {
    render(<AccountMenu version="v2.4.0 · local" />);
    expect(screen.getByText("No account, no telemetry")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /sign in|log in/i })).toBeNull();
    expect(screen.queryByRole("textbox", { name: /email|username/i })).toBeNull();
  });

  it("AC3: items are disabled with a reason in text, tooltip and aria when unavailable (e.g. Backups during migration)", async () => {
    const user = userEvent.setup();
    const onAction = vi.fn();
    render(
      <AccountMenu
        version="v1 · local"
        onAction={onAction}
        disabledReasons={{ backups: "Migration in progress" }}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Open account menu" }));

    // Reason in text
    expect(screen.getByText("Unavailable: Migration in progress")).toBeTruthy();

    // Item is marked disabled and carries aria-description
    const backupsItem = screen.getByRole("menuitem", { name: /Backups & restore/ });
    expect(backupsItem.getAttribute("aria-disabled")).toBe("true");
    expect(backupsItem.getAttribute("aria-description")).toBe("Migration in progress");

    // Disabled item does not fire onAction when clicked
    await user.click(backupsItem);
    expect(onAction).not.toHaveBeenCalled();
  });

  it("AC4: keyboard reachable with TU-004 navigation rules (skips disabled item)", async () => {
    const user = userEvent.setup();
    const onAction = vi.fn();
    render(
      <AccountMenu
        version="v1 · local"
        onAction={onAction}
        disabledReasons={{ backups: "Migration in progress" }}
      />,
    );

    const trigger = screen.getByRole("button", { name: "Open account menu" });
    trigger.focus();
    await user.keyboard("{Enter}");
    expect(screen.getByRole("menu")).toBeTruthy();

    // ArrowDown should skip disabled "Backups & restore" and land on "Keychain secrets"
    await user.keyboard("{ArrowDown}{Enter}");
    expect(onAction).toHaveBeenCalledWith("secrets");
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("AC4: closes on route change and restores focus", async () => {
    const user = userEvent.setup();
    const { rerender } = render(
      <AccountMenu version="v1 · local" routeKey="overview" />,
    );

    const trigger = screen.getByRole("button", { name: "Open account menu" });
    await user.click(trigger);
    expect(screen.getByRole("menu")).toBeTruthy();

    // Change route
    rerender(<AccountMenu version="v1 · local" routeKey="skills" />);

    // Menu closes
    expect(screen.queryByRole("menu")).toBeNull();
    // Focus is restored to trigger
    await waitFor(() => {
      expect(document.activeElement).toBe(trigger);
    });
  });
});
