import type { Block, Job, JobStatus } from "@/api/client";

const blockLabels: Record<string, string> = {
  doc_title: "文档标题",
  paragraph_title: "段落标题",
  text: "正文",
  content: "目录",
  abstract: "摘要",
  table: "表格",
  image: "图片",
  chart: "图表",
  algorithm: "算法",
  display_formula: "公式",
  inline_formula: "行内公式",
  reference: "参考文献",
  figure_title: "图注",
  table_title: "表题",
  header: "页眉",
  footer: "页脚",
  number: "页码",
  footnote: "脚注",
  aside_text: "旁注",
};

/** Preserves the schema's unknown-label variant instead of assuming every layout label is a string. */
export function blockLabel(value: Block["label"]): string {
  const raw = typeof value === "string" ? value : value.unknown;
  return blockLabels[raw] ?? raw.replaceAll("_", " ");
}

export const statusNames: Record<JobStatus, string> = {
  queued: "排队中",
  running: "解析中",
  succeeded: "已完成",
  failed: "失败",
};

/** Formats bounded upload sizes while leaving metadata absent on legacy jobs explicitly unknown. */
export function fileSize(bytes?: number | null): string {
  if (bytes == null) return "大小未知";
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** Shows server-measured execution time only for terminal jobs; retries and legacy records have no final duration yet. */
export function jobDuration(job: Job): string {
  const ms = job.duration_ms;
  if ((job.status !== "succeeded" && job.status !== "failed") || ms == null)
    return "—";
  if (ms < 1000) return `${ms} 毫秒`;
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${(ms / 1000).toFixed(1)} 秒`;
  return `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒`;
}

/** Presents the real pipeline stage; percentages apply only within stages with a measurable page count. */
export function jobProgress(job: Job): { label: string; percent?: number } {
  if (job.status === "queued")
    return { label: job.attempts > 0 ? "等待重新处理" : "等待工作节点接收" };
  if (job.status === "failed") return { label: "解析未能完成" };
  if (job.status === "succeeded") return { label: "结果已保存", percent: 100 };
  const progress = job.progress;
  if (!progress) return { label: "正在准备解析" };
  switch (progress.stage) {
    case "opening":
      return { label: "正在打开文档" };
    case "scanning":
      return {
        label: `扫描页面 ${progress.completed} / ${progress.total}`,
        percent: progress.total
          ? (progress.completed / progress.total) * 100
          : undefined,
      };
    case "analyzing":
      return {
        label: `分析页面 ${progress.completed} / ${progress.total}`,
        percent: progress.total
          ? (progress.completed / progress.total) * 100
          : undefined,
      };
    case "linking":
      return { label: "正在整理文档结构" };
    case "complete":
      return { label: "正在保存解析结果" };
  }
}
