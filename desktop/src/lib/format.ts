/** Presentation helpers: every number in the UI is formatted here. */

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return "0 B";
  }
  const units = ["B", "KB", "MB", "GB", "TB"];
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1000)), units.length - 1);
  const value = bytes / 1000 ** exponent;
  return `${value.toFixed(value >= 100 || exponent === 0 ? 0 : 1)} ${units[exponent]}`;
}

export function formatMegabytes(megabytes: number): string {
  return formatBytes(megabytes * 1_000_000);
}

export function formatGigabytes(gigabytes: number): string {
  return `${gigabytes.toFixed(gigabytes >= 10 ? 0 : 1)} GB`;
}

export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) {
    return "—";
  }
  if (ms < 1000) {
    return `${Math.round(ms)} ms`;
  }
  if (ms < 60_000) {
    return `${(ms / 1000).toFixed(1)} s`;
  }
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.round((ms % 60_000) / 1000);
  return `${minutes}m ${seconds}s`;
}

export function formatUptime(seconds: number): string {
  if (seconds < 60) {
    return `${seconds}s`;
  }
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes}m ${seconds % 60}s`;
  }
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

export function formatRate(tokensPerSecond: number): string {
  if (!Number.isFinite(tokensPerSecond) || tokensPerSecond <= 0) {
    return "0";
  }
  return tokensPerSecond >= 100
    ? tokensPerSecond.toFixed(0)
    : tokensPerSecond.toFixed(1);
}

export function formatPercent(value: number): string {
  return `${value.toFixed(value >= 10 ? 0 : 1)}%`;
}

export function formatTime(timestampMs: number): string {
  const date = new Date(timestampMs);
  if (Number.isNaN(date.getTime())) {
    return "—";
  }
  return date.toLocaleTimeString([], { hour12: false });
}

export function formatDateTime(timestampMs: number): string {
  const date = new Date(timestampMs);
  if (Number.isNaN(date.getTime())) {
    return "—";
  }
  return `${date.toLocaleDateString()} ${date.toLocaleTimeString([], { hour12: false })}`;
}

/** `qwen2.5-0.5b-instruct-gguf` reads better as `Qwen2.5 0.5B Instruct`. */
export function humaniseModelId(id: string): string {
  const cleaned = id.replace(/\.gguf$/i, "").replace(/[-_]/g, " ");
  return cleaned.replace(/\b\w/g, (character) => character.toUpperCase());
}

/** Turn an error-ish value into one friendly sentence. */
export function describeError(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === "string") {
    return error;
  }
  return "Something went wrong.";
}

export function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}
