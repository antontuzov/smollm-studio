// The cards a run is parked behind, and the press that ends the wait.
//
// The host can prove a prompt was raised and that it took an answer; only this store can be tested
// against what the window promises on top of that: one card per question rather than one per event, a
// press that reaches the command exactly once, a card that survives a press the host never took, and no
// card left asking over a run that has already ended.
import { beforeEach, describe, expect, it, vi } from "vitest";

import { ipc, type AgentEvent, type RunReport } from "../lib/ipc";
import { secondsLeft, useApproval, type Prompt } from "./approval";

vi.mock("../lib/ipc", () => ({ ipc: { resolveApproval: vi.fn() } }));

const mock = vi.mocked(ipc);

const REPORT: RunReport = {
  generatedTokens: null,
  promptTokens: null,
  cachedTokens: null,
  generatedPerSecond: null,
  promptPerSecond: null,
  elapsedMs: 10,
};

function asked(
  callId: string,
  timeoutMs = 120_000,
  command: string | null = "cargo test",
): AgentEvent {
  return {
    type: "approvalRequested",
    callId,
    toolId: "terminal",
    headline: "Run `cargo test`",
    permission: "side_effect",
    timeoutMs,
    command,
  };
}

function settled(callId: string): AgentEvent {
  return { type: "approvalResolved", callId, decision: "userApproved" };
}

function ended(): AgentEvent {
  return {
    type: "runFinished",
    runId: "run-1",
    stopped: false,
    contextUsed: null,
    totalDiff: null,
    run: REPORT,
  };
}

/** The card for one call, read out loud rather than indexed: a store that dropped it is a failure that
 * says which prompt it lost, not a `possibly undefined` on the next line. */
function card(callId: string): Prompt {
  const prompt = useApproval.getState().prompts.find((p) => p.callId === callId);
  if (!prompt) throw new Error(`no card open for ${callId}`);
  return prompt;
}

beforeEach(() => {
  vi.clearAllMocks();
  useApproval.setState(useApproval.getInitialState());
});

describe("opening cards", () => {
  it("opens a card that counts down to the host's own patience", () => {
    const before = Date.now();
    useApproval.getState().apply(asked("call-1", 30_000));
    const prompt = card("call-1");
    expect(prompt).toMatchObject({
      callId: "call-1",
      headline: "Run `cargo test`",
      permission: "side_effect",
      command: "cargo test",
      pressed: null,
    });
    // Stamped from the event's own timeout rather than a number the card invents, so the seconds the
    // user reads are the seconds that decide.
    expect(prompt.deadline).toBeGreaterThanOrEqual(before + 30_000);
    expect(prompt.deadline).toBeLessThanOrEqual(Date.now() + 30_000);
  });

  it("holds one card per call when the same question arrives twice", () => {
    useApproval.getState().apply(asked("call-1"));
    useApproval.getState().apply(asked("call-1"));
    expect(useApproval.getState().prompts).toHaveLength(1);
  });

  it("holds both cards when a round asks twice", () => {
    // A round's calls go out together, so two of them can be waiting at once — and answering one must
    // not be allowed to settle the other.
    useApproval.getState().apply(asked("call-1"));
    useApproval.getState().apply(asked("call-2"));
    expect(useApproval.getState().prompts.map((p) => p.callId)).toEqual(["call-1", "call-2"]);
    useApproval.getState().apply(settled("call-1"));
    expect(useApproval.getState().prompts.map((p) => p.callId)).toEqual(["call-2"]);
  });

  it("drops the command line a prompt never carried", () => {
    useApproval.getState().apply(asked("call-1", 120_000, null));
    expect(card("call-1").command).toBeNull();
  });
});

describe("answering", () => {
  it("sends the press once and stands the card down while the host takes it", async () => {
    let release: () => void = () => {};
    mock.resolveApproval.mockReturnValue(
      new Promise<void>((resolve) => {
        release = resolve;
      }),
    );
    useApproval.getState().apply(asked("call-1"));

    const pressing = useApproval.getState().answer("call-1", true);
    // Marked before the command resolved: the wait ends the instant the host takes the answer, and a
    // card that looked pressable across that gap would invite a second answer.
    expect(card("call-1").pressed).toBe("allow");
    await useApproval.getState().answer("call-1", false);
    expect(mock.resolveApproval).toHaveBeenCalledTimes(1);
    expect(mock.resolveApproval).toHaveBeenCalledWith("call-1", true);

    release();
    await pressing;
    expect(card("call-1").pressed).toBe("allow");
  });

  it("records a denial as a denial", async () => {
    mock.resolveApproval.mockResolvedValue(undefined);
    useApproval.getState().apply(asked("call-1"));
    await useApproval.getState().answer("call-1", false);
    expect(mock.resolveApproval).toHaveBeenCalledWith("call-1", false);
    expect(card("call-1").pressed).toBe("deny");
  });

  it("leaves the card standing when the answer never reached the host", async () => {
    const logged = vi.spyOn(console, "error").mockImplementation(() => {});
    mock.resolveApproval.mockRejectedValue("no window");
    useApproval.getState().apply(asked("call-1"));
    await useApproval.getState().answer("call-1", true);
    // The run really is still waiting, so the card stays and the press can simply be made again.
    expect(card("call-1").pressed).toBeNull();
    await useApproval.getState().answer("call-1", true);
    expect(mock.resolveApproval).toHaveBeenCalledTimes(2);
    logged.mockRestore();
  });

  it("ignores a press on a card that is not on screen", async () => {
    mock.resolveApproval.mockResolvedValue(undefined);
    await useApproval.getState().answer("call-gone", true);
    expect(mock.resolveApproval).not.toHaveBeenCalled();
  });
});

describe("closing", () => {
  it("closes every card when the run ends, however it closed them", () => {
    useApproval.getState().apply(asked("call-1"));
    useApproval.getState().apply(asked("call-2"));
    useApproval.getState().apply(ended());
    expect(useApproval.getState().prompts).toEqual([]);
  });

  it("takes a failure as the end of the run too", () => {
    useApproval.getState().apply(asked("call-1"));
    useApproval.getState().apply({ type: "failed", message: "the stream died" });
    expect(useApproval.getState().prompts).toEqual([]);
  });

  it("is unmoved by the events that name no prompt", () => {
    useApproval.getState().apply(asked("call-1"));
    useApproval.getState().apply({ type: "thoughtDelta", text: "thinking" });
    expect(useApproval.getState().prompts).toHaveLength(1);
  });
});

describe("the countdown", () => {
  it("counts whole seconds up, and never below the moment the host stops waiting", () => {
    useApproval.getState().apply(asked("call-1", 10_000));
    const prompt = card("call-1");
    // Rounded up, so the number on screen is never less time than the run will actually hold for — a
    // card that read "0 s" while it still waited two hundred milliseconds would be a lie about the one
    // thing the user is deciding on.
    expect(secondsLeft(prompt, prompt.deadline - 6_400)).toBe(7);
    expect(secondsLeft(prompt, prompt.deadline - 200)).toBe(1);
    expect(secondsLeft(prompt, prompt.deadline)).toBe(0);
    expect(secondsLeft(prompt, prompt.deadline + 60_000)).toBe(0);
  });
});
