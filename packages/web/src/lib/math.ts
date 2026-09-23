import { renderToString } from "katex";
import MarkdownIt from "markdown-it";
import { tex } from "@mdit/plugin-tex";
import DOMPurify from "dompurify";

const options = { trust: false, strict: "ignore" as const, throwOnError: true, maxExpand: 1000, maxSize: 20 };
// Share bounded math rendering between existing inline previews and full-document Markdown.
const mathPlugin = {
  // Let parse errors reach the UI instead of falling back to visible LaTeX source.
  render: (source: string, display: boolean, env: unknown) => {
    try { return renderToString(source, { ...options, displayMode: display }); }
    catch (error) {
      // A malformed inline expression must not erase the surrounding paragraph.
      if (typeof env === "object" && env !== null && "preserveProse" in env && env.preserveProse === true) return '<span class="formula-render-error" role="status">[formula]</span>';
      throw error;
    }
  },
};
// PDF/model content is untrusted. Images are disabled to avoid unsolicited resource requests.
const markdown = new MarkdownIt({ html: false, linkify: false }).disable("image").use(tex, mathPlugin);
const documentMarkdown = new MarkdownIt({ html: true, linkify: false }).disable("image").use(tex, mathPlugin);

/** Allows the server's merged-cell tables while stripping arbitrary HTML, attributes and resource loads. */
documentMarkdown.renderer.rules.html_block = (tokens, index) => {
  const container = document.createElement("div");
  container.innerHTML = DOMPurify.sanitize(tokens[index].content, {
    ALLOWED_TAGS: ["table", "thead", "tbody", "tfoot", "tr", "th", "td", "br"],
    ALLOWED_ATTR: ["rowspan", "colspan"],
    ALLOW_DATA_ATTR: false,
    ALLOW_ARIA_ATTR: false,
  });
  // Markdown parsers skip raw HTML blocks; explicitly typeset math inside sanitized cells.
  for (const cell of container.querySelectorAll("td, th")) {
    cell.innerHTML = documentMarkdown.renderInline(cell.innerHTML, { preserveProse: true });
  }
  return container.innerHTML;
};

/** Retains cell line breaks without admitting arbitrary inline HTML from extracted document text. */
documentMarkdown.renderer.rules.html_inline = (tokens, index) => DOMPurify.sanitize(tokens[index].content, {
  ALLOWED_TAGS: ["br"], ALLOWED_ATTR: [], ALLOW_DATA_ATTR: false, ALLOW_ARIA_ATTR: false,
});

/** Renders the server's complete Markdown, including sanitized tables and bounded formulas. */
export function renderMarkdownDocument(source: string): string {
  return documentMarkdown.render(source, { preserveProse: true });
}

/** Produces HTML only through the restricted Markdown/KaTeX renderers, never from raw source HTML. */
export function renderMath(source: string, format: "latex" | "markdown", display = false, preserveProse = false): string {
  return format === "latex" ? renderToString(source, { ...options, displayMode: display }) : markdown.render(source, { preserveProse });
}
