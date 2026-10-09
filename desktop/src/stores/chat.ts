/**
 * The chat transcript.
 *
 * Tokens arrive through Tauri events, so streaming survives moving between
 * pages: the store owns the turns, the Chat page only renders them.
 *
 * The store also owns the *identity* of the saved conversation, but never a
 * second copy of its turns: `lib/sessions.ts` maps `turns` to disk at turn
 * boundaries, so what the user sees and what survives a quit cannot diverge.
 */

import { create } from "zustand";

import type { ChatMessage, ChatSession, Role, SamplingParams, TokenUsage } from "@/lib/types";

export interface ChatTurn {
  id: string;
  role: Role;
  content: string;
  /** Set while tokens are still arriving. */
  streaming: boolean;
  error?: string;
  /** When this turn happened. An answer is dated when it finishes, not starts. */
  createdAtMs: number;
  /** Measured decode rate, kept per turn so an export can show each answer. */
  tokensPerSecond?: number;
}

/** Which file on disk this transcript belongs to, and what it is called. */
export interface ChatIdentity {
  id: string;
  title: string;
  createdAtMs: number;
}

export interface StreamStats {
  tokensPerSecond: number;
  elapsedMs: number;
  usage: TokenUsage | null;
  finishReason: string | null;
  simulated: boolean;
}

interface ChatState {
  turns: ChatTurn[];
  /** Null until the first turn is written out. */
  session: ChatIdentity | null;
  /** Which model last answered: a stream can load one the Engine store never saw. */
  modelId: string | null;
  /** Unsent input, so another page can hand a prompt to the Chat page. */
  draft: string;
  systemPrompt: string;
  preset: string;
  params: SamplingParams | null;
  requestId: string | null;
  streaming: boolean;
  stats: StreamStats | null;
  /** Last stream failure, so the page can show a retry affordance. */
  lastError: string | null;
  setDraft: (value: string) => void;
  setSystemPrompt: (value: string) => void;
  setPreset: (preset: string, params: SamplingParams) => void;
  setParams: (params: SamplingParams) => void;
  /** Append the user message and the empty assistant turn the stream fills. */
  beginStream: (
    userMessage: string,
    requestId: string,
    assistantId: string,
    modelId: string,
  ) => void;
  appendToken: (text: string) => void;
  finishStream: (stats: StreamStats) => void;
  failStream: (message: string) => void;
  stopStream: () => void;
  /** Empty the window. The conversation itself stays on disk. */
  startNew: () => void;
  /** Show a stored conversation, including the instruction it was had under. */
  openSession: (session: ChatSession) => void;
  /** Adopt the title and date Rust settled on for this transcript. */
  adoptSession: (identity: ChatIdentity) => void;
  renameSession: (title: string) => void;
  removeTurn: (id: string) => void;
  retryLast: () => ChatMessage[] | null;
}

let sequence = 0;

function nextId(prefix: string): string {
  sequence += 1;
  return `${prefix}-${sequence}`;
}

export const useChat = create<ChatState>((set, get) => ({
  turns: [],
  session: null,
  modelId: null,
  draft: "",
  systemPrompt: "You are a concise, honest assistant running locally on this machine.",
  preset: "balanced",
  params: null,
  requestId: null,
  streaming: false,
  stats: null,
  lastError: null,

  setDraft: (value) => set({ draft: value }),
  setSystemPrompt: (value) => set({ systemPrompt: value }),
  setPreset: (preset, params) => set({ preset, params }),
  setParams: (params) => set({ params, preset: "custom" }),

  beginStream: (userMessage, requestId, assistantId, modelId) =>
    set((state) => {
      const now = Date.now();
      return {
        requestId,
        modelId,
        streaming: true,
        stats: null,
        lastError: null,
        turns: [
          ...state.turns,
          { id: nextId("user"), role: "user", content: userMessage, streaming: false, createdAtMs: now },
          { id: assistantId, role: "assistant", content: "", streaming: true, createdAtMs: now },
        ],
      };
    }),

  appendToken: (text) =>
    set((state) => {
      const turns = [...state.turns];
      const last = turns[turns.length - 1];
      if (last && last.role === "assistant" && last.streaming) {
        turns[turns.length - 1] = { ...last, content: last.content + text };
      }
      return { turns };
    }),

  finishStream: (stats) =>
    set((state) => {
      const turns = [...state.turns];
      const last = turns[turns.length - 1];
      if (last && last.role === "assistant") {
        turns[turns.length - 1] = {
          ...last,
          streaming: false,
          createdAtMs: Date.now(),
          tokensPerSecond: stats.tokensPerSecond,
        };
      }
      return { turns, streaming: false, requestId: null, stats };
    }),

  failStream: (message) =>
    set((state) => {
      const turns = [...state.turns];
      const last = turns[turns.length - 1];
      if (last && last.role === "assistant" && last.streaming) {
        if (last.content.length === 0) {
          turns[turns.length - 1] = { ...last, streaming: false, error: message };
        } else {
          turns[turns.length - 1] = { ...last, streaming: false };
        }
      }
      return { turns, streaming: false, requestId: null, lastError: message };
    }),

  stopStream: () =>
    set((state) => ({
      streaming: false,
      turns: state.turns.map((turn) => (turn.streaming ? { ...turn, streaming: false } : turn)),
    })),

  startNew: () =>
    set({
      turns: [],
      session: null,
      modelId: null,
      stats: null,
      requestId: null,
      streaming: false,
      lastError: null,
    }),

  openSession: (session) =>
    set({
      // A stored conversation replays under the instruction it was started
      // with, and the sidebar shows that value rather than a global default.
      systemPrompt: session.systemPrompt,
      modelId: session.modelId,
      session: { id: session.id, title: session.title, createdAtMs: session.createdAtMs },
      stats: null,
      lastError: null,
      requestId: null,
      streaming: false,
      turns: session.turns.map((turn, index) => ({
        // The `stored-` prefix is disjoint from the page's `turn-N` and the
        // store's `user-N`, so a restored transcript cannot collide with a
        // live one.
        id: `stored-${index}`,
        role: turn.role,
        content: turn.content,
        streaming: false,
        error: turn.error ?? undefined,
        createdAtMs: turn.createdAtMs,
        tokensPerSecond: turn.tokensPerSecond ?? undefined,
      })),
    }),

  adoptSession: (identity) => set({ session: identity }),

  renameSession: (title) =>
    set((state) => (state.session ? { session: { ...state.session, title } } : state)),

  removeTurn: (id) => set((state) => ({ turns: state.turns.filter((turn) => turn.id !== id) })),

  /** Drop the last exchange and hand the remaining history back for a resend. */
  retryLast: () => {
    const { turns } = get();
    const withoutLastAnswer = [...turns];
    while (withoutLastAnswer.length > 0 && withoutLastAnswer[withoutLastAnswer.length - 1].role !== "user") {
      withoutLastAnswer.pop();
    }
    if (withoutLastAnswer.length === 0) {
      return null;
    }
    const prompt = withoutLastAnswer[withoutLastAnswer.length - 1];
    const history = withoutLastAnswer.slice(0, -1).map(
      (turn): ChatMessage => ({ role: turn.role, content: turn.content }),
    );
    set({
      turns: withoutLastAnswer.slice(0, -1),
      streaming: false,
      requestId: null,
      stats: null,
      lastError: null,
    });
    return [...history, { role: "user", content: prompt.content } satisfies ChatMessage];
  },
}));
