import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { AddSkillWizard } from "./AddSkillWizard";

const ipc = vi.hoisted(() => ({ calls: [] as { command: string; args: unknown }[], reply: (_command: string, _args: unknown): Promise<unknown> => Promise.resolve(undefined) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string, args?: unknown) => { ipc.calls.push({ command, args }); return ipc.reply(command, args); } }));

type StubState = { step: string; source: string | null; details: Record<string, string | null>; name: string; scope: string | null; agents: string[]; completed: boolean };
const initial = (): StubState => ({ step: "source", source: null, details: { repo_url: "", repo_reference: "", library_selection: null, marketplace_id: "", folder_path: "", zip_path: "", description: "", template: null, fetched_repo_selection: null }, name: "", scope: null, agents: [], completed: false });

afterEach(() => { cleanup(); ipc.calls.length = 0; });

  test("shows the policy-controlled marketplace unavailable state without searching", async () => {
    ipc.reply = async (command, args) => {
      if (command === "network_policy") return { offline: false, marketplace_enabled: false, statement: "Network actions disabled" };
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = structuredClone(request.wizard ?? initial());
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "next" && state.step === "source") state.step = "details";
      return [state, []];
    };
    render(<AddSkillWizard onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: "Marketplace" }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByText(/network policy disables it/i)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Search" })).toBeNull();
    expect(ipc.calls.some(({ command }) => command === "skill_marketplace_search")).toBe(false);
  });
describe("AddSkillWizard", () => {
  test("chooses local folder paths through the native dialog command", async () => {
    ipc.reply = async (command, args) => {
      if (command === "skill_import_choose_folder") return "/sandbox/library/skill";
      if (command === "skill_import_candidates") return { candidates: [{ id: "", name: "Folder Skill", description: "A local candidate.", markdown: "---\nname: folder-skill\n---\n", error: null }], error: null };
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = structuredClone(request.wizard ?? initial());
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "set_name") state.name = String(request.event.payload);
      if (request.event.type === "next" && state.step === "source") state.step = "details";
      if (request.event.type === "set_details") state.details = request.event.payload as StubState["details"];
      return [state, []];
    };
    render(<AddSkillWizard onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: "Local folder" }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByLabelText("Folder path")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Browse…" }));
    await waitFor(() => expect((screen.getByLabelText("Folder path") as HTMLInputElement).value).toBe("/sandbox/library/skill"));
    fireEvent.click(screen.getByRole("button", { name: "Scan folder" }));
    expect(await screen.findByRole("radio", { name: /Folder Skill/ })).toBeTruthy();
    fireEvent.click(screen.getByRole("radio", { name: /Folder Skill/ }));
    await waitFor(() => expect((screen.getByRole("radio", { name: /Folder Skill/ }) as HTMLInputElement).checked).toBe(true));
    expect(await screen.findByLabelText("Folder Skill SKILL.md preview")).toBeTruthy();
    expect(ipc.calls.some(({ command }) => command === "skill_import_choose_folder")).toBe(true);
  });
  test("renders canonical new-skill template previews and gates the description limit", async () => {
    ipc.reply = async (command, args) => {
      if (command === "skill_template_preview") return { slug: "daily-review", markdown: "# Daily Review", name_error: null, description_error: null };
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = request.wizard ?? initial();
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "next" && state.step === "source") state.step = "details";
      if (request.event.type === "set_name") state.name = String(request.event.payload);
      if (request.event.type === "set_details") state.details = request.event.payload as StubState["details"];
      return [structuredClone(state), []];
    };
    render(<AddSkillWizard onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: /Write a new skill/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.click(await screen.findByRole("radio", { name: /Blank/ }));
    await waitFor(() => expect(screen.getByRole("radio", { name: /Blank/ }).getAttribute("aria-checked")).toBe("true"));
    fireEvent.click(screen.getByRole("radio", { name: /Checklist/ }));
    await waitFor(() => expect(screen.getByRole("radio", { name: /Checklist/ }).getAttribute("aria-checked")).toBe("true"));
    fireEvent.change(screen.getByLabelText("Skill name"), { target: { value: "Daily Review" } });
    fireEvent.change(screen.getByLabelText("Use when description"), { target: { value: "Review daily changes" } });
    await waitFor(() => expect((screen.getByLabelText("Skill name") as HTMLInputElement).value).toBe("Daily Review"));
    await waitFor(() => expect((screen.getByLabelText("Use when description") as HTMLTextAreaElement).value).toBe("Review daily changes"));
    await waitFor(() => expect(ipc.calls.some(({ command }) => command === "skill_template_preview")).toBe(true));
    expect(await screen.findByText("# Daily Review")).toBeTruthy();
    expect(await screen.findByText("Slug: daily-review")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Use when description"), { target: { value: "x".repeat(1024) } });
    expect(await screen.findByText("1024/1024")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(true);
  });
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
    expect(await screen.findByText("Where does the skill come from?")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Import from git/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByLabelText("Repository URL")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Repository URL"), { target: { value: "https://example.invalid/skills.git" } });
    fireEvent.change(screen.getByLabelText("Skill name"), { target: { value: "Example" } });
    await waitFor(() => expect((screen.getByRole("button", { name: "Back" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(await screen.findByText("Where does the skill come from?")).toBeTruthy();
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
    fireEvent.click(screen.getByRole("button", { name: /Import from git/ }));
    await waitFor(() => expect(screen.getByRole("button", { name: /Import from git/ }).getAttribute("aria-pressed")).toBe("true"));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(await screen.findByRole("alertdialog")).toBeTruthy();
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  test("shows the three illustrated sources and uses Escape to cancel", async () => {
    const onClose = vi.fn();
    ipc.reply = async (_command, args) => {
      const request = args as { wizard: StubState | null; event: { type: string } };
      return [request.wizard ?? initial(), [{ type: "discard_draft" }]];
    };
    render(<AddSkillWizard onClose={onClose} />);
    expect(screen.getByRole("button", { name: /Write a new skill/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /Import from git/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /From your library/ })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Marketplace" })).toBeTruthy();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  test("Enter advances after a source is selected", async () => {
    ipc.reply = async (_command, args) => {
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = request.wizard ?? initial();
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "next") state.step = "details";
      return [structuredClone(state), []];
    };
    render(<AddSkillWizard onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: /Write a new skill/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.keyDown(window, { key: "Enter" });
    expect(await screen.findByRole("radio", { name: /Blank/ })).toBeTruthy();
  });
  test("keeps Next disabled until source details are valid", async () => {
    ipc.reply = async (_command, args) => {
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = request.wizard ?? initial();
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "next" && state.step === "source") state.step = "details";
      return [structuredClone(state), []];
    };
    render(<AddSkillWizard onClose={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: /Write a new skill/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await waitFor(() => expect(screen.getByRole("radio", { name: /Blank/ })).toBeTruthy());
    expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(true);
  });
  test("previews a typed file plan and shows only the applied outcome", async () => {
    const writtenPath = "/sandbox/user/skills/plan-skill/SKILL.md";
    ipc.reply = async (command, args) => {
      if (command === "skill_template_preview") return { slug: "plan-skill", markdown: "# Plan Skill", name_error: null, description_error: null };
      if (command === "skill_add_preview") {
        return {
          preview_id: "preview-skill-add",
          canonical_dir: "/sandbox/user/skills/plan-skill",
          files: [{ path: writtenPath, package_path: "SKILL.md", diff: null }],
          deployments: [{ agent: "omp", path: "/sandbox/.omp/skills/plan-skill", mode: "symlink" }],
          stage_available: false,
          stage_requested: false,
          requires_overwrite_confirmation: false,
        };
      }
      if (command === "skill_add_apply") {
        return {
          skill_name: "Plan Skill",
          operation_id: "op-skill-add",
          paths_written: [writtenPath],
          deployments: [{ agent: "omp", path: "/sandbox/.omp/skills/plan-skill", mode: "symlink" }],
          staged: false,
          staged_paths: [],
          stage_error: null,
          backup_hint: "Operation op-skill-add is recorded. Backups for replaced files, if any, are stored in /sandbox/support/backups and can be restored through operation history and recovery.",
        };
      }
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = structuredClone(request.wizard ?? initial());
      const effects: unknown[] = [];
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "set_name") state.name = String(request.event.payload);
      if (request.event.type === "set_details") state.details = request.event.payload as StubState["details"];
      if (request.event.type === "set_scope") state.scope = String(request.event.payload);
      if (request.event.type === "set_agents") state.agents = request.event.payload as string[];
      if (request.event.type === "next" && state.step === "source") state.step = "details";
      else if (request.event.type === "next" && state.step === "details") state.step = "scope_agents";
      else if (request.event.type === "next" && state.step === "scope_agents") state.step = "review";
      else if (request.event.type === "next" && state.step === "review") effects.push({ type: "ready_to_import" });
      if (request.event.type === "import_completed") {
        state.step = "done";
        state.completed = true;
      }
      return [state, effects];
    };
    render(<AddSkillWizard onClose={() => undefined} projectId={null} />);
    fireEvent.click(screen.getByRole("button", { name: /Write a new skill/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.click(await screen.findByRole("radio", { name: /Blank/ }));
    await waitFor(() => expect(screen.getByRole("radio", { name: /Blank/ }).getAttribute("aria-checked")).toBe("true"));
    fireEvent.change(screen.getByLabelText("Skill name"), { target: { value: "Plan Skill" } });
    fireEvent.change(screen.getByLabelText("Use when description"), { target: { value: "Use this skill to plan work." } });
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.change(await screen.findByLabelText("Scope"), { target: { value: "user" } });
    fireEvent.click(screen.getByLabelText("omp"));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByText("/sandbox/user/skills/plan-skill")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Import skill" }));
    expect(await screen.findByText(writtenPath)).toBeTruthy();
    expect(screen.getByText("Operation: op-skill-add")).toBeTruthy();
    expect(screen.queryByText("user skill store/Plan Skill/SKILL.md")).toBeNull();
    expect(ipc.calls.some(({ command }) => command === "skill_add_preview")).toBe(true);
    expect(ipc.calls.some(({ command }) => command === "skill_add_apply")).toBe(true);
  });
});

