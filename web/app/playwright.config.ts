import { createHash } from "node:crypto";
import path from "node:path";
import { defineConfig, devices } from "@playwright/test";

// Real gateway (web/server.py) on a loopback port, fake `nebula` CLI and synthetic Pi files.
// Workers reload this file without the main process's argv, so the lane travels in the environment.
const asked = process.argv.some((arg, i, argv) => arg === "--project=real" || (arg === "--project" && argv[i + 1] === "real"));
process.env.NEBULA_E2E_LANE ??= asked ? "real" : "fake";
const real = process.env.NEBULA_E2E_LANE === "real";
const checkout = path.resolve(import.meta.dirname, "../..");
process.env.REAL_NEBULA_STATE ??= `/tmp/nre-${createHash("sha1").update(checkout).digest("hex").slice(0, 8)}`;
process.env.REAL_NEBULA_BIN ??= path.join(checkout, "target/verify-nebula/debug/nebula");
const PORT = real ? 8791 : 8790;
const scratch = real ? "../../.scratch/real-e2e" : "../../.scratch/mobile-chat-e2e";

export default defineConfig({
  testDir: "../e2e",
  outputDir: `${scratch}/results`,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"], ["../e2e/e2e-run-reporter.ts", { outputDir: `${scratch}/e2e-run`, title: real ? "web e2e, real lane" : "web e2e" }]],
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    ...devices["Desktop Chrome"],
    locale: "ro-RO",
    trace: "retain-on-failure",
    screenshot: "on",
  },
  ...(real ? { projects: [{ name: "real", testDir: "../e2e/real" }] } : { testIgnore: "**/real/**" }),
  webServer: {
    command: real ? "python3 ../e2e/serve.py --real" : `python3 ../e2e/serve.py`,
    url: `http://127.0.0.1:${PORT}/`,
    reuseExistingServer: false,
    timeout: 30_000,
    env: { E2E_PORT: String(PORT) },
    ...(real ? { gracefulShutdown: { signal: "SIGTERM" as const, timeout: 10_000 } } : {}),
  },
});
