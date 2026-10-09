/**
 * Typed bridge to the Rust commands and events.
 *
 * Pages import `api` and `events` rather than Tauri itself, so command names and
 * argument shapes live in exactly one place.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { CommandError } from "./types";
import type {
  AppInfo,
  BenchmarkConfig,
  BenchmarkProgress,
  BenchmarkResult,
  CatalogEntry,
  CatalogFacets,
  CatalogFilterInput,
  ChatDoneEvent,
  ChatErrorEvent,
  ChatRequest,
  ChatSession,
  ChatTokenEvent,
  DoctorReport,
  DownloadCancelled,
  DownloadCompleted,
  DownloadFailedEvent,
  DownloadProgress,
  DownloadTask,
  EngineMetrics,
  ErrorPayload,
  ExportFormat,
  HardwareReport,
  LoadModelOptions,
  LoadModelResponse,
  LocalModel,
  LogEntry,
  LogFilter,
  ModelVerification,
  Relocation,
  ResetOutcome,
  SamplingPreset,
  ServerConfig,
  ServerExamples,
  ServerLogEvent,
  ServerStatus,
  SessionHit,
  SessionIndex,
  Settings,
  TokenStatus,
} from "./types";

/** A command rejection is an `AppError` serialised as `{ code, message, detail }`. */
export function toCommandError(value: unknown): CommandError {
  if (value instanceof CommandError) {
    return value;
  }
  if (value instanceof Error) {
    return new CommandError({ code: "unknown", message: value.message });
  }
  if (typeof value === "string") {
    return new CommandError({ code: "unknown", message: value });
  }
  const payload = (value ?? {}) as Partial<ErrorPayload>;
  return new CommandError({
    code: payload.code ?? "unknown",
    message: payload.message ?? "The command failed without a message.",
    detail: payload.detail ?? null,
  });
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw toCommandError(error);
  }
}

export const api = {
  detectHardware: () => call<HardwareReport>("detect_hardware"),
  getAppInfo: () => call<AppInfo>("get_app_info"),
  getDoctorReport: () => call<DoctorReport>("get_doctor_report"),

  listCatalogModels: (input: CatalogFilterInput) =>
    call<CatalogEntry[]>("list_catalog_models", {
      filters: {
        query: input.query,
        sort: input.sort,
        minParametersB: input.minParametersB ?? null,
        maxParametersB: input.maxParametersB ?? null,
        quantization: input.quantization ?? null,
        tag: input.tag ?? null,
        license: input.license ?? null,
        architecture: input.architecture ?? null,
        hidePlaceholders: input.hidePlaceholders,
      },
      // `null` means "both downloaded and not" so the command can tell the
      // three states apart.
      downloaded: input.downloaded === undefined ? null : input.downloaded,
    }),
  catalogFacets: () => call<CatalogFacets>("catalog_facets"),
  listLocalModels: () => call<LocalModel[]>("list_local_models"),
  pullModel: (modelId: string) => call<DownloadTask>("pull_model", { modelId }),
  cancelDownload: (downloadId: string) =>
    call<DownloadTask | null>("cancel_download", { downloadId }),
  retryDownload: (downloadId: string) => call<DownloadTask>("retry_download", { downloadId }),
  downloadSnapshot: () => call<DownloadTask[]>("get_download_snapshot"),
  deleteLocalModel: (fileName: string) => call<string>("delete_local_model", { fileName }),
  importModel: (path: string) => call<LocalModel>("import_model", { path }),
  verifyModels: (fileName?: string) =>
    call<ModelVerification[]>("verify_local_models", { fileName: fileName ?? null }),

  loadModel: (modelId: string, options?: LoadModelOptions) =>
    call<LoadModelResponse>("load_model", { modelId, options }),
  unloadModel: () => call<EngineMetrics>("unload_model"),
  engineMetrics: () => call<EngineMetrics>("get_engine_metrics"),

  startChatStream: (request: ChatRequest) => call<string>("start_chat_stream", { request }),
  stopGeneration: (requestId: string) => call<boolean>("stop_generation", { requestId }),

  newChatSession: (systemPrompt: string, modelId?: string | null) =>
    call<ChatSession>("new_chat_session", { systemPrompt, modelId: modelId ?? null }),
  saveChatSession: (session: ChatSession) => call<ChatSession>("save_chat_session", { session }),
  listChatSessions: () => call<SessionIndex>("list_chat_sessions"),
  searchChatSessions: (query: string) => call<SessionHit[]>("search_chat_sessions", { query }),
  getChatSession: (id: string) => call<ChatSession>("get_chat_session", { id }),
  renameChatSession: (id: string, title: string) =>
    call<ChatSession>("rename_chat_session", { id, title }),
  deleteChatSession: (id: string) => call<null>("delete_chat_session", { id }),
  exportChatSession: (id: string, path: string, format: ExportFormat) =>
    call<string>("export_chat_session", { id, path, format }),

  startServer: (config?: ServerConfig) => call<ServerStatus>("start_server", { config }),
  stopServer: () => call<ServerStatus>("stop_server"),
  serverStatus: () => call<ServerStatus>("get_server_status"),
  serverExamples: (modelId?: string) =>
    call<ServerExamples>("get_server_examples", { modelId }),

  runBenchmark: (config: BenchmarkConfig) => call<BenchmarkResult>("run_benchmark", { config }),

  getLogs: (filter?: LogFilter) => call<LogEntry[]>("get_logs", { filter }),
  clearLogs: () => call<number>("clear_logs"),

  getSettings: () => call<Settings>("get_settings"),
  saveSettings: (settings: Settings) => call<Settings>("save_settings", { settings }),
  setModelDir: (modelDir: string) => call<Relocation>("set_model_dir", { modelDir }),
  hfTokenStatus: () => call<TokenStatus>("get_hf_token_status"),
  setHfToken: (token: string) => call<TokenStatus>("set_hf_token", { token }),
  clearHfToken: () => call<TokenStatus>("clear_hf_token"),
  getPresets: () => call<SamplingPreset[]>("get_presets"),

  openModelFolder: () => call<string>("open_model_folder"),
  openLogFolder: () => call<string>("open_log_folder"),
  resetAppData: () => call<ResetOutcome>("reset_app_data"),
  exportDiagnostics: () => call<string>("export_diagnostics"),
};

type Listener<T> = (payload: T) => void;

export const events = {
  onChatToken: (handler: Listener<ChatTokenEvent>) =>
    listen<ChatTokenEvent>("chat-token", (raw) => handler(raw.payload)),
  onChatDone: (handler: Listener<ChatDoneEvent>) =>
    listen<ChatDoneEvent>("chat-done", (raw) => handler(raw.payload)),
  onChatError: (handler: Listener<ChatErrorEvent>) =>
    listen<ChatErrorEvent>("chat-error", (raw) => handler(raw.payload)),
  onServerLog: (handler: Listener<ServerLogEvent>) =>
    listen<ServerLogEvent>("server-log", (raw) => handler(raw.payload)),
  onBenchmarkProgress: (handler: Listener<BenchmarkProgress>) =>
    listen<BenchmarkProgress>("benchmark-progress", (raw) => handler(raw.payload)),
  onDownloadProgress: (handler: Listener<DownloadProgress>) =>
    listen<DownloadProgress>("download-progress", (raw) => handler(raw.payload)),
  onDownloadComplete: (handler: Listener<DownloadCompleted>) =>
    listen<DownloadCompleted>("download-complete", (raw) => handler(raw.payload)),
  onDownloadError: (handler: Listener<DownloadFailedEvent>) =>
    listen<DownloadFailedEvent>("download-error", (raw) => handler(raw.payload)),
  onDownloadCancelled: (handler: Listener<DownloadCancelled>) =>
    listen<DownloadCancelled>("download-cancelled", (raw) => handler(raw.payload)),
};

/** One shape for every download event, so stores need a single handler. */
export type DownloadUpdate =
  | { kind: "progress"; payload: DownloadProgress }
  | { kind: "complete"; payload: DownloadCompleted }
  | { kind: "error"; payload: DownloadFailedEvent }
  | { kind: "cancelled"; payload: DownloadCancelled };

/** Subscribe to all four download events with one callback. */
export async function onDownloadUpdates(handler: (update: DownloadUpdate) => void): Promise<UnlistenFn> {
  const subscriptions = await Promise.all([
    events.onDownloadProgress((payload) => handler({ kind: "progress", payload })),
    events.onDownloadComplete((payload) => handler({ kind: "complete", payload })),
    events.onDownloadError((payload) => handler({ kind: "error", payload })),
    events.onDownloadCancelled((payload) => handler({ kind: "cancelled", payload })),
  ]);
  return () => {
    for (const unlisten of subscriptions) {
      unlisten();
    }
  };
}
