import type { ParserProgress } from "./types.js";

/** Fetches one named artifact without logging its potentially credential-bearing URL. */
export async function artifact(name: string, url: string, progress?: (event: ParserProgress) => void): Promise<Uint8Array> {
  const response = await fetch(url);
  if (!response.ok) throw Object.assign(new Error(`${name} download failed with HTTP ${response.status}`), { code: "ArtifactFetch" });
  if (!progress || !response.body) return new Uint8Array(await response.arrayBuffer());
  const encoding = response.headers.get("content-encoding")?.trim().toLowerCase();
  const length = Number(response.headers.get("content-length"));
  // Fetch yields decoded bytes, but Content-Length describes the encoded body.
  // CORS may hide Content-Encoding while still exposing Content-Length, so absence
  // of that header is trustworthy only for a same-origin/basic response. An exposed
  // identity encoding can also establish that both counts use the same byte units.
  const unencoded = encoding === "identity" || (!encoding && response.type !== "cors");
  const total = unencoded && Number.isSafeInteger(length) && length > 0 ? length : undefined;
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let loaded = 0, lastReport = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value); loaded += value.byteLength;
    if (performance.now() - lastReport > 100) {
      progress({ stage: "downloading", artifact: name, loaded, total });
      lastReport = performance.now();
    }
  }
  progress({ stage: "downloading", artifact: name, loaded, total });
  const bytes = new Uint8Array(loaded);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  return bytes;
}
