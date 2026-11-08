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
  const unavailableScopes = new Set(
    options
      .filter(({ value }) =>
        ((value === "project" || value === "local") && !hasProject) ||
        (value === "global" && globalLocked),
      )
      .map(({ value }) => value),
  );
  const tabStop = options.find((option) => option.value === scope)?.value ??
    options.find((option) => !unavailableScopes.has(option.value))?.value;
  function onKeyDown(event: KeyboardEvent<HTMLButtonElement>, index: number) {
    const direction = event.key === "ArrowRight" || event.key === "ArrowDown"
      ? 1
      : event.key === "ArrowLeft" || event.key === "ArrowUp"
        ? -1
        : 0;
    if (direction === 0) return;
    event.preventDefault();
    let next = index;
    for (let count = 1; count < options.length; count += 1) {
      next = (next + direction + options.length) % options.length;
      const candidate = options[next];
      if (candidate !== undefined && !unavailableScopes.has(candidate.value)) {
        const target = event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("button[data-scope]")[next];
        target?.focus();
        if (target !== undefined) onScopeSelected(candidate.value);
        break;
      }
    }
  }
  // Reserve the four-scope width in every state so the top bar does not jump.

  return (
    <div
      role="radiogroup"
      aria-label="Scope"
      data-slot="scope-switcher"
      data-theme={theme}
      className="flex w-[420px] min-w-0 items-center gap-2 rounded-10 border border-border bg-surface p-2"
    >
      {options.map((option, index) => {
        const disabled = unavailableScopes.has(option.value);
        const reason = (option.value === "project" || option.value === "local") && !hasProject
          ? "Choose a project first"
          : undefined;
        const locked = option.value === "global" && globalLocked;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={scope === option.value}
            aria-disabled={disabled || undefined}
            aria-label={option.label}
            aria-describedby={reason ? "scope-project-required" : undefined}
            data-scope={option.value}
            data-state={scope === option.value ? "active" : "normal"}
            data-locked={locked || undefined}
            tabIndex={tabStop === option.value ? 0 : -1}
            title={reason ?? (locked ? "Global store not available" : undefined)}
            onClick={() => { if (!disabled) onScopeSelected(option.value); }}
            onKeyDown={(event) => onKeyDown(event, index)}
            className={cn(
              "inline-flex h-32 shrink-0 items-center justify-center whitespace-nowrap rounded-6 border px-12 text-13 transition-colors max-[1439px]:px-8",
              "border-transparent text-text-secondary hover:text-text focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
              "aria-disabled:cursor-not-allowed aria-disabled:text-text-secondary",
              scope === option.value && "border-border bg-raised font-medium text-text",
              locked && "text-text-secondary",
            )}
          >
            <span aria-hidden="true" className={cn("h-8 w-8 shrink-0 rounded-full", SCOPE_DOT_CLASSES[option.value])} />
            {option.value === "local" ? (
              <>
                <span className="hidden max-[1439px]:inline">Local</span>
                <span className="max-[1439px]:hidden">{option.label}</span>
              </>
            ) : option.label}
          </button>
        );
      })}
      {!localEnabled ? (
        <span aria-hidden="true" className="invisible inline-flex h-32 shrink-0 items-center gap-4 rounded-6 border border-transparent px-12 text-13 max-[1439px]:hidden">
          <span className={cn("h-8 w-8 shrink-0 rounded-full", SCOPE_DOT_CLASSES.local)} />
          Local (only you, this repo)
        </span>
      ) : null}
      {!hasProject ? <span id="scope-project-required" className="sr-only">Choose a project first</span> : null}
    </div>
  );
}
