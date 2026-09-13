import { useEffect, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router";
import {
  ArrowLeft,
  Check,
  ChevronLeft,
  ChevronRight,
  Download,
  Layers2,
  Link2,
  LoaderCircle,
  Maximize,
  Minus,
  Plus,
  WifiOff,
} from "lucide-react";
import { apiUrl } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { ErrorNotice } from "@/components/ErrorNotice";
import { StatusBadge } from "@/components/StatusBadge";
import { fileSize, jobDuration, jobProgress } from "@/lib/format";
import { useJob } from "@/features/jobs/queries";
import { PdfPage, Thumbnail, usePdf } from "./PdfViewer";
import { ResultInspector } from "./ResultInspector";
import { useDocumentResult } from "./result";

/** Restores a document by URL, joins its durable status with the immutable result and coordinates the two inspection panes. */
export function WorkspacePage() {
  const [params, setParams] = useSearchParams();
  const id = params.get("job") || "";
  const job = useJob(id);
  const rawPage = Number(params.get("page") || 1);
  const requested = Number.isSafeInteger(rawPage) ? Math.max(1, rawPage) : 1;
  const result = useDocumentResult(
    id,
    requested,
    job.data?.status === "succeeded",
  );
  const { pdf, error: pdfError } = usePdf(job.data?.id || "");
  const total = pdf?.numPages ?? result.pageCount;
  const number = Math.max(
    1,
    Math.min(total || 1, Number.isSafeInteger(requested) ? requested : 1),
  );
  const page = result.page?.page_number === number ? result.page : undefined;
  const selected = params.get("block") || undefined;
  const [zoom, setZoom] = useState(1);
  const [overlays, setOverlays] = useState(true);
  const [mode, setMode] = useState("pdf");
  const [width, setWidth] = useState(620);
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState<unknown>();
  const viewer = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const node = viewer.current;
    if (!node) return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry && entry.contentRect.width > 0)
        setWidth(Math.max(220, entry.contentRect.width - 48));
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, [job.data?.id]);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 2000);
    return () => clearTimeout(timer);
  }, [copied]);

  /** Persists page selection while removing any region from the previous page. */
  function selectPage(value: number) {
    const next = new URLSearchParams(params);
    next.set("page", String(Math.max(1, Math.min(total || 1, value))));
    next.delete("block");
    setParams(next, { replace: true });
    viewer.current?.scrollTo({ top: 0 });
  }

  /** Shares one region identity between the URL, PDF overlay and content inspector. */
  function selectBlock(block: string) {
    const next = new URLSearchParams(params);
    next.set("block", block);
    next.set("page", String(number));
    setParams(next, { replace: true });
  }

  /** Copies a durable deep link including the current page and region. */
  async function copyLink() {
    try {
      await navigator.clipboard.writeText(window.location.href);
      setCopied(true);
      setCopyError(undefined);
    } catch {
      setCopyError(new Error("无法复制链接，请从浏览器地址栏复制。"));
    }
  }

  if (!id)
    return (
      <main className="empty-state" id="main-content">
        <h1>请选择一份文档</h1>
        <Link to="/">返回文档列表</Link>
      </main>
    );
  // Missing snapshots disable both readers above, releasing cached PDF and result workers before showing this state.
  if (job.notFound)
    return (
      <main className="empty-state" id="main-content">
        <h1 className="text-base font-semibold text-foreground">
          文档已删除或不存在
        </h1>
        <p className="text-sm text-muted-foreground">
          这条解析记录已不可用，请返回文档列表。
        </p>
        <Button asChild variant="outline" size="sm">
          <Link to="/">返回文档列表</Link>
        </Button>
      </main>
    );
  if (!job.data)
    return (
      <main className="jobs-page" id="main-content">
        <Link to="/" className="back-link">
          <ArrowLeft size={16} />
          返回文档
        </Link>
        {job.error ? (
          <ErrorNotice error={job.error} />
        ) : (
          <div className="empty-state" role="status">
            <LoaderCircle className="animate-spin" />
            <p>正在恢复文档任务…</p>
          </div>
        )}
      </main>
    );
  const progress = jobProgress(job.data);
  const name = job.data.filename || `文档 ${id.slice(0, 8)}.pdf`;
  return (
    <main className="document-page" id="main-content">
      <div className="document-heading">
        <div className="flex min-w-0 items-center gap-3">
          <Button asChild variant="outline" size="icon-sm">
            <Link to="/" aria-label="返回文档列表">
              <ArrowLeft size={17} />
            </Link>
          </Button>
          <div className="min-w-0">
            <h1 title={name}>{name}</h1>
            <p>
              {fileSize(job.data.size_bytes)}
              <span>·</span>
              {total ? `${total} 页` : "正在读取页数"}
              <span className="hidden sm:inline">·</span>
              <span className="hidden sm:inline">任务 {id.slice(0, 8)}</span>
            </p>
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <span className="hidden sm:block">
            <StatusBadge status={job.data.status} />
          </span>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void copyLink()}
            aria-label="复制文档链接"
          >
            {copied ? <Check size={15} /> : <Link2 size={15} />}
            <span className="hidden md:inline">
              {copied ? "已复制" : "复制链接"}
            </span>
          </Button>
          {job.data.status === "succeeded" ? (
            <Button asChild size="sm">
              <a
                href={apiUrl("jobs/result", { id })}
                download={`${name.replace(/\.pdf$/i, "")}.json`}
                aria-label="下载 JSON"
              >
                <Download size={15} />
                <span className="hidden sm:inline">下载 JSON</span>
              </a>
            </Button>
          ) : (
            <Button size="sm" disabled aria-label="下载 JSON">
              <Download size={15} />
              <span className="hidden sm:inline">下载 JSON</span>
            </Button>
          )}
        </div>
      </div>
      <div className="document-status" role="status">
        <div className="flex min-w-0 flex-wrap items-center gap-2">
          {job.data.status === "running" ? (
            <LoaderCircle className="animate-spin text-blue-600" size={14} />
          ) : job.data.status === "succeeded" ? (
            <Check className="text-emerald-600" size={15} />
          ) : null}
          <span>{progress.label}</span>
          {/* Read the durable snapshot so reconnects and reloads retain the same completed duration. */}
          <span className="text-muted-foreground tabular-nums">
            · 解析耗时 {jobDuration(job.data)}
          </span>
          {job.data.attempts > 1 && (
            <span className="text-muted-foreground">
              · 第 {job.data.attempts} 次尝试
            </span>
          )}
          {result.errors.length > 0 && (
            <span className="text-amber-700">
              · {result.errors.length} 项解析错误
            </span>
          )}
        </div>
        {job.connection === "reconnecting" &&
        job.data.status !== "succeeded" &&
        job.data.status !== "failed" ? (
          <span className="flex items-center gap-1.5 text-amber-700">
            <WifiOff size={13} />
            连接中断，正在恢复进度
          </span>
        ) : (
          <span className="hidden text-muted-foreground sm:block">
            {job.data.status === "succeeded"
              ? "结果已持久化保存"
              : "离开页面不影响已提交的任务"}
          </span>
        )}
      </div>
      {job.data.status === "running" && progress.percent !== undefined && (
        <Progress
          value={progress.percent}
          className="h-0.5 rounded-none"
          aria-label={progress.label}
        />
      )}
      {copyError != null && <ErrorNotice error={copyError} />}
      {job.data.status === "failed" && (
        <ErrorNotice
          error={new Error(job.data.error || "解析失败，请重新提交文件。")}
        />
      )}
      {result.error && (
        <div className="flex items-center gap-3 px-4 py-2">
          <ErrorNotice error={result.error} />
          <Button variant="outline" size="sm" onClick={result.reload}>
            重新读取结果
          </Button>
        </div>
      )}
      <div className="viewer-toolbar">
        <div className="flex items-center gap-1">
          <Button
            size="icon-sm"
            variant="ghost"
            disabled={number <= 1}
            onClick={() => selectPage(number - 1)}
            aria-label="上一页"
          >
            <ChevronLeft size={16} />
          </Button>
          <label className="page-input">
            <span className="sr-only">当前页码</span>
            <input
              type="number"
              disabled={!total}
              min={1}
              max={total || 1}
              value={number}
              onChange={(event) => selectPage(Number(event.target.value) || 1)}
            />
          </label>
          <span className="mr-1 text-xs text-muted-foreground">
            / {total || "—"}
          </span>
          <Button
            size="icon-sm"
            variant="ghost"
            disabled={!total || number >= total}
            onClick={() => selectPage(number + 1)}
            aria-label="下一页"
          >
            <ChevronRight size={16} />
          </Button>
        </div>
        <div className="mobile-view-switch" aria-label="查看模式">
          <button aria-pressed={mode === "pdf"} onClick={() => setMode("pdf")}>
            原文
          </button>
          <button
            aria-pressed={mode === "result"}
            onClick={() => setMode("result")}
          >
            结果
          </button>
        </div>
        <div className="flex items-center gap-1">
          <Button
            size="icon-sm"
            variant="ghost"
            onClick={() => setZoom(Math.max(0.5, zoom - 0.25))}
            disabled={zoom <= 0.5}
            aria-label="缩小"
          >
            <Minus size={15} />
          </Button>
          <span className="w-10 text-center text-xs tabular-nums">
            {Math.round(zoom * 100)}%
          </span>
          <Button
            size="icon-sm"
            variant="ghost"
            onClick={() => setZoom(Math.min(2, zoom + 0.25))}
            disabled={zoom >= 2}
            aria-label="放大"
          >
            <Plus size={15} />
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            onClick={() => setZoom(1)}
            aria-label="适应宽度"
          >
            <Maximize size={14} />
          </Button>
          <span className="toolbar-divider" />
          <Button
            size="sm"
            variant={overlays ? "secondary" : "ghost"}
            onClick={() => setOverlays(!overlays)}
            aria-pressed={overlays}
            aria-label="显示解析区域"
          >
            <Layers2 size={14} />
            <span className="hidden sm:inline">区域</span>
          </Button>
        </div>
      </div>
      <div className="workbench" data-mobile-mode={mode}>
        <aside className="page-rail" aria-label="页面导航">
          <p>页面</p>
          {pdf &&
            Array.from({ length: pdf.numPages }, (_, index) => (
              <Thumbnail
                key={index}
                pdf={pdf}
                number={index + 1}
                selected={index + 1 === number}
                onSelect={() => selectPage(index + 1)}
              />
            ))}
        </aside>
        <div className="pdf-pane" ref={viewer}>
          {pdf ? (
            <PdfPage
              key={number}
              pdf={pdf}
              number={number}
              width={Math.min(width, 1000) * zoom}
              page={page}
              selected={selected}
              overlays={overlays}
              onSelect={selectBlock}
            />
          ) : pdfError ? (
            <div className="p-5">
              <ErrorNotice error={pdfError} />
              <a
                href={apiUrl("jobs/source", { id })}
                className="mt-3 inline-block text-sm text-primary"
                target="_blank"
                rel="noreferrer"
              >
                打开原 PDF
              </a>
            </div>
          ) : (
            <div className="empty-state" role="status">
              <LoaderCircle className="animate-spin" />
              <p>正在打开原 PDF…</p>
            </div>
          )}
        </div>
        <ResultInspector
          page={page}
          selected={selected}
          onSelect={selectBlock}
          pending={
            job.data.status === "queued" || job.data.status === "running"
              ? "文档正在处理中"
              : result.isFetching
                ? "正在读取解析结果"
                : undefined
          }
        />
      </div>
    </main>
  );
}
