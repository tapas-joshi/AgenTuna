import * as SwitchPrimitive from "@radix-ui/react-switch";
import { useId } from "react";
import type { ComponentPropsWithoutRef } from "react";
import { cn } from "./cn";

/** Track size. `sm` fits table cells, `md` is the form control. */
export type ToggleSize = "sm" | "md";

/** Where the visible label sits relative to the switch. */
export type ToggleLabelPlacement = "inline" | "below" | "hidden";

const TRACK_CLASSES: Readonly<Record<ToggleSize, string>> = {
  sm: "h-20 w-40 p-2",
  md: "h-24 w-48 p-2",
};

const THUMB_CLASSES: Readonly<Record<ToggleSize, string>> = {
  sm: "size-16 data-[state=checked]:translate-x-20",
  md: "size-20 data-[state=checked]:translate-x-24",
};

const BASE_CLASSES =
  "relative shrink-0 cursor-pointer rounded-full border border-transparent bg-border transition-colors " +
  "data-[state=checked]:bg-accent-fill " +
  "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus " +
  "disabled:cursor-not-allowed disabled:opacity-50";

const THUMB_BASE =
  "block rounded-full bg-text-muted transition-transform data-[state=checked]:bg-text";

/** Props accepted by {@link Toggle}. */
export interface ToggleProps
  extends Omit<ComponentPropsWithoutRef<typeof SwitchPrimitive.Root>, "checked" | "onCheckedChange" | "children"> {
  /** Current state; the caller owns it. */
  readonly checked: boolean;
  /** Reports the requested state; the caller applies it. */
  readonly onCheckedChange: (checked: boolean) => void;
  /** Visible label, also the accessible name. Never colour alone. */
  readonly label: string;
  /** Optional second line with the consequence of the switch. */
  readonly description?: string;
  readonly labelPlacement?: ToggleLabelPlacement;
  readonly size?: ToggleSize;
}

/**
 * An on/off switch (Radix `Switch`): `role="switch"` with `aria-checked` and a
 * roving label. Space and Enter both toggle it because it is a native button.
 */
export function Toggle({
  checked,
  onCheckedChange,
  label,
  description,
  labelPlacement = "inline",
  size = "md",
  className,
  id,
  disabled,
  ...rest
}: ToggleProps) {
  const generatedId = useId();
  const controlId = id ?? generatedId;
  const labelId = `${controlId}-label`;
  const descriptionId = `${controlId}-description`;

  return (
    <span
      className={cn(
        "inline-flex gap-8",
        labelPlacement === "below" ? "flex-col items-start" : "items-center",
        className,
      )}
    >
      <SwitchPrimitive.Root
        {...rest}
        id={controlId}
        data-slot="toggle"
        checked={checked}
        onCheckedChange={onCheckedChange}
        disabled={disabled}
        aria-labelledby={labelId}
        aria-describedby={description === undefined ? undefined : descriptionId}
        className={cn(BASE_CLASSES, TRACK_CLASSES[size])}
      >
        <SwitchPrimitive.Thumb className={cn(THUMB_BASE, THUMB_CLASSES[size])} />
      </SwitchPrimitive.Root>
      <label
        id={labelId}
        htmlFor={controlId}
        className={cn(
          "cursor-pointer text-text-secondary select-none",
          size === "sm" ? "text-11" : "text-13",
          disabled && "cursor-not-allowed opacity-50",
          labelPlacement === "hidden" && "sr-only",
        )}
      >
        {label}
      </label>
      {description === undefined ? null : (
        <span
          id={descriptionId}
          className={cn("text-text-muted", size === "sm" ? "text-11" : "text-12")}
        >
          {description}
        </span>
      )}
    </span>
  );
}
