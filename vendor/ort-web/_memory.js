/** Enables views that remain valid when asynchronous inference overlaps Rust heap growth. */
export function configureMemory(memory) {
  if (memory.buffer.resizable) return;
  if (typeof memory.toResizableBuffer === "function") {
    try {
      // The module must declare a maximum. Conversion preserves the existing memory
      // contents and allocation size; subsequent growth keeps this buffer attached.
      memory.toResizableBuffer();
      console.info("ort-web: using growable Rust memory for borrowed tensor inputs");
      return;
    } catch (error) {
      console.warn("ort-web: growable Rust memory is unavailable; input snapshots are required", error);
      return;
    }
  }
  console.info("ort-web: this engine requires input snapshots across Rust heap growth");
}

/** Borrows stable memory or snapshots a detachable view before ORT can retain it. */
export function stableTensorData(view) {
  // ORT prepares inputs across awaits. Refreshing a detachable view once before run()
  // is insufficient: PDFium may grow Rust memory before ORT reads the next input.
  return view.buffer.resizable ? view : view.slice();
}
