import { defineConfig, devices } from "@playwright/test";

// Real gateway (web/server.py) on a loopback port, fake `nebula` CLI and synthetic Pi files.
const PORT = 8790;

export default defineConfig({
  testDir: "../e2e",
  outputDir: "../../.scratch/mobile-chat-e2e/results",
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    ...devices["Desktop Chrome"],
    locale: "ro-RO",
    trace: "retain-on-failure",
  },
  webServer: {
    command: `python3 ../e2e/serve.py`,
    url: `http://127.0.0.1:${PORT}/`,
    reuseExistingServer: false,
    timeout: 30_000,
    env: { E2E_PORT: String(PORT) },
  },
});
