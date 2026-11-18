import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/noto-serif-sc/wght.css";
import "@fontsource/ma-shan-zheng/400.css";
import "@fontsource/atkinson-hyperlegible-mono/400.css";
import "@fontsource/atkinson-hyperlegible-mono/700.css";
import "./styles.css";
import App from "./App";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
