import { useId } from "react";
import type { ComponentPropsWithRef, ReactNode, Ref } from "react";
import { cn } from "./cn";

/** Height of the single-line box. Both sizes are at least 32px tall. */
export type InputSize = "md" | "lg";

const SIZE_CLASSES: Readonly<Record<InputSize, string>> = {
  md: "h-32 px-8 text-13",
  lg: "h-32 px-12 text-14",
};

interface InputBaseProps extends Omit<ComponentPropsWithRef<"input">, "size"> {
  readonly ref?: Ref<HTMLInputElement>;
  /** Visible label. Omit only together with `aria-label`. */
  readonly label?: string;
  /** Helper line below the field. */
  readonly hint?: string;
  /** Error line below the field; also sets `aria-invalid` and the danger border. */
  readonly error?: string;
  /** Decorative icon rendered before the text, for example a search glyph. */
  readonly leadingIcon?: ReactNode;
  /** Renders the value in the monospace face, for identifiers and paths. */
  readonly mono?: boolean;
  readonly inputSize?: InputSize;
}

/**
 * Labelling is mandatory: either a visible `label` or an `aria-label`, so an
 * unlabelled text field is a type error rather than an accessibility bug.
 */
export type InputProps = InputBaseProps &
  ({ readonly label: string } | { readonly label?: undefined; readonly "aria-label": string });

/**
 * A single-line text input.
 *
 * Controlled through the standard `value`/`onChange` props. The wrapper draws
 * the focus ring with `focus-within` so the ring surrounds the whole box, and
 * `aria-describedby` links the hint and error text to the field.
 */
export function Input({
  label,
  hint,
  error,
  leadingIcon,
  mono = false,
  inputSize = "md",
  className,
  id,
  disabled,
  ref,
  ...rest
}: InputProps) {
  const generatedId = useId();
  const controlId = id ?? generatedId;
  const hintId = `${controlId}-hint`;
  const errorId = `${controlId}-error`;
  const describedBy =
    [hint === undefined ? null : hintId, error === undefined ? null : errorId].filter(Boolean).join(" ") ||
    undefined;

  return (
    <div className={cn("flex min-w-0 flex-col gap-4", className)}>
      {label === undefined ? null : (
        <label
          htmlFor={controlId}
          className={cn("text-12 text-text-secondary select-none", disabled && "opacity-50")}
        >
          {label}
        </label>
      )}
      <span
        className={cn(
          "flex min-w-0 items-center gap-8 rounded-6 border bg-raised",
          error === undefined ? "border-border" : "border-danger",
          "has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-offset-2 has-[:focus-visible]:outline-focus",
          disabled === true && "opacity-60",
          SIZE_CLASSES[inputSize],
        )}
      >
        {leadingIcon === undefined ? null : (
          <span aria-hidden="true" className="flex shrink-0 items-center text-text-muted">
            {leadingIcon}
          </span>
        )}
        <input
          ref={ref}
          {...rest}
          id={controlId}
          data-slot="input"
          disabled={disabled}
          aria-invalid={error === undefined ? undefined : true}
          aria-describedby={describedBy}
          className={cn(
            "h-full w-full min-w-0 border-0 bg-transparent p-0 text-text placeholder:text-text-muted",
            "focus-visible:outline-none disabled:cursor-not-allowed",
            mono ? "font-mono text-12" : "font-ui",
          )}
        />
      </span>
      {hint === undefined ? null : (
        <span id={hintId} className="text-11 text-text-muted">
          {hint}
        </span>
      )}
      {error === undefined ? null : (
        <span id={errorId} className="text-11 text-danger">
          {error}
        </span>
      )}
    </div>
  );
}
