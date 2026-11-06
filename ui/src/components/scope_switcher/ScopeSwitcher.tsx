import type { KeyboardEvent } from "react";
import type { ScopeToken } from "../../theme";
import { cn } from "../cn";

/** Scope selected by the app-level scope authority. */
export type ScopeSelected = ScopeToken;

/** Scope switcher presentation and selection callback. */
export interface ScopeSwitcherProps {
  /** Scope provided by TA-053; this widget does not retain it. */
  readonly scope: ScopeToken;
  /** Whether this project supports the Local scope. */
  readonly localEnabled: boolean;
  /** Whether a project is active. */
  readonly hasProject: boolean;
  /** Whether Global is locked by policy. */
  readonly globalLocked?: boolean;
  /** Receives a single event for each user selection. */
  readonly onScopeSelected: (scope: ScopeSelected) => void;
  /** Theme marker used by visual snapshot coverage. */
  readonly theme?: "dark" | "light";
}

const SCOPE_OPTIONS: readonly { readonly value: ScopeToken; readonly label: string }[] = [
  { value: "global", label: "Global" },
  { value: "user", label: "User" },
  { value: "project", label: "Project" },
  { value: "local", label: "Local (only you, this repo)" },
];
const SCOPE_DOT_CLASSES: Readonly<Record<ScopeToken, string>> = {
  global: "bg-scope-global",
  user: "bg-scope-user",
  project: "bg-scope-project",
  local: "bg-scope-local",
};

/** Presentational Global/User/Project/Local scope selector. */
export function ScopeSwitcher({
  scope,
  localEnabled,
  hasProject,
  globalLocked = false,
  onScopeSelected,
  theme = "dark",
}: ScopeSwitcherProps) {
  const options = SCOPE_OPTIONS.filter((option) => option.value !== "local" || localEnabled);
  function onKeyDown(event: KeyboardEvent<HTMLButtonElement>, index: number) {
    let next = index;
    if (event.key === "ArrowRight" || event.key === "ArrowDown") next = (index + 1) % options.length;
    else if (event.key === "ArrowLeft" || event.key === "ArrowUp") next = (index + options.length - 1) % options.length;
    else return;
    event.preventDefault();
    const target = event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("button[data-scope]")[next];
    target?.focus();
    const selected = options[next];
    if (target !== undefined && !target.disabled && selected !== undefined) onScopeSelected(selected.value);
  }

  return (
    <div
      role="radiogroup"
      aria-label="Scope"
      data-slot="scope-switcher"
      data-theme={theme}
      className="inline-flex min-w-0 items-center gap-2 rounded-10 border border-border bg-surface p-2"
    >
      {options.map((option, index) => {
        const disabled = (option.value === "project" || option.value === "local") && !hasProject;
        const reason = disabled ? "Choose a project first" : undefined;
        const locked = option.value === "global" && globalLocked;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={scope === option.value}
            aria-disabled={disabled || locked || undefined}
            aria-label={option.label}
            aria-describedby={reason ? "scope-project-required" : undefined}
            data-scope={option.value}
            data-state={scope === option.value ? "active" : "normal"}
            data-locked={locked || undefined}
            disabled={disabled || locked}
            tabIndex={0}
            title={reason ?? (locked ? "Global scope is locked" : undefined)}
            onClick={() => onScopeSelected(option.value)}
            onKeyDown={(event) => onKeyDown(event, index)}
            className={cn(
              "inline-flex h-32 shrink-0 items-center justify-center whitespace-nowrap rounded-6 border px-12 text-13 transition-colors",
              "border-transparent text-text-secondary hover:text-text focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
              "disabled:cursor-not-allowed disabled:opacity-50",
              scope === option.value && "border-border bg-raised font-medium text-text",
              locked && "text-text-secondary",
            )}
          >
            <span aria-hidden="true" className={cn("h-8 w-8 shrink-0 rounded-full", SCOPE_DOT_CLASSES[option.value])} />
            {option.label}
          </button>
        );
      })}
      {!localEnabled ? (
        <span aria-hidden="true" className="invisible inline-flex h-32 shrink-0 items-center gap-4 rounded-6 border border-transparent px-12 text-13">
          <span className={cn("h-8 w-8 shrink-0 rounded-full", SCOPE_DOT_CLASSES.local)} />
          Local (only you, this repo)
        </span>
      ) : null}
      {!hasProject ? <span id="scope-project-required" className="sr-only">Choose a project first</span> : null}
    </div>
  );
}
