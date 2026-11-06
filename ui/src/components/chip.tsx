import * as TogglePrimitive from "@radix-ui/react-toggle";
import type { ComponentPropsWithoutRef } from "react";
import { cn } from "./cn";

/** Dot colours available to a filter chip. Scope tones carry the scope label text. */
export type ChipDotTone =
  | "neutral"
  | "success"
  | "warning"
  | "danger"
  | "info"
  | "locked"
  | "scope-global"
  | "scope-user"
  | "scope-project"
  | "scope-local";

const DOT_CLASSES: Readonly<Record<ChipDotTone, string>> = {
  neutral: "bg-text-secondary",
  success: "bg-success",
  warning: "bg-warning",
  danger: "bg-danger",
  info: "bg-scope-user",
  locked: "bg-scope-global",
  "scope-global": "bg-scope-global",
  "scope-user": "bg-scope-user",
  "scope-project": "bg-scope-project",
  "scope-local": "bg-scope-local",
};

const BASE_CLASSES =
  "inline-flex h-32 shrink-0 items-center gap-4 rounded-full border px-8 text-12 transition-colors " +
  "data-[state=on]:border-text data-[state=on]:bg-raised data-[state=on]:font-semibold data-[state=on]:text-text " +
  "data-[state=off]:border-border data-[state=off]:text-text-secondary data-[state=off]:hover:border-text-muted data-[state=off]:hover:text-text " +
  "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus " +
  "disabled:cursor-not-allowed disabled:opacity-50";

/** Props accepted by {@link Chip}. */
export interface ChipProps
  extends Omit<
    ComponentPropsWithoutRef<typeof TogglePrimitive.Root>,
    "asChild" | "pressed" | "onPressedChange" | "defaultPressed"
  > {
  /** Current pressed state; the caller owns it. */
  readonly pressed: boolean;
  /** Reports the requested state; the caller applies it. */
  readonly onPressedChange: (pressed: boolean) => void;
  /** Optional result count rendered after the label. */
  readonly count?: number;
  /** Optional leading dot; the chip label still names the thing it filters. */
  readonly dotTone?: ChipDotTone;
}

/**
 * A filter chip: a two-state button with `aria-pressed`.
 *
 * Controlled through `pressed`/`onPressedChange`; the caller owns the filter
 * value and re-renders the chip.
 */
export function Chip({ count, dotTone, className, children, ...rest }: ChipProps) {
  return (
    <TogglePrimitive.Root
      {...rest}
      data-slot="chip"
      className={cn(BASE_CLASSES, className)}
    >
      {dotTone === undefined ? null : (
        <span aria-hidden="true" className={cn("size-8 shrink-0 rounded-full", DOT_CLASSES[dotTone])} />
      )}
      <span className="truncate">{children}</span>
      {count === undefined ? null : <span className="tabular-nums opacity-80">{count}</span>}
    </TogglePrimitive.Root>
  );
}
