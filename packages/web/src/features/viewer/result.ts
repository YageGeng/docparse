import { useEffect, useReducer, useRef, useState } from "react";
import {
  ApiError,
  apiUrl,
  type DocumentResult,
  type PageResult,
} from "@/api/client";

export type ResultRequest =
  { kind: "load"; url: string; page: number } | { kind: "page"; page: number };
export type ResultMessage =
  | {
      kind: "page";
      requested: number;
      pageCount: number;
      errors: DocumentResult["errors"];
      page?: PageResult;
    }
  | {
      kind: "error";
      message: string;
      status?: number;
      code?: number;
      detail?: string;
      requestId?: string;
    };
type ResultState = {
  documentId?: string;
  pageCount: number;
  errors: DocumentResult["errors"];
  page?: PageResult;
  isFetching: boolean;
  error?: Error;
};
const empty: ResultState = { pageCount: 0, errors: [], isFetching: false };

/** Reads one page in a worker, cancelling stale downloads and releasing data on navigation. */
export function useDocumentResult(id: string, page: number, enabled: boolean) {
  const [state, setState] = useState<ResultState>(empty);
  const [revision, reload] = useReducer((value: number) => value + 1, 0);
  const worker = useRef<Worker | null>(null);
  const requested = useRef(page);
  requested.current = page;
  useEffect(() => {
    setState({ ...empty, isFetching: enabled });
    if (!enabled || !id) return;
    let live = true;
    const reader = new Worker(new URL("./result.worker.ts", import.meta.url), {
      type: "module",
    });
    worker.current = reader;
    reader.onmessage = ({ data }: MessageEvent<ResultMessage>) => {
      if (!live) return;
      if (data.kind === "error") {
        const error =
          data.status !== undefined
            ? new ApiError(
                data.status,
                data.code,
                data.detail || data.message,
                data.requestId,
              )
            : new Error(data.message);
        setState({ ...empty, error });
      } else if (data.requested === requested.current) {
        setState({
          documentId: id,
          pageCount: data.pageCount,
          errors: data.errors,
          page: data.page,
          isFetching: false,
        });
      }
    };
    reader.onerror = () => {
      if (live)
        setState({ ...empty, error: new Error("无法读取解析结果，请重试。") });
    };
    reader.postMessage({
      kind: "load",
      url: apiUrl("jobs/result", { id }),
      page: requested.current,
    } satisfies ResultRequest);
    return () => {
      live = false;
      reader.terminate();
      worker.current = null;
    };
  }, [id, enabled, revision]);
  useEffect(() => {
    if (!worker.current) return;
    setState((state) =>
      state.error ? state : { ...state, page: undefined, isFetching: true },
    );
    worker.current.postMessage({ kind: "page", page } satisfies ResultRequest);
  }, [page, enabled]);
  return { ...state, reload };
}
