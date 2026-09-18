/** Validates formula selection before either the SDK or Worker downloads any model assets. */
export function formulaEngineError(value: unknown, enabled = true): string | undefined {
  if (value === undefined) return;
  if (!value || typeof value !== "object" || Array.isArray(value)) return "formula.engine must be an object";
  const engine = value as Record<string, unknown>;
  const keys = engine.type === "mineru" ? ["type", "server_url", "concurrency"] : engine.type === "texo" ? ["type", "sessions"] : ["type"];
  if (!["pp", "texo", "mineru"].includes(String(engine.type)) || Object.keys(engine).some(key => !keys.includes(key))) return "formula.engine accepts pp, texo, or mineru and its engine-specific settings";
  if (engine.type === "texo" && engine.sessions !== undefined && engine.sessions !== 1) return "Browser Texo requires sessions = 1";
  if (engine.type !== "mineru" || !enabled) return;
  try {
    if (typeof engine.server_url !== "string" || !engine.server_url.trim()) throw new Error("missing URL");
    const url = new URL(engine.server_url);
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error("invalid URL");
  } catch { return "MinerU server_url must be an absolute HTTP(S) URL without credentials, query, or fragment"; }
  if (engine.concurrency !== undefined && (typeof engine.concurrency !== "number" || !Number.isSafeInteger(engine.concurrency) || engine.concurrency < 1 || engine.concurrency > 1024)) return "MinerU concurrency must be an integer between 1 and 1024";
}
