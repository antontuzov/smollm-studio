// The prompts a run is parked on: what the host asked, what the window answered, and the press that
// carries the answer back.
//
// This is deliberately not part of the transcript. A run that is waiting on an answer is not waiting
// politely at the bottom of a scroll: the question has to be where the composer is, in front of the
// person who is asked, and it has to leave the moment it is answered. The call the prompt is about is
// already a transcript row — the host opens it before the wait begins — so the card adds the two
// controls and the countdown, and nothing else.
import { create } from "zustand";

import type { AgentEvent, PermissionClass } from "../lib/ipc";
import { ipc } from "../lib/ipc";

/** One open prompt: the call the run is waiting on, in the words the host chose for it. */
export type Prompt = {
  callId: string;
  toolId: string;
  headline: string;
  permission: PermissionClass;
  /** The exact command, when this is a call that runs one. The headline says what the run is about to
   * do; this is the line behind it, unedited, and it is the reason the card exists. */
  command: string | null;
  /** The instant the host stops waiting, as an epoch. The card counts down to the host's own patience
   * rather than to a number it invented, so the time the user reads is the time that decides. */
  deadline: number;
  /** Set on the press, before the host has answered it: the buttons stop answering while the wait is
   * still closing, and a second press would be a second answer to a question already settled. */
  pressed: "allow" | "deny" | null;
};

type ApprovalStore = {
  /** Every prompt waiting, oldest first. A round's calls go out together, so a run can be held on two
   * answers at once, and one card per call is the only reading that matches the rows above. */
  prompts: Prompt[];
  apply: (event: AgentEvent) => void;
  answer: (callId: string, approved: boolean) => Promise<void>;
};

/** The prompts the run is parked on, and the press that ends the wait. */
export const useApproval = create<ApprovalStore>((set, get) => ({
  prompts: [],
  apply: (event) => {
    if (event.type === "approvalRequested") {
      // The deadline is stamped here, on arrival, rather than sent: the host's clock and this one are
      // the same clock, and a card opened from a reopened conversation has no wait to count down.
      const prompt: Prompt = {
        callId: event.callId,
        toolId: event.toolId,
        headline: event.headline,
        permission: event.permission,
        command: event.command ?? null,
        deadline: Date.now() + event.timeoutMs,
        pressed: null,
      };
      // One card per call, and a repeat of a call already on screen is the same question rather than
      // a second one — the host can only be waiting on a call once.
      set({
        prompts: [...get().prompts.filter((p) => p.callId !== prompt.callId), prompt],
      });
      return;
    }
    if (event.type === "approvalResolved") {
      // The host's word that it stopped waiting. Whether it ran, was refused, or timed out is the row's
      // business — the row the call already has says that — and this store only has to stop asking.
      set({ prompts: get().prompts.filter((p) => p.callId !== event.callId) });
      return;
    }
    if (event.type === "runFinished" || event.type === "failed") {
      // Normally nothing is left: stopping a run settles its open prompts and each one arrives
      // resolved. This is the other case — a run that ended by some path that closed no prompt — and
      // a card over a finished run is a question that can never be answered.
      if (get().prompts.length) set({ prompts: [] });
    }
  },
  answer: async (callId, approved) => {
    const prompt = get().prompts.find((p) => p.callId === callId);
    if (!prompt || prompt.pressed) return;
    // Marked before the command rather than after it. The wait ends as soon as the host takes the
    // answer, so a card that cleared itself on the reply would blink out a moment early and let a
    // second press through in the gap.
    set({
      prompts: get().prompts.map((p) =>
        p.callId === callId ? { ...p, pressed: approved ? "allow" : "deny" } : p,
      ),
    });
    try {
      await ipc.resolveApproval(callId, approved);
    } catch (error) {
      // The press did not reach the host. The card stays, unanswered, because the run really is still
      // waiting on it — and the press can simply be made again.
      set({
        prompts: get().prompts.map((p) => (p.callId === callId ? { ...p, pressed: null } : p)),
      });
      console.error("approval could not be sent:", error);
    }
  },
}));

/** The seconds left before the host stops waiting, floored at zero: the countdown the card shows. */
export function secondsLeft(prompt: Prompt, now: number): number {
  return Math.max(0, Math.ceil((prompt.deadline - now) / 1000));
}
