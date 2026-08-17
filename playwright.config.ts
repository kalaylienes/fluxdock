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
  // Two projects, because one of these specs asserts and the other produces.
  // `docs` writes the README images into a tracked folder, so running it as
  // part of an ordinary test run turns every run into a diff nobody asked for.
  projects: [
    { name: "suite", testIgnore: "docs-shots.spec.ts" },
    { name: "docs", testMatch: "docs-shots.spec.ts" },
  ],
  webServer: {
    command: "npx vite --port 1420 --strictPort",
    url: "http://localhost:1420",
    reuseExistingServer: true,
    timeout: 60_000,
  },
});
