import React from "react";
import ReactDOM from "react-dom/client";
import "lxgw-wenkai-lite-webfont/lxgwwenkailite-regular.css";
import "lxgw-wenkai-lite-webfont/lxgwwenkailite-bold.css";
import "@fontsource/noto-sans-sc/400.css";
import "@fontsource/noto-sans-sc/700.css";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/500.css";
import "@fontsource/jetbrains-mono/700.css";
import "katex/dist/katex.min.css";
import "./styles.css";
import App from "./App";
import { Guard } from "./components/Guard";

/**
 * The webview's own menu on a right click (back, reload, save as, print, inspect) belongs to a browser, not to this
 * app, and is kept away. Where it is of use it stays: in a field, for cutting and pasting, and over words picked out
 * on the page, for copying them.
 */
document.addEventListener("contextmenu", (event) => {
  const target = event.target instanceof Element ? event.target : null;
  const editable = target?.closest("input, textarea, [contenteditable]:not([contenteditable='false'])");
  const picked = !!window.getSelection()?.toString().trim();
  if (!editable && !picked) event.preventDefault();
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {/* The last guard: what no guard inside the app caught still leaves a page that says what happened. */}
    <Guard name="页面" whole>
      <App />
    </Guard>
  </React.StrictMode>,
);
