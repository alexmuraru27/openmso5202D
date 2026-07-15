import { defineConfig } from "vite";

// Tauri expects a fixed dev-server port and ignores Vite's HMR websocket errors.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    // Match the Rust toolchain's supported targets; keep source maps in debug builds.
    target: "es2021",
    outDir: "dist",
    emptyOutDir: true,
  },
});
