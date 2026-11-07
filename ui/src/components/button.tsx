import { Slot } from "@radix-ui/react-slot";
import type { ComponentPropsWithoutRef, ReactNode } from "react";
import { cn } from "./cn";

/** Visual weight of a button. `primary` carries the one main action on a screen. */
export type ButtonVariant = "primary" | "secondary" | "ghost" | "destructive";

/** Control height. `sm` is 24px for dense rows, `md`/`lg` meet the 32px desktop touch target. */
export type ButtonSize = "sm" | "md" | "lg";

const VARIANT_CLASSES: Readonly<Record<ButtonVariant, string>> = {
  primary:
    "border border-accent-fill bg-accent-fill text-on-accent hover:border-accent-fill/90 hover:bg-accent-fill/90 active:bg-accent-fill/80",
  secondary: "border border-border bg-raised text-text hover:bg-overlay active:bg-overlay",
  ghost:
    "border border-transparent bg-transparent text-text-secondary hover:bg-overlay hover:text-text active:bg-raised",
  destructive:
    "border border-danger/60 bg-transparent text-danger hover:border-danger hover:bg-danger/10 active:bg-danger/20",
};

const SIZE_CLASSES: Readonly<Record<ButtonSize, string>> = {
  sm: "h-24 gap-4 px-8 text-11",
  md: "h-32 gap-8 px-12 text-13",
  lg: "h-32 gap-8 px-16 text-14",
};

const BASE_CLASSES =
  "inline-flex shrink-0 items-center justify-center rounded-6 font-medium transition-colors " +
  "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus " +
  "disabled:cursor-not-allowed disabled:opacity-50";

/** Props accepted by {@link Button}. */
export interface ButtonProps extends Omit<ComponentPropsWithoutRef<"button">, "type"> {
  /** Visual weight. Defaults to `secondary`, the neutral action. */
  readonly variant?: ButtonVariant;
  /** Control height. Defaults to `md` (32px). */
  readonly size?: ButtonSize;
  /** Defaults to `button` so a button never submits a form by accident. */
  readonly type?: "button" | "submit" | "reset";
  /** Renders the child element (for example an `<a>`) with button styling. `loading` does not apply. */
  readonly asChild?: boolean;
  /** Shows a spinner, sets `aria-busy` and disables the control while work runs. */
  readonly loading?: boolean;
  readonly leadingIcon?: ReactNode;
  readonly trailingIcon?: ReactNode;
}

/**
 * The shared action button.
 *
 * Controlled: it renders the props it is given and reports the click through
 * `onClick`. It never changes model data itself.
 */
export function Button({
  variant = "secondary",
  size = "md",
  type = "button",
  asChild = false,
  loading = false,
  leadingIcon,
  trailingIcon,
  className,
  disabled,
  children,
  ...rest
}: ButtonProps) {
  const classes = cn(BASE_CLASSES, VARIANT_CLASSES[variant], SIZE_CLASSES[size], className);

  if (asChild) {
    return (
      <Slot data-slot="button" data-variant={variant} className={classes} {...rest}>
        {children}
      </Slot>
    );
  }

  return (
    <button
      {...rest}
      data-slot="button"
      data-variant={variant}
      type={type}
      className={classes}
      disabled={disabled === true || loading}
      aria-busy={loading || undefined}
    >
      {loading ? (
        <span
          aria-hidden="true"
          className="size-12 animate-spin rounded-full border-2 border-current border-t-transparent"
        />
      ) : (
        leadingIcon
      )}
      {children}
      {trailingIcon}
    </button>
  );
}
