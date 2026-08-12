import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The port is fixed and `strictPort` is on because `tauri.conf.json` points the
// desktop window at http://localhost:1420. If Vite silently moved to 1421 the
// window would open on a blank page with no obvious cause.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    // The webview is a known, current engine — there is no old browser to
    // support, and shipping transpiled-down output would only make the bundle
    // larger and the stack traces worse.
    target: "esnext",
    sourcemap: true,
  },
});
