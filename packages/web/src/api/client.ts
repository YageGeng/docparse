import type { components } from "./schema";

export type Job = components["schemas"]["JobSnapshot"];
export type JobList = components["schemas"]["JobList"];
export type JobStatus = components["schemas"]["JobStatus"];
export type DocumentResult = components["schemas"]["DocumentResult"];
export type PageResult = components["schemas"]["PageResult"];
export type JobPage = components["schemas"]["Pagenation_PageResult"];
export type Block = components["schemas"]["Block"];
export type Table = components["schemas"]["Table"];
export const apiPrefix = (import.meta.env.VITE_API_PREFIX ?? "/api/v1/docparse").replace(
  /\/$/,
  "",
);

const messages: Record<number, string> = {
  4001001: "请选择有效的 PDF 文件。",
  4001002: "请求参数无效，请重新打开任务。",
  4041001: "没有找到这个任务。上传可能尚未完成。",
  4081001: "上传超时，请检查任务是否已保存后重试。",
  4091001: "这个提交编号已被使用，请开始新的上传。",
  4091002: "结果仍在生成中，请稍候。",
  4091003: "解析失败，暂时无法下载结果。",
  4091004: "任务仍在排队或解析中，请完成后再删除。",
  4131001: "文件超过了服务器允许的大小。",
  4291001: "服务器上传通道繁忙，请稍后重试。",
  5031001: "服务正在重启，请稍后重试。",
  5031002: "暂时无法读取任务，请稍后重试。",
  5031003: "暂时无法访问文档存储，请稍后重试。",
};

/** Preserves the API code and correlation ID while presenting a readable operation failure. */
export class ApiError extends Error {
  /** Maps known business codes to clear UI copy while retaining the server's diagnostic detail. */
  constructor(
    public status: number,
    public code: number | undefined,
    public detail: string,
    public requestId?: string,
  ) {
    super(
      (code && messages[code]) ||
        (status >= 500 ? "服务暂时不可用，请稍后重试。" : detail),
    );
    this.name = "ApiError";
  }
}

/** Builds same-origin URLs without hardcoding the backend's configurable API prefix. */
export function apiUrl(
  path: string,
  query: Record<string, string | number | undefined> = {},
): string {
  const url = new URL(`${apiPrefix}/${path}`, window.location.origin);
  for (const [key, value] of Object.entries(query))
    if (value !== undefined) url.searchParams.set(key, String(value));
  return url.href;
}

/** Unwraps the shared success/error envelope for both fetch responses and multipart uploads. */
export function decodeResponse<T>(
  text: string,
  status: number,
  requestId?: string,
): T {
  let payload: {
    success?: boolean;
    data?: T;
    error?: { code?: number; message?: string };
  };
  try {
    payload = JSON.parse(text);
  } catch {
    throw new ApiError(
      status,
      undefined,
      "服务返回了无法识别的响应。",
      requestId,
    );
  }
  if (
    !payload ||
    status < 200 ||
    status >= 300 ||
    payload.success !== true ||
    !("data" in payload)
  ) {
    throw new ApiError(
      status,
      payload?.error?.code,
      payload?.error?.message || "请求未能完成。",
      requestId,
    );
  }
  return payload.data as T;
}

/** Sends a typed JSON request and lets TanStack Query cancel obsolete reads. */
export async function request<T>(
  path: string,
  query: Record<string, string | number | undefined> = {},
  signal?: AbortSignal,
  method: "GET" | "POST" = "GET",
): Promise<T> {
  const response = await fetch(apiUrl(path, query), {
    method,
    signal,
    credentials: "same-origin",
    headers: { Accept: "application/json" },
  });
  return decodeResponse<T>(
    await response.text(),
    response.status,
    response.headers.get("x-request-id") ?? undefined,
  );
}

/** Sends the File directly as multipart data and reports transport progress without copying the full PDF into JS memory. */
export function uploadPdf(
  file: File,
  id: string,
  onProgress: (percent: number) => void,
  signal: AbortSignal,
): Promise<Job> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    const abort = () => xhr.abort();
    xhr.open("POST", apiUrl("jobs"));
    xhr.setRequestHeader("Idempotency-Key", id);
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable)
        onProgress(Math.round((event.loaded / event.total) * 100));
    };
    xhr.onload = () => {
      try {
        resolve(
          decodeResponse<Job>(
            xhr.responseText,
            xhr.status,
            xhr.getResponseHeader("x-request-id") ?? undefined,
          ),
        );
      } catch (error) {
        reject(error);
      }
    };
    xhr.onerror = () =>
      reject(new Error("连接中断，提交状态尚未确认。可以检查任务后重试。"));
    xhr.onabort = () =>
      reject(new Error("上传已中断。已保存的任务仍会继续处理。"));
    xhr.onloadend = () => signal.removeEventListener("abort", abort);
    signal.addEventListener("abort", abort, { once: true });
    if (signal.aborted) {
      reject(new DOMException("Upload cancelled", "AbortError"));
      return;
    }
    const body = new FormData();
    body.append("file", file);
    xhr.send(body);
  });
}
