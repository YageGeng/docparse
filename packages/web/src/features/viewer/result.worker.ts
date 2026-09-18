/// <reference lib="webworker" />
import { ApiError, decodeResponse, type JobPage } from "@/api/client";
import type { ResultMessage, ResultRequest } from "./result";

const scope = self as unknown as DedicatedWorkerGlobalScope;
let url: string | undefined;
let active: AbortController | undefined;
let requested = 0;
let pageCount: number | undefined;

/** Fetches only the selected page; native HTTP decoding handles gzip before JSON parsing. */
scope.onmessage = async ({ data }: MessageEvent<ResultRequest>) => {
  if (data.kind === "load") {
    url = data.url;
    pageCount = undefined;
  }
  if (!url) return;
  // Mounting sends load and page messages together; do not cancel and repeat the identical request.
  if (data.kind === "page" && data.page === requested) return;
  requested = data.page;
  active?.abort();
  const controller = new AbortController();
  active = controller;
  const pageUrl = new URL(url);
  try {
    // Bootstrap from page one when the count is unknown, including when the PDF preview is unavailable.
    let number = Math.max(1, Math.min(pageCount ?? 1, data.page));
    for (;;) {
      pageUrl.searchParams.set("page", String(number));
      const response = await fetch(pageUrl, {
        credentials: "same-origin",
        headers: { Accept: "application/json" },
        signal: controller.signal,
      });
      const document = decodeResponse<JobPage>(
        await response.text(),
        response.status,
        response.headers.get("x-request-id") ?? undefined,
      );
      if (controller.signal.aborted) return;
      pageCount = document.page_count;
      const normalized = Math.max(1, Math.min(pageCount, data.page));
      if (number !== normalized) {
        number = normalized;
        continue;
      }
      scope.postMessage({
        kind: "page",
        requested: data.page,
        pageCount: document.page_count,
        errors: document.errors,
        page: document.page ?? undefined,
      } satisfies ResultMessage);
      return;
    }
  } catch (error) {
    if (controller.signal.aborted) return;
    scope.postMessage({
      kind: "error",
      message: error instanceof Error ? error.message : "结果读取失败。",
      ...(error instanceof ApiError
        ? {
            status: error.status,
            code: error.code,
            detail: error.detail,
            requestId: error.requestId,
          }
        : {}),
    } satisfies ResultMessage);
  }
};
