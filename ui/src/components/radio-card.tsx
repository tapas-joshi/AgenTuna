import * as RadioGroupPrimitive from "@radix-ui/react-radio-group";
import { useId } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { cn } from "./cn";

/** One card in a {@link RadioCardGroup}. */
export interface RadioCardOption<T extends string = string> {
  readonly value: T;
  /** Card heading; also the accessible name of the radio. */
  readonly title: string;
  /** One or two lines explaining the choice. */
  readonly description?: string;
  /** Optional badge, for example a scope or status chip. */
  readonly badge?: ReactNode;
  /** Optional decorative icon. */
  readonly icon?: ReactNode;
  readonly disabled?: boolean;
}

/** Props accepted by {@link RadioCardGroup}. */
export interface RadioCardGroupProps<T extends string = string> {
  /** Accessible name of the group. */
  readonly label: string;
  /** Currently selected value; the caller owns it. */
  readonly value: T;
  /** Reports the requested value; the caller applies it. */
  readonly onValueChange: (value: T) => void;
  readonly options: readonly RadioCardOption<T>[];
  readonly columns?: 1 | 2 | 3;
  readonly disabled?: boolean;
  readonly className?: string;
}

const COLUMN_CLASSES: Readonly<Record<1 | 2 | 3, string>> = {
  1: "grid-cols-1",
  2: "grid-cols-1 sm:grid-cols-2",
  3: "grid-cols-1 sm:grid-cols-2 lg:grid-cols-3",
};

const CARD_CLASSES =
  "group flex w-full min-w-0 cursor-pointer flex-col gap-8 rounded-10 border border-border bg-raised p-12 text-left transition-colors " +
  "hover:border-text-muted " +
  "data-[state=checked]:border-accent data-[state=checked]:bg-accent/10 " +
  "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus " +
  "disabled:cursor-not-allowed disabled:opacity-50";

/**
 * A radio card group: selectable cards with radio-group semantics, used for
 * scope and deploy choices where each option needs an explanation.
 *
 * Keyboard: Tab reaches the checked card, arrow keys move the selection, Space
 * and Enter select. The caller owns the value.
 */
export function RadioCardGroup<T extends string>({
  label,
  value,
  onValueChange,
  options,
  columns = 1,
  disabled = false,
  className,
}: RadioCardGroupProps<T>) {
  const groupId = useId();

  return (
    <RadioGroupPrimitive.Root
      data-slot="radio-card-group"
      aria-label={label}
      value={value}
      onValueChange={(next) => {
        if (next !== value) {
          onValueChange(next as T);
        }
      }}
      orientation="vertical"
      disabled={disabled}
      className={cn("grid gap-8", COLUMN_CLASSES[columns], className)}
    >
      {options.map((option) => {
        const titleId = `${groupId}-${option.value}-title`;
        const descriptionId = `${groupId}-${option.value}-description`;

        return (
          <RadioGroupPrimitive.Item
            key={option.value}
            value={option.value}
            disabled={option.disabled === true}
            data-slot="radio-card"
            aria-labelledby={titleId}
            aria-describedby={option.description === undefined ? undefined : descriptionId}
            className={CARD_CLASSES}
            onKeyDown={(event: KeyboardEvent<HTMLButtonElement>) => {
              if (event.key === "Enter" && option.value !== value) {
                event.preventDefault();
                onValueChange(option.value);
              }
            }}
          >
            <span className="flex items-start gap-8">
              <span
                aria-hidden="true"
                className="mt-2 flex size-16 shrink-0 items-center justify-center rounded-full border border-border group-data-[state=checked]:border-accent"
              >
                <RadioGroupPrimitive.Indicator className="size-8 rounded-full bg-accent-fill" />
              </span>
              <span className="flex min-w-0 flex-1 flex-col gap-2">
                <span className="flex min-w-0 flex-wrap items-center gap-8">
                  {option.icon === undefined ? null : <span aria-hidden="true">{option.icon}</span>}
                  <span id={titleId} className="text-13 font-medium text-text">
                    {option.title}
                  </span>
                  {option.badge}
                </span>
                {option.description === undefined ? null : (
                  <span id={descriptionId} className="text-12 text-text-muted">
                    {option.description}
                  </span>
                )}
              </span>
            </span>
          </RadioGroupPrimitive.Item>
        );
      })}
    </RadioGroupPrimitive.Root>
  );
}
