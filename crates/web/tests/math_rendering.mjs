import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

// Optional input is emitted by the real Rust Table::to_markdown regression, never a parser substitute.
const exportedTable = process.argv[2] ? await readFile(process.argv[2], "utf8") : undefined;

// These strings match the Rust list regressions and exercise real Markdown parsing, including HTML-disabled previews.
const restartedList = "1. first\n2. second\n\n[docparse-list-break]: #\n\n1. third\n2. fourth";
const resumedParent = "1. parent\n   1. child\n   2. child two\n\n   parent continues\n2. next";

// Exercise both shipping renderers, including hostile content, without replacing any parser backend.
for (const path of ["../../../packages/web/src/lib/math.ts", "../../../packages/wasm-web/example/src/math.ts"]) {
  const { renderMath } = await import(path);
  assert.match(renderMath(String.raw`\frac{a}{b}`, "latex", true), /class="katex-display"/);
  const markdown = renderMath(String.raw`**Result:** $\frac{a}{b}$`, "markdown");
  assert.match(markdown, /<strong>Result:<\/strong>/);
  assert.match(markdown, /class="katex"/);
  assert.match(renderMath("$$\n\\frac{a}{b}\n$$", "markdown"), /class="katex-display"/);
  assert.doesNotMatch(renderMath('<img src=x onerror="alert(1)"> ![image](https://example.invalid/a.png)', "markdown"), /<img\b/i);
  assert.doesNotMatch(renderMath(String.raw`\href{javascript:alert(1)}{x}`, "latex"), /<a\b|href=/i);
  assert.throws(() => renderMath(String.raw`\frac{a`, "latex"));
  assert.throws(() => renderMath(String.raw`$\frac{a$`, "markdown"));
  const paragraph = renderMath(String.raw`Before $\frac{a$ after`, "markdown", false, true);
  assert.match(paragraph, /Before /);
  assert.match(paragraph, /\[formula\]/);
  assert.match(paragraph, / after/);
  const lists = renderMath(restartedList, "markdown", false, true);
  assert.equal((lists.match(/<ol>/g) ?? []).length, 2, "Numbering restart must create two lists");
  assert.doesNotMatch(lists, /docparse-list-break/, "List boundary must remain invisible");
  const nested = renderMath(resumedParent, "markdown", false, true);
  assert.match(nested, /<\/ol>\s*<p>parent continues<\/p>\s*<\/li>/, "Resumed prose must remain inside the parent item");
  if (exportedTable) {
    const html = renderMath(exportedTable, "markdown");
    assert.equal((html.match(/class="katex"/g) ?? []).length, 5);
    for (const latex of ["a < b", "a > b", "|x|", String.raw`\|x\|`, String.raw`\begin{matrix}a & b\\ c & d\end{matrix}`]) {
      assert(html.includes(renderMath(latex, "latex")), `Table export changed TeX content: ${latex}`);
    }
    assert.doesNotMatch(html, /<b>/);
  }
  console.log(`Passed rendered math, Markdown, source error and untrusted-content checks: ${path}`);
}

// The full-document preview accepts only embedded images and caller-approved task files.
const { renderMarkdownDocument } = await import("../../../packages/web/src/lib/math.ts");
assert.match(renderMarkdownDocument("![inline](data:image/png;base64,AA==)"), /<img[^>]+src="data:image\/png;base64,AA=="/);
assert.match(renderMarkdownDocument("![file](</tmp/figure 1.png>)", source => {
  assert.equal(source, "/tmp/figure%201.png");
  return `/api/figure?path=${encodeURIComponent(decodeURIComponent(source))}`;
}), /<img[^>]+src="\/api\/figure\?path=%2Ftmp%2Ffigure%201.png"/);
assert.doesNotMatch(renderMarkdownDocument("![remote](https://example.invalid/a.png)"), /<img\b/i);

const lists = renderMarkdownDocument(restartedList);
assert.equal((lists.match(/<ol>/g) ?? []).length, 2);
assert.doesNotMatch(lists, /docparse-list-break/);
assert.match(renderMarkdownDocument(resumedParent), /<\/ol>\s*<p>parent continues<\/p>\s*<\/li>/);
