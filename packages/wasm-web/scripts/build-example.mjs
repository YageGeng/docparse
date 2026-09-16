import { build } from "esbuild";
import { fileURLToPath } from "node:url";

// Bundle presentation dependencies and fonts locally; the production parser SDK keeps its own assets.
await build({
  absWorkingDir: fileURLToPath(new URL("..", import.meta.url)),
  entryPoints: ["example/src/main.ts"],
  outfile: "example/dist/main.js",
  bundle: true,
  format: "esm",
  target: "es2022",
  external: ["../../dist/index.js"],
  loader: { ".woff2": "file", ".woff": "file", ".ttf": "file" },
  assetNames: "fonts/[name]-[hash]",
  minify: true,
});
