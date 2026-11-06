import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { ToastProvider } from "../toasts";
import { EnableSkillPopover } from "./EnableSkillPopover";

const ipc = vi.hoisted(() => ({
  calls: [] as { command: string; args: unknown }[],
  reply: (_command: string, _args?: unknown): Promise<unknown> => Promise.resolve(undefined),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args?: unknown) => {
    ipc.calls.push({ command, args });
    return ipc.reply(command, args);
  },
}));

afterEach(() => {
  cleanup();
  ipc.calls.length = 0;
});

const defaultAgents = [
  { id: "claude_code", label: "Claude Code", enabled: true },
  { id: "codex", label: "Codex", enabled: false },
  { id: "cursor", label: "Cursor", enabled: false, disabledReason: "Not installed" },
];

function renderPopover(props: Partial<Parameters<typeof EnableSkillPopover>[0]> = {}) {
  const onClose = props.onClose ?? vi.fn();
  const onApplied = props.onApplied ?? vi.fn();
  const rendered = render(
    <ToastProvider>
      <EnableSkillPopover
        itemId={props.itemId ?? "skill-1"}
        scope={props.scope ?? "user"}
        projectId={props.projectId ?? null}
        agents={props.agents ?? defaultAgents}
        onClose={onClose}
        onApplied={onApplied}
      />
    </ToastProvider>,
  );
  return { ...rendered, onClose, onApplied };
}

describe("EnableSkillPopover (TS-038)", () => {
  test("reflects current per-agent state and Apply sends only differing agents", async () => {
    const onApplied = vi.fn();
    const onClose = vi.fn();

    ipc.reply = (command) => {
      if (command === "skill_agent_preview") {
        return Promise.resolve({
          preview_id: "prev-1",
          enabled: true,
          mode: "link",
          paths: ["/sandbox/.codex/skills/skill-1"],
          coupled_agents: [],
          warnings: [],
          no_op: false,
        });
      }
      if (command === "skill_agent_apply") {
        return Promise.resolve({
          operation_id: "op-1",
          enabled: true,
          changed: true,
        });
      }
      return Promise.resolve(undefined);
    };

    renderPopover({ onApplied, onClose });

    // Claude Code checked, Codex unchecked, Cursor disabled
    const claudeCheck = screen.getByRole("checkbox", { name: /Claude Code/ }) as HTMLInputElement;
    const codexCheck = screen.getByRole("checkbox", { name: /Codex/ }) as HTMLInputElement;
    const cursorCheck = screen.getByRole("checkbox", { name: /Cursor/ }) as HTMLInputElement;

    expect(claudeCheck.checked).toBe(true);
    expect(codexCheck.checked).toBe(false);
    expect(cursorCheck.disabled).toBe(true);

    // Toggle Codex only
    fireEvent.click(codexCheck);
    expect(codexCheck.checked).toBe(true);

    const applyBtn = screen.getByRole("button", { name: "Apply" });
    fireEvent.click(applyBtn);

    await waitFor(() => expect(onApplied).toHaveBeenCalled());
    expect(onClose).toHaveBeenCalled();

    // Verify only Codex was sent
    const previewCalls = ipc.calls.filter((c) => c.command === "skill_agent_preview");
    expect(previewCalls.length).toBe(1);
    expect((previewCalls[0]?.args as { request: { agent_id: string } }).request.agent_id).toBe("codex");
  });

  test("NativeConflict keeps popover open with Adopt and Move aside actions and reverts toggle", async () => {
    ipc.reply = (command) => {
      if (command === "skill_agent_preview") {
        return Promise.reject({
          code: "refused_overwrite",
          message: "/sandbox/.codex/skills/demo holds an unmanaged skill folder; adopt it or move it aside first.",
          refusal: {
            path: "/sandbox/.codex/skills/demo",
            kind: "directory",
            existing_hash: null,
            remedies: ["adopt", "move_aside"],
          },
        });
      }
      return Promise.resolve(undefined);
    };

    renderPopover();

    const codexCheck = screen.getByRole("checkbox", { name: "Codex" }) as HTMLInputElement;
    fireEvent.click(codexCheck);

    const applyBtn = screen.getByRole("button", { name: "Apply" });
    fireEvent.click(applyBtn);

    // Alert rendered
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Adopt" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Move aside" })).toBeTruthy();

    // Toggle reverted back to false
    expect(codexCheck.checked).toBe(false);
  });

  test("Escape key calls onClose", async () => {
    const onClose = vi.fn();
    renderPopover({ onClose });

    const dialog = screen.getByRole("dialog");
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });
});
