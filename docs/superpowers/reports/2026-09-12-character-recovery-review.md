# Character recovery review

Scope: the complete uncommitted character-recovery change, including its parser,
PDFium-worker, table, native database and renderer call paths. Review and fixes
were performed in the existing workspace without subagents or commits.

## Findings addressed

| Priority | Finding | Resolution |
| --- | --- | --- |
| P1 | A plausible first letter could make an untrusted, truncated ligature look already expanded. | A failing real-PDF test reproduced the loss. Deduplication now requires a complete neighboring sequence sharing its source code and text object. |
| P1 | Imported cmap parsing could read beyond a declared subtable, mishandle TTC absolute offsets or expand excessive/overflowing ranges. | Table slices and arithmetic are checked, TTC offsets stay file-relative, and total expansion work is bounded. Regression cases cover formats 0/4/6/12, truncation, non-Unicode tables and overflow. |
| P1 | Multi-character glyph recovery could lose tail text or corrupt measured word ranges. | One fact carries a first character and tail; every resulting byte stays in its measured source word, with one source code and geometry contribution. |
| P2 | General AGL recovery activated the table baseline exception originally written only for circle symbols. | The exception now requires a recovered symbol-only run and covers every supported recovery source. |
| P2 | Matching valid text was marked repaired, and ordinary characters acquired per-character String allocations. | Confirmation-only lookups retain their original status; ordinary glyphs stay scalar-valued, with owned tails only for expanded text. Cached decoded strings are borrowed. |
| P2 | Font scans repeatedly checked PDFium's character count and resolved font metadata on clean pages. | Existing character iterators cache the count; a cheap Unicode pass avoids the font-statistics pass when no suspicious mappings occur. |
| P2 | Circle composition depended on a name-recovery flag and failed across font changes. | Circle aliases share the general name parser; composition uses geometry with MCID, watermark and rotation boundaries. |
| P2 | Lists split across styled TextItems were missed. | Shared marker detection reads a bounded prefix across source-item iterators without constructing another line string. |
| P2 | Recovery providers expanded ligatures before the segmenter could record the transformation. | `ResolvedGlyph::normalize` now owns expansion and evidence for every source; glyph-name parsing retains presentation codepoints. |
| P2 | The font/code cache stored a result selected using the first occurrence's Unicode and geometry. | Raw name, cmap and outline candidates are independently lazy; occurrence decisions run outside the cache. A real PDF containing valid and suspicious records for one code passes in both lookup orders. |
| P3 | Markdown depended on a semantic-assembly export for basic list recognition. | Pure character and list rules now live in `text_rules`, shared directly by both consumers. |
| P3 | Two extraction tests maintained their own PDF xref/trailer writers. | Both use the serializer in `tests/common/pdf.rs` from their test-only modules. |

## Organization and quality

The AGL table and name grammar, cmap parser, page/font recovery state, public
outline contract, native filesystem adapter and presentation projections have
separate responsibilities. Existing segmentation and parser/runtime injection
are reused. Font state remains page-scoped; only owned resolver handles cross
the PDFium worker boundary. Native filesystem and environment access stay in
`wasm_compat`; the browser uses the same deterministic recovery implementation.

New functions have English comments. Dependencies are declared centrally;
MessagePack uses `rmp` and hashing reuses the workspace's `blake3`. No unsafe
blocks or test-only production fields were introduced. Source attribution and
the derived fixture's existing font license are documented.

Font caches use `OnceCell` for raw candidates and the embedded reverse cmap.
They never retain Unicode-trust or painted-space decisions from a particular
occurrence. `ResolvedGlyph` owns normalization and its repair evidence, while
`SegmentBuilder` consumes normalized facts and honors structural markers.
PDFium-expanded ligatures retain their multiple source records without claiming
an additional DocParse repair. Shared PDF fixture serialization remains entirely
under the permitted test boundary.

## Validation and remaining limits

- Native default-member tests: 326 passed, 11 ignored. Core tests account for
  257 passed and 7 ignored, including real PDF extraction, cache-order checks and parser injection.
- Browser target: `cargo check -p docparse-web --target wasm32-unknown-unknown` passed.
- Pre-commit checks passed: platform boundary, formatting and Clippy with warnings denied.
- No representative-corpus performance benchmark or external production font
  database was exercised during this review. The allocation/FFI improvements are
  code-level observations, not a measured throughput claim.
- Font trust and Markdown dehyphenation retain documented heuristics. Raw
  Markdown and canonical source ranges remain available. Type3 fonts without
  PDFium-exposed outlines cannot use outline recovery.
- Concurrent first lookups can read a shard twice; this deliberate cold-cache
  tradeoff is documented beside the cache. Shards, records and cmap work are
  bounded, and unavailable/malformed shards are memoized as misses.
