import { useEffect, useRef, useState, type RefObject } from "react";
import {
  ArrowUpFromLine,
  FileText,
  LoaderCircle,
  RotateCcw,
  Upload,
} from "lucide-react";
import { apiPrefix, request, uploadPdf, type Job } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { ErrorNotice } from "@/components/ErrorNotice";
import { fileSize } from "@/lib/format";

type PendingUpload = { id: string; name: string; size: number };
const storageKey = `docparse.pending-upload:${apiPrefix}`;

/** Restores only a submission identity; task existence and completion always come from the server. */
function readPending(): PendingUpload | undefined {
  try {
    const value = JSON.parse(
      localStorage.getItem(storageKey) || "null",
    ) as PendingUpload | null;
    if (
      value &&
      typeof value.id === "string" &&
      typeof value.name === "string" &&
      typeof value.size === "number"
    )
      return value;
  } catch {
    /* Local storage may be disabled; ordinary uploads still work. */
  }
}

/** Offers streamed PDF upload and recovery of an acknowledgement lost to a refresh or network interruption. */
export function UploadPanel({
  inputRef,
  onUploaded,
}: {
  inputRef: RefObject<HTMLInputElement | null>;
  onUploaded: (job: Job) => void;
}) {
  const [pending, setPending] = useState(readPending);
  const [busy, setBusy] = useState<"upload" | "recover" | null>(null);
  const [percent, setPercent] = useState(0);
  const [error, setError] = useState<unknown>();
  const [dragging, setDragging] = useState(false);
  const active = useRef<AbortController | null>(null);
  useEffect(() => () => active.current?.abort(), []);

  /** Publishes the durable acknowledgement before navigating away from the upload screen. */
  function accepted(job: Job) {
    try {
      localStorage.removeItem(storageKey);
    } catch {
      /* Persistence is optional after the server acknowledges. */
    }
    setPending(undefined);
    onUploaded(job);
  }

  /** Lets a changed file start a fresh submission after an idempotency conflict without deleting any existing server task. */
  function startNew() {
    try {
      localStorage.removeItem(storageKey);
    } catch {
      /* A new in-memory identity still allows uploading. */
    }
    setPending(undefined);
    setError(undefined);
    inputRef.current?.click();
  }

  /** Serializes transfers before asynchronous header validation, preserving one idempotency key for a retried file. */
  async function submit(file: File) {
    if (active.current) return;
    const controller = new AbortController();
    active.current = controller;
    setBusy("upload");
    setPercent(0);
    setError(undefined);
    try {
      if (!file.size || (await file.slice(0, 5).text()) !== "%PDF-")
        throw new Error("请选择有效的 PDF 文件。");
      if (controller.signal.aborted) return;
      const submission = {
        id:
          pending?.name === file.name && pending.size === file.size
            ? pending.id
            : crypto.randomUUID(),
        name: file.name,
        size: file.size,
      };
      setPending(submission);
      try {
        localStorage.setItem(storageKey, JSON.stringify(submission));
      } catch {
        /* The server history remains available after acknowledgement. */
      }
      accepted(
        await uploadPdf(file, submission.id, setPercent, controller.signal),
      );
    } catch (error) {
      setError(error);
    } finally {
      active.current = null;
      setBusy(null);
      if (inputRef.current) inputRef.current.value = "";
    }
  }

  /** Queries a previously saved submission before asking the user to transfer any PDF bytes again. */
  async function recover() {
    if (!pending || active.current) return;
    const controller = new AbortController();
    active.current = controller;
    setBusy("recover");
    setError(undefined);
    try {
      accepted(
        await request<Job>(
          "jobs/status",
          { id: pending.id },
          controller.signal,
        ),
      );
    } catch (error) {
      setError(error);
    } finally {
      active.current = null;
      setBusy(null);
    }
  }

  return (
    <section aria-label="上传文档" className="space-y-3">
      <div
        className={`upload-zone ${dragging ? "is-dragging" : ""}`}
        onDragOver={(event) => {
          event.preventDefault();
          setDragging(true);
        }}
        onDragLeave={() => setDragging(false)}
        onDrop={(event) => {
          event.preventDefault();
          setDragging(false);
          const file = event.dataTransfer.files[0];
          if (file) void submit(file);
        }}
      >
        <input
          ref={inputRef}
          type="file"
          accept="application/pdf,.pdf"
          aria-label="选择 PDF 文件"
          tabIndex={-1}
          className="sr-only"
          onChange={(event) => {
            const file = event.target.files?.[0];
            if (file) void submit(file);
          }}
        />
        <div className="upload-emblem">
          {busy ? (
            <LoaderCircle className="animate-spin" size={25} />
          ) : (
            <ArrowUpFromLine size={25} />
          )}
        </div>
        <div className="min-w-0 flex-1">
          <h2>
            {busy === "upload"
              ? percent === 100
                ? "正在确认任务…"
                : "正在上传文档…"
              : busy === "recover"
                ? "正在检查任务…"
                : "让每一页文档，都有清晰的结构"}
          </h2>
          <p>
            {busy && pending
              ? `${pending.name} · ${fileSize(pending.size)}`
              : "拖拽 PDF 到这里，或选择文件。解析完成后，可查看文本、表格和原始结果。"}
          </p>
          {busy === "upload" && (
            <div className="mt-4 flex items-center gap-3">
              <Progress
                value={percent}
                aria-label="文件上传进度"
                className="h-1.5 max-w-md"
              />
              <span className="text-xs tabular-nums">{percent}%</span>
            </div>
          )}
        </div>
        {busy ? (
          <Button variant="outline" onClick={() => active.current?.abort()}>
            {busy === "recover" ? "取消检查" : "取消上传"}
          </Button>
        ) : (
          <Button onClick={() => inputRef.current?.click()}>
            <Upload size={16} />
            选择 PDF
          </Button>
        )}
      </div>
      {!busy && pending && (
        <div className="pending-upload">
          <FileText size={18} aria-hidden="true" />
          <div className="min-w-0 flex-1">
            <p className="font-medium">有一项提交尚未确认</p>
            <p className="truncate text-xs text-muted-foreground">
              {pending.name} · 可以先检查任务，或重新选择同一文件重新提交。
            </p>
          </div>
          <Button variant="outline" size="sm" onClick={() => void recover()}>
            <RotateCcw size={14} />
            检查任务
          </Button>
          <Button variant="ghost" size="sm" onClick={startNew}>
            新建上传
          </Button>
        </div>
      )}
      {error != null && <ErrorNotice error={error} />}
    </section>
  );
}
