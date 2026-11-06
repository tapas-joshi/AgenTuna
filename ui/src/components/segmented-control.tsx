import * as RadioGroupPrimitive from "@radix-ui/react-radio-group";
import type { KeyboardEvent, ReactNode } from "react";
import { cn } from "./cn";

/** One segment of a {@link SegmentedControl}. */
export interface SegmentedOption<T extends string = string> {
  readonly value: T;
  readonly label: string;
  /** Optional decorative icon before the label. */
  readonly icon?: ReactNode;
  readonly disabled?: boolean;
}

/** Props accepted by {@link SegmentedControl}. */
export interface SegmentedControlProps<T extends string = string> {
  /** Accessible name of the group; also the visible caption when `showLabel` is set. */
  readonly label: string;
  /** Currently selected value; the caller owns it. */
  readonly value: T;
  /** Reports the requested value; the caller applies it. */
  readonly onValueChange: (value: T) => void;
  readonly options: readonly SegmentedOption<T>[];
  readonly size?: "md" | "lg";
  readonly disabled?: boolean;
  /** Renders `label` visibly above the segments. */
  readonly showLabel?: boolean;
  readonly className?: string;
}

const ITEM_BASE =
  "inline-flex flex-1 items-center justify-center gap-4 rounded-6 border border-transparent whitespace-nowrap transition-colors " +
  "text-text-secondary hover:text-text " +
  "data-[state=checked]:border-border data-[state=checked]:bg-raised data-[state=checked]:font-medium data-[state=checked]:text-text " +
  "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus " +
  "disabled:cursor-not-allowed disabled:opacity-50";

/** Both sizes are 32px tall so they meet the desktop touch target. */
const ITEM_SIZE_CLASSES: Readonly<Record<"md" | "lg", string>> = {
  md: "h-32 px-12 text-13",
  lg: "h-32 px-16 text-14",
};

/**
 * A segmented control: one pill with radio-group semantics, used for the scope
 * switcher and view toggles.
 *
 * Keyboard: Tab reaches the group, arrow keys move between segments, Space and
 * Enter select. The whole group is a single tab stop, as radio groups are.
 */
export function SegmentedControl<T extends string>({
  label,
  value,
  onValueChange,
  options,
  size = "md",
  disabled = false,
  showLabel = false,
  className,
}: SegmentedControlProps<T>) {
  const select = (next: string) => {
    if (next !== value) {
      onValueChange(next as T);
    }
  };

  return (
    <div className="flex min-w-0 flex-col gap-4">
      {showLabel ? <span className="text-12 text-text-secondary">{label}</span> : null}
      <RadioGroupPrimitive.Root
        data-slot="segmented-control"
        aria-label={label}
        value={value}
        onValueChange={select}
        orientation="horizontal"
        disabled={disabled}
        className={cn(
          "inline-flex w-fit min-w-0 items-stretch gap-2 rounded-10 border border-border bg-surface p-2",
          className,
        )}
      >
        {options.map((option) => (
          <RadioGroupPrimitive.Item
            key={option.value}
            value={option.value}
            disabled={option.disabled === true}
            data-slot="segmented-item"
            className={cn(ITEM_BASE, ITEM_SIZE_CLASSES[size])}
            // Radix cancels Enter to avoid form submission; this control is a
            // selection widget, so Enter selects like Space does.
            onKeyDown={(event: KeyboardEvent<HTMLButtonElement>) => {
              if (event.key === "Enter") {
                event.preventDefault();
                select(option.value);
              }
            }}
          >
            {option.icon === undefined ? null : <span aria-hidden="true">{option.icon}</span>}
            {option.label}
          </RadioGroupPrimitive.Item>
        ))}
      </RadioGroupPrimitive.Root>
    </div>
  );
}
