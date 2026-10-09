/**
 * Keeping conversations.
 *
 * The store holds the live transcript; this is the only place that turns it
 * into a `ChatSession` for disk. Saves land at turn boundaries instead of per
 * token, and a failed save is a toast rather than a broken chat: losing the
 * file is bad, but losing the conversation in front of the user is worse.
 */

import { save } from "@tauri-apps/plugin-dialog";

import { reportFailure } from "./actions";
import { api } from "./api";
import { queryClient } from "./query-client";
import { queryKeys } from "./queries";
import { useChat } from "@/stores/chat";
import { toast } from "@/stores/ui";

import type { ChatSession, ExportFormat, StoredTurn } from "./types";
import type { ChatIdentity, ChatTurn } from "@/stores/chat";

function identityOf(session: ChatSession): ChatIdentity {
  return { id: session.id, title: session.title, createdAtMs: session.createdAtMs };
}

function toStored(turn: ChatTurn): StoredTurn {
  return {
    role: turn.role,
    content: turn.content,
    createdAtMs: turn.createdAtMs,
    error: turn.error ?? null,
    tokensPerSecond: turn.tokensPerSecond ?? null,
  };
}

/** An empty bubble says nothing, so it earns neither a replay nor a disk write. */
function isWorthKeeping(turn: ChatTurn): boolean {
  return turn.content.trim().length > 0 || (turn.error?.trim().length ?? 0) > 0;
}

function buildSession(
  identity: ChatIdentity,
  systemPrompt: string,
  modelId: string | null,
  turns: ChatTurn[],
): ChatSession {
  return {
    ...identity,
    // Rust dates the transcript when it writes it; this only has to parse.
    updatedAtMs: identity.createdAtMs,
    modelId,
    systemPrompt,
    turns: turns.filter(isWorthKeeping).map(toStored),
  };
}

/**
 * One save at a time. Without this, a `chat-done` and a click landing in the
 * same tick would both find no session id and mint two files for one chat.
 */
let queue: Promise<void> = Promise.resolve();

export function persistChat(): Promise<void> {
  queue = queue.then(saveTranscript);
  return queue;
}

async function saveTranscript(): Promise<void> {
  const state = useChat.getState();
  if (state.turns.length === 0) {
    return;
  }
  try {
    // Rust mints the id, so the file name and the store cannot disagree about
    // which conversation a turn belongs to.
    let identity = state.session;
    if (identity === null) {
      const created = await api.newChatSession(state.systemPrompt, state.modelId);
      identity = identityOf(created);
    }
    const saved = await api.saveChatSession(
      buildSession(identity, state.systemPrompt, state.modelId, state.turns),
    );
    // The first question names the transcript on disk; adopt that title back.
    useChat.getState().adoptSession(identityOf(saved));
    await queryClient.invalidateQueries({ queryKey: queryKeys.sessions });
  } catch (error) {
    reportFailure("This conversation is not saved", error);
  }
}

export async function openConversation(id: string): Promise<boolean> {
  try {
    const session = await api.getChatSession(id);
    useChat.getState().openSession(session);
    return true;
  } catch (error) {
    reportFailure("Could not open that conversation", error);
    return false;
  }
}

export async function renameConversation(id: string, title: string): Promise<boolean> {
  try {
    const saved = await api.renameChatSession(id, title);
    if (useChat.getState().session?.id === id) {
      useChat.getState().renameSession(saved.title);
    }
    await queryClient.invalidateQueries({ queryKey: queryKeys.sessions });
    return true;
  } catch (error) {
    reportFailure("Could not rename that conversation", error);
    return false;
  }
}

export async function deleteConversation(id: string): Promise<boolean> {
  try {
    await api.deleteChatSession(id);
    // A window still showing the deleted file would re-create it on the next
    // turn, so it goes back to being an unsaved transcript instead.
    if (useChat.getState().session?.id === id) {
      useChat.getState().startNew();
    }
    await queryClient.invalidateQueries({ queryKey: queryKeys.sessions });
    toast({ title: "Conversation deleted" });
    return true;
  } catch (error) {
    reportFailure("Could not delete that conversation", error);
    return false;
  }
}

/**
 * Ask the OS for a path, then export in whatever format that path implies.
 * A cancelled panel is not a failure, so it reports false without a toast.
 */
export async function exportConversation(id: string, title: string): Promise<boolean> {
  try {
    const chosen = await save({
      title: "Export conversation",
      defaultPath: `${safeFileName(title)}.md`,
      filters: [
        { name: "Markdown", extensions: ["md"] },
        { name: "JSON", extensions: ["json"] },
      ],
    });
    if (chosen === null) {
      return false;
    }
    const format: ExportFormat = /\.json$/i.test(chosen) ? "json" : "markdown";
    const written = await api.exportChatSession(id, chosen, format);
    toast({ title: "Conversation exported", description: written, variant: "success" });
    return true;
  } catch (error) {
    reportFailure("Could not write the export", error);
    return false;
  }
}

/** Only the first Chat page mount after launch reopens a conversation. */
let restoreAttempted = false;

export async function restoreLatestConversation(): Promise<void> {
  if (restoreAttempted) {
    return;
  }
  restoreAttempted = true;
  const state = useChat.getState();
  if (state.turns.length > 0 || state.session !== null || state.streaming) {
    return;
  }
  try {
    const index = await api.listChatSessions();
    const latest = index.sessions.find((entry) => entry.turnCount > 0);
    if (!latest) {
      return;
    }
    const session = await api.getChatSession(latest.id);
    // A message sent while this was in flight owns the window now.
    if (useChat.getState().turns.length > 0) {
      return;
    }
    useChat.getState().openSession(session);
  } catch (error) {
    reportFailure("Could not reopen the last conversation", error);
  }
}

/** A title is whatever the user typed; a file name is not allowed to be a path. */
function safeFileName(title: string): string {
  const cleaned = title.replace(/[\\/:*?"<>|]/g, "-").trim().slice(0, 60);
  return cleaned.length > 0 ? cleaned : "conversation";
}
