import { AlertCircle } from "lucide-react";
import { ApiError } from "@/api/client";

/** Displays actionable errors and preserves optional API diagnostics without rendering server text as HTML. */
export function ErrorNotice({ error }: { error: unknown }) {
  return (
    <div className="error-notice" role="alert">
      <AlertCircle size={17} aria-hidden="true" />
      <div>
        <p>{error instanceof Error ? error.message : "操作未完成，请重试。"}</p>
        {error instanceof ApiError && (
          <details>
            <summary>诊断信息</summary>
            <p className="break-all text-xs">
              {error.code ? `API ${error.code} · ` : ""}
              {error.detail}
              {error.requestId ? ` · 请求 ${error.requestId}` : ""}
            </p>
          </details>
        )}
      </div>
    </div>
  );
}
