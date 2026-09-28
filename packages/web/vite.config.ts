import { fileURLToPath, URL } from "node:url";
import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

/**
 * Keeps browser URLs identical in development and production.
 *
 * The server mounts the WebUI at `{api_prefix}/webui`, so the build shares that base
 * path and every asset URL stays absolute. `VITE_BASE_PATH` overrides the derived base
 * for an unusual deployment.
 */
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const prefix = (env.VITE_API_PREFIX ?? "/api/v1/docparse").replace(/\/$/, "");
  const target = env.VITE_API_TARGET || "http://127.0.0.1:8080";
  // Only API routes are proxied: the WebUI path itself must be served by Vite.
  const apiRoutes = "jobs|monitoring|health|ready|docs|openapi\\.json";
  return {
    plugins: [react(), tailwindcss()],
    base: env.VITE_BASE_PATH || `${prefix}/webui/`,
    resolve: {
      alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
    },
    server: {
      proxy: {
        [`^${prefix}/(?:${apiRoutes})(?:[/?]|$)`]: {
          target,
          changeOrigin: true,
          // SSE must remain a stream even while model jobs run for several minutes.
          timeout: 0,
          proxyTimeout: 0,
        },
        "^/metrics(?:[/?]|$)": { target, changeOrigin: true },
      },
    },
  };
});
