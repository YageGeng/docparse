import { useMemo, useState } from "react";
import type { PageResult } from "@/api/client";
import { Button } from "@/components/ui/button";
import { renderMath } from "@/lib/math";
import "katex/dist/katex.min.css";

/** Renders a bounded, untrusted formula projection without replacing its copyable source. */
export function MathPreview({ source, format, display = false, preserveProse = false }: { source: string; format: "latex" | "markdown"; display?: boolean; preserveProse?: boolean }) {
  const html = useMemo(() => {
    try { return renderMath(source, format, display, preserveProse); }
    catch { return null; }
  }, [source, format, display, preserveProse]);
  return html === null
    ? <p className="formula-render-error" role="status">公式暂时无法排版，可复制源码查看。</p>
    : <div className="formula-preview" data-format={format} dangerouslySetInnerHTML={{ __html: html }} />;
}

/** Displays both rendered representations and copies the exact corresponding JSON strings. */
export function FormulaView({ formulas }: { formulas: NonNullable<PageResult["formulas"]> }) {
  const [copied, setCopied] = useState("");
  const [failed, setFailed] = useState(false);
  /** Leaves source bytes intact, including Markdown delimiters and LaTeX escape sequences. */
  async function copy(source: string, key: string) {
    try { await navigator.clipboard.writeText(source); setCopied(key); setFailed(false); }
    catch { setFailed(true); }
  }
  return <div className="formula-results">
    {formulas.map(formula => <section className="formula-result" data-formula-id={formula.id} key={formula.id}>
      <h3>{formula.label === "display_formula" ? "行间公式" : "行内公式"}</h3>
      {formula.error && <p className="formula-render-error" role="status">识别失败：{formula.error}</p>}
      {([ ["latex", "LaTeX"], ["markdown", "Markdown"] ] as const).map(([format, label]) => {
        const source = formula[format];
        if (!source) return null;
        const key = `${formula.id}-${format}`;
        return <div className="formula-representation" key={format}>
          <div className="formula-representation-heading"><span>{label}</span><Button size="sm" variant="ghost" aria-label={`复制 ${label} 源码`} onClick={() => void copy(source, key)}>{copied === key ? "已复制" : "复制源码"}</Button></div>
          <MathPreview source={source} format={format} display={formula.label === "display_formula"} />
        </div>;
      })}
    </section>)}
    {failed && <p className="formula-render-error" role="alert">复制失败，请检查浏览器剪贴板权限。</p>}
  </div>;
}
