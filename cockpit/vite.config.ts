/// <reference types="vitest/config" />
import { defineConfig, type ProxyOptions } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import pkg from "./package.json" with { type: "json" };

// Browser mode (dev server / e2e tests) talks to the agent and Coolify
// through these proxies, so no CORS setup is needed. Inside the Tauri app
// requests go through Rust commands instead.
function proxies(): Record<string, ProxyOptions> {
  const out: Record<string, ProxyOptions> = {};
  const agent = process.env.VERCELIFY_AGENT_URL;
  const coolify = process.env.VERCELIFY_COOLIFY_URL;
  if (agent) out["/__agent"] = { target: agent, changeOrigin: true, rewrite: (p) => p.replace(/^\/__agent/, "") };
  if (coolify) out["/__coolify"] = { target: coolify, changeOrigin: true, rewrite: (p) => p.replace(/^\/__coolify/, "") };
  return out;
}

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  // ".." lets tests read the shared fixtures in crates/vercelify-core.
  server: { port: 1420, strictPort: true, proxy: proxies(), fs: { allow: [".."] } },
  preview: { port: 1421, strictPort: true, proxy: proxies() },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
  build: {
    target: "es2022",
    sourcemap: false,
    chunkSizeWarningLimit: 900,
  },
  test: {
    globals: true,
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
