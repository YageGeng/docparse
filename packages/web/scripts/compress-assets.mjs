import { readdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { promisify } from "node:util";
import { gzip } from "node:zlib";

// Precompressed siblings let static hosts serve gzip without spending CPU on every request.
const compress = promisify(gzip);
for (const entry of await readdir("dist", { recursive: true, withFileTypes: true })) {
  if (!entry.isFile() || !/\.(?:html|m?js|css|json|svg|wasm)$/.test(entry.name)) continue;
  const path = join(entry.parentPath, entry.name);
  const source = await readFile(path);
  if (source.length < 1024) continue;
  const encoded = await compress(source, { level: 6 });
  if (encoded.length < source.length) await writeFile(`${path}.gz`, encoded);
}
console.log("Prepared gzip assets in dist");
