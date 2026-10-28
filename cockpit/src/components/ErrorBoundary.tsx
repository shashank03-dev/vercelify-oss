import { TriangleAlert } from "lucide-react";
import { Component, type ErrorInfo, type ReactNode } from "react";
import { Button, EmptyState } from "./ui";

/**
 * A render error in one page shows here instead of blanking the window.
 * `full` is the last line of defence around the whole app.
 */
export class ErrorBoundary extends Component<{ children: ReactNode; full?: boolean }, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.warn("render error", error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    const body = (
      <EmptyState
        icon={<TriangleAlert />}
        title="This page hit a problem"
        action={
          <div className="flex gap-2">
            {!this.props.full && <Button onClick={() => this.setState({ error: null })}>Try again</Button>}
            <Button variant="primary" onClick={() => window.location.reload()}>
              Reload the app
            </Button>
          </div>
        }
      >
        <span data-testid="page-error" className="selectable font-mono text-xs">
          {error.message}
        </span>
      </EmptyState>
    );
    return this.props.full ? <div className="flex h-full items-center justify-center bg-bg text-fg">{body}</div> : body;
  }
}
