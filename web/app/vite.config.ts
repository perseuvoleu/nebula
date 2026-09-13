import path from "node:path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Build output is served by web/server.py from web/static through a startup allowlist.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": path.resolve(import.meta.dirname, "./src") } },
  build: { outDir: "../static", emptyOutDir: true, sourcemap: false },
  server: { port: 5173, strictPort: true, host: "127.0.0.1" },
});
