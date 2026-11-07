import * as CheckboxPrimitive from "@radix-ui/react-checkbox";
import { useId } from "react";
import type { ComponentPropsWithoutRef, KeyboardEvent } from "react";
import { cn } from "./cn";

const BASE_CLASSES =
  "inline-flex size-16 shrink-0 cursor-pointer items-center justify-center rounded-3 border border-border bg-raised text-surface transition-colors " +
  "data-[state=checked]:border-accent-fill data-[state=checked]:bg-accent-fill data-[state=checked]:text-on-accent " +
  "data-[state=indeterminate]:border-accent-fill data-[state=indeterminate]:bg-accent-fill data-[state=indeterminate]:text-on-accent " +
  "hover:border-text-muted data-[state=checked]:hover:bg-accent-fill/90 " +
  "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus " +
  "disabled:cursor-not-allowed disabled:opacity-50";

interface CheckboxBaseProps
  extends Omit<ComponentPropsWithoutRef<typeof CheckboxPrimitive.Root>, "checked" | "onCheckedChange" | "children"> {
  /** Current state; `"indeterminate"` renders the mixed mark. */
  readonly checked: boolean | "indeterminate";
  /** Reports the requested state; the caller applies it. */
  readonly onCheckedChange: (checked: boolean | "indeterminate") => void;
  /** Visible label. Omit only together with `aria-label`, for example a table header. */
  readonly label?: string;
  /** Optional second line explaining the choice. */
  readonly description?: string;
}

/**
 * Labelling is mandatory: either a visible `label` or an `aria-label`. This
 * makes an unlabelled checkbox a type error rather than an accessibility bug.
 */
export type CheckboxProps = CheckboxBaseProps &
  ({ readonly label: string } | { readonly label?: undefined; readonly "aria-label": string });

/**
 * A checkbox (Radix `Checkbox`) with `role="checkbox"` and `aria-checked`
 * (`"mixed"` when indeterminate). Controlled; it never stores the value.
 */
export function Checkbox({
  checked,
  onCheckedChange,
  label,
  description,
  className,
  id,
  disabled,
  onKeyDown,
  ...rest
}: CheckboxProps) {
  const generatedId = useId();
  const controlId = id ?? generatedId;
  const descriptionId = `${controlId}-description`;

  // Radix consumes Enter to stop form submission without selecting. A checkbox
  // in this app is never a submit control, so Enter selects as Space does.
  const handleKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    onKeyDown?.(event);
    if (event.defaultPrevented || event.key !== "Enter" || disabled === true) {
      return;
    }
    event.preventDefault();
    onCheckedChange(checked === "indeterminate" ? true : !checked);
  };

  const control = (
    <CheckboxPrimitive.Root
      {...rest}
      id={controlId}
      data-slot="checkbox"
      checked={checked}
      onCheckedChange={onCheckedChange}
      disabled={disabled}
      onKeyDown={handleKeyDown}
      aria-describedby={description === undefined ? undefined : descriptionId}
      className={cn(BASE_CLASSES, className)}
    >
      <CheckboxPrimitive.Indicator className="flex items-center justify-center" data-slot="checkbox-indicator">
        {checked === "indeterminate" ? (
          <svg aria-hidden="true" viewBox="0 0 12 12" className="size-12">
            <path d="M3 6h6" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
          </svg>
        ) : (
          <svg aria-hidden="true" viewBox="0 0 12 12" className="size-12">
            <path
              d="M2.5 6.25 5 8.75 9.5 3.5"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        )}
      </CheckboxPrimitive.Indicator>
    </CheckboxPrimitive.Root>
  );

  if (label === undefined) {
    return control;
  }

  return (
    <span className="inline-flex items-start gap-8">
      {control}
      <span className="flex flex-col gap-2">
        <label
          htmlFor={controlId}
          className={cn("cursor-pointer text-13 text-text-secondary select-none", disabled && "cursor-not-allowed opacity-50")}
        >
          {label}
        </label>
        {description === undefined ? null : (
          <span id={descriptionId} className="text-11 text-text-muted">
            {description}
          </span>
        )}
      </span>
    </span>
  );
}
