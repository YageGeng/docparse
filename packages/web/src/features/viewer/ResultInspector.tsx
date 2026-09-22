import { useEffect, useMemo, useState } from "react";
import { Braces, Check, Copy, FileText, ScanText, Table2 } from "lucide-react";
import type { Block, PageResult, Table } from "@/api/client";
import { blockLabel } from "@/lib/format";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { FormulaView, MathPreview } from "./FormulaView";

/** Previews delivered image bytes without treating server filesystem paths as browser URLs. */
function FigureView({ image, name }: { image: NonNullable<Block["image"]>; name: string }) {
  const [failed, setFailed] = useState(false);
  // Restrict data URLs to the image formats in the API, keeping the original download bytes intact.
  const extension = { "image/jpeg": "jpg", "image/png": "png", "image/jp2": "jp2", "image/jpx": "jpx" }[image.media_type];
  const src = useMemo(
    () => extension && image.delivery.type === "inline" && image.delivery.data_base64
      ? `data:${image.media_type};base64,${image.delivery.data_base64}`
      : undefined,
    [image, extension],
  );
  useEffect(() => setFailed(false), [image]);

  return (
    <figure className="my-3 min-w-0 space-y-2">
      {src && !failed ? (
        <img
          src={src}
          alt={name}
          width={image.width}
          height={image.height}
          loading="lazy"
          decoding="async"
          className="max-h-96 max-w-full rounded border border-border object-contain"
          onError={() => setFailed(true)}
        />
      ) : (
        <p className="text-xs text-muted-foreground" role="status">
          {image.delivery.type === "file"
            ? "图片保存在服务器，暂不支持在线预览。"
            : src
              ? "图片无法预览，可下载原图查看。"
              : "图片数据不可用，请查看原 PDF。"}
        </p>
      )}
      <figcaption className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
        <span>{image.source === "embedded" ? "内嵌原图" : "页面截图"} · {image.width} × {image.height}</span>
        {src && (
          <Button asChild size="sm" variant="outline">
            <a href={src} download={`${name}.${extension}`}>下载原图</a>
          </Button>
        )}
      </figcaption>
    </figure>
  );
}

/** Presents canonical table cells with their original row/column spans and escaped text content. */
function TableView({ table }: { table: Table }) {
  return (
    <div className="table-scroll">
      <table className="result-table">
        <tbody>
          {Array.from({ length: table.row_count }, (_, row) => (
            <tr key={row}>
              {table.cells
                .filter((cell) => cell.row === row)
                .sort((a, b) => a.column - b.column)
                .map((cell) => {
                  const Cell = cell.is_header ? "th" : "td";
                  return (
                    <Cell
                      key={cell.column}
                      rowSpan={cell.row_span}
                      colSpan={cell.column_span}
                    >
                      {cell.markdown ? <MathPreview source={cell.markdown} format="markdown" preserveProse /> : cell.text}
                    </Cell>
                  );
                })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** Shows only the current page's content and keeps region selection synchronized with the PDF overlay. */
export function ResultInspector({
  page,
  selected,
  onSelect,
  pending,
}: {
  page?: PageResult;
  selected?: string;
  onSelect: (id: string) => void;
  pending?: string;
}) {
  const [tab, setTab] = useState("text");
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState(false);
  const selectedBlock = page?.blocks.find((block) => block.id === selected);
  const tables = page?.blocks.filter((block) => block.table) ?? [];
  // Keep the canonical copy source separate from the bounded on-screen image projection.
  const jsonValue = useMemo(
    () => selectedBlock ? { ...selectedBlock, formulas: page?.formulas?.filter(formula => formula.block_id === selectedBlock.id) ?? [] } : page,
    [page, selectedBlock],
  );
  const json = useMemo(
    () =>
      tab === "json" && page
        ? JSON.stringify(jsonValue, (key, value) => key === "data_base64" && typeof value === "string" ? `[图片数据已省略：${value.length} 个 Base64 字符]` : value, 2)
        : "",
    [tab, page, jsonValue],
  );
  useEffect(() => {
    if (selected)
      document
        .getElementById(`content-${selected}`)
        ?.scrollIntoView({ block: "nearest" });
  }, [selected, page, tab]);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 2000);
    return () => clearTimeout(timer);
  }, [copied]);

  /** Copies a selected region or page projection without requiring the browser WASM parser. */
  async function copy() {
    const text =
      tab === "json"
        ? JSON.stringify(jsonValue, null, 2) ?? ""
        : (selectedBlock?.markdown ?? selectedBlock?.text ??
          page?.blocks.map((block) => block.markdown ?? block.text).join("\n\n") ??
          "");
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setCopyError(false);
    } catch {
      setCopyError(true);
    }
  }

  return (
    <section className="inspector" aria-label="解析结果">
      <div className="inspector-heading">
        <div>
          <h2>解析结果</h2>
          <p>
            {page
              ? `第 ${page.page_number} 页 · ${page.blocks.length} 个区域`
              : "结构化内容将在解析完成后显示"}
          </p>
        </div>
        <Button
          variant="ghost"
          size="icon-sm"
          disabled={!page}
          onClick={() => void copy()}
          aria-label={copied ? "已复制内容" : "复制内容"}
        >
          {copied ? <Check size={16} /> : <Copy size={16} />}
        </Button>
      </div>
      <Tabs value={tab} onValueChange={setTab} className="min-h-0 flex-1 gap-0">
        <TabsList variant="line" className="inspector-tabs">
          <TabsTrigger value="text">
            <FileText size={14} />
            内容
          </TabsTrigger>
          <TabsTrigger value="tables">
            <Table2 size={14} />
            表格
            {tables.length > 0 && (
              <span className="tab-count">{tables.length}</span>
            )}
          </TabsTrigger>
          <TabsTrigger value="json">
            <Braces size={14} />
            JSON
          </TabsTrigger>
        </TabsList>
        {copyError && (
          <p className="px-4 py-2 text-xs text-red-600" role="alert">
            复制失败，请手动选择内容复制。
          </p>
        )}
        <TabsContent value="text" className="inspector-content">
          {!page ? (
            <div className="inspector-empty">
              <ScanText size={32} />
              <h3>{pending || "这一页暂无解析结果"}</h3>
              <p>
                {pending
                  ? "可以先浏览原 PDF，结果准备好后会自动显示。"
                  : "你仍然可以查看原 PDF 或切换到其他页面。"}
              </p>
            </div>
          ) : (
            <>
              {page.warnings.length > 0 && (
                <details className="page-warnings">
                  <summary>{page.warnings.length} 项需要留意的提示</summary>
                  {page.warnings.map((warning, index) => (
                    <p key={index}>{warning.message}</p>
                  ))}
                </details>
              )}
              {page.blocks.map((block) => (
                <article
                  id={`content-${block.id}`}
                  key={block.id}
                  className={`content-block ${block.id === selected ? "is-selected" : ""}`}
                >
                  <button
                    onClick={() => onSelect(block.id)}
                    className="block-label"
                    aria-pressed={block.id === selected}
                  >
                    <span>
                      {String(block.final_order + 1).padStart(2, "0")}
                    </span>
                    {blockLabel(block.label)}
                  </button>
                  {/* Figure bytes accompany, rather than replace, extracted text and PDF selection. */}
                  {block.image && <FigureView image={block.image} name={`第 ${page.page_number} 页 · 区域 ${block.final_order + 1} · ${blockLabel(block.label)}`} />}
                  {block.table ? (
                    <TableView table={block.table} />
                  ) : (["inline_formula", "display_formula"].includes(typeof block.label === "string" ? block.label : "") && page.formulas?.some(formula => formula.block_id === block.id && formula.latex)) ? null : (
                    block.markdown ? <div className="inline-prose"><MathPreview source={block.markdown} format="markdown" preserveProse /></div> : block.image && !block.text ? null :
                    <p
                      className={
                        block.label === "doc_title" ||
                        block.label === "paragraph_title"
                          ? "font-semibold"
                          : ""
                      }
                    >
                      {block.text || "此区域没有可提取的文字。"}
                    </p>
                  )}
                  {block.markdown ? <details className="formula-details"><summary>公式详情与复制</summary><FormulaView formulas={page.formulas?.filter(formula => formula.block_id === block.id) ?? []} /></details> : <FormulaView formulas={page.formulas?.filter(formula => formula.block_id === block.id) ?? []} />}
                </article>
              ))}
              <FormulaView formulas={page.formulas?.filter(formula => !formula.block_id) ?? []} />
              {!page.blocks.length && (
                <p className="p-5 text-sm text-muted-foreground">
                  这一页没有检测到内容区域。
                </p>
              )}
            </>
          )}
        </TabsContent>
        <TabsContent value="tables" className="inspector-content">
          {tables.length ? (
            tables.map((block) => (
              <article className="content-block" key={block.id}>
                <button
                  className="block-label"
                  onClick={() => onSelect(block.id)}
                >
                  <Table2 size={14} />
                  区域 {block.final_order + 1} · {block.table!.row_count} 行 ×{" "}
                  {block.table!.column_count} 列
                </button>
                <TableView table={block.table!} />
              </article>
            ))
          ) : (
            <div className="inspector-empty">
              <Table2 size={30} />
              <h3>这一页暂无表格</h3>
              <p>识别出的表格会保留合并单元格与阅读顺序。</p>
            </div>
          )}
        </TabsContent>
        <TabsContent value="json" className="inspector-content">
          <div className="json-caption">
            {selectedBlock ? "当前选中区域" : "当前页面"} ·
            图片 Base64 已折叠；复制保留完整数据，完整文档可通过顶部按钮下载
          </div>
          <pre className="json-view">{json || "结果尚未生成"}</pre>
        </TabsContent>
      </Tabs>
      <div className="inspector-footer">
        <span className="h-1.5 w-1.5 rounded-full bg-blue-500" />
        <span>点击 PDF 区域或内容标签，可联动定位</span>
      </div>
    </section>
  );
}
