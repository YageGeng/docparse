/// <reference lib="webworker" />
import { ApiError, decodeResponse, type DocumentResult } from "@/api/client";
import type { ResultMessage, ResultRequest } from "./result";

const scope = self as unknown as DedicatedWorkerGlobalScope;
// ponytail: worker memory scales with full JSON; add a server page-result endpoint if larger documents exceed browser memory.
let document: DocumentResult | undefined;
let requested = 1;

/** Copies one page rather than cloning the complete document graph onto the UI thread. */
function sendPage() {
  if (!document) return;
  const number = Math.min(document.context.page_count, Math.max(1, requested));
  scope.postMessage({
    kind: "page",
    requested,
    pageCount: document.context.page_count,
    errors: document.errors,
    page: document.pages.find((page) => page.page_number === number),
  } satisfies ResultMessage);
}

/** Owns the full JSON fetch and parse; requests received during loading select the page sent when loading finishes. */
scope.onmessage = async ({ data }: MessageEvent<ResultRequest>) => {
  requested = data.page;
  if (data.kind === "page") {
    sendPage();
    return;
  }
  try {
    const response = await fetch(data.url, {
      credentials: "same-origin",
      headers: { Accept: "application/json" },
    });
    document = decodeResponse<DocumentResult>(
      await response.text(),
      response.status,
      response.headers.get("x-request-id") ?? undefined,
    );
    sendPage();
  } catch (error) {
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
