import { createServer } from "node:http";
import { createReadStream } from "node:fs";
import { realpath, stat } from "node:fs/promises";
import { dirname, resolve, sep, extname } from "node:path";
import { fileURLToPath } from "node:url";
import { pipeline } from "node:stream/promises";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const routes = new Map([
  ["/example/", resolve(packageRoot, "example")],
  ["/dist/", resolve(packageRoot, "dist")],
  ["/models/slanet-plus/", resolve(packageRoot, "../../models/slanet-plus")],
  ["/models/rtdetr-table-cell-wireless/", resolve(packageRoot, "../../models/rtdetr-table-cell-wireless")],
  ["/models/pp-formulanet-plus-s/", resolve(packageRoot, "../../models/pp-formulanet-plus-s")],
  ["/models/pp-formulanet-plus-m/", resolve(packageRoot, "../../models/pp-formulanet-plus-m")],
  ["/models/pp-formulanet-plus-l/", resolve(packageRoot, "../../models/pp-formulanet-plus-l")],
  // Specific OCR routes precede the legacy layout-model prefix.
  ...["pp-ocrv6-medium-det", "pp-ocrv6-medium-rec", "pp-lcnet-textline-ori"].map(name => [`/models/${name}/`, resolve(packageRoot, "../../models", name)]),
  ["/models/", resolve(packageRoot, "../../models/pp-doclayout-v3")],
]);
const mime = { ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".mjs": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".json": "application/json" };
const port = Number(process.env.PORT ?? 8768);

/** Serves only the example, built SDK and model artifacts; PDFs never reach this server. */
const server = createServer(async (request, response) => {
  try {
    const path = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
    if (path === "/" || path === "/example") { response.writeHead(302, { Location: "/example/" }); response.end(); return; }
    if (!["GET", "HEAD"].includes(request.method)) { response.writeHead(405); response.end(); return; }
    const route = [...routes].find(([prefix]) => path.startsWith(prefix));
    if (!route) { response.writeHead(404); response.end(); return; }
    const [prefix, directory] = route;
    const root = await realpath(directory);
    const file = await realpath(resolve(root, path.slice(prefix.length) || "index.html"));
    if (!file.startsWith(root + sep)) { response.writeHead(403); response.end(); return; }
    const info = await stat(file);
    if (!info.isFile()) { response.writeHead(404); response.end(); return; }
    response.writeHead(200, { "Content-Type": mime[extname(file)] ?? "application/octet-stream", "Content-Length": info.size, "Cache-Control": "no-cache" });
    if (request.method === "HEAD") response.end();
    else await pipeline(createReadStream(file), response);
  } catch (error) {
    if (!response.headersSent) response.writeHead(error.code === "ENOENT" ? 404 : 400);
    response.end();
  }
});
server.listen(port, "127.0.0.1", () => console.log(`DocParse example: http://127.0.0.1:${server.address().port}/example/`));
