import { fileURLToPath, URL } from "node:url";
import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

/** Keeps browser API URLs identical in development and production, including a root API prefix. */
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const prefix = (env.VITE_API_PREFIX ?? "/api/v1/docparse").replace(/\/$/, "");
  return {
    plugins: [react(), tailwindcss()],
    base: env.VITE_BASE_PATH || "/",
    resolve: {
      alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
    },
    server: {
      proxy: {
        [prefix || "^/(jobs|monitoring|health|ready|openapi\\.json|docs)(?:[/?]|$)"]: {
          target: env.VITE_API_TARGET || "http://127.0.0.1:8080",
          changeOrigin: true,
          // SSE must remain a stream even while model jobs run for several minutes.
          timeout: 0,
          proxyTimeout: 0,
        },
      },
    },
  };
});
