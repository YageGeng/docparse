import { mkdir, writeFile } from "node:fs/promises";
import openapiTS, { astToString } from "openapi-typescript";
import { loadEnv } from "vite";

// Generate from the real utoipa document; normal frontend builds use this checked-in declaration file.
const env = loadEnv("development", process.cwd(), "");
const url =
  process.env.DOCPARSE_OPENAPI_URL ||
  env.DOCPARSE_OPENAPI_URL ||
  "http://127.0.0.1:8080/api/openapi.json";
const response = await fetch(url);
if (!response.ok)
  throw new Error(`OpenAPI request failed: HTTP ${response.status}`);
const document = await response.json();
if (
  !document.components?.schemas?.DocumentResult ||
  !document.components?.schemas?.JobList
) {
  throw new Error(
    "The server schema is missing the workbench endpoints; rebuild docparse-server first.",
  );
}
await mkdir("src/api", { recursive: true });
await writeFile(
  "src/api/schema.d.ts",
  "/** Generated from docparse-server's utoipa OpenAPI document. Do not edit by hand. */\n" +
    astToString(await openapiTS(document)),
);
console.log("Generated src/api/schema.d.ts from the server's OpenAPI document");
