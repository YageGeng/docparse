import { useEffect, useRef, useState } from "react";
import { Link } from "react-router";
import {
  onlineManager,
  type InfiniteData,
  useMutation,
  useQueryClient,
} from "@tanstack/react-query";
import { AlertDialog } from "radix-ui";
import {
  ArrowDown,
  ArrowRight,
  FileText,
  FolderOpen,
  LoaderCircle,
  Plus,
  Search,
  Trash2,
} from "lucide-react";
import { request, type Job, type JobList, type JobStatus } from "@/api/client";
import { Button } from "@/components/ui/button";
import { StatusBadge } from "@/components/StatusBadge";
import { ErrorNotice } from "@/components/ErrorNotice";
import { fileSize, jobDuration, jobProgress } from "@/lib/format";
import { useJobs } from "./queries";
import { UploadPanel } from "./UploadPanel";

const filters = [
  [undefined, "全部"],
  ["running", "解析中"],
  ["queued", "排队中"],
  ["succeeded", "已完成"],
  ["failed", "失败"],
] as const;

/** Presents persisted history and a single upload entry, keeping all task identities navigable by URL. */
export function JobsPage() {
  const [status, setStatus] = useState<JobStatus>();
  const [search, setSearch] = useState("");
  const [query, setQuery] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const searchInput = useRef<HTMLInputElement>(null);
  const deleteTrigger = useRef<HTMLButtonElement>(null);
  const [deleting, setDeleting] = useState<Job>();
  const client = useQueryClient();
  useEffect(() => {
    const timer = setTimeout(() => setQuery(search.trim()), 250);
    return () => clearTimeout(timer);
  }, [search]);
  const jobs = useJobs(status, query);
  const items = jobs.data?.pages.flatMap((page) => page.items) ?? [];
  const deletion = useMutation({
    // Reject offline actions immediately instead of queuing a paused mutation that locks the modal until reconnect.
    networkMode: "always",
    retry: false,
    mutationFn: (id: string) => {
      if (!onlineManager.isOnline())
        throw new Error("当前离线，请恢复网络后再删除。");
      return request<string>(
        "jobs/delete",
        { id },
        AbortSignal.timeout(15000),
        "POST",
      );
    },
    onSuccess: async (_result, id) => {
      // Stop older responses from restoring the deleted row, then close without waiting for another network request.
      await Promise.all([
        client.cancelQueries({ queryKey: ["job", id] }),
        client.cancelQueries({ queryKey: ["jobs"] }),
      ]);
      client.setQueryData(["job", id], null);
      client.setQueriesData<InfiniteData<JobList>>(
        { queryKey: ["jobs"] },
        (current) =>
          current && {
            ...current,
            pages: current.pages.map((page) => ({
              ...page,
              items: page.items.filter((job) => job.id !== id),
            })),
          },
      );
      setDeleting(undefined);
      void client.invalidateQueries({ queryKey: ["jobs"] });
    },
  });

  /** Opens an accessible in-page confirmation and remembers where keyboard focus should return. */
  function deleteResult(job: Job, trigger: HTMLButtonElement) {
    deletion.reset();
    deleteTrigger.current = trigger;
    setDeleting(job);
  }

  /** Refreshes each acknowledged task without navigating away and cancelling sibling uploads. */
  function uploaded(job: Job) {
    client.setQueryData(["job", job.id], job);
    void client.invalidateQueries({ queryKey: ["jobs"] });
  }

  return (
    <main className="jobs-page" id="main-content">
      <div className="page-heading">
        <div>
          <p className="eyebrow">DOCUMENT WORKSPACE</p>
          <h1>文档</h1>
          <p className="text-muted-foreground">
            从原始 PDF 到结构化内容，在这里完成。
          </p>
        </div>
        <Button onClick={() => input.current?.click()}>
          <Plus size={17} />
          上传文档
        </Button>
      </div>
      <UploadPanel inputRef={input} onUploaded={uploaded} />
      <section className="history-panel" aria-labelledby="history-title">
        <div className="history-heading">
          <div className="flex items-center gap-2.5">
            <h2 id="history-title">解析记录</h2>
            <span className="text-xs text-muted-foreground">
              {items.length
                ? `已加载 ${items.length} 项`
                : "所有提交都会保存在这里"}
            </span>
          </div>
          <span className="hidden items-center gap-1.5 text-xs text-muted-foreground sm:flex">
            <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
            自动更新
          </span>
        </div>
        <div className="history-toolbar">
          <div className="filter-list" aria-label="按任务状态筛选">
            {filters.map(([value, label]) => (
              <button
                key={label}
                aria-pressed={status === value}
                onClick={() => setStatus(value)}
              >
                {label}
              </button>
            ))}
          </div>
          <label className="search-box">
            <Search size={16} aria-hidden="true" />
            <span className="sr-only">搜索文件名</span>
            <input
              ref={searchInput}
              type="search"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
              placeholder="搜索文件名…"
              maxLength={200}
            />
          </label>
        </div>
        {deletion.isSuccess && (
          <p className="px-5 pb-4 text-sm text-muted-foreground" role="status">
            解析结果已删除
          </p>
        )}
        {jobs.error && (
          <div className="px-5 pb-4">
            <ErrorNotice error={jobs.error} />
            <Button
              className="mt-3"
              size="sm"
              variant="outline"
              onClick={() => void jobs.refetch()}
            >
              重新连接
            </Button>
          </div>
        )}
        {jobs.isPending ? (
          <div className="empty-state" role="status">
            <LoaderCircle className="animate-spin" size={24} />
            <p>正在读取任务记录…</p>
          </div>
        ) : !items.length && !jobs.error ? (
          <div className="empty-state">
            <div className="empty-icon">
              <FolderOpen size={27} />
            </div>
            <h3>
              {status || query ? "没有符合条件的文档" : "从第一份 PDF 开始"}
            </h3>
            <p>
              {status || query
                ? "试试其他筛选条件或文件名。"
                : "上传后即可查看进度，离开页面也不会中断已提交的任务。"}
            </p>
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="jobs-table">
              <thead>
                <tr>
                  <th>文档名称</th>
                  <th>状态</th>
                  <th className="hidden lg:table-cell">当前进度</th>
                  <th className="hidden sm:table-cell">解析耗时</th>
                  <th className="hidden sm:table-cell">创建时间</th>
                  <th className="text-right">
                    <span className="sr-only">操作</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {items.map((job) => (
                  <tr key={job.id}>
                    <td>
                      <div className="flex items-center gap-3">
                        <div className="file-icon">
                          <FileText size={21} />
                        </div>
                        <div className="min-w-0">
                          <Link
                            className="document-name"
                            to={`/document?job=${job.id}`}
                          >
                            {job.filename || `文档 ${job.id.slice(0, 8)}.pdf`}
                          </Link>
                          <p className="mt-1 text-xs text-muted-foreground">
                            {fileSize(job.size_bytes)}
                            <span className="mx-2 text-slate-300">·</span>
                            <span className="font-mono">
                              {job.id.slice(0, 8)}
                            </span>
                          </p>
                          {/* Keep duration visible on narrow screens without adding another table column. */}
                          <p className="mt-1 text-xs text-muted-foreground tabular-nums sm:hidden">
                            解析耗时 {jobDuration(job)}
                          </p>
                        </div>
                      </div>
                    </td>
                    <td>
                      <StatusBadge status={job.status} />
                    </td>
                    <td className="hidden text-xs text-muted-foreground lg:table-cell">
                      {jobProgress(job).label}
                    </td>
                    <td className="hidden whitespace-nowrap text-xs text-muted-foreground tabular-nums sm:table-cell">
                      {jobDuration(job)}
                    </td>
                    <td className="hidden whitespace-nowrap text-xs text-muted-foreground sm:table-cell">
                      {new Date(job.created_at).toLocaleString("zh-CN", {
                        month: "2-digit",
                        day: "2-digit",
                        hour: "2-digit",
                        minute: "2-digit",
                      })}
                    </td>
                    <td>
                      <div className="flex items-center justify-end gap-1">
                        <Link
                          className="row-open"
                          to={`/document?job=${job.id}`}
                          aria-label={`打开 ${job.filename || job.id}`}
                        >
                          <ArrowRight size={17} />
                        </Link>
                        {(job.status === "succeeded" ||
                          job.status === "failed") && (
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            className="text-muted-foreground hover:text-destructive"
                            disabled={deletion.isPending}
                            onClick={(event) =>
                              deleteResult(job, event.currentTarget)
                            }
                            aria-label={`删除 ${job.filename || job.id} 的解析结果`}
                          >
                            {deletion.isPending &&
                            deletion.variables === job.id ? (
                              <LoaderCircle
                                className="animate-spin"
                                size={16}
                              />
                            ) : (
                              <Trash2 size={16} />
                            )}
                          </Button>
                        )}
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {jobs.hasNextPage && (
          <div className="border-t px-5 py-3 text-center">
            <Button
              size="sm"
              variant="ghost"
              disabled={jobs.isFetchingNextPage}
              onClick={() => void jobs.fetchNextPage()}
            >
              {jobs.isFetchingNextPage ? (
                <LoaderCircle className="animate-spin" size={14} />
              ) : (
                <ArrowDown size={14} />
              )}
              加载更多
            </Button>
          </div>
        )}
      </section>
      <p className="page-footnote">
        文档与结果由服务器持久化保存 · 刷新或短暂断网后，可继续查看任务
      </p>
      {/* Keep asynchronous deletion inside the dialog, preserving errors and avoiding a blocking native confirm prompt. */}
      <AlertDialog.Root
        open={Boolean(deleting)}
        onOpenChange={(open) => {
          if (!open && !deletion.isPending) setDeleting(undefined);
        }}
      >
        <AlertDialog.Portal>
          <AlertDialog.Overlay className="fixed inset-0 z-50 bg-black/40" />
          <AlertDialog.Content
            className="fixed top-1/2 left-1/2 z-50 w-[calc(100%-2rem)] max-w-md -translate-x-1/2 -translate-y-1/2 rounded-xl border bg-background p-6 shadow-xl"
            onCloseAutoFocus={(event) => {
              event.preventDefault();
              // Successful deletion removes its trigger; return to search instead of losing keyboard focus.
              (deleteTrigger.current?.isConnected
                ? deleteTrigger.current
                : searchInput.current
              )?.focus();
            }}
          >
            <AlertDialog.Title className="text-lg font-semibold">
              删除解析结果？
            </AlertDialog.Title>
            <AlertDialog.Description className="mt-3 text-sm leading-6 text-muted-foreground break-words">
              将删除“{deleting?.filename || deleting?.id}
              ”的解析结果和列表记录，原始 PDF 将保留。此操作无法撤销。
            </AlertDialog.Description>
            {deletion.error && (
              <div className="mt-4">
                <ErrorNotice error={deletion.error} />
              </div>
            )}
            <div className="mt-6 flex justify-end gap-2">
              <AlertDialog.Cancel asChild>
                <Button variant="outline" disabled={deletion.isPending}>
                  取消
                </Button>
              </AlertDialog.Cancel>
              <Button
                variant="destructive"
                disabled={deletion.isPending}
                aria-busy={deletion.isPending}
                onClick={() => deleting && deletion.mutate(deleting.id)}
              >
                {deletion.isPending && (
                  <LoaderCircle className="animate-spin" size={16} />
                )}
                {deletion.isPending ? "正在删除…" : "确认删除"}
              </Button>
            </div>
          </AlertDialog.Content>
        </AlertDialog.Portal>
      </AlertDialog.Root>
    </main>
  );
}
