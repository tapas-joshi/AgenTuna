import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  snapshotPathTemplate: "{testDir}/snapshots/{projectName}/{arg}{ext}",
  // CI keeps the HTML report as an artifact next to Playwright's own diff files; the side-by-side
  // reporter merges expected/actual/diff into one image per failed comparison.
  reporter: process.env.CI
    ? [["list"], ["html", { open: "never" }], ["./e2e/support/side-by-side-reporter.ts"]]
    : [["list"], ["./e2e/support/side-by-side-reporter.ts"]],
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
    command: `${process.env.PLAYWRIGHT_SKIP_BUILD === "1" ? "" : "pnpm run build && "}pnpm run preview -- --host 127.0.0.1 --port 4173 --strictPort`,
    url: "http://127.0.0.1:4173",
    reuseExistingServer: !process.env.CI,
    timeout: 300_000,
  },
  expect: {
    toHaveScreenshot: { animations: "disabled", maxDiffPixelRatio: 0.001 },
  },
});
