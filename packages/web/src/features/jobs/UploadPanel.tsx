import { useEffect, useRef, useState, type RefObject } from "react";
import {
  ArrowUpFromLine,
  FileText,
  FolderOpen,
  LoaderCircle,
  RotateCcw,
  Upload,
  X,
} from "lucide-react";
import {
  ApiError,
  apiPrefix,
  request,
  uploadPdf,
  type Job,
} from "@/api/client";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { ErrorNotice } from "@/components/ErrorNotice";
import { fileSize } from "@/lib/format";

type PendingUpload = {
  id: string;
  name: string;
  size: number;
  relativePath?: string;
};
type UploadEntry = PendingUpload & {
  file?: File;
  status:
    "pending" | "uploading" | "retrying" | "checking" | "accepted" | "failed";
  percent: number;
  retries?: number;
  error?: unknown;
};
const storageKey = `docparse.pending-upload:${apiPrefix}`;
const navigation = performance.getEntriesByType("navigation")[0] as
  | PerformanceNavigationTiming
  | undefined;
// HTTP/1.x uploads share six origin sockets with status reads and SSE; leave two sockets available for those requests.
const uploadConcurrency =
  navigation?.nextHopProtocol === "h2" || navigation?.nextHopProtocol === "h3"
    ? 100
    : 4;
const maxRateLimitRetries = 6;

/** Restores pending identities, including the previous single-file format, without retaining PDF bytes. */
function readPending(): UploadEntry[] {
  try {
    const value: unknown = JSON.parse(
      localStorage.getItem(storageKey) || "null",
    );
    return (Array.isArray(value) ? value : [value])
      .filter(
        (entry): entry is PendingUpload =>
          entry != null &&
          typeof entry.id === "string" &&
          typeof entry.name === "string" &&
          Number.isSafeInteger(entry.size) &&
          entry.size > 0,
      )
      .map(({ id, name, size, relativePath }) => ({
        id,
        name,
        size,
        // Older pending records have no directory path; keep their existing retry identities.
        relativePath:
          typeof relativePath === "string" ? relativePath : undefined,
        status: "pending",
        percent: 0,
      }));
  } catch {
    // Local storage may be disabled; ordinary uploads still work.
    return [];
  }
}

/** Uploads a bounded file queue with independent progress, failures, and durable retry identities. */
export function UploadPanel({
  inputRef,
  onUploaded,
}: {
  inputRef: RefObject<HTMLInputElement | null>;
  onUploaded: (job: Job) => void;
}) {
  const [uploads, setUploads] = useState(readPending);
  const current = useRef(uploads);
  const [busy, setBusy] = useState<"upload" | "check" | null>(null);
  const [dragging, setDragging] = useState(false);
  const [emptyDirectory, setEmptyDirectory] = useState(false);
  const active = useRef<AbortController | null>(null);
  const directoryInput = useRef<HTMLInputElement | null>(null);
  const retryInput = useRef<HTMLInputElement | null>(null);
  const retryTarget = useRef<string | null>(null);
  const mounted = useRef(true);
  const persisted = useRef<string | undefined>(undefined);
  useEffect(() => {
    mounted.current = true;
    // Defer restoration until mount settles so StrictMode cleanup cannot strand an aborted check.
    const restore = window.setTimeout(() => {
      void run(
        current.current.filter(
          (entry) => !entry.file && entry.status === "pending",
        ),
        "check",
      );
    }, 0);
    return () => {
      window.clearTimeout(restore);
      mounted.current = false;
      active.current?.abort();
    };
  }, []);

  /** Updates concurrent transfers synchronously and persists identities before sending bytes. */
  function update(entries: UploadEntry[]) {
    // Late cancellation callbacks must not overwrite identities created after navigation.
    if (!mounted.current) return;
    current.current = entries;
    setUploads(entries);
    const pending = entries
      .filter((entry) => entry.status !== "accepted")
      .map(({ id, name, size, relativePath }) => ({
        id, name, size, relativePath,
      }));
    const serialized = JSON.stringify(pending);
    // Progress events do not rewrite unchanged local-storage metadata.
    if (serialized === persisted.current) return;
    try {
      if (pending.length) localStorage.setItem(storageKey, serialized);
      else localStorage.removeItem(storageKey);
      persisted.current = serialized;
    } catch {
      // The server history remains available after acknowledgement.
    }
  }

  /** Changes only the matching row so another concurrent transfer cannot overwrite its state. */
  function change(id: string, patch: Partial<UploadEntry>) {
    update(
      current.current.map((entry) =>
        entry.id === id ? { ...entry, ...patch } : entry,
      ),
    );
  }

  /** Runs the bounded queue, delaying rate-limited files without changing their identities or cancelling siblings. */
  async function run(queue: UploadEntry[], mode: "upload" | "check") {
    if (active.current || !queue.length) return;
    const controller = new AbortController();
    active.current = controller;
    setBusy(mode);
    let next = 0;

    /** Claims the next file synchronously before awaiting network or header validation. */
    async function transfer() {
      while (!controller.signal.aborted) {
        const entry = queue[next++];
        if (!entry) return;
        change(entry.id, {
          status: mode === "upload" ? "uploading" : "checking",
          percent: 0,
          retries: 0,
          error: undefined,
        });
        try {
          let job: Job;
          if (mode === "check") {
            job = await request<Job>(
              "jobs/status",
              { id: entry.id },
              // An unreachable server must not keep restored uploads locked in checking forever.
              AbortSignal.any([
                controller.signal,
                AbortSignal.timeout(15000),
              ]),
            );
          } else {
            if (
              !entry.file ||
              !entry.file.size ||
              (await entry.file.slice(0, 5).text()) !== "%PDF-"
            )
              throw new Error("请选择有效的 PDF 文件。");
            for (let retry = 0; ; retry++) {
              if (controller.signal.aborted) return;
              change(entry.id, { status: "uploading", percent: 0 });
              try {
                job = await uploadPdf(
                  entry.file,
                  entry.id,
                  (percent) => change(entry.id, { percent }),
                  controller.signal,
                );
                break;
              } catch (error) {
                if (
                  !(error instanceof ApiError) ||
                  error.status !== 429 ||
                  retry >= maxRateLimitRetries
                )
                  throw error;
                change(entry.id, {
                  status: "retrying",
                  percent: 0,
                  retries: retry + 1,
                });
                // Jitter prevents concurrent clients from retrying in lockstep; cancellation wakes the wait immediately.
                const delay = Math.ceil(
                  Math.min(1000 * 2 ** retry, 30_000) *
                    (0.75 + Math.random() * 0.25),
                );
                const wake = AbortSignal.any([
                  controller.signal,
                  AbortSignal.timeout(delay),
                ]);
                await new Promise<void>((resolve) => {
                  if (wake.aborted) resolve();
                  else
                    wake.addEventListener("abort", () => resolve(), {
                      once: true,
                    });
                });
              }
            }
          }
          // A server acknowledgement remains valid even if cancellation races with its response.
          if (!mounted.current) return;
          change(entry.id, {
            status: "accepted",
            percent: 100,
            file: undefined,
          });
          onUploaded(job);
        } catch (error) {
          // Only an authoritative missing-job response proves a restored upload needs its file again.
          const unconfirmed =
            mode === "check" &&
            !(
              error instanceof ApiError &&
              error.status === 404 &&
              error.code === 4041001
            );
          change(entry.id, {
            status: unconfirmed ? "pending" : "failed",
            error,
          });
        }
      }
    }

    try {
      await Promise.all(
        Array.from(
          { length: Math.min(uploadConcurrency, queue.length) },
          transfer,
        ),
      );
    } finally {
      // Cancelled files stay recoverable, including queued files that never started transferring.
      if (controller.signal.aborted) {
        update(
          current.current.map((entry) =>
            entry.status === "uploading" ||
            entry.status === "retrying" ||
            entry.status === "checking"
              ? { ...entry, status: "pending", percent: 0 }
              : entry,
          ),
        );
      }
      active.current = null;
      setBusy(null);
      if (inputRef.current) inputRef.current.value = "";
    }
  }

  /** New selections get fresh identities; reselection can reuse only the explicitly chosen pending row. */
  function select(files: File[], retryId?: string) {
    if (active.current || !files.length) return;
    setEmptyDirectory(false);
    const pending = retryId
      ? current.current.find(
          (entry) => entry.id === retryId && entry.status !== "accepted",
        )
      : undefined;
    if (retryId && !pending) return;
    if (
      pending &&
      (files.length !== 1 ||
        files.some(
          (file) => file.name !== pending.name || file.size !== pending.size,
        ))
    ) {
      change(pending.id, {
        status: "failed",
        error: new Error(
          `请选择原文件“${pending.name}”。其他文件请使用“选择 PDF”新建上传。`,
        ),
      });
      return;
    }
    const queue: UploadEntry[] = files.map((file) => {
      const id = pending?.id ?? crypto.randomUUID();
      return {
        id,
        name: file.name,
        size: file.size,
        // Paths distinguish files in different subdirectories; uploads and retry matching retain basenames.
        relativePath: file.webkitRelativePath || pending?.relativePath,
        file,
        status: "pending",
        percent: 0,
      };
    });
    const replaced = new Set(queue.map((entry) => entry.id));
    update([
      ...current.current.filter((entry) => !replaced.has(entry.id)),
      ...queue,
    ]);
    void run(queue, "upload");
  }

  const accepted = uploads.filter(
    (entry) => entry.status === "accepted",
  ).length;
  // Acknowledged files belong in server history, not in the actionable upload queue.
  const outstanding = uploads.filter((entry) => entry.status !== "accepted");
  const retryable = outstanding.filter((entry) => entry.file);

  return (
    <section aria-label="上传文档" className="space-y-3">
      <input
        ref={retryInput}
        type="file"
        accept="application/pdf,.pdf"
        disabled={busy != null}
        aria-label="重新选择待确认的 PDF 文件"
        tabIndex={-1}
        className="sr-only"
        onChange={(event) => {
          const id = retryTarget.current;
          retryTarget.current = null;
          const files = Array.from(event.currentTarget.files ?? []);
          event.currentTarget.value = "";
          if (id) select(files, id);
        }}
      />
      <div
        className={`upload-zone ${dragging && !busy ? "is-dragging" : ""}`}
        onDragOver={(event) => {
          event.preventDefault();
          setDragging(true);
        }}
        onDragLeave={() => setDragging(false)}
        onDrop={(event) => {
          event.preventDefault();
          setDragging(false);
          select(Array.from(event.dataTransfer.files));
        }}
      >
        <input
          ref={inputRef}
          type="file"
          accept="application/pdf,.pdf"
          multiple
          disabled={busy != null}
          aria-label="选择 PDF 文件"
          tabIndex={-1}
          className="sr-only"
          onChange={(event) => select(Array.from(event.target.files ?? []))}
        />
        {/* Native directory selection supplies all descendants; React's input typings omit this attribute. */}
        <input
          ref={directoryInput}
          type="file"
          {...{ webkitdirectory: "" }}
          multiple
          disabled={busy != null}
          aria-label="选择包含 PDF 的目录"
          tabIndex={-1}
          className="sr-only"
          onChange={(event) => {
            // Directory pickers do not reliably honor accept; filter before entering the shared upload queue.
            const files = Array.from(event.currentTarget.files ?? []).filter(
              (file) =>
                /\.pdf$/i.test(file.name) || file.type === "application/pdf",
            );
            event.currentTarget.value = "";
            if (files.length) select(files);
            else setEmptyDirectory(true);
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
            {busy === "check"
              ? "正在检查任务…"
              : busy
                ? "正在提交文档…"
                : "让每一页文档，都有清晰的结构"}
          </h2>
          <p>
            支持选择或拖拽多个 PDF，也可选择目录，自动上传目录及所有子目录中的 PDF。
          </p>
          {!!uploads.length && (
            <p role="status">
              已创建 {accepted}/{uploads.length} 项任务
            </p>
          )}
        </div>
        {busy ? (
          <Button variant="outline" onClick={() => active.current?.abort()}>
            {busy === "check" ? "取消检查" : "取消剩余上传"}
          </Button>
        ) : (
          <div className="flex w-full flex-wrap gap-2 sm:w-auto sm:shrink-0">
            <Button
              variant="outline"
              className="flex-1 sm:flex-none"
              onClick={() => directoryInput.current?.click()}
            >
              <FolderOpen size={16} aria-hidden="true" />
              选择目录
            </Button>
            <Button
              className="flex-1 sm:flex-none"
              onClick={() => inputRef.current?.click()}
            >
              <Upload size={16} aria-hidden="true" />
              选择 PDF
            </Button>
          </div>
        )}
      </div>
      {emptyDirectory && (
        <p role="status" className="text-sm text-muted-foreground">
          此目录及其子目录中没有 PDF 文件。
        </p>
      )}
      {!!outstanding.length && (
        <div
          className="space-y-3 rounded-xl border bg-background p-4"
          aria-label="上传队列"
        >
          {!busy && retryable.length > 1 && (
            <Button
              variant="outline"
              size="sm"
              onClick={() => void run(retryable, "upload")}
            >
              <RotateCcw size={14} />
              重试未完成文件
            </Button>
          )}
          <ul className="space-y-3">
            {outstanding.map((entry) => (
              <li
                key={entry.id}
                className="space-y-2 rounded-lg border p-3"
                aria-label={`上传 ${entry.relativePath || entry.name}`}
              >
                <div className="flex flex-wrap items-center gap-3">
                  <FileText
                    size={18}
                    className="shrink-0"
                    aria-hidden="true"
                  />
                  {/* Keep paths readable on narrow screens by wrapping actions before squeezing filenames. */}
                  <div className="min-w-0 flex-[1_1_10rem]">
                    <p className="break-all text-sm font-medium">
                      {entry.relativePath || entry.name}
                    </p>
                    <p className="text-xs text-muted-foreground">
                      {fileSize(entry.size)} ·{" "}
                      {entry.status === "checking"
                        ? "正在检查任务…"
                        : entry.status === "retrying"
                          ? `服务器繁忙，等待第 ${entry.retries} 次重试…`
                          : entry.status === "uploading"
                            ? entry.percent === 100
                              ? "正在确认任务…"
                              : `正在上传 ${entry.percent}%`
                            : entry.status === "failed"
                              ? "提交未完成"
                              : busy
                                ? "等待上传"
                                : "提交待确认"}
                    </p>
                  </div>
                  {!busy && (
                    <div className="flex flex-wrap gap-2">
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => void run([entry], "check")}
                      >
                        检查任务
                      </Button>
                      {entry.file &&
                      entry.error instanceof ApiError &&
                      entry.error.code === 4091001 ? (
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => {
                            const file = entry.file;
                            if (!file) return;
                            update(
                              current.current.filter(
                                (item) => item.id !== entry.id,
                              ),
                            );
                            select([file]);
                          }}
                        >
                          新建上传
                        </Button>
                      ) : entry.file ? (
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => void run([entry], "upload")}
                        >
                          重试上传
                        </Button>
                      ) : entry.status === "failed" ? (
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => {
                            retryTarget.current = entry.id;
                            retryInput.current?.click();
                          }}
                        >
                          重新选择文件
                        </Button>
                      ) : null}
                    </div>
                  )}
                  {!busy && (
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={`移除 ${entry.relativePath || entry.name} 的上传记录`}
                      onClick={() =>
                        update(
                          current.current.filter(
                            (item) => item.id !== entry.id,
                          ),
                        )
                      }
                    >
                      <X size={15} />
                    </Button>
                  )}
                </div>
                {entry.status === "uploading" && (
                  <Progress
                    value={entry.percent}
                    aria-label={`${entry.relativePath || entry.name} 上传进度`}
                    className="h-1.5"
                  />
                )}
                {entry.error != null && <ErrorNotice error={entry.error} />}
                {entry.status === "failed" &&
                  entry.error instanceof ApiError &&
                  entry.error.status === 429 &&
                  entry.retries === maxRateLimitRetries && (
                    <p className="text-xs text-muted-foreground">
                      已自动重试 {maxRateLimitRetries} 次，请稍后手动重试。
                    </p>
                  )}
                {!entry.file && entry.status === "failed" && (
                  <p className="text-xs text-muted-foreground">
                    上传未完成，请重新选择原 PDF 文件后重试。
                  </p>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
