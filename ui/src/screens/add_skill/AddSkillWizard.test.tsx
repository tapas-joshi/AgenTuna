import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { ToastProvider } from "../../overlays/toasts";
import { AddSkillWizard } from "./AddSkillWizard";
import type { ScopeToken } from "../../theme";

const ipc = vi.hoisted(() => ({
  calls: [] as { command: string; args: unknown }[],
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
  reply: (_command: string, _args: unknown): Promise<unknown> => Promise.resolve(undefined),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string, args?: unknown) => { ipc.calls.push({ command, args }); return ipc.reply(command, args); } }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, callback: (event: { payload: unknown }) => void) => {
    ipc.listeners.set(name, callback);
    return () => ipc.listeners.delete(name);
  },
}));

type StubState = { step: string; source: string | null; details: Record<string, unknown>; name: string; scope: string | null; agents: string[]; completed: boolean };
const initial = (): StubState => ({ step: "source", source: null, details: { repo_url: "", repo_reference: "", library_selections: [], library_mode: "copy", marketplace_id: "", folder_path: "", zip_path: "", description: "", template: null, fetched_repo_selections: [], folder_selections: [], zip_selections: [] }, name: "", scope: null, agents: [], completed: false });

function scopeOptionsReply(args: unknown) {
  const { request } = args as { request: { scope: string | null; names: string[] } };
  const scope = request.scope ?? "user";
  const skillsDir = scope === "global"
    ? "/sandbox/global/skills"
    : scope === "project"
      ? "/sandbox/project/.agents/skills"
      : scope === "local"
        ? "/sandbox/project/.agents/skills"
        : "/sandbox/user/skills";
  return {
    scopes: [
      { scope: "global", store_path: "/sandbox/global", available: true, disabled_reason: null, note: "Machine-wide. Requires administrator access.", lock_note: "1 Global item is protected by locks." },
      { scope: "user", store_path: "/sandbox/user", available: true, disabled_reason: null, note: "Your account on this Mac; not committed to a repository.", lock_note: null },
      { scope: "project", store_path: "/sandbox/project/.config-studio", available: true, disabled_reason: null, note: "Shared with people using this repository.", lock_note: null },
      { scope: "local", store_path: "/sandbox/project/.agentuna/local", available: true, disabled_reason: null, note: "Only you in this repo. Local files are not committed.", lock_note: null },
    ],
    agents: [
      { agent: "codex", name: "Codex", installed: true, disabled_reason: null, skills_dir: skillsDir, targets: request.names.map((name) => ({ name, path: `${skillsDir}/${name.toLowerCase().replaceAll(" ", "-")}` })), shared_with: ["omp"] },
      { agent: "omp", name: "omp", installed: true, disabled_reason: null, skills_dir: scope === "project" || scope === "local" ? skillsDir : "/sandbox/.omp/skills", targets: request.names.map((name) => ({ name, path: `${scope === "project" || scope === "local" ? skillsDir : "/sandbox/.omp/skills"}/${name.toLowerCase().replaceAll(" ", "-")}` })), shared_with: scope === "project" || scope === "local" ? ["codex"] : [] },
      { agent: "cursor", name: "Cursor", installed: false, disabled_reason: "This agent is not installed. Set a skills path to include it.", skills_dir: null, targets: [], shared_with: [] },
    ],
    clashes: [],
  };
}

function renderWizard(onClose: () => void, projectId: string | null = null, initialScope?: ScopeToken) {
  return render(<ToastProvider><AddSkillWizard onClose={onClose} projectId={projectId} initialScope={initialScope} /></ToastProvider>);
}

function hasSkillScopeRequestFor(scope: string): boolean {
  return ipc.calls.some(({ command, args }) => {
    if (command !== "skill_scope_options" || typeof args !== "object" || args === null || !("request" in args)) return false;
    const request = args.request;
    return typeof request === "object" && request !== null && "scope" in request && request.scope === scope;
  });
}

afterEach(() => { cleanup(); ipc.calls.length = 0; ipc.listeners.clear(); });

  test("shows the policy-controlled marketplace unavailable state without searching", async () => {
    ipc.reply = async (command, args) => {
      if (command === "network_policy") return { offline: false, marketplace_enabled: false, statement: "Network actions disabled" };
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = structuredClone(request.wizard ?? initial());
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "next" && state.step === "source") state.step = "details";
      return [state, []];
    };
    renderWizard(() => undefined);
    fireEvent.click(screen.getByRole("button", { name: "Marketplace" }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByText(/network policy disables it/i)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Search" })).toBeNull();
    expect(ipc.calls.some(({ command }) => command === "skill_marketplace_search")).toBe(false);
  });
describe("AddSkillWizard", () => {
  test("scans a folder as a cancellable bridge job and previews the returned skill", async () => {
    ipc.reply = async (command, args) => {
      if (command === "skill_import_choose_folder") return "/sandbox/library";
      if (command === "skill_import_local_start") return { job_id: "folder-import-job", import_id: "folder-import-session" };
      if (command === "skill_import_job_result") return { candidates: [{ id: "folder-skill", name: "Folder Skill", description: "A local candidate.", markdown: "---\nname: folder-skill\n---\n", error: null }, { id: "invalid-folder-skill", name: null, description: null, markdown: null, error: "Malformed SKILL.md frontmatter" }], error: null };
      if (command === "skill_import_release") return true;
      if (command === "jobs_cancel") return true;
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = structuredClone(request.wizard ?? initial());
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "next" && state.step === "source") state.step = "details";
      if (request.event.type === "set_details") state.details = request.event.payload as StubState["details"];
      return [state, []];
    };
    renderWizard(() => undefined);
    await waitFor(() => expect(ipc.listeners.has("agentuna:job-finished")).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "Local folder" }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.click(await screen.findByRole("button", { name: "Browse…" }));
    await waitFor(() => expect((screen.getByLabelText("Folder path") as HTMLInputElement).value).toBe("/sandbox/library"));
    fireEvent.click(screen.getByRole("button", { name: "Scan folder" }));
    await screen.findByRole("button", { name: "Cancel import" });
    ipc.listeners.get("agentuna:job-progress")?.({ payload: { id: "folder-import-job", name: "Scan skill folder", fraction: 0.5, label: "Found 2 skill candidates" } });
    expect(await screen.findByText("Found 2 skill candidates")).toBeTruthy();
    ipc.listeners.get("agentuna:job-finished")?.({ payload: { id: "folder-import-job", name: "Scan skill folder", state: "succeeded", error: null } });
    expect(await screen.findByText("2 skills ready to select.")).toBeTruthy();
    const candidate = await screen.findByRole("checkbox", { name: /Folder Skill/ });
    const invalidCandidate = await screen.findByRole("checkbox", { name: /Invalid skill/ });
    expect((invalidCandidate as HTMLInputElement).disabled).toBe(true);
    expect(invalidCandidate.closest("label")?.getAttribute("title")).toBe("Malformed SKILL.md frontmatter");
    fireEvent.click(screen.getByRole("button", { name: "Select all" }));
    await waitFor(() => expect((candidate as HTMLInputElement).checked).toBe(true));
    expect((invalidCandidate as HTMLInputElement).checked).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Select none" }));
    await waitFor(() => expect((candidate as HTMLInputElement).checked).toBe(false));
    fireEvent.click(candidate);
    await waitFor(() => expect((candidate as HTMLInputElement).checked).toBe(true));
    expect((await screen.findByRole("region", { name: "Selected skill preview" })).textContent).toContain("name: folder-skill");
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    expect(ipc.calls.some(({ command }) => command === "skill_import_local_start")).toBe(true);
  });
  test("cancels a Git bridge job and keeps the wizard open for retry", async () => {
    ipc.reply = async (command, args) => {
      if (command === "skill_import_git_start") return { job_id: "git-import-job", import_id: "git-import-session" };
      if (command === "jobs_cancel") return true;
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = structuredClone(request.wizard ?? initial());
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "next" && state.step === "source") state.step = "details";
      if (request.event.type === "set_details") state.details = request.event.payload as StubState["details"];
      return [state, []];
    };
    renderWizard(() => undefined);
    await waitFor(() => expect(ipc.listeners.has("agentuna:job-finished")).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: /Import from git/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.change(await screen.findByLabelText("Repository URL"), { target: { value: "https://example.invalid/skills.git" } });
    await waitFor(() => expect((screen.getByLabelText("Repository URL") as HTMLInputElement).value).toBe("https://example.invalid/skills.git"));
    await waitFor(() => expect((screen.getByRole("button", { name: "Fetch skills" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Fetch skills" }));
    await screen.findByRole("button", { name: "Cancel import" });
    ipc.listeners.get("agentuna:job-progress")?.({ payload: { id: "git-import-job", name: "Fetch git skills", fraction: 0.25, label: "Cloning public repository…" } });
    expect(await screen.findByText("Cloning public repository…")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Cancel import" }));
    await waitFor(() => expect(ipc.calls.some(({ command }) => command === "jobs_cancel")).toBe(true));
    ipc.listeners.get("agentuna:job-finished")?.({ payload: { id: "git-import-job", name: "Fetch git skills", state: "cancelled", error: null } });
    expect(await screen.findByText("Import cancelled. You can retry without losing your wizard selections.")).toBeTruthy();
    expect(screen.getByLabelText("Repository URL")).toBeTruthy();
    expect(ipc.calls.some(({ command }) => command === "skill_import_job_result")).toBe(false);
  });
  test("selects across library sources, refreshes scope targets, and reviews the exact Link plan", async () => {
    const sources = [
      { id: "user", label: "User scope", kind: "scope", count: 1, last_used_at: null },
      { id: "/sandbox/projects/demo", label: "Project workspace", kind: "project", count: 2, last_used_at: null },
    ];
    const skills = [
      { id: "user-skill", source_id: "user", source_label: "User scope", path: "/sandbox/user/skills/user-skill", name: "User Skill", description: null, error: null, clash_reason: null },
      { id: "project-skill", source_id: "/sandbox/projects/demo", source_label: "Project workspace", path: "/sandbox/projects/demo/.agents/skills/project-skill", name: "Project Skill", description: null, error: null, clash_reason: null },
      { id: "project-clash", source_id: "/sandbox/projects/demo", source_label: "Project workspace", path: "/sandbox/projects/demo/.agents/skills/existing", name: "Existing Skill", description: null, error: null, clash_reason: "A skill with this name already exists in the target scope." },
    ];
    ipc.reply = async (command, args) => {
      if (command === "agents_list") return [{ id: "cursor", name: "Cursor", installed: false, primary_path: null }];
      if (command === "agent_path_candidates") return [];
      if (command === "agent_path_validate") return { exists: false, is_directory: false, structure_warning: null, reason: "Choose an existing folder." };
      if (command === "skill_scope_options") return scopeOptionsReply(args);
      if (command === "skill_library_catalog") return { sources, skills, target_scope: "User skills" };
      if (command === "skill_add_preview") return {
        preview_id: "library-preview",
        import_id: null,
        items: [
          { name: "User Skill", source_path: "/sandbox/user/skills/user-skill", target_path: "/sandbox/user/skills/user-skill", mode: "link" },
          { name: "Project Skill", source_path: "/sandbox/projects/demo/.agents/skills/project-skill", target_path: "/sandbox/user/skills/project-skill", mode: "link" },
        ],
        canonical_dir: "/sandbox/user/skills",
        files: [
          { path: "/sandbox/user/skills/user-skill/SKILL.md", package_path: "user-skill/SKILL.md", diff: null },
          { path: "/sandbox/user/skills/project-skill/SKILL.md", package_path: "project-skill/SKILL.md", diff: "--- old\n+++ new\n-old copy\n+reviewed copy\n" },
        ],
        deployments: [
          { agent: "codex", path: "/sandbox/user/skills/user-skill", mode: "link" },
          { agent: "codex", path: "/sandbox/user/skills/project-skill", mode: "link" },
        ],
        stage_available: false,
        stage_requested: false,
        requires_overwrite_confirmation: false,
        store_init: null,
      };
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = structuredClone(request.wizard ?? initial());
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "set_details") state.details = request.event.payload as StubState["details"];
      if (request.event.type === "set_scope") state.scope = String(request.event.payload);
      if (request.event.type === "set_agents") state.agents = request.event.payload as string[];
      if (request.event.type === "next" && state.step === "source") state.step = "details";
      else if (request.event.type === "next" && state.step === "details") state.step = "scope_agents";
      else if (request.event.type === "next" && state.step === "scope_agents") state.step = "review";
      return [state, []];
    };
    renderWizard(() => undefined, "prj_demo");
    fireEvent.click(screen.getByRole("button", { name: /From your library/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await screen.findByRole("combobox", { name: "Library source" });
    expect(screen.getByRole("option", { name: /Project workspace.*\/sandbox\/projects\/demo/ })).toBeTruthy();
    expect(screen.getByText(/2 sources · 3 skills · Target scope: User skills/)).toBeTruthy();
    fireEvent.click(screen.getByRole("checkbox", { name: /User Skill/ }));
    await waitFor(() => expect(screen.getByLabelText("Selected library skills").textContent).toContain("Selected (1)"));
    fireEvent.change(screen.getByRole("combobox", { name: "Library source" }), { target: { value: "/sandbox/projects/demo" } });
    const clashCheckbox = screen.getByRole("checkbox", { name: /Existing Skill/ }) as HTMLInputElement;
    expect(clashCheckbox.disabled).toBe(true);
    expect(clashCheckbox.closest("label")?.getAttribute("title")).toContain("already exists");
    fireEvent.click(screen.getByRole("button", { name: "Select all available in this source" }));
    await waitFor(() => expect(screen.getByLabelText("Selected library skills").textContent).toContain("Selected (2)"));
    expect(screen.getByLabelText("Selected library skills").textContent).toContain("User Skill");
    expect(screen.getByLabelText("Selected library skills").textContent).toContain("Project Skill");
    expect((screen.getByRole("checkbox", { name: /Project Skill/ }) as HTMLInputElement).checked).toBe(true);
    expect((clashCheckbox as HTMLInputElement).checked).toBe(false);
    fireEvent.click(screen.getByRole("radio", { name: /Link/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await screen.findByRole("radiogroup", { name: "Skill destination scope" });
    fireEvent.click(await screen.findByRole("radio", { name: /User/ }));
    const cursorCheckbox = screen.getByRole("checkbox", { name: /Cursor/ }) as HTMLInputElement;
    expect(cursorCheckbox.disabled).toBe(true);
    expect(screen.getByText("This agent is not installed. Set a skills path to include it.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Set path" }));
    const agentDialog = await screen.findByRole("dialog", { name: "Set path for Cursor" });
    expect(await within(agentDialog).findByLabelText("Agent configuration path")).toBeTruthy();
    fireEvent.click(within(agentDialog).getByRole("button", { name: "Cancel" }));
    expect(screen.getByRole("radio", { name: /User/ }).getAttribute("aria-checked")).toBe("true");
    fireEvent.click(screen.getByRole("checkbox", { name: "Codex" }));
    await waitFor(() => expect(screen.getByText("Project Skill: /sandbox/user/skills/project-skill")).toBeTruthy());
    const scopeCallCount = ipc.calls.filter(({ command }) => command === "skill_scope_options").length;
    fireEvent.click(screen.getByRole("radio", { name: /Global/ }));
    await waitFor(() => expect(screen.getByText("Project Skill: /sandbox/global/skills/project-skill")).toBeTruthy());
    expect(ipc.calls.filter(({ command }) => command === "skill_scope_options").length).toBeGreaterThan(scopeCallCount);
    expect(screen.getByText("1 Global item is protected by locks.")).toBeTruthy();
    fireEvent.click(screen.getByRole("radio", { name: /User/ }));
    fireEvent.click(screen.getByRole("button", { name: "Show Local scope" }));
    fireEvent.click(screen.getByRole("radio", { name: /Local/ }));
    await waitFor(() => expect(screen.getByText("Project Skill: /sandbox/project/.agents/skills/project-skill")).toBeTruthy());
    expect(screen.getByText("/sandbox/project/.agentuna/local")).toBeTruthy();
    fireEvent.click(screen.getByRole("radio", { name: /User/ }));
    await waitFor(() => expect(screen.getByText("Project Skill: /sandbox/user/skills/project-skill")).toBeTruthy());
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByRole("heading", { name: "Review skill" })).toBeTruthy();
    expect(await screen.findByText("Link · Project Skill")).toBeTruthy();
    expect(screen.getByText("/sandbox/projects/demo/.agents/skills/project-skill → /sandbox/user/skills/project-skill")).toBeTruthy();
    expect(screen.getByLabelText("Planned file paths").textContent).toContain("/sandbox/user/skills/project-skill/SKILL.md");
    fireEvent.click(screen.getByText("Review existing-file diff"));
    expect(screen.getByText(/reviewed copy/)).toBeTruthy();
    expect(screen.getByLabelText("Planned deployments").textContent).toMatch(/codex\s*:\s*link\s*→\s*\/sandbox\/user\/skills\/project-skill/);
    expect(ipc.calls.some(({ command }) => command === "skill_add_preview")).toBe(true);
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
    renderWizard(() => undefined);
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
    renderWizard(() => undefined);
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByText("Where does the skill come from?")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Import from git/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByLabelText("Repository URL")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Repository URL"), { target: { value: "https://example.invalid/skills.git" } });
    await waitFor(() => expect((screen.getByRole("button", { name: "Back" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(await screen.findByText("Where does the skill come from?")).toBeTruthy();
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    const repoInput = await screen.findByLabelText("Repository URL") as HTMLInputElement;
    expect(repoInput.value).toBe("https://example.invalid/skills.git");
    expect(screen.getByText(/Only public HTTPS repositories are supported/)).toBeTruthy();
    expect(ipc.calls.some(({ command }) => command === "skill_wizard_transition")).toBe(true);
  });

  test("clean cancel closes immediately", async () => {
    const onClose = vi.fn();
    ipc.reply = async (_command, args) => {
      const request = args as { wizard: StubState | null; event: { type: string } };
      return [request.wizard ?? initial(), [{ type: "discard_draft" }]];
    };
    renderWizard(onClose);
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
    renderWizard(onClose);
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
    renderWizard(onClose);
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
    renderWizard(() => undefined);
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
    renderWizard(() => undefined);
    fireEvent.click(screen.getByRole("button", { name: /Write a new skill/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await waitFor(() => expect(screen.getByRole("radio", { name: /Blank/ })).toBeTruthy());
    expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(true);
  });
  test("streams apply progress, stays on Review after failure, and shows Done only after success", async () => {
    const writtenPath = "/sandbox/user/skills/plan-skill/SKILL.md";
    let applyStarts = 0;
    ipc.reply = async (command, args) => {
      if (command === "skill_scope_options") return scopeOptionsReply(args);
      if (command === "skill_template_preview") return { slug: "plan-skill", markdown: "# Plan Skill", name_error: null, description_error: null };
      if (command === "skill_add_preview") {
        return {
          preview_id: "preview-skill-add",
          import_id: null,
          items: [],
          canonical_dir: "/sandbox/user/skills/plan-skill",
          files: [{ path: writtenPath, package_path: "SKILL.md", diff: null }],
          deployments: [{ agent: "omp", path: "/sandbox/.omp/skills/plan-skill", mode: "symlink" }],
          stage_available: false,
          stage_requested: false,
          requires_overwrite_confirmation: false,
          store_init: { path: "/sandbox/user/.agentuna", files_created: 6n, foreign: [] },
        };
      }
      if (command === "skill_add_apply_start") return { job_id: `skill-add-job-${++applyStarts}`, result_id: `skill-add-result-${applyStarts}` };
      if (command === "skill_add_apply_result") {
        return {
          skill_names: ["Plan Skill"],
          operation_id: "op-skill-add",
          paths_written: [writtenPath],
          deployments: [{ agent: "omp", path: "/sandbox/.omp/skills/plan-skill", mode: "symlink" }],
          staged: false,
          staged_paths: [],
          stage_error: null,
          backup_hint: "Operation op-skill-add is recorded.",
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
    renderWizard(() => undefined);
    fireEvent.click(screen.getByRole("button", { name: /Write a new skill/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.click(await screen.findByRole("radio", { name: /Blank/ }));
    await waitFor(() => expect(screen.getByRole("radio", { name: /Blank/ }).getAttribute("aria-checked")).toBe("true"));
    fireEvent.change(screen.getByLabelText("Skill name"), { target: { value: "Plan Skill" } });
    fireEvent.change(screen.getByLabelText("Use when description"), { target: { value: "Use this skill to plan work." } });
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await screen.findByRole("radiogroup", { name: "Skill destination scope" });
    fireEvent.click(await screen.findByRole("radio", { name: /User/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: "omp" }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByText("/sandbox/user/skills/plan-skill")).toBeTruthy();
    const storeLine = screen.getByLabelText("Store creation").textContent ?? "";
    expect(storeLine).toContain("Create store at");
    expect(storeLine).toContain("/sandbox/user/.agentuna");
    fireEvent.click(screen.getByRole("button", { name: "Create skill" }));
    await screen.findByText("Creating skill…");
    expect(screen.queryByRole("heading", { name: "Plan Skill is ready" })).toBeNull();
    ipc.listeners.get("agentuna:job-finished")?.({ payload: { id: "skill-add-job-1", name: "Create skill", state: "failed", error: { kind: "failed", message: "Safe write rejected the plan." } } });
    expect(await screen.findByText("Safe write rejected the plan. Review the refreshed plan before trying again.")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Review skill" })).toBeTruthy();
    await waitFor(() => expect((screen.getByRole("button", { name: "Create skill" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Create skill" }));
    await screen.findByText("Creating skill…");
    ipc.listeners.get("agentuna:job-progress")?.({ payload: { id: "skill-add-job-2", name: "Create skill", fraction: 0.8, label: "Writing the reviewed files…" } });
    expect(await screen.findByText("Writing the reviewed files…")).toBeTruthy();
    ipc.listeners.get("agentuna:job-finished")?.({ payload: { id: "skill-add-job-2", name: "Create skill", state: "succeeded", error: null } });
    expect(await screen.findByRole("heading", { name: "Plan Skill is ready" })).toBeTruthy();
    expect(screen.getByRole("list", { name: "Written file paths" }).textContent).toContain(writtenPath);
    expect(screen.getByText("Operation: op-skill-add")).toBeTruthy();
    expect(screen.queryByText("user skill store/Plan Skill/SKILL.md")).toBeNull();
    expect(ipc.calls.some(({ command }) => command === "skill_add_preview")).toBe(true);
    expect(ipc.calls.filter(({ command }) => command === "skill_add_apply_start")).toHaveLength(2);
  });
  test("starts with the active Add-menu scope preselected", async () => {
    ipc.reply = async (command, args) => {
      if (command === "skill_scope_options") return scopeOptionsReply(args);
      if (command === "skill_template_preview") return { slug: "scope-check", markdown: "# Scope Check", name_error: null, description_error: null };
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = structuredClone(request.wizard ?? initial());
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "set_name") state.name = String(request.event.payload);
      if (request.event.type === "set_details") state.details = request.event.payload as StubState["details"];
      if (request.event.type === "next" && state.step === "source") state.step = "details";
      else if (request.event.type === "next" && state.step === "details") state.step = "scope_agents";
      return [state, []];
    };
    renderWizard(() => undefined, "prj_demo", "project");
    fireEvent.click(screen.getByRole("button", { name: /Write a new skill/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.click(await screen.findByRole("radio", { name: /Blank/ }));
    fireEvent.change(screen.getByLabelText("Skill name"), { target: { value: "Scope Check" } });
    fireEvent.change(screen.getByLabelText("Use when description"), { target: { value: "Check active scope." } });
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    const projectScope = await screen.findByRole("radio", { name: /Project/ });
    expect(projectScope.getAttribute("aria-checked")).toBe("true");
    expect(hasSkillScopeRequestFor("project")).toBe(true);
  });
});


function transition(args: unknown) {
  const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
  const state = structuredClone(request.wizard ?? initial());
  if (request.event.type === "select_source") state.source = String(request.event.payload);
  if (request.event.type === "next" && state.step === "source") state.step = "details";
  if (request.event.type === "set_details") state.details = request.event.payload as StubState["details"];
  return [state, []];
}

async function openSource(name: string | RegExp) {
  await waitFor(() => expect(ipc.listeners.has("agentuna:job-finished")).toBe(true));
  fireEvent.click(screen.getByRole("button", { name }));
  await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
  fireEvent.click(screen.getByRole("button", { name: "Next" }));
}

describe("AddSkillWizard source steps", () => {
  test("Git fetch lists candidates, disables invalid ones and previews clone content offline", async () => {
    ipc.reply = async (command, args) => {
      if (command === "skill_import_git_start") return { job_id: "git-list-job", import_id: "git-list-session" };
      if (command === "skill_import_job_result") return { candidates: [
        { id: "review", name: "Review", description: "Checks diffs", markdown: "---\nname: review\n---\n", error: null },
        { id: "broken", name: null, description: null, markdown: null, error: "Missing name" },
      ], error: null };
      return transition(args);
    };
    renderWizard(() => undefined);
    await openSource(/Import from git/);
    fireEvent.change(await screen.findByLabelText("Repository URL"), { target: { value: "https://example.invalid/skills.git" } });
    fireEvent.click(await screen.findByRole("button", { name: "Fetch skills" }));
    await screen.findByRole("button", { name: "Cancel import" });
    ipc.listeners.get("agentuna:job-progress")?.({ payload: { id: "git-list-job", name: "Fetch git skills", fraction: 0.5, label: "Cloning public repository…" } });
    expect(await screen.findByText("Cloning public repository…")).toBeTruthy();
    ipc.listeners.get("agentuna:job-finished")?.({ payload: { id: "git-list-job", name: "Fetch git skills", state: "succeeded", error: null } });
    expect(await screen.findByText("2 skills ready to select.")).toBeTruthy();

    const valid = await screen.findByRole("checkbox", { name: /Review/ });
    const invalid = await screen.findByRole("checkbox", { name: /Invalid skill/ });
    expect((invalid as HTMLInputElement).disabled).toBe(true);
    expect(invalid.closest("label")?.getAttribute("title")).toBe("Missing name");
    fireEvent.click(screen.getByRole("button", { name: "Select all" }));
    await waitFor(() => expect((valid as HTMLInputElement).checked).toBe(true));
    expect((invalid as HTMLInputElement).checked).toBe(false);

    fireEvent.focus(valid);
    const preview = await screen.findByRole("region", { name: "Selected skill preview" });
    expect(preview.textContent).toContain("name: review");
    expect(ipc.calls.filter(({ command }) => command === "skill_import_job_result")).toHaveLength(1);
    expect(ipc.calls.filter(({ command }) => command === "skill_import_git_start")).toHaveLength(1);
  });

  test("an empty Git fetch shows the No skills found hint without an error toast", async () => {
    ipc.reply = async (command, args) => {
      if (command === "skill_import_git_start") return { job_id: "git-empty-job", import_id: "git-empty-session" };
      if (command === "skill_import_job_result") return { candidates: [], error: null };
      return transition(args);
    };
    renderWizard(() => undefined);
    await openSource(/Import from git/);
    fireEvent.change(await screen.findByLabelText("Repository URL"), { target: { value: "https://example.invalid/empty.git" } });
    fireEvent.click(await screen.findByRole("button", { name: "Fetch skills" }));
    await screen.findByRole("button", { name: "Cancel import" });
    ipc.listeners.get("agentuna:job-finished")?.({ payload: { id: "git-empty-job", name: "Fetch git skills", state: "succeeded", error: null } });
    expect(await screen.findByText(/No skills found\. Check that this public repository contains a SKILL\.md\./)).toBeTruthy();
    expect(screen.getByTestId("toast-viewport").querySelectorAll("li")).toHaveLength(0);
  });

  test("an authentication failure explains the public-URL rule without requesting credentials", async () => {
    ipc.reply = async (command, args) => {
      if (command === "skill_import_git_start") return { job_id: "git-auth-job", import_id: "git-auth-session" };
      if (command === "skill_import_job_result") return { candidates: [], error: "Authentication required: this repository is private" };
      return transition(args);
    };
    renderWizard(() => undefined);
    await openSource(/Import from git/);
    fireEvent.change(await screen.findByLabelText("Repository URL"), { target: { value: "https://example.invalid/private.git" } });
    fireEvent.click(await screen.findByRole("button", { name: "Fetch skills" }));
    await screen.findByRole("button", { name: "Cancel import" });
    ipc.listeners.get("agentuna:job-finished")?.({ payload: { id: "git-auth-job", name: "Fetch git skills", state: "succeeded", error: null } });
    const gitSection = screen.getByRole("region", { name: "Fetched repository skills" });
    expect((await within(gitSection).findByRole("alert")).textContent).toContain("Authentication required");
    expect(screen.getByText(/use its public clone URL; no credentials are requested or stored/)).toBeTruthy();
    expect(document.querySelector('input[type="password"]')).toBeNull();
    expect(ipc.calls.some(({ command }) => command === "skill_import_job_result")).toBe(true);
  });

  test("the library step offers the git import when no other source exists", async () => {
    ipc.reply = async (command, args) => {
      if (command === "skill_library_catalog") return { sources: [], skills: [], target_scope: "user" };
      return transition(args);
    };
    renderWizard(() => undefined);
    await openSource(/From your library/);
    expect(await screen.findByText("No other skill sources are available.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Import from Git instead" }));
    expect(await screen.findByLabelText("Repository URL")).toBeTruthy();
  });

  test("the offline marketplace shows cached formatting and waits for an explicit Search", async () => {
    ipc.reply = async (command, args) => {
      if (command === "network_policy") return { offline: true, marketplace_enabled: false, statement: "Offline", background_health: "local", updates_opt_in: false, allowed_hosts: [], actions: [], events: [], capacity: 20 };
      if (command === "skill_marketplace_search") return { provenance: "skills.sh · cached 2 minutes ago", offline: true, stale: false, results: [{ id: "m1", name: "Lint", author: "ana", installs: 1200, source: "https://example.invalid/lint", description: "Lints changes" }] };
      return transition(args);
    };
    renderWizard(() => undefined);
    await openSource("Marketplace");
    expect(await screen.findByText(/cached marketplace results may be available/i)).toBeTruthy();
    expect(ipc.calls.some(({ command }) => command === "skill_marketplace_search")).toBe(false);

    fireEvent.change(screen.getByLabelText("Search marketplace"), { target: { value: "lint" } });
    fireEvent.click(screen.getByRole("button", { name: "Search" }));
    expect(await screen.findByText(/1\.2k installs/)).toBeTruthy();
    expect(screen.getByText(/skills\.sh · cached 2 minutes ago · cached\/offline/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Lint/ }));
    await waitFor(() => expect(screen.getByRole("button", { name: /Lint/ }).getAttribute("aria-pressed")).toBe("true"));
  });

  test("a zip traversal error blocks import and names the offending entry", async () => {
    ipc.reply = async (command, args) => {
      if (command === "skill_import_choose_zip") return "/sandbox/evil.zip";
      if (command === "skill_import_local_start") return { job_id: "zip-evil-job", import_id: "zip-evil-session" };
      if (command === "skill_import_job_result") return { candidates: [], error: "ZIP entry '../evil/SKILL.md' escapes the archive" };
      return transition(args);
    };
    renderWizard(() => undefined);
    await openSource("Zip file");
    fireEvent.click(await screen.findByRole("button", { name: "Browse…" }));
    await waitFor(() => expect((screen.getByLabelText("ZIP path") as HTMLInputElement).value).toBe("/sandbox/evil.zip"));
    fireEvent.click(screen.getByRole("button", { name: "Scan ZIP" }));
    await screen.findByRole("button", { name: "Cancel import" });
    ipc.listeners.get("agentuna:job-finished")?.({ payload: { id: "zip-evil-job", name: "Scan zip", state: "succeeded", error: null } });
    const detected = screen.getByRole("region", { name: "Detected skills" });
    expect((await within(detected).findByRole("alert")).textContent).toContain("../evil/SKILL.md");
    expect((screen.getByRole("button", { name: "Select all" }) as HTMLButtonElement).disabled).toBe(true);
    expect(ipc.calls.some(({ command }) => command === "skill_import_job_result")).toBe(true);
  });

  test("Back then forward restores the fetched Git candidates, selection and scroll position", async () => {
    // jsdom has no layout, so `scrollTo` is stubbed to record the requested offset on the element.
    Object.defineProperty(HTMLElement.prototype, "scrollTo", {
      configurable: true,
      writable: true,
      value: function (this: HTMLElement, options?: ScrollToOptions | number, y?: number) {
        this.scrollTop = typeof options === "object" && options !== null ? options.top ?? 0 : typeof options === "number" ? options : y ?? 0;
      },
    });
    ipc.reply = async (command, args) => {
      if (command === "skill_import_git_start") return { job_id: "git-restore-job", import_id: "git-restore-session" };
      if (command === "skill_import_job_result") return { candidates: [
        { id: "review", name: "Review", description: "Checks diffs", markdown: "---\nname: review\n---\n", error: null },
        { id: "broken", name: null, description: null, markdown: null, error: "Missing name" },
      ], error: null };
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = structuredClone(request.wizard ?? initial());
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "set_details") state.details = request.event.payload as StubState["details"];
      if (request.event.type === "next" && state.step === "source") state.step = "details";
      if (request.event.type === "back" && state.step === "details") state.step = "source";
      return [state, []];
    };
    renderWizard(() => undefined);
    await openSource(/Import from git/);
    fireEvent.change(await screen.findByLabelText("Repository URL"), { target: { value: "https://example.invalid/skills.git" } });
    fireEvent.click(await screen.findByRole("button", { name: "Fetch skills" }));
    await screen.findByRole("button", { name: "Cancel import" });
    ipc.listeners.get("agentuna:job-finished")?.({ payload: { id: "git-restore-job", name: "Fetch git skills", state: "succeeded", error: null } });
    expect(await screen.findByText("2 skills ready to select.")).toBeTruthy();
    const list = screen.getByTestId("git-import-candidate-list");
    fireEvent.click(await screen.findByRole("checkbox", { name: /Review/ }));
    await waitFor(() => expect((screen.getByRole("checkbox", { name: /Review/ }) as HTMLInputElement).checked).toBe(true));
    list.scrollTop = 173;
    fireEvent.scroll(list);

    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(await screen.findByText("Where does the skill come from?")).toBeTruthy();
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));

    const restored = await screen.findByTestId("git-import-candidate-list");
    await waitFor(() => expect(restored.scrollTop).toBe(173));
    expect((screen.getByRole("checkbox", { name: /Review/ }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: /Invalid skill/ }) as HTMLInputElement).disabled).toBe(true);
    expect(ipc.calls.filter(({ command }) => command === "skill_import_job_result")).toHaveLength(1);
  });

  test("Copy and Link each shape the reviewed library plan", async () => {
    const sourceSkill = {
      id: "shared-skill",
      source_id: "user",
      source_label: "User scope",
      path: "/sandbox/user/skills/shared-skill",
      name: "Shared Skill",
      description: null,
      error: null,
      clash_reason: null,
    };
    const previewModes: string[] = [];
    ipc.reply = async (command, args) => {
      if (command === "skill_scope_options") return scopeOptionsReply(args);
      if (command === "skill_library_catalog") {
        return { sources: [{ id: "user", label: "User scope", kind: "scope", count: 1, last_used_at: null }], skills: [sourceSkill], target_scope: "Project skills" };
      }
      if (command === "skill_add_preview") {
        const { request } = args as { request: { wizard: StubState } };
        const mode = request.wizard.details.library_mode as string;
        previewModes.push(mode);
        return {
          preview_id: `preview-${mode}`,
          import_id: null,
          items: [{ name: "Shared Skill", source_path: sourceSkill.path, target_path: "/sandbox/project/.config-studio/skills/shared-skill", mode }],
          canonical_dir: "/sandbox/project/.config-studio/skills/shared-skill",
          files: mode === "link"
            ? [{ path: "/sandbox/project/.config-studio/skills/shared-skill", package_path: "linked skill folder", diff: null }]
            : [{ path: "/sandbox/project/.config-studio/skills/shared-skill/SKILL.md", package_path: "SKILL.md", diff: null }],
          deployments: [],
          stage_available: false,
          stage_requested: false,
          requires_overwrite_confirmation: false,
          store_init: null,
        };
      }
      const request = args as { wizard: StubState | null; event: { type: string; payload?: unknown } };
      const state = structuredClone(request.wizard ?? initial());
      if (request.event.type === "select_source") state.source = String(request.event.payload);
      if (request.event.type === "set_details") state.details = request.event.payload as StubState["details"];
      if (request.event.type === "set_scope") state.scope = String(request.event.payload);
      if (request.event.type === "set_agents") state.agents = request.event.payload as string[];
      if (request.event.type === "next" && state.step === "source") state.step = "details";
      else if (request.event.type === "next" && state.step === "details") state.step = "scope_agents";
      else if (request.event.type === "next" && state.step === "scope_agents") state.step = "review";
      else if (request.event.type === "back" && state.step === "review") state.step = "scope_agents";
      else if (request.event.type === "back" && state.step === "scope_agents") state.step = "details";
      return [state, []];
    };
    renderWizard(() => undefined, "prj_demo", "project");
    fireEvent.click(screen.getByRole("button", { name: /From your library/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.click(await screen.findByRole("checkbox", { name: /Shared Skill/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await screen.findByRole("radiogroup", { name: "Skill destination scope" });
    fireEvent.click(await screen.findByRole("checkbox", { name: "Codex" }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));

    expect(await screen.findByText("Copy · Shared Skill")).toBeTruthy();
    expect(within(screen.getByLabelText("Planned file paths")).getByText("/sandbox/project/.config-studio/skills/shared-skill/SKILL.md")).toBeTruthy();
    expect(previewModes).toEqual(["copy"]);

    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    await screen.findByRole("radiogroup", { name: "Skill destination scope" });
    await waitFor(() => expect((screen.getByRole("button", { name: "Back" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    fireEvent.click(await screen.findByRole("radio", { name: /Link/ }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));

    expect(await screen.findByText("Link · Shared Skill")).toBeTruthy();
    expect(within(screen.getByLabelText("Planned file paths")).getByText("/sandbox/project/.config-studio/skills/shared-skill")).toBeTruthy();
    expect(previewModes).toEqual(["copy", "link"]);
  });
});
