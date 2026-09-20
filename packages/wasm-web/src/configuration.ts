/** Validates formula selection before either the SDK or Worker downloads any model assets. */
export function formulaEngineError(value: unknown, enabled = true): string | undefined {
  if (value === undefined) return;
  if (!Array.isArray(value) || (enabled && value.length === 0)) return "formula.engine must be a non-empty consumer list";
  for (const item of value) {
    if (!item || typeof item !== "object" || Array.isArray(item)) return "each formula consumer must be an object";
    const engine = item as Record<string, unknown>;
    const keys = engine.type === "http" ? ["type", "server_url", "worker_size", "prompt", "model"] : ["type", "worker_size", "batch_size"];
    if (!["pp", "texo", "http"].includes(String(engine.type)) || Object.keys(engine).some(key => !keys.includes(key))) return "formula consumers accept pp, texo, or http and their engine-specific settings";
    if (!enabled) continue;
    const workers = engine.worker_size;
    if (workers !== undefined && (typeof workers !== "number" || !Number.isSafeInteger(workers) || workers < 1 || (engine.type === "http" && workers > 1024))) return "worker_size must be a positive safe integer (at most 1024 for HTTP)";
    if (engine.type !== "http") {
      const batch = engine.batch_size;
      if (batch !== undefined && (typeof batch !== "number" || !Number.isSafeInteger(batch) || batch < 1 || batch > 32)) return "batch_size must be an integer between 1 and 32";
      continue;
    }
    try {
      if (typeof engine.server_url !== "string" || !engine.server_url.trim()) throw new Error("missing URL");
      const url = new URL(engine.server_url);
      if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error("invalid URL");
    } catch { return "HTTP server_url must be an absolute HTTP(S) URL without credentials, query, or fragment"; }
    if (engine.prompt !== undefined && (typeof engine.prompt !== "string" || !engine.prompt.trim())) return "HTTP prompt must be a non-empty string when configured";
    if (engine.model !== undefined && (typeof engine.model !== "string" || (engine.prompt !== undefined && !engine.model.trim()))) return "HTTP model must be a string and non-empty for chat completions";
  }
}
/** Validates shared runtime settings and explicit queue capacities before model assets are fetched. */
export function queueSizesError(value: unknown): string | undefined {
  const settings = value && typeof value === "object" ? value as Record<string, unknown> : undefined;
  const formula = settings?.formula;
  if (formula && typeof formula === "object" && Object.hasOwn(formula, "batch_size")) return "formula.batch_size moved into each local formula.engine entry";
  const render = settings?.render;
  if (!render || typeof render !== "object" || (render as Record<string, unknown>).workers !== 1) return "render.workers is required and must be 1 in a browser Worker";
  const runtime = settings?.runtime;
  if (runtime && typeof runtime === "object" && ["stage_pages", "render_queue_capacity", "blocking_task_limit", "page_limit"].some(key => Object.hasOwn(runtime, key))) return "Retired runtime capacity setting; use render.queue_size";
  // Keep the browser boundary aligned with Rust's four-level global enum before downloading models.
  if (runtime && typeof runtime === "object" && Object.hasOwn(runtime, "optimization_level") && !["level1", "level2", "level3", "all"].includes((runtime as Record<string, unknown>).optimization_level as string)) return "runtime.optimization_level must be level1, level2, level3, or all";
  if (runtime && typeof runtime === "object" && Object.hasOwn(runtime, "memory_pattern") && typeof (runtime as Record<string, unknown>).memory_pattern !== "boolean") return "runtime.memory_pattern must be a boolean";
  for (const path of ["render", "layout", "tsr", "tsr.cell_detection", "ocr.detection", "ocr.recognition", "ocr.orientation", "formula"]) {
    let section: unknown = value;
    for (const key of path.split(".")) section = section && typeof section === "object" ? (section as Record<string, unknown>)[key] : undefined;
    const size = section && typeof section === "object" ? (section as Record<string, unknown>).queue_size : undefined;
    if (typeof size !== "number" || !Number.isSafeInteger(size) || size < 1 || size > 536869887) return `${path}.queue_size is required and must be an integer between 1 and 536869887`;
  }
}
