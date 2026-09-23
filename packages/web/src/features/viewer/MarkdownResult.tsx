import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Check, Copy, LoaderCircle } from "lucide-react";
import { apiUrl, decodeResponse } from "@/api/client";
import { Button } from "@/components/ui/button";
import { ErrorNotice } from "@/components/ErrorNotice";
import { renderMarkdownDocument } from "@/lib/math";

/** Reads the complete server-rendered Markdown on demand and offers its preview and exact source. */
export function MarkdownResult({ id, ready }: { id: string; ready: boolean }) {
  const [source, setSource] = useState(false);
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState<unknown>();
  const result = useQuery({
    queryKey: ["job-markdown", id],
    enabled: ready,
    staleTime: Infinity,
    gcTime: 0,
    // Keep the existing typed API errors while accepting successful Markdown as plain text.
    queryFn: async ({ signal }) => {
      const response = await fetch(apiUrl("jobs/result", { id, format: "markdown" }), {
        signal,
        credentials: "same-origin",
        headers: { Accept: "text/markdown" },
      });
      const text = await response.text();
      if (!response.ok) decodeResponse(text, response.status, response.headers.get("x-request-id") ?? undefined);
      if (!response.headers.get("content-type")?.startsWith("text/markdown"))
        throw new Error("服务返回了无法识别的 Markdown 响应。");
      return text;
    },
  });
  const preview = useMemo(() => {
    // View toggles do not change immutable Markdown, so they must not invalidate its parsed HTML.
    if (result.data === undefined) return {};
    try { return { html: renderMarkdownDocument(result.data) }; }
    catch { return { error: new Error("Markdown 暂时无法预览，请切换源码或下载文件查看。") }; }
  }, [result.data]);

  /** Copies the exact server output, preserving Markdown and formula delimiters. */
  async function copy() {
    try {
      await navigator.clipboard.writeText(result.data ?? "");
      setCopied(true);
      setCopyError(undefined);
    } catch {
      setCopyError(new Error("复制失败，请切换源码手动复制。"));
    }
  }

  if (!ready) return <div className="inspector-empty"><p>解析完成后可查看完整文档 Markdown。</p></div>;
  if (result.isPending) return <div className="inspector-empty" role="status"><LoaderCircle className="animate-spin" /><p>正在读取完整文档 Markdown…</p></div>;
  if (result.error) return <div className="space-y-3 p-4"><ErrorNotice error={result.error} /><Button variant="outline" size="sm" onClick={() => void result.refetch()}>重新读取 Markdown</Button></div>;
  return <div className="markdown-result">
    <div className="markdown-toolbar">
      <span>完整文档</span>
      <div className="flex items-center gap-2">
        <div className="markdown-view-switch" role="group" aria-label="Markdown 显示方式">
          <Button size="sm" variant="ghost" aria-pressed={!source} onClick={() => setSource(false)}>预览</Button>
          <Button size="sm" variant="ghost" aria-pressed={source} onClick={() => setSource(true)}>源码</Button>
        </div>
        <Button size="icon-sm" variant="ghost" title="复制 Markdown 源码" aria-label={copied ? "已复制 Markdown" : "复制 Markdown"} onClick={() => void copy()}>{copied ? <Check size={15} /> : <Copy size={15} />}</Button>
      </div>
    </div>
    {/* Only the document scrolls; mode controls remain visible above long Markdown output. */}
    <div className="markdown-scroll">
    {copyError != null && <div className="p-4"><ErrorNotice error={copyError} /></div>}
    {/* Hide representations without discarding their DOM, avoiding another full HTML parse on every toggle. */}
    <pre hidden={!source} className="markdown-source">{result.data || "此文档没有可导出的 Markdown 内容。"}</pre>
    <div hidden={source}>
      {preview.error ? <div className="p-4"><ErrorNotice error={preview.error} /></div>
        : result.data ? <article className="markdown-document" aria-label="Markdown 预览" dangerouslySetInnerHTML={{ __html: preview.html ?? "" }} />
        : <div className="inspector-empty"><p>此文档没有可导出的 Markdown 内容。</p></div>}
    </div>
    </div>
  </div>;
}
