// A small client for the Chrome DevTools Protocol, enough to drive the app's
// window through WebView2's remote debugging port: evaluate script in the
// page, type, and press keys. Node's own WebSocket is all it needs.

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Connect to the page served on a debugging port, waiting for the app to open it. */
export async function connect(port, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs;
  let target;
  while (!target) {
    try {
      const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      target = targets.find((candidate) => candidate.type === "page" && candidate.webSocketDebuggerUrl);
    } catch {
      // The window is not up yet.
    }
    if (!target) {
      if (Date.now() > deadline) throw new Error(`no page appeared on debugging port ${port}`);
      await sleep(250);
    }
  }
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
    socket.addEventListener("error", () => reject(new Error("cannot open the debugging socket")), { once: true });
  });
  let next = 0;
  const waiting = new Map();
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    const pending = waiting.get(message.id);
    if (!pending) return;
    waiting.delete(message.id);
    if (message.error) pending.reject(new Error(message.error.message));
    else pending.resolve(message.result);
  });
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = ++next;
      waiting.set(id, { resolve, reject });
      socket.send(JSON.stringify({ id, method, params }));
    });

  /** Run an expression in the page and return its value. A thrown error rejects. */
  const evaluate = async (expression) => {
    const result = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
    return result.result.value;
  };

  /** Poll an expression until it is truthy, and return that value. */
  const waitFor = async (what, expression, timeout = 30_000) => {
    const until = Date.now() + timeout;
    for (;;) {
      const value = await evaluate(expression);
      if (value) return value;
      if (Date.now() > until) throw new Error(`timed out waiting for ${what}`);
      await sleep(150);
    }
  };

  /**
   * A page expression finding the enabled button with this text: one whose
   * text is exactly `text` if there is one, else the first that contains it.
   * Exact first, so "改动" finds the tab and not "撤回这一轮起的改动".
   */
  const button = (text) => {
    const wanted = JSON.stringify(text);
    const enabled = `[...document.querySelectorAll("button")].filter((b) => !b.disabled)`;
    return `(${enabled}.find((b) => b.textContent.trim() === ${wanted}) ?? ${enabled}.find((b) => b.textContent.includes(${wanted})))`;
  };

  return {
    evaluate,
    waitFor,
    /** Click the button with this text, waiting for it to appear. */
    click: async (text) => {
      await waitFor(`the button "${text}"`, `!!${button(text)}`);
      await evaluate(`${button(text)}.click()`);
    },
    /** Type into the element as a user would, so the page's own input handling runs. */
    type: async (selector, text) => {
      await waitFor(selector, `!!document.querySelector(${JSON.stringify(selector)})`);
      await evaluate(`document.querySelector(${JSON.stringify(selector)}).focus()`);
      await send("Input.insertText", { text });
    },
    /** Press and release Enter in the focused element. */
    enter: async () => {
      const key = { key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13 };
      await send("Input.dispatchKeyEvent", { type: "rawKeyDown", ...key });
      await send("Input.dispatchKeyEvent", { type: "keyUp", ...key });
    },
    /** Whether the page shows this text anywhere. */
    waitForText: (text, timeout) => waitFor(`the text "${text}"`, `document.body.innerText.includes(${JSON.stringify(text)})`, timeout),
    close: () => socket.close(),
  };
}
