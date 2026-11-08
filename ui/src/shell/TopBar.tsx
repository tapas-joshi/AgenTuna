/**
 * Top bar chrome (TU-006).
 *
 * One horizontal bar above the routed body: project picker trigger on the
 * left, the TU-040 scope switcher slot in the middle, and the Cmd+K palette
 * hint on the right. The window title lives in the document title (set by the
 * router in TU-010); the top bar carries the current section name instead so
 * the bar stays useful at 900px.
 *
 * Everything here is presentational: picking a project, changing scope and
 * opening the palette all leave through callbacks or a caller-supplied slot.
 * The scope switcher itself is TU-040's widget; the shell only reserves its
 * place and forwards the change event.
 */

import type { JSX, ReactNode } from "react";
import { cn } from "../components/cn";
import { getThemeTokens } from "../theme";
import type { ScopeToken, ThemeMode } from "../theme";
import { ChevronDownIcon, ProjectIcon, SearchIcon } from "./icons";

/** Props accepted by {@link TopBar}. */
export interface TopBarProps {
  /** Label of the current screen/section, shown as the bar's title. */
  readonly title: string;
  /**
   * Active project label for the picker trigger. `null` renders the empty
   * project state ("No project").
   */
  readonly projectLabel: string | null;
  /** Called when the project picker trigger is activated. */
  readonly onProjectPicker: () => void;
  /** Whether a previous route is available. */
  readonly canGoBack?: boolean;
  /** Navigate to the previous route in AppState's bounded history. */
  readonly onBack?: () => void;
  /**
   * The TU-040 scope switcher widget, rendered as-is. The shell does not
   * construct it: scope state, routing and persistence belong to TA-053.
   */
  readonly scopeSwitcher?: ReactNode;
  /** Optional Add menu trigger supplied by the app action registry. */
  readonly addControl?: ReactNode;
  /** Current scope, used to tint its dot when `scopeSwitcher` is absent. */
  readonly scope?: ScopeToken;
  /** Theme mode for the scope dot; defaults to `dark`. */
  readonly themeMode?: ThemeMode;
  /** Called when the palette hint is activated (click or Enter/Space). */
  readonly onPalette: () => void;
  /** Whether the palette shortcut is Cmd (macOS) or Ctrl; affects the hint. */
  readonly paletteModifier?: "meta" | "ctrl";
  readonly className?: string;
}

/**
 * Renders the top bar.
 *
 * @example
 * ```tsx
 * <TopBar
 *   title="Skills"
 *   projectLabel="ai-coding-config-studio"
 *   onProjectPicker={openProjectPicker}
 *   scopeSwitcher={<ScopeSwitcher … />}
 *   onPalette={openPalette}
 * />
 * ```
 */
export function TopBar({
  title,
  projectLabel,
  onProjectPicker,
  scopeSwitcher,
  scope,
  themeMode = "dark",
  onPalette,
  addControl,
  paletteModifier = "meta",
  className,
  canGoBack = false,
  onBack,
}: TopBarProps): JSX.Element {
  const paletteHint = paletteModifier === "meta" ? "⌘K" : "Ctrl+K";
  const scopeColor =
    scope === undefined ? undefined : getThemeTokens(themeMode).color.scope[scope];

  return (
    <header
      data-slot="shell-topbar"
      className={cn(
        "flex h-[40px] shrink-0 items-center gap-12 border-b border-border bg-surface px-16 max-[1023px]:gap-8",
        className,
      )}
    >
      <button
        type="button"
        aria-label="Back"
        aria-keyshortcuts="Meta+["
        disabled={!canGoBack}
        onClick={onBack}
        className="inline-flex h-[28px] shrink-0 items-center rounded-6 border border-border px-8 text-12 text-text focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus disabled:cursor-not-allowed disabled:opacity-50"
      >
        <span aria-hidden="true">←</span>
        <span className="sr-only">Back</span>
      </button>
      {/* Window/screen title */}
      <span className="min-w-0 shrink truncate text-14 font-medium text-text" data-slot="shell-title">
        {title}
      </span>

      {/* Project picker trigger */}
      <button
        type="button"
        data-slot="shell-project-picker"
        onClick={onProjectPicker}
        aria-haspopup="dialog"
        aria-label={
          projectLabel === null
            ? "Choose a project"
            : `Project: ${projectLabel}. Choose a different project`
        }
        className={cn(
          "flex h-[28px] w-[160px] max-[1023px]:w-[140px] min-w-0 shrink-0 items-center gap-[6px] rounded-6 border px-8 text-12",
          "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
          projectLabel === null
            ? "border-border bg-raised text-text-muted hover:bg-overlay hover:text-text"
            : "border-scope-project/60 bg-raised text-text hover:bg-overlay",
        )}
      >
        <ProjectIcon />
        <span className="shrink-0 text-text-muted">Project</span>
        <span className="min-w-0 truncate font-medium">
          {projectLabel ?? "No project"}
        </span>
        <span aria-hidden="true" className="ml-auto shrink-0 text-text-muted">
          <ChevronDownIcon />
        </span>
      </button>

      {/* Scope switcher slot (TU-040) — fixed-width, in-flow slot */}
      <div className="flex w-[420px] shrink-0 items-center justify-start gap-8">
        {scopeSwitcher ??
          (scope === undefined ? null : (
            <span
              data-slot="shell-scope-fallback"
              className="inline-flex items-center gap-4 text-12 text-text-secondary"
            >
              <span
                aria-hidden="true"
                className="inline-block size-8 rounded-full"
                style={{ backgroundColor: scopeColor }}
              />
              {scope}
            </span>
          ))}
      </div>
      {addControl}


      {/* Palette hint */}
      <button
        type="button"
        data-slot="shell-palette-hint"
        onClick={onPalette}
        aria-keyshortcuts={paletteModifier === "meta" ? "Meta+K" : "Control+K"}
        aria-label={`Open command palette (${paletteHint})`}
        className={cn(
          "ml-auto flex h-[28px] w-[220px] shrink-0 items-center gap-8 rounded-6 border border-border bg-raised px-8 max-[1439px]:w-[40px] max-[1439px]:justify-center",
          "text-12 text-text-muted",
          "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
          "hover:border-text-muted hover:text-text-secondary",
        )}
      >
        <SearchIcon />
        <span className="min-w-0 flex-1 truncate text-left max-[1439px]:hidden">Search skills, servers, files</span>
        <kbd
          aria-hidden="true"
          className="shrink-0 rounded-3 border border-border bg-overlay px-4 py-[2px] font-mono text-11 text-text-secondary max-[1439px]:hidden"
        >
          {paletteHint}
        </kbd>
      </button>
    </header>
  );
}
