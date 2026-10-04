import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

// Tauri expects a fixed dev port; `clearScreen: false` keeps Rust errors visible.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: "es2021",
    outDir: "dist",
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
});
