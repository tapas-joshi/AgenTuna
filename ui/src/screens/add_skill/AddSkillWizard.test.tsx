import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { AddSkillWizard } from "./AddSkillWizard";

const ipc = vi.hoisted(() => ({ calls: [] as { command: string; args: unknown }[], reply: (_command: string, _args: unknown): Promise<unknown> => Promise.resolve(undefined) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string, args?: unknown) => { ipc.calls.push({ command, args }); return ipc.reply(command, args); } }));

type StubState = { step: string; source: string | null; details: Record<string, string | null>; name: string; scope: string | null; agents: string[]; completed: boolean };
const initial = (): StubState => ({ step: "source", source: null, details: { repo_url: "", repo_reference: "", library_selection: null, marketplace_id: "", folder_path: "", zip_path: "", template: null, fetched_repo_selection: null }, name: "", scope: null, agents: [], completed: false });

afterEach(() => { cleanup(); ipc.calls.length = 0; });

describe("AddSkillWizard", () => {
  test("gates invalid navigation and preserves source detail when backing up", async () => {
    ipc.reply = async (_command, args) => {
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = request.wizard ?? initial();
      const event = request.event;
      if (event.type === "select_source") state.source = String(event.payload);
      if (event.type === "set_name") state.name = String(event.payload);
      if (event.type === "set_details") state.details = event.payload as StubState["details"];
      if (event.type === "next" && state.step === "source" && state.source !== null) state.step = "details";
      if (event.type === "back" && state.step === "details") state.step = "source";
      return [structuredClone(state), []];
    };
    render(<AddSkillWizard onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByText("Where is the skill coming from?")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Git repository/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByLabelText("Repository URL")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Repository URL"), { target: { value: "https://example.invalid/skills.git" } });
    fireEvent.change(screen.getByLabelText("Skill name"), { target: { value: "Example" } });
    await waitFor(() => expect((screen.getByRole("button", { name: "Back" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(await screen.findByText("Where is the skill coming from?")).toBeTruthy();
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    const repoInput = await screen.findByLabelText("Repository URL") as HTMLInputElement;
    expect(repoInput.value).toBe("https://example.invalid/skills.git");
    expect((screen.getByLabelText("Skill name") as HTMLInputElement).value).toBe("Example");
    expect(screen.getByPlaceholderText("Repository subpath")).toBeTruthy();
    expect(ipc.calls.some(({ command }) => command === "skill_wizard_transition")).toBe(true);
  });

  test("clean cancel closes immediately", async () => {
    const onClose = vi.fn();
    ipc.reply = async (_command, args) => {
      const request = args as { wizard: StubState | null; event: { type: string } };
      return [request.wizard ?? initial(), [{ type: "discard_draft" }]];
    };
    render(<AddSkillWizard onClose={onClose} />);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  test("dirty cancel waits for explicit discard confirmation", async () => {
    const onClose = vi.fn();
    ipc.reply = async (_command, args) => {
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = request.wizard ?? initial();
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "cancel" && state.source !== null) return [structuredClone(state), [{ type: "confirm_discard" }]];
      if (request.event.type === "confirm_discard") return [structuredClone(state), [{ type: "discard_draft" }]];
      return [structuredClone(state), []];
    };
    render(<AddSkillWizard onClose={onClose} />);
    fireEvent.click(screen.getByRole("button", { name: /Git repository/ }));
    await waitFor(() => expect(screen.getByRole("button", { name: /Git repository/ }).getAttribute("aria-pressed")).toBe("true"));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(await screen.findByRole("alertdialog")).toBeTruthy();
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });
});
