// Capability-only fault injection keeps the actual CPU model and parser in fallback tests.
const benchmarkParameters = new URL(location.href).searchParams;
const preferredLayout = benchmarkParameters.get("preferredLayout");
if (preferredLayout !== null && !["NCHW", "NHWC"].includes(preferredLayout)) throw new Error("Unknown benchmark preferredLayout");
if (new URL(location.href).searchParams.has("noGpu")) Object.defineProperty(navigator, "gpu", {value: undefined, configurable: true});
// Exercise older engines at the capability boundary while retaining the actual parser and ORT.
if (new URL(location.href).searchParams.has("legacyMemory")) Object.defineProperty(WebAssembly.Memory.prototype, "toResizableBuffer", {value: undefined, configurable: true});
const metrics = { calls: 0, liveTensors: 0, sessions: 0, fetches: [], outputs: [], memoryBytes: [], providers: [], models: [], gpuSubmissions: 0, inferenceMs: [], borrowedInputBytes: 0, copiedInputBytes: 0 };
const toResizableBuffer = WebAssembly.Memory.prototype.toResizableBuffer;
if (toResizableBuffer) {
  /** Observes conversion without allocating or replacing the real module memory. */
  WebAssembly.Memory.prototype.toResizableBuffer = function (...args) {
    const before = this.buffer.byteLength;
    const buffer = toResizableBuffer.apply(this, args);
    metrics.memoryConversion = { before, after: buffer.byteLength, maximum: buffer.maxByteLength };
    return buffer;
  };
}
// Observe real GPU work rather than accepting only the requested provider name as proof.
if (typeof GPUQueue !== "undefined") {
  const submit = GPUQueue.prototype.submit;
  /** Counts successfully submitted GPU command buffers without altering execution. */
  GPUQueue.prototype.submit = function (...args) {
    const result = submit.apply(this, args);
    metrics.gpuSubmissions++;
    return result;
  };
}
const memories = new Set();
let rustMemory;
const seen = new WeakSet();
const send = globalThis.postMessage.bind(globalThis);
const queued = [];
/** Holds startup messages while the recording entry asynchronously imports production code. */
const bufferStartup = event => { event.stopImmediatePropagation(); queued.push(event.data); };
globalThis.addEventListener("message", bufferStartup);

/** Records actual runtime buffers without retaining tensor objects in the test. */
function observe(tensor) {
  if (seen.has(tensor)) return tensor;
  seen.add(tensor);
  metrics.liveTensors++;
  const dispose = tensor.dispose;
  let disposed = false;
  tensor.dispose = function (...args) {
    const result = dispose.apply(this, args);
    if (!disposed) { disposed = true; metrics.liveTensors--; }
    return result;
  };
  return tensor;
}

let runtime;
Object.defineProperty(globalThis, "ort", {
  configurable: true,
  get: () => runtime,
  set(value) {
    runtime = value;
    const Tensor = value.Tensor;
    value.Tensor = new Proxy(Tensor, {
      /** Measures intermediate input storage at the actual ORT boundary without retaining it. */
      construct(target, args) {
        const data = args[1];
        if (ArrayBuffer.isView(data)) {
          metrics[data.buffer === rustMemory?.buffer ? "borrowedInputBytes" : "copiedInputBytes"] += data.byteLength;
        }
        return observe(Reflect.construct(target, args));
      },
    });
    const create = value.InferenceSession.create;
    value.InferenceSession.create = async function (...args) {
      // A/B tests change only this real session option, keeping the release Worker and models identical.
      if (preferredLayout !== null) args[1] = { ...args[1], executionProviders: (args[1]?.executionProviders ?? []).map(provider =>
        (provider.name ?? provider) === "webgpu" ? { ...(typeof provider === "string" ? { name: provider } : provider), preferredLayout } : provider) };
      metrics.providers = args[1]?.executionProviders ?? ["wasm"];
      // Reject only the GPU session boundary; recovery must initialize and run real CPU inference.
      if ((new URL(location.href).searchParams.has("failGpuInit") || (new URL(location.href).searchParams.has("failTsrGpuInit") && metrics.sessions === 1)) && metrics.providers.some(provider => (provider.name ?? provider) === "webgpu")) {
        throw new Error("Injected WebGPU session initialization failure");
      }
      const session = await create.apply(this, args);
      if (benchmarkParameters.has("benchmark") && !metrics.adapter) {
        const device = await value.env.webgpu.device;
        const info = device.adapterInfo;
        metrics.adapter = info ? { vendor: info.vendor, architecture: info.architecture, device: info.device, description: info.description, isFallbackAdapter: info.isFallbackAdapter } : null;
        metrics.deviceFeatures = [...device.features];
        metrics.wasmThreads = value.env.wasm.numThreads;
      }
      metrics.sessions++;
      const model = { name: session.inputNames.includes("x") ? "tsr" : "layout", providers: metrics.providers, calls: 0, gpuSubmissions: 0 };
      // Observe the exact optimization options received by the real ORT Web session.
      model.graphOptimizationLevel = args[1]?.graphOptimizationLevel;
      model.enableMemPattern = args[1]?.enableMemPattern;
      metrics.models.push(model);
      const release = session.release;
      session.release = async function (...args) { const result = await release.apply(this, args); metrics.sessions--; return result; };
      const run = session.run;
      session.run = async function (...args) {
        if (new URL(location.href).searchParams.has("growMemory")) {
          if (!rustMemory) throw new Error("DocParse memory was not observed");
          // Grow the actual parser heap after input creation, just as PDFium prefetch can
          // do while an asynchronous ORT run holds its inputs. Inference remains real.
          rustMemory.grow(1);
          metrics.forcedMemoryGrowth = (metrics.forcedMemoryGrowth ?? 0) + 1;
        }
        if (args[0].image?.dims?.[2] === 640) model.name = "tsr_cell_detection";
        metrics.calls++;
        metrics.fetches = Array.isArray(args[1]) ? args[1] : Object.keys(args[1] ?? {});
        const started = performance.now();
        const submissions = metrics.gpuSubmissions;
        const outputs = await run.apply(this, args);
        model.calls++;
        model.gpuSubmissions += metrics.gpuSubmissions - submissions;
        metrics.inferenceMs.push(performance.now() - started);
        metrics.outputs = Object.keys(outputs);
        for (const value of Object.values(outputs)) observe(value);
        return outputs;
      };
      return session;
    };
  },
});

// ORT may import a memory instead of exporting it from its module.
const Memory = WebAssembly.Memory;
WebAssembly.Memory = new Proxy(Memory, {
  construct(target, argumentsList) {
    const memory = Reflect.construct(target, argumentsList);
    memories.add(memory);
    return memory;
  },
});

/** Observes allocated module memories while preserving the real WebAssembly APIs. */
for (const name of ["instantiate", "instantiateStreaming"]) {
  const original = WebAssembly[name];
  WebAssembly[name] = async function (...args) {
    const result = await original.apply(this, args);
    const instance = result.instance ?? result;
    if (typeof instance.exports?.webparser_create === "function") rustMemory = instance.exports.memory;
    for (const value of Object.values(instance.exports ?? {})) { if (value instanceof WebAssembly.Memory) memories.add(value); }
    return result;
  };
}

/** Adds test observations to ordinary responses without changing the production protocol. */
globalThis.postMessage = function (message, ...args) {
  // Timing callbacks must not repeatedly clone the growing inference history during benchmarks.
  if (benchmarkParameters.has("benchmark") && !("ok" in message)) { send(message, ...args); return; }
  metrics.memoryBytes = [...memories].map(memory => memory.buffer.byteLength);
  metrics.resizableMemory = rustMemory?.buffer.resizable === true;
  send({ ...message, metrics }, ...args);
};
await import(new URL(location.href).searchParams.get("worker"));
globalThis.removeEventListener("message", bufferStartup);
for (const data of queued) globalThis.dispatchEvent(new MessageEvent("message", { data }));
