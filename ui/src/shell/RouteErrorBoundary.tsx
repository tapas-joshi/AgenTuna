/**
 * Route-level crash guard (TF-035 companion).
 *
 * Routed screens are lazy chunks: a failed fetch (asset hash changed under a
 * running app, interrupted load) rejects the `React.lazy` promise, and a
 * render-time crash inside a screen rejects with the screen's error. Without
 * a boundary React unmounts the whole tree and the window goes blank. This
 * boundary keeps the shell and overlays alive and offers a reload, which
 * re-fetches the entry chunk and the current asset hashes.
 */
import { Component, type ErrorInfo, type ReactNode } from "react";
import { ErrorState } from "../components/states";
import { commandErrorMessage } from "../lib/commandError";

/** Props accepted by {@link RouteErrorBoundary}. */
export interface RouteErrorBoundaryProps {
  /** The routed body to guard. */
  readonly children: ReactNode;
  /** Overrides the default recovery (a full reload); tests inject a spy. */
  readonly onRetry?: () => void;
}

interface RouteErrorBoundaryState {
  readonly error: unknown | null;
}

/** Catches chunk-fetch and render failures for the routed screen region. */
export class RouteErrorBoundary extends Component<RouteErrorBoundaryProps, RouteErrorBoundaryState> {
  state: RouteErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: unknown): RouteErrorBoundaryState {
    return { error };
  }

  override componentDidCatch(error: unknown, info: ErrorInfo): void {
    // Diagnostics only: chunk URLs and component stacks carry no secrets.
    console.error("Routed screen failed:", error, info.componentStack);
  }

  private readonly retry = (): void => {
    if (this.props.onRetry !== undefined) {
      this.props.onRetry();
      return;
    }
    window.location.reload();
  };

  override render(): ReactNode {
    if (this.state.error !== null) {
      return (
        <main aria-label="Screen error" className="mx-auto flex w-full max-w-[1200px] flex-col p-24">
          <ErrorState
            error={{
              code: "unavailable",
              message: commandErrorMessage(this.state.error, "The screen module failed to load."),
            }}
            title="This screen could not load"
            body="A screen module failed to load or render. The rest of AgenTuna is still running; Retry reloads the app to fetch the current screen bundle."
            onRetry={this.retry}
            minHeight={600}
          />
        </main>
      );
    }
    return this.props.children;
  }
}
