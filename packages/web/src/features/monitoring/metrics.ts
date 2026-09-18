import type { components } from "@/api/schema";

export type Sample = components["schemas"]["Sample"];
export type Snapshot = components["schemas"]["Snapshot"];
export type Chart = components["schemas"]["Chart"];
export type Matrix = {
  result: { metric: Record<string, string>; values: [number, string][] }[];
};

export const charts = {
  queue_wait: ["模型队列等待 P95", "秒"],
  admission_wait: ["模型提交背压等待 P95", "秒"],
  page_pressure: ["页面占用率", "比例"],
  oldest: ["最老任务年龄", "秒"],
  turnaround: ["任务总耗时 P95", "秒"],
  backlog: ["任务积压", "任务"],
  queue: ["模型队列占用率", "比例"],
  blocked: ["阻塞的提交者", "提交者"],
  throughput: ["任务吞吐", "任务/分钟"],
  wait: ["排队耗时 P95", "秒"],
  parse: ["解析耗时 P95", "秒"],
  inference: ["实际 ONNX 调用 P95", "秒"],
  workers: ["模型消费者忙碌率", "比例"],
  pages: ["页面吞吐", "页/分钟"],
} as const satisfies Record<Chart, readonly [string, string]>;
/** Keeps unknown values visibly distinct from valid zero measurements. */
export function formatMetric(value: number | undefined) {
  return value === undefined || !Number.isFinite(value)
    ? "—"
    : value.toLocaleString("zh-CN", { maximumFractionDigits: 3 });
}

/** Finds one bounded metric series without conflating different models or graphs. */
function value(
  samples: Sample[],
  name: string,
  labels: Record<string, string> = {},
) {
  return samples.find(
    (sample) =>
      sample.name === name &&
      Object.entries(labels).every(([key, val]) => sample.labels[key] === val),
  )?.value;
}

/** Interprets metric names and counter lifetimes once, leaving view components independent of the exporter. */
export function projectSnapshot(current?: Snapshot, before?: Snapshot) {
  const samples = current?.samples ?? [];
  const process = current?.process;
  const hasWorker = process?.role === "all" || process?.role === "worker";
  const interval =
    current && before ? current.collected_at - before.collected_at : 0;
  const fresh =
    current &&
    before &&
    interval > 0 &&
    interval <= 15 &&
    current.process?.id !== undefined &&
    current.process.id === before.process?.id;
  const deltas = fresh
    ? samples.flatMap((sample) => {
        const old = value(before.samples, sample.name, sample.labels);
        return old !== undefined && sample.value >= old
          ? [{ ...sample, value: sample.value - old }]
          : [];
      })
    : [];
  const completed = deltas.filter(
    (sample) => sample.name === "docparse_jobs_completed_total",
  );
  const parsedCount = value(deltas, "docparse_job_parse_seconds_count");
  return {
    process,
    hasWorker,
    collectedAt: current?.collected_at,
    historyAvailable: current?.history_available,
    dbStale:
      value(samples, "docparse_database_collection_success") !== 1 ||
      (current?.collected_at ?? 0) -
        (value(samples, "docparse_database_collection_timestamp_seconds") ??
          0) >
        15,
    cards: [
      {
        label: "任务吞吐",
        value:
          hasWorker && completed.length
            ? (completed.reduce((sum, sample) => sum + sample.value, 0) /
                interval) *
              60
            : undefined,
        unit: "任务/分钟 · 最近采样间隔",
      },
      {
        label: "平均解析耗时",
        value:
          hasWorker && parsedCount
            ? (value(deltas, "docparse_job_parse_seconds_sum") ?? 0) /
              parsedCount
            : undefined,
        unit: "秒 · 最近采样间隔完成的解析",
      },
      {
        label: "页面背压",
        value: hasWorker
          ? value(samples, "docparse_page_blocked_producers")
          : undefined,
        unit: "提交者 · 正在等待页面空位",
      },
      {
        label: "等待处理",
        value: value(samples, "docparse_jobs", { state: "queued" }),
        unit: "任务 · 数据库全局",
      },
      {
        label: "解析中",
        value: value(samples, "docparse_jobs", { state: "running" }),
        unit: "任务 · 有效租约",
      },
      {
        label: "待恢复",
        value: value(samples, "docparse_jobs", { state: "recovery" }),
        unit: "任务 · 租约过期",
      },
      {
        label: "最老排队",
        value: value(samples, "docparse_job_oldest_age_seconds", {
          state: "queued",
        }),
        unit: "秒 · 当前仍在等待",
      },
      {
        label: "页面占用",
        value: hasWorker
          ? value(samples, "docparse_page_slots_used")
          : undefined,
        unit: `页 / ${formatMetric(value(samples, "docparse_page_slots_capacity"))} 个容量`,
      },
      {
        label: "PDFium 文档",
        value: hasWorker
          ? value(samples, "docparse_pdfium_documents_active")
          : undefined,
        unit: `活动 / ${formatMetric(value(samples, "docparse_pdfium_workers_configured"))} 个 worker`,
      },
    ],
    queues: samples
      .filter((sample) => sample.name === "docparse_queue_capacity_items")
      .map((sample) => {
        const labels = { queue: sample.labels.queue };
        const used = value(samples, "docparse_queue_items", labels);
        return {
          name: labels.queue,
          capacity: sample.value,
          used,
          percent: used === undefined ? undefined : (used / sample.value) * 100,
          blocked: value(samples, "docparse_queue_blocked_producers", labels),
          enqueued: value(
            samples,
            "docparse_queue_enqueued_items_total",
            labels,
          ),
        };
      }),
    models: samples
      .filter((sample) => sample.name === "docparse_model_workers_configured")
      .map((sample) => {
        const labels = { model: sample.labels.model };
        return {
          name: labels.model,
          configured: sample.value,
          alive: value(samples, "docparse_model_workers_alive", labels),
          busy: value(samples, "docparse_model_workers_busy", labels),
          batchLimit: value(samples, "docparse_model_batch_limit", labels),
        };
      }),
    inference: samples
      .filter((sample) => sample.name === "docparse_onnx_active_calls")
      .map((sample) => {
        const identity = {
          model: sample.labels.model,
          graph: sample.labels.graph,
        };
        const batches = value(
          samples,
          "docparse_onnx_batch_items_count",
          identity,
        );
        const successes =
          value(samples, "docparse_onnx_run_seconds_count", {
            ...identity,
            outcome: "success",
          }) ?? 0;
        return {
          ...identity,
          successes,
          averageMs: successes
            ? ((value(samples, "docparse_onnx_run_seconds_sum", {
                ...identity,
                outcome: "success",
              }) ?? 0) *
                1000) /
              successes
            : undefined,
          averageBatch: batches
            ? (value(samples, "docparse_onnx_batch_items_sum", identity) ?? 0) /
              batches
            : undefined,
          failures:
            value(samples, "docparse_onnx_run_seconds_count", {
              ...identity,
              outcome: "error",
            }) ?? 0,
        };
      }),
  };
}
