import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

// Optional input is emitted by the real Rust Table::to_markdown regression, never a parser substitute.
const exportedTable = process.argv[2] ? await readFile(process.argv[2], "utf8") : undefined;

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
