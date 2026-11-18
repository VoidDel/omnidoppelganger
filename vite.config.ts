import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
// @ts-expect-error type error without @types/node package
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

/**
 * The font packages list every file twice, as woff2 and as woff. The app runs in
 * WebView2 and WKWebView, which both read woff2, so the woff copies are left out
 * of the bundle: they would add several megabytes of Chinese glyphs nobody loads.
 */
const woff2Only = {
  name: "woff2-only",
  enforce: "pre" as const,
  transform(code: string, id: string) {
    if (!/@fontsource.*\.css/.test(id)) return null;
    return code.replace(/,\s*url\([^)]*\.woff\)\s*format\(['"]woff['"]\)/g, "");
  },
};

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [react(), woff2Only],

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
