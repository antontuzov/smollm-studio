/**
 * Domain values -> presentation.
 *
 * Ratings, download states and log levels reach several pages; mapping them to
 * tones once keeps the pills consistent.
 */

import type { BadgeTone } from "@/components/ui/badge";
import type { Backend, DownloadState, Rating } from "./types";

export function capitalize(value: string): string {
  if (value.length === 0) {
    return value;
  }
  return value.charAt(0).toUpperCase() + value.slice(1);
}

const ratingTones: Record<Rating, BadgeTone> = {
  excellent: "success",
  good: "info",
  fair: "warning",
  poor: "danger",
};

export function ratingTone(rating: Rating): BadgeTone {
  return ratingTones[rating];
}

const downloadStateTones: Record<DownloadState, BadgeTone> = {
  queued: "neutral",
  running: "info",
  retrying: "warning",
  verifying: "primary",
  complete: "success",
  cancelled: "warning",
  failed: "danger",
};

export function downloadStateTone(state: DownloadState): BadgeTone {
  return downloadStateTones[state];
}

export function downloadStateLabel(state: DownloadState): string {
  return state === "verifying" ? "Verifying checksum" : capitalize(state);
}

const backendTones: Record<Backend, BadgeTone> = {
  cpu: "neutral",
  metal: "success",
  cuda: "success",
  vulkan: "info",
  mock: "warning",
};

export function backendTone(backend: Backend): BadgeTone {
  return backendTones[backend];
}

export function backendLabel(backend: Backend): string {
  return backend === "mock" ? "mock engine" : `${backend} backend`;
}

/** Levels arrive as free-form strings from the server log and the log store. */
export function levelTone(level: string): BadgeTone {
  switch (level.toLowerCase()) {
    case "error":
    case "critical":
      return "danger";
    case "warn":
    case "warning":
      return "warning";
    case "info":
      return "info";
    case "debug":
    case "trace":
      return "neutral";
    default:
      return "neutral";
  }
}

/** An engine that is simulating deserves a visible flag, not a silent green. */
export function engineTone(simulated: boolean): BadgeTone {
  return simulated ? "warning" : "success";
}

export function engineLabel(engine: string, simulated: boolean): string {
  return simulated ? `${engine} (simulated)` : engine;
}
