import type { DocumentResult, ExecutionProvider, ModelSource, PageImageResult, ParserProgress, RenderFormat, WebParseConfig } from "./types.js";

/** Correlates each private Worker operation with its payload and successful result. */
export interface WorkerOperations {
  init: {
    payload: { artifacts: ModelSource; config?: WebParseConfig; executionProvider: ExecutionProvider; allowCpuFallback: boolean; runtimeBaseUrl?: string; observeProgress: boolean };
    result: ExecutionProvider;
  };
  parse: {
    payload: { bytes: Uint8Array; observeProgress: boolean; pageImages: boolean };
    result: DocumentResult;
  };
  render: {
    payload: { document: DocumentResult; format: RenderFormat; observeProgress?: false };
    result: string;
  };
}

export type WorkerMethod = keyof WorkerOperations;
export type WorkerResult<M extends WorkerMethod> = WorkerOperations[M]["result"];
/** Discriminated payloads prevent a method from accidentally receiving another method's arguments. */
export type WorkerCommand = { [M in WorkerMethod]: { method: M; payload: WorkerOperations[M]["payload"] } }[WorkerMethod];
export type WorkerRequest = WorkerCommand & { id: number };
export type WorkerSuccess = { [M in WorkerMethod]: { id: number; ok: true; method: M; value: WorkerResult<M> } }[WorkerMethod];

/** Intermediate events never settle a request; terminal responses retain the originating operation. */
export type WorkerResponse =
  | { id: number; event: "progress"; value: ParserProgress }
  | { id: number; event: "page_image"; value: PageImageResult }
  | WorkerSuccess
  | { id: number; ok: false; code: string; message: string; stack?: string }
  | { fatal: true; code: string; message: string; stack?: string };
