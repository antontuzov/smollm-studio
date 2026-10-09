/**
 * TypeScript mirrors of the Rust command payloads.
 *
 * The backend serialises every app DTO in camelCase, so these types are a
 * transcription rather than a translation: if a field disagrees with Rust, the
 * Rust definition wins and this file needs the update.
 */

export type Backend = "cpu" | "metal" | "cuda" | "vulkan" | "mock";

export type Platform = "macos" | "windows" | "linux" | "other";

/** An accelerator the linked inference engine reports it can really use. */
export interface Accelerator {
  name: string;
  description: string;
  backend: string;
  usableMemoryGb: number;
  freeMemoryGb: number;
}

export interface HardwareReport {
  platform: Platform;
  arch: string;
  cpuBrand: string;
  logicalCores: number;
  physicalCores: number;
  totalRamGb: number;
  availableRamGb: number;
  appleSilicon: boolean;
  metalAvailable: boolean;
  nvidiaGpu: string | null;
  vulkanAvailable: boolean;
  gpuName: string;
  diskFreeGb: number;
  modelVolumeFreeGb: number;
  /** `null` when no linked engine can name a device to offload to. */
  accelerator: Accelerator | null;
}

export interface DoctorReport {
  platform: string;
  arch: string;
  cpuCores: number;
  ramGb: number;
  gpu: string;
  backendRecommendation: Backend;
  recommendedModels: string[];
  warnings: string[];
  headline: string;
}

export interface AppInfo {
  name: string;
  version: string;
  tagline: string;
  engine: string;
  simulatedEngine: boolean;
  platform: string;
  arch: string;
  dataDir: string;
  modelsDir: string;
  logsDir: string;
}

export type ModelFamily = "qwen2" | "llama3" | "smollm2" | "phi3" | "gemma" | "other";

export type CatalogStatus = "verified" | "placeholder";

export type Rating = "excellent" | "good" | "fair" | "poor";

export interface ModelDescriptor {
  id: string;
  displayName: string;
  hfRepo: string;
  filename: string;
  revision: string;
  parametersB: number;
  quantization: string;
  sizeMb: number;
  contextLength: number;
  vision: boolean;
  license: string;
  family: ModelFamily;
  /** GGUF `general.architecture`; decides which engine can load the file. */
  architecture: string;
  tags: string[];
  recommendedRamGb: number;
  quality: Rating;
  speed: Rating;
  status: CatalogStatus;
  description: string;
}

/** A catalog entry enriched with library and memory facts. */
/** One option in a Models page filter dropdown, with how many entries offer it. */
export interface FacetValue {
  value: string;
  count: number;
}

/** Filter option lists derived from the loaded catalog, so overlays widen them. */
export interface CatalogFacets {
  quantizations: FacetValue[];
  tags: FacetValue[];
  licenses: FacetValue[];
  architectures: FacetValue[];
  maxParametersB: number;
}

/** Everything the Models page filter bar can constrain. */
export interface CatalogFilterInput {
  query: string;
  sort: string;
  /** Parameter-count band edges in billions; `undefined` means unbounded. */
  minParametersB?: number;
  maxParametersB?: number;
  quantization?: string;
  tag?: string;
  license?: string;
  architecture?: string;
  /** `true` = only downloaded, `false` = only not downloaded, `undefined` = both. */
  downloaded?: boolean;
  hidePlaceholders: boolean;
}

export interface CatalogEntry extends ModelDescriptor {
  downloaded: boolean;
  estimatedRamGb: number;
  fitsMemory: boolean;
  downloading: boolean;
  downloadPercent: number;
  /** State of this model's newest transfer, or `null` if there never was one. */
  downloadState: DownloadState | null;
  downloadError: string | null;
}

export interface ModelMetadata {
  name?: string | null;
  architecture?: string | null;
  quantization?: string | null;
  parameterCount?: number | null;
  parametersB?: number | null;
  contextLength?: number | null;
  trainType?: string | null;
  license?: string | null;
  vocabSize?: number | null;
  blockCount?: number | null;
  embeddingLength?: number | null;
  headCount?: number | null;
  headCountKv?: number | null;
  /** Bytes of tensor data in this file, measured from its own header. */
  weightBytes?: number | null;
  nTensors: number;
  ggufVersion: number;
}

export interface LocalModel {
  catalogId: string | null;
  fileName: string;
  path: string;
  sizeBytes: number;
  modifiedMs: number;
  metadata: ModelMetadata;
  parseError: string | null;
}

/**
 * One integrity question about a file already in the library. `skipped` means
 * the file gave no number to compare against — GGUF carries no checksum, so a
 * flipped byte inside tensor data is not something any check here can see.
 */
export type CheckStatus = "passed" | "failed" | "skipped";

export interface VerificationCheck {
  label: string;
  status: CheckStatus;
  detail: string;
}

export interface ModelVerification {
  fileName: string;
  path: string;
  ok: boolean;
  checks: VerificationCheck[];
}

/**
 * What moving the model folder did. Renames are free and instant on one volume;
 * a copy means the folders were on different ones. Anything listed here is a file
 * the old folder still holds, because a move never overwrites a name the new
 * folder already has.
 */
export interface Relocation {
  from: string;
  to: string;
  moved: number;
  copied: number;
  bytes: number;
  duplicates: string[];
  conflicts: string[];
  failures: string[];
}

export type DownloadState =
  | "queued"
  | "running"
  | "retrying"
  | "verifying"
  | "complete"
  | "cancelled"
  | "failed";

export interface DownloadTask {
  id: string;
  modelId: string;
  displayName: string;
  fileName: string;
  url: string;
  path: string;
  totalBytes: number | null;
  downloadedBytes: number;
  bytesPerSecond: number;
  percent: number;
  state: DownloadState;
  error: string | null;
  startedMs: number;
  finishedMs: number | null;
  resumed: boolean;
  sha256: string | null;
  /** 1 until a transient network failure forces a resumed attempt. */
  attempt: number;
}

export interface DownloadProgress {
  downloadId: string;
  modelId: string;
  fileName: string;
  state: DownloadState;
  downloadedBytes: number;
  totalBytes: number | null;
  percent: number;
  bytesPerSecond: number;
  error: string | null;
  attempt: number;
  maxAttempts: number;
}

export interface DownloadCompleted {
  downloadId: string;
  modelId: string;
  path: string;
  sizeBytes: number;
  sha256: string | null;
  elapsedMs: number;
}

export interface DownloadFailedEvent {
  downloadId: string;
  modelId: string;
  code: string;
  message: string;
}

export interface DownloadCancelled {
  downloadId: string;
  modelId: string;
  partialBytes: number;
}

export interface SamplingParams {
  temperature: number;
  topP: number;
  topK: number;
  minP: number;
  maxTokens: number;
  repeatPenalty: number;
  presencePenalty: number;
  seed: number | null;
}

export interface SamplingPreset {
  name: string;
  label: string;
  params: SamplingParams;
}

export interface LoadModelOptions {
  contextLength: number;
  gpuLayers: number;
  backend: Backend;
  threads?: number | null;
}

export interface ModelHandle {
  id: string;
  modelId: string;
  displayName: string;
  path: string;
  engine: string;
  contextLength: number;
  metadata: ModelMetadata;
}

export interface LoadModelResponse {
  handle: ModelHandle;
  engine: string;
  simulated: boolean;
  warnings: string[];
}

export interface EngineMetrics {
  engine: string;
  simulated: boolean;
  modelId: string | null;
  requests: number;
  tokensGenerated: number;
  totalGenerationMs: number;
  tokensPerSecond: number;
  loaded: boolean;
  backend: Backend;
}

export type Role = "system" | "user" | "assistant";

export interface ChatMessage {
  role: Role;
  content: string;
}

export interface ChatRequest {
  requestId: string;
  modelId: string;
  messages: ChatMessage[];
  systemPrompt?: string | null;
  params: SamplingParams;
  stop: string[];
}

/** One turn as stored on disk. */
export interface StoredTurn {
  role: Role;
  content: string;
  createdAtMs: number;
  /** Set when the answer failed; the partial text is kept alongside it. */
  error?: string | null;
  tokensPerSecond?: number | null;
}

/** A conversation, persisted as one JSON file per session. */
export interface ChatSession {
  id: string;
  title: string;
  createdAtMs: number;
  updatedAtMs: number;
  modelId: string | null;
  systemPrompt: string;
  turns: StoredTurn[];
}

export interface SessionSummary {
  id: string;
  title: string;
  createdAtMs: number;
  updatedAtMs: number;
  turnCount: number;
  modelId: string | null;
  preview: string;
}

/** A search match: the summary plus the line that matched. */
export interface SessionHit extends SessionSummary {
  snippet: string;
}

export interface SessionIndex {
  sessions: SessionSummary[];
  /** Transcript files that exist but do not parse, named rather than hidden. */
  unreadable: string[];
}

export type ExportFormat = "markdown" | "json";

export interface TokenUsage {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
}

export interface GenToken {
  text: string;
  finishReason: string | null;
  usage?: TokenUsage | null;
}

export interface ChatTokenEvent {
  requestId: string;
  token: GenToken;
}

export interface ChatDoneEvent {
  requestId: string;
  text: string;
  finishReason: string | null;
  usage?: TokenUsage | null;
  elapsedMs: number;
  tokensPerSecond: number;
  simulated: boolean;
}

export interface ErrorPayload {
  code: string;
  message: string;
  detail?: string | null;
}

export interface ChatErrorEvent extends ErrorPayload {
  requestId: string;
}

export interface ServerConfig {
  host: string;
  port: number;
  defaultModelId?: string | null;
}

export interface ServerStatus {
  running: boolean;
  host: string;
  port: number;
  baseUrl: string;
  engine: string;
  loadedModel: string | null;
  /** Exactly what `/v1/models` offers, resident model first. */
  servedModels: string[];
  requests: number;
  uptimeSeconds: number;
  simulated: boolean;
}

export interface ServerExamples {
  curl: string;
  /** The same call with `stream: true`, for `curl -N`. */
  curlStream: string;
  python: string;
  health: string;
  baseUrl: string;
  model: string;
}

export interface ServerLogEvent {
  timestampMs: number;
  level: string;
  message: string;
}

export interface BenchmarkConfig {
  modelId: string;
  promptTokens: number;
  maxTokens: number;
  contextLength: number;
  gpuLayers: number;
  backend: Backend;
  runs: number;
}

export interface BenchmarkResult {
  modelId: string;
  engine: string;
  simulated: boolean;
  backend: Backend;
  loadMs: number;
  promptTokensPerSecond: number;
  generationTokensPerSecond: number;
  timeToFirstTokenMs: number;
  peakRssMb: number | null;
  promptTokens: number;
  generatedTokens: number;
  runs: number;
  contextLength: number;
  warnings: string[];
}

export interface BenchmarkProgress {
  stage: string;
  percent: number;
  message: string;
}

export type LogStream = "app" | "engine" | "download" | "server";

export interface LogEntry {
  timestampMs: number;
  level: string;
  target: string;
  stream: LogStream;
  message: string;
}

export interface LogFilter {
  levels?: string[];
  stream?: LogStream | null;
  contains?: string | null;
  limit?: number | null;
}

export type ThemeMode = "light" | "dark" | "system";

export interface Settings {
  theme: ThemeMode;
  modelDir?: string | null;
  defaultModelId?: string | null;
  defaultContextLength: number;
  defaultGpuLayers: number;
  defaultBackend: Backend;
  sampling: SamplingParams;
  serverHost: string;
  serverPort: number;
  autoUpdateChecks: boolean;
  onboardingComplete: boolean;
  chatPreset: string;
}

export interface ResetOutcome {
  info: AppInfo;
  clearedSettings: boolean;
  removedPartFiles: number;
  keptModels: number;
  clearedConversations: number;
  note: string;
}

/** Every command error arrives in this shape. */
export class CommandError extends Error {
  readonly code: string;
  readonly detail?: string;

  constructor(payload: ErrorPayload) {
    super(payload.message);
    this.name = "CommandError";
    this.code = payload.code;
    this.detail = payload.detail ?? undefined;
  }
}
