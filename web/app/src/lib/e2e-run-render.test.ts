import test from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

test("the e2e-run view shows the run's own outcome apart from its step summary", async () => {
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)),
    server: { middlewareMode: true }, appType: "custom" });
  try {
    const { E2eRunView } = await server.ssrLoadModule("/src/components/board/E2eRunView.tsx");
    const render = (status: string) => renderToStaticMarkup(createElement(E2eRunView, { assetUrl: (p: string) => p,
      run: { title: "web e2e", status, started_at: 1764000000000, duration_ms: 900, steps: [{ name: "drag", status: "passed", duration_ms: 900 }] } }));
    const failed = render("failed");
    assert.match(failed, /Rulare eșuată/);
    assert.match(failed, /Toți cei 1 pași au trecut/);
    assert.match(render("passed"), /Rulare trecută/);
  } finally {
    await server.close();
  }
});
