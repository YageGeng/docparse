import type { DocumentResult, ExecutionProvider, ModelSource, PageImageResult, ParserProgress, ParserTiming, RenderFormat, WebParseConfig, TableOptions, TsrTableInput, TsrTableRequest } from "./types.js";

/** Correlates each private Worker operation with its payload and successful result. */
export interface WorkerOperations {
  init: {
    payload: { artifacts: ModelSource; config?: WebParseConfig; executionProvider: ExecutionProvider; allowCpuFallback: boolean; runtimeBaseUrl?: string; observeProgress: boolean; observeTiming: boolean };
    result: ExecutionProvider;
  };
  parse: {
    payload: { bytes: Uint8Array; table?: TableOptions; externalTables?: boolean; observeProgress: boolean; observeTiming: boolean; pageImages: boolean };
    result: DocumentResult;
  };
  render: {
    payload: { document: DocumentResult; format: RenderFormat; observeProgress?: false; observeTiming?: false };
    result: string;
  };
}

export type WorkerMethod = keyof WorkerOperations;
export type WorkerResult<M extends WorkerMethod> = WorkerOperations[M]["result"];
/** Discriminated payloads prevent a method from accidentally receiving another method's arguments. */
export type WorkerCommand = { [M in WorkerMethod]: { method: M; payload: WorkerOperations[M]["payload"] } }[WorkerMethod];
export type WorkerRequest = WorkerCommand & { id: number };
/** Table replies bypass the operation Busy gate while the owning parse awaits them. */
export type WorkerTableReply = { id: number; method: "table_structure_reply"; requestId: string } & ({ ok: true; value: TsrTableInput } | { ok: false; message: string });
export type WorkerInbound = WorkerRequest | WorkerTableReply;
/** Worker-local ABI metadata, before RGB pixels are encoded into the public crop Blob. */
export interface TsrCropPixels extends Omit<TsrTableRequest, "image"> { width: number; height: number; pixels: Uint8Array }

export type WorkerSuccess = { [M in WorkerMethod]: { id: number; ok: true; method: M; value: WorkerResult<M> } }[WorkerMethod];

/** Intermediate events never settle a request; terminal responses retain the originating operation. */
export type WorkerResponse =
  | { id: number; event: "progress"; value: ParserProgress }
  | { id: number; event: "timing"; value: ParserTiming }
  | { id: number; event: "page_image"; value: PageImageResult }
  | { id: number; event: "table_structure_request"; value: TsrTableRequest }
  | { id: number; event: "table_structure_cancel"; requestId: string }
  | WorkerSuccess
  | { id: number; ok: false; code: string; message: string; stack?: string }
  | { fatal: true; code: string; message: string; stack?: string };
