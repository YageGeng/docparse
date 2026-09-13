import { cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";

// Avoid rewriting public assets on every build while Vite's development public-file index is in use.
const directories = ["cmaps", "standard_fonts", "iccs", "wasm"];
const { version } = JSON.parse(
  await readFile("node_modules/pdfjs-dist/package.json", "utf8"),
);
const signature = `${version}:${directories.join(",")}`;
const installed = await readFile("public/pdfjs/.version", "utf8").catch(
  (error) => {
    if (error.code === "ENOENT") return "";
    throw error;
  },
);
if (installed !== signature) {
  await rm("public/pdfjs", { recursive: true, force: true });
  await mkdir("public/pdfjs", { recursive: true });
  for (const directory of directories) {
    await cp(
      `node_modules/pdfjs-dist/${directory}`,
      `public/pdfjs/${directory}`,
      { recursive: true },
    );
  }
  await cp("node_modules/pdfjs-dist/LICENSE", "public/pdfjs/LICENSE");
  await writeFile("public/pdfjs/.version", signature);
}
