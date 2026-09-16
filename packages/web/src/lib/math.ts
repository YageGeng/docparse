import { renderToString } from "katex";
import MarkdownIt from "markdown-it";
import { tex } from "@mdit/plugin-tex";

const options = { trust: false, strict: "ignore" as const, throwOnError: true, maxExpand: 1000, maxSize: 20 };
// PDF/model content is untrusted. Images are disabled to avoid unsolicited resource requests.
const markdown = new MarkdownIt({ html: false, linkify: false }).disable("image").use(tex, {
  // Let parse errors reach the UI instead of falling back to visible LaTeX source.
  render: (source: string, display: boolean, env: unknown) => {
    try { return renderToString(source, { ...options, displayMode: display }); }
    catch (error) {
      // A malformed inline expression must not erase the surrounding paragraph.
      if (typeof env === "object" && env !== null && "preserveProse" in env && env.preserveProse === true) return '<span class="formula-render-error" role="status">[formula]</span>';
      throw error;
    }
  },
});

/** Produces HTML only through the restricted Markdown/KaTeX renderers, never from raw source HTML. */
export function renderMath(source: string, format: "latex" | "markdown", display = false, preserveProse = false): string {
  return format === "latex" ? renderToString(source, { ...options, displayMode: display }) : markdown.render(source, { preserveProse });
}
