# VoiceOver release smoke checklist

Run against the macOS 14+ release-profile AgenTuna build. This manual session cannot be automated; attach the completed checklist and any defects to the release record. **Current session: not run — VoiceOver itself was unavailable to the agent.** Do not mark items passed until a human completes them.

## 15-minute procedure

1. **0:00–2:00 — Launch and shell.** Enable VoiceOver (`Cmd+F5`), start AgenTuna, and use `VO+Right` / `VO+Left` plus `Tab` / `Shift+Tab`. Verify the Workspace navigation landmark, each sidebar item’s name and selected state, top-bar project picker, Add control, and Command palette shortcut hint.
2. **2:00–4:00 — Scope switcher.** Reach the Scope radio group from the top bar. Verify each available scope is announced as a radio with checked state; use arrow keys to change it. With no project active, confirm Project and Local are disabled and announce why.
3. **4:00–7:00 — Skills and agent toggles.** Open Skills, traverse table headers and rows, then focus a per-agent toggle. Verify its announcement includes skill, agent and enabled/disabled state; locked, shadowed and native status must be distinguishable without colour.
4. **7:00–9:00 — Server detail.** Open an MCP server and traverse its heading, transport, health, tools and available actions. Confirm icon-only controls have names and health is described in text.
5. **9:00–11:00 — Rule-file dialog.** Open Add → New rule file. Verify the dialog name, labelled controls, validation/errors, and Cancel/Save names. Tab through controls and confirm focus stays inside the dialog until it closes.
6. **11:00–13:00 — Command palette.** Open with `Cmd+K`, type a query, move through results using Up/Down, and activate with Enter. Verify the search field, result names, group and selected result are announced; Escape closes and restores focus to the opener.
7. **13:00–15:00 — Final focus/announcement pass.** Trigger one toast and verify it is announced once as a status message. Dismiss any dialog with Escape, confirm focus returns to its opener, and note any keyboard trap or pointer-only action.


## Flows not yet available

Do not attempt to mark these checks passed until their owning stories land:

- Per-agent skill toggle: TS-010.
- MCP server list/detail: TM-020 and TM-021.
- Rule-file creation dialog and persistence: TA-057 and TA-013.

These are implementation gaps, not VoiceOver waivers. Run the corresponding checklist rows after those screens and behaviors exist.
## Results to attach to the release record

Record the build/version, macOS version, tester and date. For every row write **Pass** or **Fail** and include the observed announcement or defect ID. A missing screen/control is a failure, not a waiver.

| Check | Expected VoiceOver result | Result / observation |
|---|---|---|
| Sidebar and top bar | Named navigation and controls; selected route exposed | Not run |
| Scope switcher | Radio group, checked/disabled state and reason exposed | Not run |
| Per-agent toggles and tables | Labels include item, agent and state; headers announced | Not run |
| MCP server detail | Heading, transport, health, tools and actions named | Not run |
| Rule-file dialog | Named dialog and fields; contained focus; focus restored | Not run |
| Palette | Search and grouped results announced; selection and action work | Not run |
| Toast | Status text announced without relying on colour | Not run |
| Pointer-only step / defect | No step requires a pointer; otherwise record screen and widget | Not run |
