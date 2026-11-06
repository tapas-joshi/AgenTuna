import type { JSX } from "react";
import { useId, useMemo, useState } from "react";
import { Button } from "../button";
import { cn } from "../cn";
import { createBrowserClipboard } from "./clipboard";
import { Skeleton } from "./skeleton";
import { Spinner } from "./spinner";
import {
  STATE_VARIANT_LABEL,
  type CopyState,
  type StateDetails,
  type StateViewProps,
  type StateVariant,
} from "./types";

const COPY_LABEL: Readonly<Record<CopyState, string>> = {
  idle: "Copy details",
  copied: "Copied",
  failed: "Copy failed",
};

const ICON_TONE: Readonly<Record<StateVariant, string>> = {
  loading: "text-text-muted",
  empty: "text-text-muted",
  error: "text-danger",
  offline: "text-warning",
};

const ICON_BOX: Readonly<Record<StateVariant, string>> = {
  loading: "border-border bg-raised",
  empty: "border-border bg-raised",
  error: "border-danger/40 bg-danger/10",
  offline: "border-warning/40 bg-warning/10",
};

/** Decorative glyph for a variant. Shape only; the title carries the meaning. */
function VariantGlyph({ variant }: { readonly variant: StateVariant }): JSX.Element | null {
  if (variant === "loading") {
    return null;
  }
  return (
    <svg
      aria-hidden="true"
      data-glyph={variant}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      focusable="false"
      className="size-24"
    >
      {variant === "empty" ? (
        <>
          <path d="M4 10h4l1.5 2.5h5L16 10h4" />
          <path d="M5.5 6.5h13l1.5 5v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1v-5z" />
        </>
      ) : null}
      {variant === "error" ? (
        <>
          <circle cx="12" cy="12" r="8" />
          <path d="M12 8v4.5" />
          <circle cx="12" cy="16" r="0.75" fill="currentColor" stroke="none" />
        </>
      ) : null}
      {variant === "offline" ? (
        <>
          <path d="M7 17.5a3.5 3.5 0 0 1-.3-6.99A4.5 4.5 0 0 1 15.5 9.6a3.75 3.75 0 0 1 1.9 6.9" />
          <path d="M4 4l16 16" />
        </>
      ) : null}
    </svg>
  );
}

interface DetailsDrawerProps {
  readonly details: StateDetails;
  readonly clipboard: StateViewProps["clipboard"];
}

/** Collapsed-by-default technical payload with Copy details. */
function DetailsDrawer({ details, clipboard }: DetailsDrawerProps): JSX.Element {
  const [open, setOpen] = useState(false);
  const [copyState, setCopyState] = useState<CopyState>("idle");
  const panelId = useId();
  const port = useMemo(() => clipboard ?? createBrowserClipboard(), [clipboard]);

  const copy = async (): Promise<void> => {
    try {
      await port.writeText(details.text);
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
  };

  return (
    <div
      data-slot="state-details"
      className="w-full max-w-[560px] rounded-6 border border-border bg-surface text-left"
    >
      <button
        type="button"
        aria-expanded={open}
        aria-controls={panelId}
        onClick={() => {
          setOpen((previous) => !previous);
        }}
        className="flex w-full items-center justify-between gap-8 rounded-6 px-12 py-8 text-12 font-medium text-text-secondary hover:bg-overlay focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus"
      >
        <span>{details.summary ?? "Technical details"}</span>
        <svg
          aria-hidden="true"
          viewBox="0 0 12 12"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
          strokeLinejoin="round"
          focusable="false"
          className={cn("size-12 shrink-0 transition-transform", open && "rotate-90")}
        >
          <path d="M4.5 2.5 8 6l-3.5 3.5" />
        </svg>
      </button>
      {open ? (
        <div id={panelId} className="flex flex-col gap-8 border-t border-border px-12 py-8">
          <pre className="m-0 max-h-[240px] overflow-auto font-mono text-12 break-words whitespace-pre-wrap text-text-muted">
            {details.text}
          </pre>
          <div className="flex flex-wrap items-center gap-8">
            <Button
              size="sm"
              variant="secondary"
              onClick={() => {
                void copy();
              }}
            >
              {COPY_LABEL[copyState]}
            </Button>
            {copyState === "idle" ? null : (
              <span role="status" aria-live="polite" className="sr-only">
                {copyState === "copied" ? "Details copied" : "Copy failed"}
              </span>
            )}
          </div>
        </div>
      ) : null}
    </div>
  );
}

/**
 * Renders the loading, empty, error or offline state of a region.
 *
 * Controlled: it owns disclosure and copy feedback only; navigation and
 * retries go through typed callbacks. Swap `minHeight` in so replacing a table
 * with an empty state does not move the page.
 *
 * @example
 * ```tsx
 * <StateView
 *   variant="empty"
 *   title="No skills in this project"
 *   body="Import an existing skill or create one from scratch."
 *   actions={[{ label: "Add skill", onClick: openAddSkill }]}
 *   minHeight={320}
 * />
 * ```
 */
export function StateView({
  variant,
  title,
  body,
  icon,
  actions = [],
  details,
  minHeight,
  skeletonRows,
  ariaLive,
  headingLevel = "h2",
  clipboard,
  className,
}: StateViewProps): JSX.Element {
  const Heading = headingLevel;
  const showSkeleton = variant === "loading" && skeletonRows !== undefined && skeletonRows > 0;
  const assertive = ariaLive === "assertive" || (ariaLive === undefined && variant === "error");

  return (
    <section
      data-slot="state-view"
      data-variant={variant}
      aria-busy={variant === "loading" ? true : undefined}
      style={minHeight === undefined ? undefined : { minHeight }}
      className={cn(
        "flex w-full flex-col items-center justify-center gap-16 px-24 py-32 text-center",
        className,
      )}
    >
      {ariaLive === "off" ? null : (
        <p
          data-slot="state-announcement"
          role={assertive ? "alert" : "status"}
          aria-live={assertive ? "assertive" : "polite"}
          aria-atomic="true"
          className="sr-only"
        >
          {`${STATE_VARIANT_LABEL[variant]}: ${title}`}
        </p>
      )}

      {showSkeleton ? null : (
        <span
          data-slot="state-icon"
          className={cn(
            "flex shrink-0 items-center justify-center rounded-full border p-12",
            ICON_TONE[variant],
            ICON_BOX[variant],
          )}
        >
          {icon ?? <VariantGlyph variant={variant} />}
          {variant === "loading" ? <Spinner label={title} /> : null}
        </span>
      )}

      <div className="flex max-w-[440px] flex-col items-center gap-4">
        <Heading className="m-0 text-16 font-semibold text-text">{title}</Heading>
        {body === undefined ? null : <div className="text-13 text-text-muted">{body}</div>}
      </div>

      {actions.length === 0 ? null : (
        <div className="flex flex-wrap items-center justify-center gap-8">
          {actions.map((action, index) => (
            <Button
              key={action.label}
              variant={action.variant ?? (index === 0 ? "primary" : "secondary")}
              loading={action.loading}
              disabled={action.disabled}
              onClick={action.onClick}
            >
              {action.label}
            </Button>
          ))}
        </div>
      )}

      {showSkeleton ? <Skeleton rows={skeletonRows} className="max-w-[560px]" /> : null}
      {details === undefined ? null : <DetailsDrawer details={details} clipboard={clipboard} />}
    </section>
  );
}
