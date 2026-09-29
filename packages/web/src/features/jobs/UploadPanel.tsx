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
  uploadPdf,
  type Job,
} from "@/api/client";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { ErrorNotice } from "@/components/ErrorNotice";
import { fileSize } from "@/lib/format";

type UploadEntry = {
  id: string;
  name: string;
  size: number;
  relativePath?: string;
  file: File;
  status: "pending" | "uploading" | "retrying" | "accepted" | "failed";
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

/** Uploads a bounded file queue with independent progress, failures, and immediate sibling transfers without status checking. */
export function UploadPanel({
  inputRef,
  onUploaded,
}: {
  inputRef: RefObject<HTMLInputElement | null>;
  onUploaded: (job: Job) => void;
}) {
  const [uploads, setUploads] = useState<UploadEntry[]>([]);
  const current = useRef(uploads);
  const [dragging, setDragging] = useState(false);
  const [emptyDirectory, setEmptyDirectory] = useState(false);
  const activeController = useRef<AbortController | null>(null);
  const activeWorkers = useRef(0);
  const directoryInput = useRef<HTMLInputElement | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    // Clean up any stale pending uploads stored by older versions.
    try {
      localStorage.removeItem(storageKey);
    } catch {
      // Local storage may be disabled.
    }
    return () => {
      mounted.current = false;
      activeController.current?.abort();
    };
  }, []);

  /** Updates the in-memory queue state and synchronizes the current ref. */
  function update(entries: UploadEntry[]) {
    if (!mounted.current) return;
    current.current = entries;
    setUploads(entries);
  }

  /** Modifies a single upload entry while preserving others. */
  function change(id: string, patch: Partial<UploadEntry>) {
    update(
      current.current.map((entry) =>
        entry.id === id ? { ...entry, ...patch } : entry,
      ),
    );
  }

  /** Dispatches pending uploads up to maximum concurrency without waiting for task status checks. */
  function startQueue() {
    if (!mounted.current) return;
    if (!activeController.current) {
      activeController.current = new AbortController();
    }
    const controller = activeController.current;

    while (activeWorkers.current < uploadConcurrency) {
      const nextEntry = current.current.find(
        (entry) => entry.status === "pending",
      );
      if (!nextEntry) {
        if (activeWorkers.current === 0) {
          activeController.current = null;
        }
        break;
      }

      // Mark as uploading immediately so another worker loop does not pick it up.
      change(nextEntry.id, {
        status: "uploading",
        percent: 0,
        retries: 0,
        error: undefined,
      });
      activeWorkers.current++;

      void (async (entry: UploadEntry) => {
        try {
          if (
            !entry.file ||
            !entry.file.size ||
            (await entry.file.slice(0, 5).text()) !== "%PDF-"
          ) {
            throw new Error("请选择有效的 PDF 文件。");
          }

          let job: Job | undefined;
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
              ) {
                throw error;
              }
              change(entry.id, {
                status: "retrying",
                percent: 0,
                retries: retry + 1,
              });
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
                else wake.addEventListener("abort", () => resolve(), { once: true });
              });
            }
          }

          if (!mounted.current) return;
          // Upload complete: mark accepted immediately and notify parent.
          change(entry.id, {
            status: "accepted",
            percent: 100,
          });
          if (job) {
            onUploaded(job);
          }
        } catch (error) {
          if (!mounted.current) return;
          change(entry.id, {
            status: "failed",
            error,
          });
        } finally {
          activeWorkers.current--;
          // Once this file finishes, immediately process next files in the queue.
          startQueue();
        }
      })(nextEntry);
    }
  }

  /** Appends newly selected files to the queue and triggers processing immediately. */
  function select(files: File[]) {
    if (!files.length) return;
    setEmptyDirectory(false);

    const queue: UploadEntry[] = files.map((file) => ({
      id: crypto.randomUUID(),
      name: file.name,
      size: file.size,
      relativePath: file.webkitRelativePath || undefined,
      file,
      status: "pending",
      percent: 0,
    }));

    update([...current.current, ...queue]);
    startQueue();
  }

  /** Cancels in-flight transfers and marks active items as pending. */
  function cancel() {
    activeController.current?.abort();
    activeController.current = null;
    update(
      current.current.map((entry) =>
        entry.status === "uploading" || entry.status === "retrying"
          ? { ...entry, status: "pending", percent: 0 }
          : entry,
      ),
    );
  }

  /** Retries a specific failed file upload. */
  function retryOne(id: string) {
    change(id, { status: "pending", percent: 0, error: undefined });
    startQueue();
  }

  /** Retries all failed files in the current list. */
  function retryAll() {
    update(
      current.current.map((entry) =>
        entry.status === "failed"
          ? { ...entry, status: "pending", percent: 0, error: undefined }
          : entry,
      ),
    );
    startQueue();
  }

  /** Removes a file entry from the upload panel. */
  function removeOne(id: string) {
    update(current.current.filter((item) => item.id !== id));
  }

  const accepted = uploads.filter((entry) => entry.status === "accepted").length;
  const outstanding = uploads.filter((entry) => entry.status !== "accepted");
  const failedCount = outstanding.filter((entry) => entry.status === "failed").length;
  const isUploading = uploads.some(
    (entry) => entry.status === "uploading" || entry.status === "retrying",
  );

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
          select(Array.from(event.dataTransfer.files));
        }}
      >
        <input
          ref={inputRef}
          type="file"
          accept="application/pdf,.pdf"
          multiple
          aria-label="选择 PDF 文件"
          tabIndex={-1}
          className="sr-only"
          onChange={(event) => {
            select(Array.from(event.target.files ?? []));
            event.target.value = "";
          }}
        />
        {/* Native directory selection supplies all descendants; React's input typings omit this attribute. */}
        <input
          ref={directoryInput}
          type="file"
          {...{ webkitdirectory: "" }}
          multiple
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
          {isUploading ? (
            <LoaderCircle className="animate-spin" size={25} />
          ) : (
            <ArrowUpFromLine size={25} />
          )}
        </div>
        <div className="min-w-0 flex-1">
          <h2>
            {isUploading ? "正在提交文档…" : "让每一页文档，都有清晰的结构"}
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
        <div className="flex w-full flex-wrap gap-2 sm:w-auto sm:shrink-0">
          {isUploading && (
            <Button variant="outline" onClick={cancel}>
              取消上传
            </Button>
          )}
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
          {failedCount > 0 && (
            <Button
              variant="outline"
              size="sm"
              onClick={retryAll}
            >
              <RotateCcw size={14} aria-hidden="true" />
              一键重传失败文件（{failedCount}）
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
                  <div className="min-w-0 flex-[1_1_10rem]">
                    <p className="break-all text-sm font-medium">
                      {entry.relativePath || entry.name}
                    </p>
                    <p className="text-xs text-muted-foreground">
                      {fileSize(entry.size)} ·{" "}
                      {entry.status === "retrying"
                        ? `服务器繁忙，等待第 ${entry.retries} 次重试…`
                        : entry.status === "uploading"
                          ? `正在上传 ${entry.percent}%`
                          : entry.status === "failed"
                            ? "上传未完成"
                            : "等待上传"}
                    </p>
                  </div>
                  <div className="flex flex-wrap gap-2">
                    {entry.status === "failed" && (
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => retryOne(entry.id)}
                      >
                        重试上传
                      </Button>
                    )}
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={`移除 ${entry.relativePath || entry.name} 的上传记录`}
                      onClick={() => removeOne(entry.id)}
                    >
                      <X size={15} />
                    </Button>
                  </div>
                </div>
                {entry.status === "uploading" && (
                  <Progress
                    value={entry.percent}
                    aria-label={`${entry.relativePath || entry.name} 上传进度`}
                    className="h-1.5"
                  />
                )}
                {entry.error != null && <ErrorNotice error={entry.error} />}
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
