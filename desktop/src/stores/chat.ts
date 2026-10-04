/**
 * The chat transcript.
 *
 * Tokens arrive through Tauri events, so streaming survives moving between
 * pages: the store owns the turns, the Chat page only renders them.
 */

import { create } from "zustand";

import type { ChatMessage, Role, SamplingParams, TokenUsage } from "@/lib/types";

export interface ChatTurn {
  id: string;
  role: Role;
  content: string;
  /** Set while tokens are still arriving. */
  streaming: boolean;
  error?: string;
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
  beginStream: (userMessage: string, requestId: string, assistantId: string) => void;
  appendToken: (text: string) => void;
  finishStream: (stats: StreamStats) => void;
  failStream: (message: string) => void;
  stopStream: () => void;
  clear: () => void;
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

  beginStream: (userMessage, requestId, assistantId) =>
    set((state) => ({
      requestId,
      streaming: true,
      stats: null,
      lastError: null,
      turns: [
        ...state.turns,
        { id: nextId("user"), role: "user", content: userMessage, streaming: false },
        { id: assistantId, role: "assistant", content: "", streaming: true },
      ],
    })),

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
        turns[turns.length - 1] = { ...last, streaming: false };
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

  clear: () => set({ turns: [], stats: null, requestId: null, streaming: false, lastError: null }),

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
