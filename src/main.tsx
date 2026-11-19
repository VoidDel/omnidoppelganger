import React from "react";
import ReactDOM from "react-dom/client";
import "lxgw-wenkai-lite-webfont/lxgwwenkailite-regular.css";
import "lxgw-wenkai-lite-webfont/lxgwwenkailite-bold.css";
import "@fontsource/atkinson-hyperlegible-mono/400.css";
import "@fontsource/atkinson-hyperlegible-mono/700.css";
import "./styles.css";
import App from "./App";
import { Guard } from "./components/Guard";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {/* The last guard: what no guard inside the app caught still leaves a page that says what happened. */}
    <Guard name="页面" whole>
      <App />
    </Guard>
  </React.StrictMode>,
);
