import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  workers: process.env.CI ? 4 : undefined,
  snapshotPathTemplate: "{testDir}/snapshots/{projectName}/{arg}{ext}",
  // Keep attached screen captures in the HTML report; the custom reporter
  // merges expected/actual/diff into one image per failed comparison.
  reporter: [["list"], ["./e2e/support/side-by-side-reporter.ts"], ["html", { open: "never" }]],
  use: {
    baseURL: "http://127.0.0.1:4173",
    ...devices["Desktop Chrome"],
    viewport: { width: 1440, height: 1000 },
    colorScheme: "dark",
    locale: "en-US",
    timezoneId: "UTC",
    reducedMotion: "reduce",
    screenshot: "only-on-failure",
  },
  projects: [
    { name: "1x", use: { deviceScaleFactor: 1 } },
    { name: "2x", use: { deviceScaleFactor: 2 } },
  ],
  webServer: {
    command: `${process.env.PLAYWRIGHT_SKIP_BUILD === "1" ? "" : "pnpm run build && "}pnpm exec vite preview --host 127.0.0.1 --port 4173 --strictPort`,
    url: "http://127.0.0.1:4173",
    reuseExistingServer: !process.env.CI,
    timeout: 300_000,
  },
  expect: {
    toHaveScreenshot: { animations: "disabled", maxDiffPixelRatio: 0.001 },
  },
});
