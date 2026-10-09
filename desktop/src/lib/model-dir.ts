/**
 * Moving the model folder.
 *
 * Pointing the app somewhere else is a settings change; taking the models with it
 * is a filesystem operation on files that can be several gigabytes each. The two
 * stay apart on purpose — **Save** re-points, **Move** does both and reports what
 * actually happened, because a rename and a copy across volumes are very
 * different waits.
 */

import { open } from "@tauri-apps/plugin-dialog";

import { reportFailure } from "./actions";
import { api } from "./api";
import { formatBytes } from "./format";
import { queryClient } from "./query-client";

import type { Relocation } from "./types";

/** Ask the OS for a folder. A closed panel is not a failure, so it returns null. */
export async function pickModelDir(current?: string): Promise<string | null> {
  try {
    const chosen = await open({
      title: "Choose a folder for the models",
      directory: true,
      defaultPath: current || undefined,
    });
    return typeof chosen === "string" ? chosen : null;
  } catch (error) {
    reportFailure("Could not open the folder panel", error);
    return null;
  }
}

/** The folder the models live in now, in the shape the setting stores it. */
export function sameFolder(a?: string | null, b?: string | null): boolean {
  const trim = (value?: string | null) => (value ?? "").replace(/[\\/]+$/, "");
  const left = trim(a);
  const right = trim(b);
  return left.length > 0 && left === right;
}

/**
 * Relocate the library and re-point everything at it.
 *
 * Every query is invalidated rather than the few that look obviously affected:
 * the model folder is behind the library list, the catalog's *downloaded* badges,
 * the download manager and the app info footer, and a move that left some of them
 * stale is a page full of files that no longer exist.
 */
export async function moveModelDir(modelDir: string): Promise<Relocation | null> {
  try {
    const report = await api.setModelDir(modelDir);
    await queryClient.invalidateQueries();
    return report;
  } catch (error) {
    reportFailure("Could not move the model folder", error);
    return null;
  }
}

/** A sentence that says what happened, including the files that did not move. */
export function describeMove(report: Relocation): {
  title: string;
  description: string;
  variant: "success" | "warning";
} {
  const moved = report.moved + report.copied;
  const left = report.duplicates.length + report.conflicts.length + report.failures.length;
  if (left > 0) {
    return {
      title: `${moved} file(s) moved, ${left} left in ${report.from}`,
      description:
        `${report.duplicates.length} name(s) the new folder already held, ` +
        `${report.conflicts.length} that disagreed about their size, ` +
        `${report.failures.length} that could not move. Nothing was overwritten — ` +
        `the old folder still holds them.`,
      variant: "warning",
    };
  }
  if (moved === 0) {
    return {
      title: "Nothing to move",
      description: `${report.to} holds the models now. The old folder had no model files in it.`,
      variant: "success",
    };
  }
  const how =
    report.copied > 0
      ? `${report.copied} of them copied, because the folders are on different volumes`
      : "renamed in place, so no bytes were re-written";
  return {
    title: `${moved} file(s) moved`,
    description: `${formatBytes(report.bytes)} now in ${report.to} — ${how}.`,
    variant: "success",
  };
}
