import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    strictPort: true,
    fs: {
      // The wasm decoder lives at ../crates/keeper-decoder/pkg (outside the
      // frontend root). Live mode (`?feed=live`) imports its .wasm at runtime,
      // so the dev server must be allowed to serve files from the repo root.
      allow: [".."],
    },
  },
});
