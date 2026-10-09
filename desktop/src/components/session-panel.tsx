/**
 * The conversations that are on disk.
 *
 * Every action here works on a file, not on the window: opening one replaces
 * the transcript, and deleting one only empties the window when it happens to
 * be the conversation already showing.
 */

import { useState } from "react";
import { Download, MessagesSquare, Pencil, Plus, Search, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { AppDialog, ConfirmDialog } from "@/components/ui/dialog";
import { EmptyState, Note, SkeletonList } from "@/components/ui/feedback";
import { Input } from "@/components/ui/field";
import { describeError, formatDateTime } from "@/lib/format";
import { useSessionSearch, useSessions } from "@/lib/queries";
import {
  deleteConversation,
  exportConversation,
  openConversation,
  renameConversation,
} from "@/lib/sessions";
import { useChat } from "@/stores/chat";
import { cn } from "@/lib/utils";

import type { SessionHit, SessionSummary } from "@/lib/types";

type Row = SessionSummary | SessionHit;

export function SessionPanel() {
  const [query, setQuery] = useState("");
  const [pendingDelete, setPendingDelete] = useState<SessionSummary | null>(null);
  const [pendingRename, setPendingRename] = useState<SessionSummary | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [busy, setBusy] = useState(false);

  const streaming = useChat((state) => state.streaming);
  const activeId = useChat((state) => state.session?.id ?? null);
  const startNew = useChat((state) => state.startNew);

  const sessions = useSessions();
  const search = useSessionSearch(query);
  const searching = query.trim().length > 0;
  const rows: Row[] = searching ? search.data ?? [] : sessions.data?.sessions ?? [];
  const unreadable = sessions.data?.unreadable ?? [];

  const run = async (action: () => Promise<boolean>) => {
    setBusy(true);
    const ok = await action();
    setBusy(false);
    return ok;
  };

  return (
    <Card>
      <CardHeader
        title="Conversations"
        description={
          sessions.isPending
            ? "Reading saved transcripts…"
            : searching
              ? `${rows.length} match(es) in titles and answers`
              : `${rows.length} saved · ${streaming ? "locked while generating" : "click one to reopen it"}`
        }
        actions={
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="New conversation"
            title="Start a new conversation"
            disabled={streaming}
            onClick={startNew}
          >
            <Plus />
          </Button>
        }
      />
      <CardContent className="space-y-3">
        <div className="relative">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            className="pl-8"
            value={query}
            placeholder="Search titles and answers"
            aria-label="Search conversations"
            onChange={(event) => setQuery(event.target.value)}
          />
        </div>

        {sessions.isPending || (searching && search.isPending) ? (
          <SkeletonList rows={3} />
        ) : sessions.isError ? (
          <Note tone="danger">
            <p>{describeError(sessions.error)}</p>
          </Note>
        ) : searching && search.isError ? (
          <Note tone="danger">
            <p>{describeError(search.error)}</p>
          </Note>
        ) : rows.length === 0 ? (
          <EmptyState
            icon={MessagesSquare}
            title={searching ? "No match" : "Nothing saved yet"}
            description={
              searching
                ? "Search covers titles and the text of every turn."
                : "A conversation is written to disk as soon as its first answer finishes, and stays there."
            }
          />
        ) : (
          <ul className="max-h-72 space-y-1.5 overflow-y-auto pr-0.5">
            {rows.map((row) => (
              <li key={row.id} className="group">
                <div
                  className={cn(
                    "rounded-lg border px-3 py-2 transition-colors",
                    row.id === activeId
                      ? "border-primary/40 bg-primary/10"
                      : "border-transparent bg-secondary/40 hover:bg-secondary/70",
                  )}
                >
                  <button
                    type="button"
                    className="block w-full min-w-0 text-left disabled:cursor-not-allowed disabled:opacity-50"
                    disabled={streaming}
                    onClick={() => void run(() => openConversation(row.id))}
                  >
                    <span className="block truncate text-xs font-medium">{row.title}</span>
                    <span className="mt-0.5 block truncate text-[11px] text-muted-foreground">
                      {row.turnCount} turns · {formatDateTime(row.updatedAtMs)}
                    </span>
                    <span className="mt-0.5 block truncate text-[11px] italic text-muted-foreground/80">
                      {"snippet" in row ? row.snippet : row.preview}
                    </span>
                  </button>
                  <div className="mt-1.5 flex items-center gap-1">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      className="size-7"
                      aria-label={`Rename ${row.title}`}
                      title="Rename"
                      disabled={streaming || busy}
                      onClick={() => {
                        setPendingRename(row);
                        setRenameDraft(row.title);
                      }}
                    >
                      <Pencil />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      className="size-7"
                      aria-label={`Export ${row.title}`}
                      title="Export as Markdown or JSON"
                      disabled={busy}
                      onClick={() => void run(() => exportConversation(row.id, row.title))}
                    >
                      <Download />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      className="ml-auto size-7 text-muted-foreground hover:text-destructive"
                      aria-label={`Delete ${row.title}`}
                      title="Delete"
                      disabled={streaming || busy}
                      onClick={() => setPendingDelete(row)}
                    >
                      <Trash2 />
                    </Button>
                  </div>
                </div>
              </li>
            ))}
          </ul>
        )}

        {unreadable.length > 0 ? (
          <Note tone="warning" icon={MessagesSquare}>
            <p className="font-medium">{unreadable.length} transcript file(s) could not be read</p>
            <ul className="mt-1 space-y-0.5 font-mono text-[11px]">
              {unreadable.map((file) => (
                <li key={file} className="truncate">
                  {file}
                </li>
              ))}
            </ul>
          </Note>
        ) : null}
      </CardContent>

      <AppDialog
        open={pendingRename !== null}
        onOpenChange={(open) => {
          if (!open) {
            setPendingRename(null);
          }
        }}
        title="Rename conversation"
        footer={
          <>
            <Button variant="outline" onClick={() => setPendingRename(null)}>
              Cancel
            </Button>
            <Button
              disabled={busy || renameDraft.trim().length === 0}
              onClick={async () => {
                const target = pendingRename;
                if (!target) {
                  return;
                }
                if (await run(() => renameConversation(target.id, renameDraft))) {
                  setPendingRename(null);
                }
              }}
            >
              Save name
            </Button>
          </>
        }
      >
        <Input
          value={renameDraft}
          autoFocus
          aria-label="Conversation title"
          onChange={(event) => setRenameDraft(event.target.value)}
        />
      </AppDialog>

      <ConfirmDialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) {
            setPendingDelete(null);
          }
        }}
        title="Delete this conversation?"
        description={
          pendingDelete
            ? `${pendingDelete.title} and its ${pendingDelete.turnCount} turns are removed from disk. Export it first if you want a copy.`
            : ""
        }
        confirmLabel="Delete conversation"
        destructive
        pending={busy}
        onConfirm={async () => {
          const target = pendingDelete;
          if (!target) {
            return;
          }
          if (await run(() => deleteConversation(target.id))) {
            setPendingDelete(null);
          }
        }}
      />
    </Card>
  );
}
