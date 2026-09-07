// Capability-only fault injection keeps the actual CPU model and parser in fallback tests.
if (new URL(location.href).searchParams.has("noGpu")) Object.defineProperty(navigator, "gpu", {value: undefined, configurable: true});
const metrics = { calls: 0, liveTensors: 0, sessions: 0, fetches: [], outputs: [], memoryBytes: [], providers: [] };
const memories = new Set();
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
    value.Tensor = new Proxy(Tensor, { construct: (target, args) => observe(Reflect.construct(target, args)) });
    const create = value.InferenceSession.create;
    value.InferenceSession.create = async function (...args) {
      metrics.providers = args[1]?.executionProviders ?? ["wasm"];
      const session = await create.apply(this, args);
      metrics.sessions++;
      const release = session.release;
      session.release = async function (...args) { const result = await release.apply(this, args); metrics.sessions--; return result; };
      const run = session.run;
      session.run = async function (...args) {
        metrics.calls++;
        metrics.fetches = Array.isArray(args[1]) ? args[1] : Object.keys(args[1] ?? {});
        const outputs = await run.apply(this, args);
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
    for (const value of Object.values(instance.exports ?? {})) { if (value instanceof WebAssembly.Memory) memories.add(value); }
    return result;
  };
}

/** Adds test observations to ordinary responses without changing the production protocol. */
globalThis.postMessage = function (message, ...args) {
  metrics.memoryBytes = [...memories].map(memory => memory.buffer.byteLength);
  send({ ...message, metrics }, ...args);
};
await import(new URL(location.href).searchParams.get("worker"));
globalThis.removeEventListener("message", bufferStartup);
for (const data of queued) globalThis.dispatchEvent(new MessageEvent("message", { data }));
