import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  /** What this part of the page is, as the user would call it. */
  name: string;
  /** When this changes the part is drawn afresh: what broke it may be gone with the 分身 or the page it was on. */
  resetKey?: unknown;
  /** Classes for the box shown in the part's place, so that it holds that place in the layout. */
  className?: string;
  /** This part is the whole page: nothing else is left to use, so only reading the page again is offered. */
  whole?: boolean;
  children: ReactNode;
}

interface State {
  error: Error | null;
  /** Which components the error came up through, as React names them. */
  through: string;
  copied: boolean;
}

/**
 * Keeps an error in drawing one part of the page from taking the whole page with it. React, left to itself,
 * takes everything off the screen when any component throws while it is drawn: a white window, with the 分身
 * still at work behind it and no way to reach it. A part that throws inside a guard is replaced, there and only
 * there, by a note that says so, with a way to draw it again and the error itself for whoever is asked about it.
 * The 分身 and its log are no part of the page, and are untouched.
 */
export class Guard extends Component<Props, State> {
  state: State = { error: null, through: "", copied: false };

  static getDerivedStateFromError(error: unknown): Partial<State> {
    return { error: error instanceof Error ? error : new Error(String(error)) };
  }

  componentDidCatch(error: unknown, info: ErrorInfo) {
    console.error(`The page could not draw ${this.props.name}:`, error, info.componentStack);
    this.setState({ through: info.componentStack ?? "" });
  }

  componentDidUpdate(before: Props) {
    if (this.state.error && before.resetKey !== this.props.resetKey) this.again();
  }

  again = () => this.setState({ error: null, through: "", copied: false });

  render() {
    const { error, through, copied } = this.state;
    if (!error) return this.props.children;
    const { name, whole, className = "" } = this.props;
    // Where it was thrown from begins, in this browser, with the error's own words: they are not said twice.
    const said = `${error.name}: ${error.message}`;
    const from = error.stack ?? "";
    const detail = `${from.startsWith(said) ? from : `${said}\n${from}`}${through ? `\n\nReact 组件：${through}` : ""}`;
    return (
      <div className={`broken ${whole ? "whole" : ""} ${className}`} role="alert">
        <div className="broken-head">
          <span className="seal" aria-hidden="true">
            误
          </span>
          <h2>{whole ? "页面画不出来了" : `「${name}」这一块画不出来了`}</h2>
        </div>
        <p>
          {whole ? "界面出了错，这一页没法显示。" : "界面的这一块出了错，别处照常能用。"}
          分身不受影响：它在后台该做什么还在做，对话的记录也都在。
          {whole ? "重新载入页面，通常就好了。" : "可以再画一次；还不行，就重新载入页面。"}
        </p>
        <div className="actions">
          {!whole && (
            <button type="button" className="button outline compact" onClick={this.again}>
              再画一次
            </button>
          )}
          <button type="button" className={`button compact ${whole ? "primary" : "text"}`} onClick={() => window.location.reload()}>
            重新载入页面
          </button>
        </div>
        <details className="broken-detail">
          <summary>出错的细节（反馈问题时附上）</summary>
          <pre>{detail}</pre>
          <button
            type="button"
            className="button text compact"
            onClick={() => navigator.clipboard.writeText(detail).then(() => this.setState({ copied: true }), () => undefined)}
          >
            {copied ? "已复制" : "复制细节"}
          </button>
        </details>
      </div>
    );
  }
}
