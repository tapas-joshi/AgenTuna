import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import type { NetworkPolicyView } from "../../generated/NetworkPolicyView";
import { NetworkActionGate, NetworkActionRegistry } from "./NetworkActionGate";

const policy = (overrides: Partial<NetworkPolicyView> = {}): NetworkPolicyView => ({
  offline: false, marketplace_enabled: false, background_health: "local", updates_opt_in: false,
  statement: "No automatic network calls.", allowed_hosts: [], events: [], capacity: 100,
  actions: [
    { action: "git_fetch", label: "Fetch repository", trigger: "user", explicit: true, state: "confirmation_required", reason: null },
    { action: "marketplace_search", label: "Search marketplace", trigger: "user", explicit: true, state: "blocked", reason: "Marketplace access is off." },
  ], ...overrides,
});

describe("network action gate", () => {
  test("renders exactly the registry set with policy reason and offline label", () => {
    const value = policy({ offline: true });
    render(<NetworkActionRegistry policy={value} onRun={vi.fn()} onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect([...document.querySelectorAll("[data-network-action]")].map(node => node.getAttribute("data-network-action"))).toEqual(value.actions.map(row => row.action));
    expect(screen.getAllByText("Offline").length).toBe(2);
    expect(screen.getByText("Marketplace access is off.")).toBeTruthy();
  });

  test("confirmation names destination and only confirm records/runs the approved action", () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    const { rerender } = render(<NetworkActionGate policy={policy()} action="git_fetch" host="example.test" onRun={vi.fn()} onConfirm={onConfirm} onCancel={onCancel} />);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(screen.getByRole("alertdialog").textContent).toContain("example.test");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onConfirm).not.toHaveBeenCalled();
    rerender(<NetworkActionGate policy={policy()} action="git_fetch" host="example.test" onRun={vi.fn()} onConfirm={onConfirm} onCancel={onCancel} />);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm connection" }));
    expect(onConfirm).toHaveBeenCalledWith("git_fetch", "example.test");
  });
});
