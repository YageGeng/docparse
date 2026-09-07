/** Supplies the pinned PDFium module's WASI preview1 operations without a virtual filesystem. */
export function createWasiImports(memory: () => WebAssembly.Memory) {
  const open = new Set([0, 1, 2]);
  /** Obtains a fresh view after any Rust/PDFium memory growth. */
  const view = () => new DataView(memory().buffer);
  return {
    /** Reports an empty process environment with initialized output sizes. */
    environ_sizes_get(count: number, size: number) { const data = view(); data.setUint32(count >>> 0, 0, true); data.setUint32(size >>> 0, 0, true); return 0; },
    /** Copies no environment entries because the browser exposes none. */
    environ_get() { return 0; },
    /** Implements realtime and monotonic clocks using their distinct browser time sources. */
    clock_time_get(clock: number, _precision: bigint, output: number) {
      if (clock !== 0 && clock !== 1) return 28;
      const milliseconds = clock === 0 ? Date.now() : performance.now();
      view().setBigUint64(output >>> 0, BigInt(Math.floor(milliseconds * 1e6)), true);
      return 0;
    },
    /** Closes only a currently open standard descriptor. */
    fd_close(fd: number) { return open.delete(fd) ? 0 : 8; },
    /** Reports standard descriptors as character devices with explicit rights. */
    fd_fdstat_get(fd: number, stat: number) {
      if (!open.has(fd)) return 8;
      const data = view();
      new Uint8Array(memory().buffer, stat >>> 0, 24).fill(0);
      data.setUint8(stat >>> 0, 2);
      data.setBigUint64((stat >>> 0) + 8, fd === 0 ? 2n : 64n, true);
      return 0;
    },
    /** Rejects unsupported descriptor-flag changes. */
    fd_fdstat_set_flags() { return 52; },
    /** Rejects filesystem metadata access without a filesystem. */
    fd_filestat_get() { return 8; },
    /** Rejects file resizing without a filesystem. */
    fd_filestat_set_size() { return 8; },
    /** Exposes no preopened directories. */
    fd_prestat_get() { return 8; },
    /** Exposes no preopened directory names. */
    fd_prestat_dir_name() { return 8; },
    /** Returns EOF for stdin and rejects other input descriptors. */
    fd_read(fd: number, _iovs: number, _length: number, read: number) { if (fd !== 0 || !open.has(fd)) return 8; view().setUint32(read >>> 0, 0, true); return 0; },
    /** Rejects directory enumeration. */
    fd_readdir() { return 8; },
    /** Rejects seeking on the standard character streams. */
    fd_seek() { return 52; },
    /** Rejects unsupported filesystem synchronization. */
    fd_sync() { return 52; },
    /** Consumes bounded output vectors without publishing document-dependent diagnostic text. */
    fd_write(fd: number, iovs: number, length: number, written: number) {
      if ((fd !== 1 && fd !== 2) || !open.has(fd)) return 8;
      const data = view(); let total = 0;
      for (let i = 0; i < length; i++) {
        const offset = (iovs >>> 0) + i * 8;
        const pointer = data.getUint32(offset, true), size = data.getUint32(offset + 4, true);
        new Uint8Array(memory().buffer, pointer, size);
        total += size;
        if (total > 0xffffffff) return 61;
      }
      // Consume stdio without publishing document-dependent native diagnostic bodies.
      data.setUint32(written >>> 0, total, true); return 0;
    },
    /** Reports that filesystem paths do not exist in this instance. */
    path_filestat_get() { return 44; },
    /** Rejects path loading so browser PDFs must use owned bytes. */
    path_open() { return 44; },
    /** Rejects directory mutation. */
    path_remove_directory() { return 52; },
    /** Rejects file mutation. */
    path_unlink_file() { return 52; },
    /** Makes a libc process exit fatal to the owning Worker. */
    proc_exit(code: number): never { throw new Error(`PDFium terminated with WASI exit ${code}`); },
  };
}

/** Uses the real WebAssembly exception tag when the pinned archive imports longjmp. */
export function createEnvImports() {
  const Tag = (WebAssembly as unknown as { Tag: new (options: { parameters: string[] }) => unknown }).Tag;
  return { __c_longjmp: new Tag({ parameters: ["i32"] }) };
}
