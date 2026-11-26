import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
// @ts-expect-error type error without @types/node package
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

/**
 * Font stylesheets (@fontsource and KaTeX) list fallback `.woff` and `.ttf` copies
 * beside `.woff2`. WebView2, WKWebView, and WebKitGTK all support `.woff2`, so
 * stripping `.woff` and `.ttf` fallbacks keeps dead font assets out of the bundle.
 */
const woff2Only = {
  name: "woff2-only",
  enforce: "pre" as const,
  transform(code: string, id: string) {
    if (!/(?:@fontsource|katex).*\.css/.test(id)) return null;
    return code
      .replace(/,\s*url\([^)]*\.woff\)\s*format\(['"]woff['"]\)/g, "")
      .replace(/,\s*url\([^)]*\.ttf\)\s*format\(['"]truetype['"]\)/g, "");
  },
};

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [react(), woff2Only],

  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes("node_modules/@xterm/")) return "xterm";
          if (id.includes("node_modules/highlight.js/")) return "highlight";
          if (
            id.includes("node_modules/katex/") ||
            id.includes("node_modules/rehype-katex/") ||
            id.includes("node_modules/remark-math/")
          ) {
            return "math";
          }
          if (
            id.includes("node_modules/react-markdown/") ||
            id.includes("node_modules/remark-gfm/") ||
            id.includes("node_modules/rehype-highlight/") ||
            id.includes("node_modules/micromark") ||
            id.includes("node_modules/mdast-") ||
            id.includes("node_modules/hast-") ||
            id.includes("node_modules/unified/")
          ) {
            return "markdown";
          }
          if (id.includes("node_modules/lucide-react/")) return "icons";
          return undefined;
        },
      },
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
