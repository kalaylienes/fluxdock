import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  timeout: 20_000,
  use: {
    baseURL: "http://localhost:1420",
    ...devices["Desktop Chrome"],
    // Matches the real logical size of the floating window.
    viewport: { width: 300, height: 88 },
  },
  webServer: {
    command: "npx vite --port 1420 --strictPort",
    url: "http://localhost:1420",
    reuseExistingServer: true,
    timeout: 60_000,
  },
});
