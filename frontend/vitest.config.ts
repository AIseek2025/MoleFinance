/**
 * Wave 14 — vitest configuration for the frontend test suite.
 *
 * The Borsh decoder tests are pure-Node (`environment: "node"`),
 * the WalletAdapter tests need a stubbed `window.solana`
 * (`environment: "jsdom"`), and the WebSocketFeedAdapter tests
 * inject a mock `Connection` so they're also pure-Node.
 *
 * We pick `jsdom` as the project default and override per-file with
 * `// @vitest-environment node` where appropriate. This keeps the
 * `window.*` surface available across the wallet tests without
 * forcing every test to opt in.
 */
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: false,
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    coverage: {
      enabled: false,
    },
  },
});
