/** Validates formula selection before either the SDK or Worker downloads any model assets. */
export function formulaEngineError(value: unknown, enabled = true): string | undefined {
  if (value === undefined) return;
  if (!value || typeof value !== "object" || Array.isArray(value)) return "formula.engine must be an object";
  const engine = value as Record<string, unknown>;
  const keys = engine.type === "mineru" ? ["type", "server_url", "concurrency"] : ["type", "session_size"];
  if (!["pp", "texo", "mineru"].includes(String(engine.type)) || Object.keys(engine).some(key => !keys.includes(key))) return "formula.engine accepts pp, texo, or mineru and its engine-specific settings";
  // Session count describes independent local model consumers on native and browser backends.
  if (engine.type !== "mineru" && engine.session_size !== undefined && (typeof engine.session_size !== "number" || !Number.isSafeInteger(engine.session_size) || engine.session_size < 1 || engine.session_size > 8)) return "session_size must be an integer between 1 and 8";
  if (engine.type !== "mineru" || !enabled) return;
  try {
    if (typeof engine.server_url !== "string" || !engine.server_url.trim()) throw new Error("missing URL");
    const url = new URL(engine.server_url);
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error("invalid URL");
  } catch { return "MinerU server_url must be an absolute HTTP(S) URL without credentials, query, or fragment"; }
  if (engine.concurrency !== undefined && (typeof engine.concurrency !== "number" || !Number.isSafeInteger(engine.concurrency) || engine.concurrency < 1 || engine.concurrency > 1024)) return "MinerU concurrency must be an integer between 1 and 1024";
}
/** Validates shared runtime settings and explicit queue capacities before model assets are fetched. */
export function queueSizesError(value: unknown): string | undefined {
  const settings = value && typeof value === "object" ? value as Record<string, unknown> : undefined;
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
