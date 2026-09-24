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
// Small formula previews keep images disabled; the document preview resolves images explicitly.
const markdown = new MarkdownIt({ html: false, linkify: false }).disable("image").use(tex, mathPlugin);
const documentMarkdown = new MarkdownIt({ html: true, linkify: false }).use(tex, mathPlugin);
const inlineImage = /^data:image\/(?:png|jpeg|jp2|jpx);base64,[A-Za-z0-9+/]+={0,2}$/i;
const validateLink = documentMarkdown.validateLink.bind(documentMarkdown);
documentMarkdown.validateLink = source => inlineImage.test(source) || validateLink(source);
const renderImage = documentMarkdown.renderer.rules.image!;
/** Only embedded image bytes or paths resolved by the owning document may load in the preview. */
documentMarkdown.renderer.rules.image = (tokens, index, options, env, self) => {
  const source = String(tokens[index].attrGet("src") ?? "");
  const resolver = (env as { imageResolver?: (source: string) => string | undefined }).imageResolver;
  const src = inlineImage.test(source) ? source : resolver?.(source);
  if (!src) return "";
  tokens[index].attrSet("src", src);
  tokens[index].attrSet("loading", "lazy");
  tokens[index].attrSet("decoding", "async");
  return renderImage(tokens, index, options, env, self);
};

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
export function renderMarkdownDocument(source: string, imageResolver?: (source: string) => string | undefined): string {
  return documentMarkdown.render(source, { preserveProse: true, imageResolver });
}

/** Produces HTML only through the restricted Markdown/KaTeX renderers, never from raw source HTML. */
export function renderMath(source: string, format: "latex" | "markdown", display = false, preserveProse = false): string {
  return format === "latex" ? renderToString(source, { ...options, displayMode: display }) : markdown.render(source, { preserveProse });
}
