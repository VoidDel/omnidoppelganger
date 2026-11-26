import { Component, type ComponentProps, type ComponentType, createElement, type ErrorInfo, lazy, type ReactNode } from "react";
import { formatMessage, getCurrentLocale } from "../i18n";

interface Props {
  /** What this part of the page is, as the user would call it. */
  name: string;
  /** When this changes the part is drawn afresh: what broke it may be gone with the 分身 or the page it was on. */
  resetKey?: unknown;
  /** Classes for the box shown in the part's place, so that it holds that place in the layout. */
  className?: string;
  /** This part is the whole page: nothing else is left to use, so only reading the page again is offered. */
  whole?: boolean;
  /** Optional dismiss handler for floating modals/palettes so an error inside them never blocks the main window. */
  onDismiss?: () => void;
  children: ReactNode;
}

interface State {
  error: Error | null;
  /** Which components the error came up through, as React names them. */
  through: string;
  copied: boolean;
}

const pendingLazyResets = new Set<() => void>();

function flushPendingLazyResets() {
  for (const reset of pendingLazyResets) {
    reset();
  }
  pendingLazyResets.clear();
}

/**
 * Wrap a dynamic `import()` factory so that if chunk loading rejects due to transient I/O or network failure,
 * clicking "Retry" inside `<Guard>` creates a fresh `React.lazy` instance and invokes `importer()` again
 * instead of re-throwing the rejected cached Promise.
 */
// biome-ignore lint/suspicious/noExplicitAny: matches React.lazy's generic component constraint
export function lazyWithRetry<T extends ComponentType<any>>(
  importer: () => Promise<{ default: T }>,
): ComponentType<ComponentProps<T>> {
  const makeLazy = () =>
    lazy(() =>
      importer().catch((error) => {
        pendingLazyResets.add(() => {
          Inner = makeLazy();
        });
        throw error;
      }),
    );
  let Inner = makeLazy();
  function RetryableLazy(props: ComponentProps<T>) {
    const Current = Inner;
    return createElement(Current, props);
  }
  return RetryableLazy;
}

/**
 * Keeps an error in drawing one part of the page from taking the whole page with it.
 */
export class Guard extends Component<Props, State> {
  state: State = { error: null, through: "", copied: false };

  static getDerivedStateFromError(error: unknown): Partial<State> {
    return { error: error instanceof Error ? error : new Error(String(error)) };
  }

  componentDidCatch(error: unknown, info: ErrorInfo) {
    flushPendingLazyResets();
    console.error(`The page could not draw ${this.props.name}:`, error, info.componentStack);
    this.setState({ through: info.componentStack ?? "" });
  }

  componentDidUpdate(before: Props) {
    if (this.state.error && before.resetKey !== this.props.resetKey) this.again();
  }

  again = () => {
    flushPendingLazyResets();
    this.setState({ error: null, through: "", copied: false });
  };

  render() {
    const { error, through, copied } = this.state;
    if (!error) return this.props.children;
    const { name, whole, className = "", onDismiss } = this.props;
    const loc = getCurrentLocale();
    const said = `${error.name}: ${error.message}`;
    const from = error.stack ?? "";
    const detail = `${from.startsWith(said) ? from : `${said}\n${from}`}${through ? `\n\nReact: ${through}` : ""}`;
    return (
      <div className={`broken ${whole ? "whole" : ""} ${className}`} role="alert">
        <div className="broken-head">
          <span className="seal" aria-hidden="true">
            {formatMessage(loc, "guard.seal")}
          </span>
          <h2>
            {whole
              ? formatMessage(loc, "guard.wholeTitle")
              : formatMessage(loc, "guard.partTitle", { name })}
          </h2>
        </div>
        <p>
          {whole ? formatMessage(loc, "guard.wholeLead") : formatMessage(loc, "guard.partLead")}{" "}
          {formatMessage(loc, "guard.agentSafe")}{" "}
          {whole ? formatMessage(loc, "guard.wholeHint") : formatMessage(loc, "guard.partHint")}
        </p>
        <div className="actions">
          {!whole && (
            <button type="button" className="button outline compact" onClick={this.again}>
              {formatMessage(loc, "guard.retry")}
            </button>
          )}
          {onDismiss && (
            <button type="button" className="button outline compact" onClick={onDismiss}>
              {formatMessage(loc, "guard.dismiss")}
            </button>
          )}
          <button
            type="button"
            className={`button compact ${whole ? "primary" : "text"}`}
            onClick={() => window.location.reload()}
          >
            {formatMessage(loc, "guard.reload")}
          </button>
        </div>
        <details className="broken-detail">
          <summary>{formatMessage(loc, "guard.detailSummary")}</summary>
          <pre>{detail}</pre>
          <button
            type="button"
            className="button text compact"
            onClick={() =>
              navigator.clipboard
                .writeText(detail)
                .then(() => this.setState({ copied: true }), () => undefined)
            }
          >
            {copied ? formatMessage(loc, "guard.copied") : formatMessage(loc, "guard.copyDetail")}
          </button>
        </details>
      </div>
    );
  }
}
