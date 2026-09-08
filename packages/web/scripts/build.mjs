import { mkdir, readFile, writeFile, copyFile, readdir, stat, cp, rm } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { dirname, resolve, join } from "node:path";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(packageRoot, "../..");
const dist = join(packageRoot, "dist");

/** Runs a build step with exact arguments and stops when that step fails. */
function run(command, args, cwd = root) {
  const result = spawnSync(command, args, { cwd, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed with status ${result.status}`);
}

await mkdir(join(dist, "pkg"), { recursive: true });
await mkdir(join(dist, "ort"), { recursive: true });
// Remove only the retired demo entry points from older combined SDK/example builds.
for (const name of ["example.js", "example.d.ts"]) await rm(join(dist, name), { force: true });
const version = spawnSync("wasm-bindgen", ["--version"], { encoding: "utf8" });
if (version.status !== 0 || version.stdout.trim() !== "wasm-bindgen 0.2.125") {
  throw new Error("Install the pinned CLI: cargo install wasm-bindgen-cli --version 0.2.125 --locked");
}
run("cargo", ["build", "-p", "docparse-web", "--target", "wasm32-unknown-unknown", "--release", "--locked", "--target-dir", join(root, "target")]);
run("wasm-bindgen", [join(root, "target/wasm32-unknown-unknown/release/docparse_web.wasm"), "--out-dir", join(dist, "pkg"), "--target", "web"]);

const gluePath = join(dist, "pkg/docparse_web.js");
let glue = await readFile(gluePath, "utf8");
let patched = 0;
glue = glue.replace(/^import \* as (\w+) from ['"](env|wasi_snapshot_preview1)['"];?\r?$/gm, (_line, name, module) => {
  patched++;
  return `const ${name} = ${module === "env" ? "createEnvImports()" : "createWasiImports(() => wasm.memory)"};`;
});
if (!patched) throw new Error("wasm-bindgen glue has no recognized WASI imports; review the generated ABI before updating the patch");
// WebIDL text codecs reject resizable buffers even though typed-array operations accept
// them. Snapshot only UTF-8 strings at these two boundaries; tensor views remain borrowed.
// Match the pinned generator exactly so an upgrade cannot silently skip this adaptation.
const stringBoundaries = [
  [
    "return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));",
    "const view = getUint8ArrayMemory0().subarray(ptr, ptr + len);\n    return cachedTextDecoder.decode(view.buffer.resizable ? view.slice() : view);",
  ],
  [
    "const ret = cachedTextEncoder.encodeInto(arg, view);",
    `let ret;
        if (view.buffer.resizable) {
            const bytes = cachedTextEncoder.encode(arg);
            view.set(bytes);
            ret = { written: bytes.length };
        } else {
            ret = cachedTextEncoder.encodeInto(arg, view);
        }`,
  ],
];
for (const [original, replacement] of stringBoundaries) {
  if (glue.split(original).length !== 2) throw new Error("Unexpected wasm-bindgen text codec glue; review resizable-memory support before updating the patch");
  glue = glue.replace(original, replacement);
}
glue = 'import { createWasiImports, createEnvImports } from "../wasm_imports.js";\n' + glue;
await writeFile(gluePath, glue);
run(process.execPath, [join(packageRoot, "node_modules/typescript/bin/tsc")], packageRoot);

const { createWasiImports, createEnvImports } = await import(new URL("../dist/wasm_imports.js", import.meta.url));
const wasi = createWasiImports(() => { throw new Error("memory cannot be accessed during import validation"); });
const env = createEnvImports();
const binary = await readFile(join(dist, "pkg/docparse_web_bg.wasm"));
const imports = WebAssembly.Module.imports(new WebAssembly.Module(binary));
for (const entry of imports) {
  if (entry.module === "wasi_snapshot_preview1" && (entry.kind !== "function" || typeof wasi[entry.name] !== "function")) throw new Error(`Unsupported WASI import: ${entry.name}`);
  if (entry.module === "env" && (entry.name !== "__c_longjmp" || entry.kind !== "tag" || !env[entry.name])) throw new Error(`Unsupported env import: ${entry.name}`);
  if (!["wasi_snapshot_preview1", "env", "./docparse_web_bg.js", "wbg"].includes(entry.module)) throw new Error(`Unknown WASM import module: ${entry.module}`);
}

const runtimeFiles = ["ort.wasm.min.mjs", "ort-wasm-simd-threaded.mjs", "ort-wasm-simd-threaded.wasm", "ort.webgpu.min.mjs", "ort-wasm-simd-threaded.jsep.mjs", "ort-wasm-simd-threaded.jsep.wasm", "ort-wasm-simd-threaded.asyncify.mjs", "ort-wasm-simd-threaded.asyncify.wasm", "ort-wasm-simd-threaded.jspi.mjs", "ort-wasm-simd-threaded.jspi.wasm"];
const hashes = {};
for (const name of runtimeFiles) {
  const source = join(packageRoot, "node_modules/onnxruntime-web/dist", name);
  await copyFile(source, join(dist, "ort", name));
  hashes[`ort/${name}`] = createHash("sha256").update(await readFile(source)).digest("hex");
}
hashes["pkg/docparse_web_bg.wasm"] = createHash("sha256").update(binary).digest("hex");
await copyFile(join(packageRoot, "licenses/onnxruntime-LICENSE"), join(dist, "ort/LICENSE"));
await copyFile(join(packageRoot, "licenses/onnxruntime-ThirdPartyNotices.txt"), join(dist, "ort/ThirdPartyNotices.txt"));
await copyFile(join(root, "vendor/ort-web/LICENSE-MIT"), join(dist, "pkg/ort-web-LICENSE-MIT"));
await copyFile(join(root, "vendor/ort-web/LICENSE-APACHE"), join(dist, "pkg/ort-web-LICENSE-APACHE"));
const buildRoot = join(root, "target/wasm32-unknown-unknown/release/build");
const sdkOutputs = [];
for (const name of await readdir(buildRoot)) {
  if (!name.startsWith("docparse-pdfium-sys-")) continue;
  const path = join(buildRoot, name, "output");
  try { sdkOutputs.push({ text: await readFile(path, "utf8"), modified: (await stat(path)).mtimeMs }); }
  catch (error) { if (error.code !== "ENOENT") throw error; }
}
const sdkOutput = sdkOutputs.sort((a, b) => b.modified - a.modified)[0]?.text;
const libraryPath = sdkOutput?.match(/^cargo:lib_path=(.+)$/m)?.[1]?.trim();
if (!libraryPath) throw new Error("PDFium build metadata is missing; cannot package its license notices");
const sdkRoot = dirname(libraryPath);
await mkdir(join(dist, "pdfium"), { recursive: true });
await copyFile(join(sdkRoot, "LICENSE"), join(dist, "pdfium/LICENSE"));
await copyFile(join(sdkRoot, "VERSION"), join(dist, "pdfium/VERSION"));
await cp(join(sdkRoot, "licenses"), join(dist, "pdfium/licenses"), { recursive: true });
const pdfiumLibraries = Object.fromEntries((await readFile(join(root, "crates/pdfium-sys/wasm-libraries.sha256"), "utf8")).trim().split("\n").map(line => { const [hash, name] = line.split(/\s+/); return [name, hash]; }));
await writeFile(join(dist, "build-manifest.json"), JSON.stringify({ rustTarget: "wasm32-unknown-unknown", ort: "2.0.0-rc.13", ortWeb: "0.3.0+1.27", onnxruntimeWeb: "1.27.0", wasmBindgen: "0.2.125", pdfium: "chromium/8028", pdfiumLibraries, imports, hashes }, null, 2) + "\n");
console.log(`Built browser package at ${dist}; verified ${imports.length} WASM imports`);
