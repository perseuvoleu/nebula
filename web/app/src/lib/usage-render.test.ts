import test from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

test('navbar renders both account indicators and an accessible detail trigger before data arrives', async () => {
  const server = await createServer({ root: fileURLToPath(new URL('../..', import.meta.url)),
    server: { middlewareMode: true }, appType: 'custom' });
  try {
    const { UsageNavbar } = await server.ssrLoadModule('/src/components/UsageNavbar.tsx');
    const html = renderToStaticMarkup(createElement(UsageNavbar));
    assert.match(html, /Săptămână/);
    assert.match(html, /Codex —/);
    assert.match(html, /Claude —/);
    assert.match(html, /aria-label="Detalii cote săptămânale Codex și Claude"/);
    assert.doesNotMatch(html, /100%/);
  } finally {
    await server.close();
  }
});
