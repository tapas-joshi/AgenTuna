import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { WhyTraceView } from "../../generated/WhyTraceView";
import { ToastProvider } from "../toasts";
import { WhyTraceDialog } from "./WhyTraceDialog";

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

const sampleTrace: WhyTraceView = {
  trace: {
    item_id: "skill-1",
    active_scope: "user",
    agent: "claude_code",
    verdict: {
      on: true,
      decisive_step: 1, // User is winner
    },
    steps: [
      {
        scope: "global",
        status: "Absent",
        location: null,
        reason: "No global definition exists.",
      },
      {
        scope: "user",
        status: "Winning",
        location: {
          path: "/sandbox/home/.claude/skills/review",
          byte_span: null,
          line: 12,
          rendering_path: null,
        },
        reason: "Active definition at user scope.",
      },
      {
        scope: "project",
        status: "Shadowed",
        location: {
          path: "/sandbox/project/.claude/skills/review",
          byte_span: null,
          line: null,
          rendering_path: null,
        },
        reason: "Shadowed by user definition.",
      },
      {
        scope: "local",
        status: { Unreadable: { os_error: "EACCES: permission denied" } },
        location: {
          path: "/sandbox/project/.claude/skills/review-local",
          byte_span: null,
          line: null,
          rendering_path: null,
        },
        reason: "Cannot read local file.",
      },
    ],
  },
  load_diagnostics: [],
};

const offTrace: WhyTraceView = {
  trace: {
    item_id: "skill-2",
    active_scope: "project",
    agent: "claude_code",
    verdict: {
      on: false,
      decisive_step: 2,
    },
    steps: [
      {
        scope: "global",
        status: "Absent",
        location: null,
        reason: "Not defined globally.",
      },
      {
        scope: "user",
        status: "Absent",
        location: null,
        reason: "Not defined in user scope.",
      },
      {
        scope: "project",
        status: "Disabled",
        location: {
          path: "/sandbox/project/.claude/skills/review",
          byte_span: null,
          line: null,
          rendering_path: null,
        },
        reason: "Disabled in project configuration.",
      },
      {
        scope: "local",
        status: "Absent",
        location: null,
        reason: "Not defined locally.",
      },
    ],
  },
  load_diagnostics: [],
};

function renderDialog(props: Partial<Parameters<typeof WhyTraceDialog>[0]> = {}) {
  const onClose = props.onClose ?? vi.fn();
  const rendered = render(
    <ToastProvider>
      <WhyTraceDialog
        open={props.open ?? true}
        itemId={props.itemId ?? "skill-1"}
        scope={props.scope ?? "user"}
        agent={props.agent ?? "claude_code"}
        onClose={onClose}
        onOpenEffective={props.onOpenEffective}
      />
    </ToastProvider>,
  );
  return { ...rendered, onClose };
}

describe("WhyTraceDialog (TA-055)", () => {
  test("renders 4 steps showing Absent (dimmed) and winning step visually distinct", async () => {
    ipc.reply = (command) => {
      if (command === "why_trace") return Promise.resolve(sampleTrace);
      return Promise.resolve(undefined);
    };

    renderDialog();

    expect(await screen.findByText("Global")).toBeTruthy();
    expect(screen.getByText("User")).toBeTruthy();
    expect(screen.getByText("Project")).toBeTruthy();
    expect(screen.getByText("Local")).toBeTruthy();

    // Absent step rendered
    expect(screen.getByText("Absent")).toBeTruthy();

    // Winning step marked
    expect(screen.getByText("Winner")).toBeTruthy();
  });

  test("verdict header displays 'On, from <scope>' when enabled and 'Off, <reason>' when disabled", async () => {
    ipc.reply = (command) => {
      if (command === "why_trace") return Promise.resolve(sampleTrace);
      return Promise.resolve(undefined);
    };

    renderDialog();
    expect((await screen.findAllByText("On, from user")).length).toBeGreaterThan(0);

    // Now test Off verdict
    cleanup();
    ipc.reply = (command) => {
      if (command === "why_trace") return Promise.resolve(offTrace);
      return Promise.resolve(undefined);
    };

    renderDialog();
    expect((await screen.findAllByText("Off, Disabled in project configuration.")).length).toBeGreaterThan(0);
  });

  test("unreadable step displays an error chip with the OS message", async () => {
    ipc.reply = (command) => {
      if (command === "why_trace") return Promise.resolve(sampleTrace);
      return Promise.resolve(undefined);
    };

    renderDialog();

    expect(await screen.findByText("Unreadable")).toBeTruthy();
    expect(screen.getByText("EACCES: permission denied")).toBeTruthy();
  });

  test("Open file launches reveal_item and Copy path writes to clipboard", async () => {
    ipc.reply = (command) => {
      if (command === "why_trace") return Promise.resolve(sampleTrace);
      if (command === "reveal_item") return Promise.resolve(undefined);
      return Promise.resolve(undefined);
    };

    const clipboardWriteText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, {
      clipboard: { writeText: clipboardWriteText },
    });

    renderDialog();

    const openFile = await screen.findByRole("button", { name: "Open file" });
    fireEvent.click(openFile);
    await waitFor(() =>
      expect(ipc.calls.some((c) => c.command === "reveal_item")).toBe(true),
    );

    const copyPath = screen.getByRole("button", { name: "Copy path" });
    fireEvent.click(copyPath);
    await waitFor(() =>
      expect(clipboardWriteText).toHaveBeenCalledWith(
        "/sandbox/project/.claude/skills/review-local",
      ),
    );
  });

  test("Close button dismisses dialog", async () => {
    const onClose = vi.fn();
    ipc.reply = (command) => {
      if (command === "why_trace") return Promise.resolve(sampleTrace);
      return Promise.resolve(undefined);
    };

    renderDialog({ onClose });

    const closeButton = await screen.findByRole("button", { name: "Close" });
    fireEvent.click(closeButton);
    expect(onClose).toHaveBeenCalled();
  });
});
