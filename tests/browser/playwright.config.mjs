import { existsSync } from "node:fs";

import { defineConfig, devices } from "@playwright/test";

const executablePath = process.env.CELLAR_CHROMIUM
  || (existsSync("/usr/bin/chromium") ? "/usr/bin/chromium" : undefined);

export default defineConfig({
  testDir: ".",
  timeout: 60_000,
  fullyParallel: false,
  reporter: [["list"], ["html", { open: "never" }]],
  use: {
    baseURL: "http://127.0.0.1:18081",
    browserName: "chromium",
    launchOptions: executablePath ? { executablePath } : undefined,
    headless: true,
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "desktop",
      use: {
        ...devices["Desktop Chrome"],
        baseURL: "http://127.0.0.1:18081",
        viewport: { width: 1280, height: 900 },
      },
    },
    {
      name: "mobile",
      use: {
        ...devices["Pixel 7"],
        baseURL: "http://127.0.0.1:18082",
        viewport: { width: 412, height: 915 },
      },
    },
  ],
});
