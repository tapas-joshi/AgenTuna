import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { ToastProvider } from "../toasts";
import { NewRuleDialog } from "./NewRuleDialog";

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

const defaultOptions = {
  global_writable: false,
  global_reason: "Global store is managed centrally and is read-only.",
  local_available: false,
  local_reason: "Project is not inside a Git work tree.",
  has_project: true,
};

function renderDialog(props: Partial<Parameters<typeof NewRuleDialog>[0]> = {}) {
  const onClose = props.onClose ?? vi.fn();
  const onCreated = props.onCreated ?? vi.fn();
  const rendered = render(
    <ToastProvider>
      <NewRuleDialog
        open={props.open ?? true}
        project={props.project ?? "project-1"}
        onClose={onClose}
        onCreated={onCreated}
      />
    </ToastProvider>,
  );
  return { ...rendered, onClose, onCreated };
}

describe("NewRuleDialog (TA-057)", () => {
  test("changing type or scope updates the target path preview immediately", async () => {
    ipc.reply = (command, args) => {
      if (command === "instruction_create_options") {
        return Promise.resolve({
          ...defaultOptions,
          global_writable: true,
          local_available: true,
        });
      }
      if (command === "instruction_create_preview") {
        const { fileType, scope } = (args as { fileType: string; scope: string }) ?? {};
        return Promise.resolve({
          path: `/sandbox/${scope}/${fileType}`,
          exists: false,
          reason: null,
        });
      }
      return Promise.resolve(undefined);
    };

    renderDialog();

    expect(await screen.findByText("/sandbox/project/CLAUDE.md")).toBeTruthy();

    // Change file type to AGENTS.md
    fireEvent.click(screen.getByRole("radio", { name: /AGENTS\.md/ }));
    await waitFor(() =>
      expect(
        ipc.calls.some(
          (c) =>
            c.command === "instruction_create_preview" &&
            (c.args as Record<string, unknown>)?.fileType === "AGENTS.md",
        ),
      ).toBe(true),
    );
    expect(await screen.findByText("/sandbox/project/AGENTS.md")).toBeTruthy();

    // Change scope to User
    fireEvent.click(screen.getByRole("radio", { name: /User/ }));
    await waitFor(() =>
      expect(
        ipc.calls.some(
          (c) =>
            c.command === "instruction_create_preview" &&
            (c.args as Record<string, unknown>)?.scope === "user",
        ),
      ).toBe(true),
    );
    expect(await screen.findByText("/sandbox/user/AGENTS.md")).toBeTruthy();
  });

  test("existing path shows 'File already exists' and keeps create disabled; no write occurs", async () => {
    ipc.reply = (command) => {
      if (command === "instruction_create_options") {
        return Promise.resolve(defaultOptions);
      }
      if (command === "instruction_create_preview") {
        return Promise.resolve({
          path: "/sandbox/project/CLAUDE.md",
          exists: true,
          reason: null,
        });
      }
      return Promise.resolve(undefined);
    };

    renderDialog();

    expect(
      await screen.findByText(/File already exists: \/sandbox\/project\/CLAUDE\.md/),
    ).toBeTruthy();

    const submit = screen.getByRole("button", { name: /^Create/ }) as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
    expect(ipc.calls.some((c) => c.command === "instruction_create")).toBe(false);
  });

  test("successful create closes dialog, emits toast, and calls onCreated", async () => {
    const onCreated = vi.fn();
    const onClose = vi.fn();
    ipc.reply = (command) => {
      if (command === "instruction_create_options") {
        return Promise.resolve(defaultOptions);
      }
      if (command === "instruction_create_preview") {
        return Promise.resolve({
          path: "/sandbox/project/CLAUDE.md",
          exists: false,
          reason: null,
        });
      }
      if (command === "instruction_create") {
        return Promise.resolve("/sandbox/project/CLAUDE.md");
      }
      return Promise.resolve(undefined);
    };

    renderDialog({ onCreated, onClose });

    const submit = (await screen.findByRole("button", { name: /^Create/ })) as HTMLButtonElement;
    await waitFor(() => expect(submit.disabled).toBe(false));
    fireEvent.click(submit);

    await waitFor(() => expect(onCreated).toHaveBeenCalledWith("/sandbox/project/CLAUDE.md", "CLAUDE.md"));
    expect(onClose).toHaveBeenCalled();
    expect(ipc.calls.some((c) => c.command === "instruction_create")).toBe(true);
  });

  test("global disabled with read-only reason, and local disabled when not a git repo", async () => {
    ipc.reply = (command) => {
      if (command === "instruction_create_options") {
        return Promise.resolve({
          global_writable: false,
          global_reason: "Global is read-only",
          local_available: false,
          local_reason: "Not a git work tree",
          has_project: true,
        });
      }
      if (command === "instruction_create_preview") {
        return Promise.resolve({
          path: "/sandbox/project/CLAUDE.md",
          exists: false,
          reason: null,
        });
      }
      return Promise.resolve(undefined);
    };

    renderDialog();

    await screen.findByText("/sandbox/project/CLAUDE.md");
    const globalRadio = screen.getByRole("radio", { name: /Global/ }) as HTMLButtonElement;
    expect(globalRadio.disabled || globalRadio.getAttribute("aria-disabled") === "true").toBeTruthy();

    const localRadio = screen.getByRole("radio", { name: /Local/ }) as HTMLButtonElement;
    expect(localRadio.disabled || localRadio.getAttribute("aria-disabled") === "true").toBeTruthy();
  });

  test("Cancel writes nothing and calls onClose", async () => {
    const onClose = vi.fn();
    ipc.reply = (command) => {
      if (command === "instruction_create_options") return Promise.resolve(defaultOptions);
      if (command === "instruction_create_preview") {
        return Promise.resolve({
          path: "/sandbox/project/CLAUDE.md",
          exists: false,
          reason: null,
        });
      }
      return Promise.resolve(undefined);
    };

    renderDialog({ onClose });

    await screen.findByText("/sandbox/project/CLAUDE.md");

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalled();
    expect(ipc.calls.some((c) => c.command === "instruction_create")).toBe(false);
  });
});
