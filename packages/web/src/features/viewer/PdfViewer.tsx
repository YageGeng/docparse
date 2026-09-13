import { useEffect, useRef, useState } from "react";
import {
  getDocument,
  GlobalWorkerOptions,
  type PDFDocumentProxy,
  type RenderTask,
} from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import { LoaderCircle } from "lucide-react";
import { apiUrl, type PageResult } from "@/api/client";
import { blockLabel } from "@/lib/format";

GlobalWorkerOptions.workerSrc = workerUrl;

/** Loads the persisted source through range requests and releases its worker when the selected job changes. */
export function usePdf(id: string) {
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(null);
  const [error, setError] = useState<unknown>();
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    // Restart failed range/decoder requests after connectivity returns, preserving the page selection in the parent.
    const reconnect = () => setRevision((value) => value + 1);
    window.addEventListener("online", reconnect);
    return () => window.removeEventListener("online", reconnect);
  }, []);
  useEffect(() => {
    setPdf(null);
    setError(undefined);
    if (!id) return;
    let stale = false;
    const assets = new URL(
      `${import.meta.env.BASE_URL}pdfjs/`,
      window.location.origin,
    ).href;
    const loading = getDocument({
      url: apiUrl("jobs/source", { id }),
      disableAutoFetch: true,
      disableStream: true,
      cMapUrl: `${assets}cmaps/`,
      cMapPacked: true,
      iccUrl: `${assets}iccs/`,
      standardFontDataUrl: `${assets}standard_fonts/`,
      wasmUrl: `${assets}wasm/`,
    });
    void loading.promise
      .then((document) => {
        if (!stale) setPdf(document);
      })
      .catch((error: unknown) => {
        if (!stale) setError(error);
      });
    return () => {
      stale = true;
      void loading.destroy();
    };
  }, [id, revision]);
  return { pdf, error };
}

/** Cancels obsolete canvas renders and keeps the overlay in the parser's canonical rotated viewport coordinates. */
export function PdfPage({
  pdf,
  number,
  width,
  page,
  selected,
  onSelect,
  overlays = true,
}: {
  pdf: PDFDocumentProxy;
  number: number;
  width: number;
  page?: PageResult;
  selected?: string;
  onSelect?: (id: string) => void;
  overlays?: boolean;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [ratio, setRatio] = useState(1 / Math.SQRT2);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState<string>();
  useEffect(() => {
    let stale = false;
    let task: RenderTask | undefined;
    const element = canvas.current;
    setBusy(true);
    setError(undefined);
    void pdf
      .getPage(number)
      .then(async (source) => {
        if (stale || !element) return;
        const base = source.getViewport({ scale: 1 });
        setRatio(base.width / base.height);
        const viewport = source.getViewport({ scale: width / base.width });
        // Limit raster allocation even for unusually tall pages or high-density displays.
        const density = Math.min(
          window.devicePixelRatio || 1,
          2,
          Math.sqrt(16_000_000 / (viewport.width * viewport.height)),
        );
        element.width = Math.max(1, Math.floor(viewport.width * density));
        element.height = Math.max(1, Math.floor(viewport.height * density));
        task = source.render({
          canvas: element,
          viewport,
          transform: density === 1 ? undefined : [density, 0, 0, density, 0, 0],
        });
        await task.promise;
        if (!stale) setBusy(false);
      })
      .catch((error: unknown) => {
        if (!stale) {
          setError(error instanceof Error ? error.message : "无法渲染这一页");
          setBusy(false);
        }
      });
    return () => {
      stale = true;
      task?.cancel();
      if (element) {
        element.width = 0;
        element.height = 0;
      }
    };
  }, [pdf, number, width]);
  return (
    <div className="pdf-sheet" style={{ width, aspectRatio: ratio }}>
      <canvas ref={canvas} aria-label={`PDF 第 ${number} 页`} />
      {busy && (
        <span
          className="canvas-loading"
          role="status"
          aria-label={`正在渲染第 ${number} 页`}
        >
          <LoaderCircle size={20} className="animate-spin" />
        </span>
      )}
      {error && (
        <p className="canvas-error" role="alert">
          此页预览不可用：{error}
        </p>
      )}
      {page && overlays && !busy && (
        <svg
          className="region-overlay"
          viewBox={`0 0 ${page.width} ${page.height}`}
          preserveAspectRatio="none"
          aria-label="解析区域"
        >
          {page.blocks.map((block) => {
            const bounds = block.bbox;
            const points = block.polygon?.length
              ? block.polygon.map((point) => `${point.x},${point.y}`).join(" ")
              : `${bounds.left},${bounds.top} ${bounds.right},${bounds.top} ${bounds.right},${bounds.bottom} ${bounds.left},${bounds.bottom}`;
            return (
              <polygon
                key={block.id}
                points={points}
                className={`region ${block.id === selected ? "is-selected" : ""}`}
                tabIndex={0}
                role="button"
                aria-label={`区域 ${block.final_order + 1}：${blockLabel(block.label)}`}
                aria-pressed={block.id === selected}
                onClick={() => onSelect?.(block.id)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    onSelect?.(block.id);
                  }
                }}
              />
            );
          })}
        </svg>
      )}
    </div>
  );
}

/** Renders thumbnails only near the visible rail and frees their canvases when they leave it. */
export function Thumbnail({
  pdf,
  number,
  selected,
  onSelect,
}: {
  pdf: PDFDocumentProxy;
  number: number;
  selected: boolean;
  onSelect: () => void;
}) {
  const root = useRef<HTMLButtonElement>(null);
  const [visible, setVisible] = useState(false);
  // Page-input navigation must reveal the active thumbnail as well as updating its selection border.
  useEffect(() => {
    if (selected) root.current?.scrollIntoView({ block: "nearest" });
  }, [selected]);
  useEffect(() => {
    const observer = new IntersectionObserver(
      ([entry]) => setVisible(Boolean(entry?.isIntersecting)),
      { root: root.current?.parentElement, rootMargin: "180px 0px" },
    );
    if (root.current) observer.observe(root.current);
    return () => observer.disconnect();
  }, []);
  return (
    <button
      ref={root}
      className={`thumbnail ${selected ? "is-selected" : ""}`}
      onClick={onSelect}
      aria-label={`前往第 ${number} 页`}
      aria-current={selected ? "page" : undefined}
    >
      <span className="thumbnail-paper">
        {visible ? (
          <PdfPage pdf={pdf} number={number} width={76} />
        ) : (
          <span className="thumbnail-placeholder" />
        )}
      </span>
      <span>{number}</span>
    </button>
  );
}
