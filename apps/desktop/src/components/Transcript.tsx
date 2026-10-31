import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { translate, type Locale } from "../i18n";
import type { HostPlanItem } from "../lib/ipc";
import { useNav } from "../state/nav";
import { effortReachesRoute, nextEffort, useSettings } from "../state/settings";
import { isCommand, useTerminal } from "../state/terminal";
import {
  ctaLabel,
  isTurn,
  liveRows,
  stepLabel,
  stepText,
  taskTitle,
  useTranscript,
  type Row,
  type Step,
  type Turn,
} from "../state/transcript";
import { Answer } from "./Answer";
import { Approvals } from "./Approval";
import { Composer } from "./Composer";
import { ExternalLink } from "./ExternalLink";
import { Icon } from "./icons";
import { NavSteps } from "./Sidebar";

function StatusMark({ step }: { step: Step }) {
  if (step.status === "running") return <span className="dot-running" aria-hidden />;
  // A notice is worth reading and not worth alarming over: same glyph, in the quiet colour the
  // answer around it is written in.
  if (step.notice) return <Icon name="warning" size={13} className="text-muted" />;
  if (step.status === "error" || step.status === "stopped")
    return <Icon name="warning" size={13} className="text-danger" />;
  // The row that restates the plan is marked with the plan's own glyph, so the list under it and the
  // header control that keeps it reachable are visibly the same thing.
  if (step.kind === "todo") return <Icon name="checklist" size={13} className="text-accent-ink" />;
  // The green of an outcome, not the accent of the theme. The reference's finished-step tick is the
  // same green in every window, because it reports what happened rather than what the window is
  // wearing — and the accent follows the interface style, so a check that took it would go near-black
  // in Ink and pink in Rose. `--success` is the one ink the theme set was tuned around: every style's
  // panel ground is chosen so this mark clears AA on it.
  return <Icon name="check" size={13} className="text-success" />;
}

/**
 * A refusal: what is wrong in one sentence, the one action that clears it, and the evidence folded
 * underneath. The row used to be a line of `· {title} · {detail}` with a bare URL at the end, which
 * reads like a panic log — so the headline carries the row, the reason sits under it at full width,
 * and paths and key names only appear when the user asks for the technical view.
 */
function RefusalRow({ step, locale }: { step: Step; locale: Locale }) {
  return (
    <li className="refusal-card row-enter">
      <div className="flex items-start gap-2">
        <span className="mt-0.5 shrink-0">
          <StatusMark step={step} />
        </span>
        <div className="min-w-0 flex-1">
          <p className="text-quiet font-medium text-fg">{stepLabel(locale, step)}</p>
          <p className="mt-1 whitespace-pre-wrap break-words text-fine text-muted">
            {stepText(locale, step)}
          </p>
          {step.cta && (
            <div className="mt-2">
              <ExternalLink
                url={step.cta.url}
                label={ctaLabel(locale, step.cta.code)}
                locale={locale}
              />
            </div>
          )}
          {step.technical && (
            <details className="mt-2">
              <summary className="cursor-pointer text-micro text-faint">
                {translate(locale, "transcript.refusal.technical")}
              </summary>
              <p className="disclosure-panel mt-1 break-words font-mono text-micro text-muted">
                {step.technical}
              </p>
            </details>
          )}
        </div>
      </div>
    </li>
  );
}

/** The path chip a tool row carries. It reads the same whether it is a label or a press, because a
 * control that looked different from its own caption would be a second thing to learn. */
const CHIP = "rounded-[var(--radius-sm)] bg-sunken px-1.5 py-0.5 font-mono text-micro text-muted";

function StepRow({
  step,
  locale,
  livePlanId,
}: {
  step: Step;
  locale: Locale;
  livePlanId?: string | null;
}) {
  // The host says which rows start folded over their children, and a finished round is the one that
  // needs it: its calls stand on screen while they run and fold under "Ran 8 tools" the moment they are
  // all in, which is what keeps a long run one skimmable line per round rather than a wall of reads.
  const [open, setOpen] = useState(!step.collapsed);
  const hasChildren = !!step.children?.length;
  const openReviewAt = useNav((s) => s.openReviewAt);
  // Only a write this window holds a diff for can be jumped to — the file it made or the file it
  // changed, both of which the review draws the same way. A row whose hunk the result budget never
  // carried would open a panel with nothing scrolled into view, which is a control that lies. A row
  // that changed several files carries no one hunk but does carry the list, and each of those paths
  // is pressable in the breakdown below.
  const jumpFile =
    (step.kind === "editFile" || step.kind === "createFile") && (step.hunk || step.files?.length)
      ? step.subject
      : undefined;
  // A command's row and the panel's tab are one call, so the row is a way into what it printed. A
  // command this window no longer holds — the panel was cleared, or the conversation was reopened —
  // gets no control, for the same reason an edit row with no hunk does not: it would open nothing.
  const held = useTerminal((s) => isCommand(step.kind) && s.runs.some((run) => run.id === step.id));
  const focusRun = useTerminal((s) => s.focus);
  // A notice's line is its catalog sentence with the run's own numbers in it; every other row's line
  // is the detail the host gave it.
  const text = stepText(locale, step);

  if (step.kind === "contextCompressed") {
    // The seam is its own disclosure: the rule marks where the server ran out of window, and the
    // sentence the host sent with it — which names the two controls that could have kept the room — is
    // there for whoever wants it, rather than standing in the column as a paragraph about a fault.
    return (
      <li className="row-enter my-[var(--space-4)]">
        <details>
          <summary className="flex cursor-pointer items-center gap-3 text-faint">
            <span className="h-px flex-1 bg-line" />
            <Icon name="context" size={14} />
            <span className="text-quiet">{stepLabel(locale, step)}</span>
            <Icon name="chevronDown" size={12} className="disclosure-icon" />
            <span className="h-px flex-1 bg-line" />
          </summary>
          <p className="disclosure-panel mt-2 whitespace-pre-wrap break-words px-[var(--space-6)] text-fine text-muted">
            {stepText(locale, step)}
          </p>
        </details>
      </li>
    );
  }

  // A notice is a one-liner beside the answer it did not stop; a refusal is its own block.
  if (step.kind === "blocked" && !step.notice) {
    return <RefusalRow step={step} locale={locale} />;
  }

  return (
    <li className="row-enter">
      <div className="flex items-center gap-2 py-1.5">
        {hasChildren ? (
          <button
            type="button"
            onClick={() => setOpen((v) => !v)}
            aria-expanded={open}
            className="icon-btn step-toggle"
            aria-label={stepLabel(locale, step)}
          >
            <Icon
              name="chevronRight"
              size={13}
              className={`disclosure-icon ${open ? "rotate-90" : ""}`}
            />
          </button>
        ) : (
          <StatusMark step={step} />
        )}
        <span className="text-quiet font-medium text-fg">{stepLabel(locale, step)}</span>
        {step.subject &&
          (jumpFile ? (
            // The chip is the file, and the file is what the review column is keyed by: pressing it
            // answers the question the row already raised — what did this one actually change.
            <button
              type="button"
              onClick={() => openReviewAt(jumpFile)}
              title={translate(locale, "transcript.edit.jump")}
              className={`${CHIP} cursor-pointer hover:bg-sunken hover:text-fg`}
            >
              {step.subject}
            </button>
          ) : (
            <code className={CHIP}>{step.subject}</code>
          ))}
        {step.diff && (
          <span className="font-mono text-micro">
            <span className="text-success">+{step.diff.added}</span>{" "}
            <span className="text-danger">−{step.diff.removed}</span>
          </span>
        )}
        {text && (
          <span
            className={
              step.notice
                ? // A notice says what the run worked around, and that sentence is the row's whole
                  // payload — it wraps rather than trailing off under a clip.
                  "min-w-0 flex-1 whitespace-pre-wrap break-words text-fine text-muted"
                : "min-w-0 flex-1 truncate text-fine text-faint"
            }
          >
            · {text}
          </span>
        )}
        {held && (
          <button
            type="button"
            onClick={() => focusRun(step.id)}
            title={translate(locale, "terminal.show")}
            aria-label={translate(locale, "terminal.show")}
            className="icon-btn shrink-0"
          >
            <Icon name="terminal" size={13} />
          </button>
        )}
      </div>
      {step.kind === "todo" && step.plan && step.plan.length > 0 && step.id === livePlanId && (
        // Only the plan the run is on now draws in full: an earlier restatement is the line above it,
        // and repeating the list under each one would make the column a history of the same five
        // steps. The list lines up under the row's label, so it reads as that row's detail.
        <PlanList items={step.plan} className="mb-1 ml-5 border-l border-line pl-2" />
      )}
      {step.files && step.files.length > 1 && (
        // A patch that named several files would otherwise leave the row's one line saying only how
        // many: the list is the files themselves, each with its own count, and each is a way into the
        // review at that file rather than at whichever the panel happens to open first.
        <ul className="ml-3 border-l border-line py-0.5 pl-3">
          {step.files.map((file) => (
            <li key={file.path} className="flex items-center gap-2 py-0.5">
              <button
                type="button"
                onClick={() => openReviewAt(file.path)}
                title={translate(locale, "transcript.edit.jump")}
                className={`${CHIP} cursor-pointer hover:bg-sunken hover:text-fg`}
              >
                {file.path}
              </button>
              <span className="font-mono text-micro">
                <span className="text-success">+{file.added}</span>{" "}
                <span className="text-danger">−{file.removed}</span>
              </span>
            </li>
          ))}
        </ul>
      )}
      {hasChildren && open && (
        <ul className="disclosure-panel ml-3 border-l border-line pl-3">
          {step.children!.map((child) => (
            <StepRow key={child.id} step={child} locale={locale} livePlanId={livePlanId} />
          ))}
        </ul>
      )}
    </li>
  );
}

/**
 * The reference renders the two sides of a conversation differently: the prompt is a bubble pushed
 * to the right, the answer is plain text in the column. Reasoning is neither — it is the collapsed
 * block above the answer, and it is never sent back to the model.
 */
function TurnRow({
  turn,
  locale,
  anchor,
  flash,
}: {
  turn: Turn;
  locale: Locale;
  anchor?: (el: HTMLLIElement | null) => void;
  flash?: boolean;
}) {
  if (turn.role === "user") {
    return (
      <li
        ref={anchor}
        data-flash={flash ? "true" : undefined}
        className="turn-anchor row-enter flex flex-col items-end gap-1.5 py-1.5"
      >
        {turn.image && (
          // The exact bytes the model was given, from the host's own history: a reopened conversation
          // shows the picture its answer was about instead of a sentence describing one that is gone.
          <img
            src={turn.image}
            alt={translate(locale, "transcript.picture")}
            className="max-h-40 max-w-[80%] rounded-[var(--radius-lg)] border border-line object-cover"
          />
        )}
        {turn.text && (
          <p className="max-w-[80%] whitespace-pre-wrap break-words rounded-[var(--radius-lg)] bg-accent-soft px-[var(--space-3)] py-1.5 text-quiet text-fg-strong">
            {turn.text}
          </p>
        )}
      </li>
    );
  }
  const waiting = turn.streaming && !turn.text && !turn.thought;
  return (
    <li className="row-enter py-1.5">
      {turn.plan && (
        // The mode is a setting on the composer, not a property of the prose: without this the plan
        // and an answer that made the change read the same, which is the one thing plan mode must not
        // let them do. The dial's own sentence is the tooltip, so the two cannot drift.
        <p className="mb-1">
          <span
            className="inline-flex items-center gap-1 rounded-[var(--radius-pill)] border border-line px-2 py-0.5 text-micro text-muted"
            title={translate(locale, "composer.approve.plan_only")}
          >
            <Icon name="outline" size={11} />
            {translate(locale, "transcript.plan")}
          </span>
        </p>
      )}
      {turn.thought && (
        // One folded line, in the shape the run's own "Ran 8 tools" header is drawn in: reasoning is
        // the model talking to itself, and a column that streamed it in full would put that thinking
        // between the user and the answer they asked for. It stays reachable — the press is there for
        // whoever wants to read how the answer was reached — but it never opens itself, not even while
        // it is the only thing the turn has said yet.
        <details className="mb-1.5">
          <summary className="flex cursor-pointer items-center gap-1.5 py-0.5 text-quiet text-fg">
            <Icon name="chevronRight" size={13} className="disclosure-icon" />
            <Icon name="thought" size={13} className="text-faint" />
            {translate(locale, turn.text ? "transcript.step.thought" : "chat.thinking")}
            {/* The row is the only thing on screen until the answer starts, so it carries the beat:
                a window with a folded line and no motion in it reads as one that stopped. */}
            {turn.streaming && !turn.text && <span className="dot-running" aria-hidden />}
          </summary>
          <p className="disclosure-panel mt-1 whitespace-pre-wrap break-words border-l border-line pl-3 text-fine text-muted">
            {turn.thought}
          </p>
        </details>
      )}
      {turn.text ? (
        <Answer text={turn.text} streaming={turn.streaming} locale={locale} />
      ) : waiting ? (
        <p className="flex items-center gap-2 text-fine text-muted">
          <span className="dot-running" aria-hidden />
          {/* Every route answers a request that is already on the wire, so this wait is the round
              trip and never a load. */}
          {translate(locale, "chat.waiting")}
        </p>
      ) : null}
      {turn.stopped && (
        <p className="mt-1 text-micro text-faint">{translate(locale, "transcript.stopped")}</p>
      )}
    </li>
  );
}

function RowView({
  row,
  locale,
  livePlanId,
}: {
  row: Row;
  locale: Locale;
  livePlanId?: string | null;
}) {
  return isTurn(row) ? (
    <TurnRow turn={row} locale={locale} />
  ) : (
    <StepRow step={row} locale={locale} livePlanId={livePlanId} />
  );
}

/** One prompt and the answer it got. `n` is the turn's position, spoken and titled but not drawn:
 *  the marker itself is a dash, and a column of numbers would be the loudest thing on the screen. */
type Checkpoint = { id: string; n: number; text: string; picture: boolean };

/**
 * A checkpoint is a user turn: the question the pair is named after. Steps are not counted — they
 * hang off whichever run was in flight, so numbering them would make the same conversation number
 * differently between two renders of the same rows.
 */
function checkpointsOf(rows: Row[]): Checkpoint[] {
  const out: Checkpoint[] = [];
  for (const row of rows) {
    if (isTurn(row) && row.role === "user")
      out.push({ id: row.id, n: out.length + 1, text: row.text, picture: Boolean(row.image) });
  }
  return out;
}

/** The prompt in one line, for the marker's tooltip and accessible name. A picture asked with no
 * words has none to preview, so it is named by what it is rather than left blank. */
function previewOf(checkpoint: Checkpoint, locale: Locale): string {
  const oneLine = checkpoint.text.replace(/\s+/g, " ").trim();
  if (oneLine) return oneLine.length > 60 ? `${oneLine.slice(0, 60)}…` : oneLine;
  return checkpoint.picture ? translate(locale, "transcript.checkpoint.picture") : "";
}

const NEAR_BOTTOM_PX = 48;

/**
 * How long the open run has been running, in whole seconds, or null when nothing is open to time.
 *
 * The engine spends ten seconds or more booting the server and paging the weights in before the
 * first token arrives, and a spinner that never changes is what a frozen window looks like from
 * where the user is sitting. So the running line counts the wait it is asking to be waited out —
 * which is also what the reference shot does with its in-flight row.
 */
function useRunSeconds(startedAt: number | null): number | null {
  const [, setBeat] = useState(0);
  useEffect(() => {
    if (startedAt === null) return;
    const id = window.setInterval(() => setBeat((n) => n + 1), 1000);
    return () => window.clearInterval(id);
  }, [startedAt]);
  if (startedAt === null) return null;
  return Math.max(0, Math.round((Date.now() - startedAt) / 1000));
}

/**
 * The round in flight, in the reference's shape: a square chevron and "Running · 71s" over the work
 * that round has done, indented under a hairline that ends with the last row. A closed round folds
 * into the "Ran N tools" line above, so this card is only ever one round's worth of rows.
 *
 * The body scrolls on its own once a round has more rows than the column should give it: a run that
 * reads forty files in one round is not a run whose card should push the composer off the window.
 * A route that streams its reasoning gets a row for it here too — the reference draws a thought as
 * one more tick in the card rather than as a block between the calls.
 */
function RunCard({
  locale,
  seconds,
  rows,
}: {
  locale: Locale;
  seconds: number | null;
  rows: Row[];
}) {
  const [open, setOpen] = useState(true);
  const body = useRef<HTMLUListElement>(null);
  // The newest row is the one being watched, and the card is the only part of the column that can
  // scroll under the user's eyes without moving the turn above it.
  useEffect(() => {
    const box = body.current;
    if (box) box.scrollTop = box.scrollHeight;
  }, [rows.length]);
  const label =
    seconds === null
      ? translate(locale, "transcript.run.starting")
      : translate(locale, "transcript.run.running", { seconds });
  return (
    <section className="run-card" aria-label={translate(locale, "transcript.run.title")}>
      <div className="run-card-head">
        <button
          type="button"
          onClick={() => setOpen((was) => !was)}
          aria-expanded={open}
          className="icon-btn step-toggle"
          aria-label={translate(locale, "transcript.run.title")}
        >
          <Icon
            name="chevronRight"
            size={13}
            className={`disclosure-icon ${open ? "rotate-90" : ""}`}
          />
        </button>
        <span className="text-fg">{label}</span>
      </div>
      {open && rows.length > 0 && (
        <ul ref={body} className="run-card-body">
          {rows.map((row) =>
            isTurn(row) ? (
              <LiveThought key={row.id} turn={row} locale={locale} />
            ) : (
              <StepRow key={row.id} step={row} locale={locale} />
            ),
          )}
        </ul>
      )}
    </section>
  );
}

/** A thought inside the live card. The reference gives reasoning the same tick as a call and the
 *  beginning of what it said; the whole text stays in the turn's own disclosure once the run is done. */
function LiveThought({ turn, locale }: { turn: Turn; locale: Locale }) {
  return (
    <li className="row-enter">
      <div className="flex items-center gap-2 py-1.5">
        {turn.streaming ? (
          <span className="dot-running" aria-hidden />
        ) : (
          <Icon name="check" size={13} className="text-success" />
        )}
        <span className="shrink-0 text-quiet font-medium text-fg">
          {translate(locale, "transcript.step.thought")}
        </span>
        <span className="min-w-0 flex-1 truncate text-fine text-faint">· {turn.thought}</span>
      </div>
    </li>
  );
}

/**
 * The reference's waiting row: a grid of lit dots and a soft "Getting ready..." under the card. It
 * stands between two rounds, which is when the run is preparing the next call rather than running one
 * — a card full of finished ticks with nothing under it reads as a run that stopped.
 */
function GettingReady({ locale }: { locale: Locale }) {
  return (
    <p className="getting-ready" role="status">
      <span className="dot-grid" aria-hidden>
        {Array.from({ length: 9 }, (_, i) => (
          <span key={i} style={{ animationDelay: `${i * 0.1}s` }} />
        ))}
      </span>
      <span className="getting-ready-label">{translate(locale, "transcript.gettingReady")}</span>
    </p>
  );
}

/**
 * The newest plan any row in the run stated, children included, with the row that stated it. A tool
 * row lands under its round's batch header, so the list is not always in the top-level order —
 * walking the children is what keeps the last plan the one the run actually last wrote. The row's id
 * is what lets the column draw that one list in full and leave the earlier restatements as a line.
 */
function latestPlan(rows: Row[]): { id: string; items: HostPlanItem[] } | null {
  let found: { id: string; items: HostPlanItem[] } | null = null;
  for (const row of rows) {
    if (isTurn(row)) continue;
    if (row.plan?.length) found = { id: row.id, items: row.plan };
    if (row.children?.length) found = latestPlan(row.children) ?? found;
  }
  return found;
}

/** The steps as the run wrote them: a tick for done, a pulse for the one being worked, a box for the
 * rest. The panel and the transcript row draw the same list, so the row can be read without opening
 * anything and the panel is only the place it stays reachable once the row has scrolled away. */
function PlanList({ items, className }: { items: HostPlanItem[]; className?: string }) {
  return (
    <ol className={`plan-list ${className ?? ""}`}>
      {items.map((item, index) => (
        <li key={`${index}-${item.text}`} className="plan-row" data-status={item.status}>
          <span className="plan-mark" aria-hidden>
            {item.status === "done" ? (
              <Icon name="tick" size={12} className="text-accent-ink" />
            ) : item.status === "active" ? (
              <span className="plan-pulse" />
            ) : (
              <span className="plan-box" />
            )}
          </span>
          <span className="plan-text">{item.text}</span>
        </li>
      ))}
    </ol>
  );
}

/**
 * The four controls the reference keeps at the right of its top bar, in its order: the effort dial,
 * the run's plan, the strip under the composer, and the column beside it.
 *
 * Each one answers to something this window has. The bolt walks the same effort order Settings lists
 * and writes the same value its select does, so the lit tile means "not the model's own choice". Its
 * label also says when the route ahead sends no effort field, so the press is not presented as one
 * an answer will keep. The two panel toggles carry their strip's state to a screen reader as
 * `aria-expanded` and stay quiet on screen: the reference's four glyphs sit unlit on a window that
 * has both strips open, and a fold that hides a column has already said itself in the layout.
 */
function HeaderControls({ locale, plan }: { locale: Locale; plan: HostPlanItem[] | null }) {
  const doc = useSettings((s) => s.doc);
  const effort = doc?.sampling.effort;
  const rightOpen = useNav((s) => s.rightOpen);
  const statusOpen = useNav((s) => s.statusOpen);
  const toggleRight = useNav((s) => s.toggleRight);
  const toggleStatus = useNav((s) => s.toggleStatus);

  // The dial is one of the window's first controls, and nothing else on this surface has asked for
  // the document yet — without this the bolt would sit at "Auto" and do nothing until the user opened
  // Settings or pressed the composer's approval pill.
  useEffect(() => {
    if (!useSettings.getState().loaded) void useSettings.getState().load();
  }, []);

  const level = translate(locale, `settings.opt.${effort ?? "auto"}`);
  // Before the document lands, the ordinary label is the honest one: nothing is known to be lost.
  const effortLabel = translate(
    locale,
    doc === null || effortReachesRoute(doc) ? "topbar.effort" : "topbar.effortNotSent",
    { level },
  );

  return (
    <div className="flex items-center gap-1.5">
      <button
        type="button"
        className="icon-btn"
        aria-label={effortLabel}
        aria-pressed={effort ? effort !== "auto" : undefined}
        title={effortLabel}
        onClick={() => {
          const store = useSettings.getState();
          const doc = store.doc;
          if (!doc) return;
          const next = nextEffort(doc.sampling.effort);
          void store.update((d) => void (d.sampling.effort = next));
        }}
      >
        <Icon name="bolt" size={14} />
      </button>
      <PlanButton locale={locale} plan={plan} />
      <button
        type="button"
        className="icon-btn"
        aria-label={translate(locale, "topbar.status")}
        aria-expanded={statusOpen}
        title={translate(locale, "topbar.status")}
        onClick={toggleStatus}
      >
        <Icon name="panelBottom" size={14} />
      </button>
      <button
        type="button"
        className="icon-btn"
        aria-label={translate(locale, "topbar.rightPanel")}
        aria-expanded={rightOpen}
        title={translate(locale, "topbar.rightPanel")}
        onClick={toggleRight}
      >
        <Icon name="panelRight" size={14} />
      </button>
    </div>
  );
}

/**
 * The run's plan, behind the header's checklist control. The control is the reference's square
 * glyph and nothing else: the count used to ride beside the icon, which does not fit a 28px square —
 * it wrapped onto a second line and pushed the button into its neighbour. The count heads the panel
 * instead, where it has a row to itself, and the transcript's plan row already states it in words.
 */
function PlanButton({ locale, plan }: { locale: Locale; plan: HostPlanItem[] | null }) {
  const [open, setOpen] = useState(
    // `?plan=1` seeds the panel open the way the deep links seed the sidebar's fold: the native window
    // takes no injected clicks, so a panel only a press can reach is one no capture can show. Seeded
    // here rather than by an effect on the run's rows, because the rows land after this render and the
    // panel's own body already waits on `plan` — an effect that opened it once arrived was the part
    // that did not hold on the real window.
    () => new URLSearchParams(window.location.search).get("plan") === "1",
  );
  const done = plan?.filter((item) => item.status === "done").length ?? 0;
  const total = plan?.length ?? 0;
  return (
    <span className="relative">
      <button
        type="button"
        className={`icon-btn ${open && plan ? "plan-btn-open" : ""} ${plan ? "" : "opacity-55"}`}
        aria-label={translate(locale, "transcript.plan.title")}
        aria-pressed={plan ? open : undefined}
        aria-disabled={plan ? undefined : true}
        title={
          plan
            ? translate(locale, "transcript.plan.count", { done, total })
            : translate(locale, "transcript.plan.empty")
        }
        onClick={() => {
          if (plan) setOpen((was) => !was);
        }}
      >
        <Icon name="checklist" size={14} />
      </button>
      {open && plan && (
        <>
          <span className="picker-scrim" onClick={() => setOpen(false)} />
          <div
            role="dialog"
            aria-modal="true"
            aria-label={translate(locale, "transcript.plan.title")}
            className="plan-panel"
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                setOpen(false);
              }
            }}
          >
            <div className="flex items-baseline justify-between px-[var(--space-3)] pt-[var(--space-2)]">
              <p className="section-label">{translate(locale, "transcript.plan.title")}</p>
              <span className="plan-badge font-mono text-micro">
                {done}/{total}
              </span>
            </div>
            <PlanList items={plan} />
            <p className="px-[var(--space-3)] pb-[var(--space-2)] text-fine text-faint">
              {translate(locale, "transcript.plan.hint")}
            </p>
          </div>
        </>
      )}
    </span>
  );
}

export function Transcript({
  locale,
  onPickModel,
  onOpenPalette,
}: {
  locale: Locale;
  onPickModel: () => void;
  onOpenPalette: () => void;
}) {
  const rows = useTranscript((s) => s.rows);
  const running = useTranscript((s) => s.running);
  const runStartedAt = useTranscript((s) => s.runStartedAt);
  const waited = useRunSeconds(running ? runStartedAt : null);
  // The header is the column's first row, so it is the one the traffic lights reach once the sidebar
  // folds to its rail.
  const rail = useNav((s) => s.collapsed);
  const scroll = useRef<HTMLDivElement>(null);
  const anchors = useRef(new Map<string, HTMLLIElement>());
  const flashTimer = useRef<number | undefined>(undefined);
  // False between a checkpoint jump and the next prompt, or while the user has scrolled up to read:
  // a column that follows the stream while the user is somewhere else is a column they cannot read.
  const follow = useRef(true);
  const [active, setActive] = useState("");
  const [flash, setFlash] = useState("");

  const checkpoints = useMemo(() => checkpointsOf(rows), [rows]);
  const latest = useMemo(() => latestPlan(rows), [rows]);
  const plan = latest?.items ?? null;
  // The round still in flight is drawn as one card under the column; the rows it holds are taken out
  // of the plain list so a call is never on screen twice. Its calls and the reasoning between them
  // both belong to the card — a thought is one more tick of the round, as the reference draws it.
  const live = useMemo(() => (running ? liveRows(rows) : []), [rows, running]);
  const liveIds = useMemo(() => new Set(live.map((row) => row.id)), [live]);
  // Between rounds the model is preparing the next call: no call is running, and the card's ticks are
  // all that is left of the round. That is the beat the reference fills with its dot grid.
  const preparing =
    running && live.length > 0 && !live.some((row) => !isTurn(row) && row.status === "running");

  // The dash marks the pair at the top of the column, which is the pair the user is looking at.
  const spy = useCallback(() => {
    const box = scroll.current;
    if (!box) return;
    const edge = box.getBoundingClientRect().top + 24;
    let current = "";
    for (const item of checkpoints) {
      const el = anchors.current.get(item.id);
      if (!el) break;
      if (el.getBoundingClientRect().top > edge) break;
      current = item.id;
    }
    setActive(current);
  }, [checkpoints]);

  useEffect(() => {
    const box = scroll.current;
    if (!box) return;
    const last = rows[rows.length - 1];
    // Sending a prompt is the one thing that always means "watch this answer".
    if (last && isTurn(last) && last.role === "user") follow.current = true;
    if (follow.current) box.scrollTop = box.scrollHeight;
    spy();
  }, [rows, spy]);

  useEffect(() => () => window.clearTimeout(flashTimer.current), []);

  const jump = (id: string) => {
    const box = scroll.current;
    const el = anchors.current.get(id);
    if (!box || !el) return;
    follow.current = false;
    box.scrollTop += el.getBoundingClientRect().top - box.getBoundingClientRect().top - 12;
    setActive(id);
    setFlash(id);
    window.clearTimeout(flashTimer.current);
    flashTimer.current = window.setTimeout(() => setFlash(""), 1400);
  };

  // The header names the task rather than saying "Task": the first prompt is the title the sidebar
  // files this conversation under, so the two agree on what the task is called. Only a window that
  // has been asked nothing yet falls back to the plain label.
  const heading = taskTitle(rows) ?? translate(locale, "transcript.untitled");

  return (
    <main className="flex min-h-0 min-w-0 flex-1 flex-col bg-bg">
      <header
        className={`app-header flex h-[var(--header-height)] shrink-0 items-center gap-2 border-b border-line px-[var(--space-4)] ${
          rail ? "clears-traffic-lights" : ""
        }`}
      >
        {/* Folded, the sidebar's header row is gone with the column, so the steps it carried — back,
            forward, search — stand here instead, clear of the traffic lights by the same padding. */}
        {rail && <NavSteps locale={locale} onOpenPalette={onOpenPalette} />}
        <h1
          className="app-title min-w-0 flex-1 truncate text-ui font-medium text-fg"
          title={heading ?? undefined}
        >
          {heading}
        </h1>
        {/* The reference's top-bar cluster, at the conversation's own header: the effort dial, the
            plan, and the two strips this column shares the window with. */}
        <HeaderControls locale={locale} plan={plan} />
      </header>

      <div className="flex min-h-0 flex-1">
        <div
          ref={scroll}
          onScroll={() => {
            const box = scroll.current;
            if (box && box.scrollHeight - box.scrollTop - box.clientHeight < NEAR_BOTTOM_PX) {
              follow.current = true;
            }
            spy();
          }}
          className="min-h-0 min-w-0 flex-1 overflow-y-auto px-[var(--space-6)] py-[var(--space-4)]"
        >
          {rows.length === 0 ? (
            // The one screen in the app with nothing on it, so it carries the name in words only: the
            // art this row used to lead with was a raster tile that takes no accent, and a picture
            // that cannot follow the interface style is a second palette inside a design system that
            // says there is only one.
            <div className="app-empty flex flex-col items-center gap-2 pt-[var(--space-6)] text-accent-ink">
              <p className="text-ui font-medium text-fg-strong">{translate(locale, "app.name")}</p>
              <p className="max-w-[360px] text-center text-quiet text-muted">
                {translate(locale, "transcript.empty")}
              </p>
            </div>
          ) : (
            <ul className="mx-auto w-full max-w-[var(--content-width)]">
              {rows.map((row) => {
                if (liveIds.has(row.id)) {
                  // The card takes the place of the round's first row, so a thought the route
                  // streams under the round still lands below the card rather than above it.
                  return row.id === live[0]?.id ? (
                    <li key="run-card" className="list-none">
                      <RunCard locale={locale} seconds={waited} rows={live} />
                    </li>
                  ) : null;
                }
                return isTurn(row) && row.role === "user" ? (
                  <TurnRow
                    key={row.id}
                    turn={row}
                    locale={locale}
                    flash={row.id === flash}
                    anchor={(el) => {
                      if (el) anchors.current.set(row.id, el);
                      else anchors.current.delete(row.id);
                    }}
                  />
                ) : (
                  <RowView key={row.id} row={row} locale={locale} livePlanId={latest?.id} />
                );
              })}
              {preparing && (
                <li className="list-none">
                  <GettingReady locale={locale} />
                </li>
              )}
            </ul>
          )}
        </div>

        {checkpoints.length > 1 && (
          <nav
            aria-label={translate(locale, "transcript.checkpoints")}
            className="min-h-0 w-10 shrink-0 overflow-y-auto py-[var(--space-4)]"
          >
            <ol className="flex flex-col">
              {checkpoints.map((c) => (
                <li key={c.id}>
                  <button
                    type="button"
                    onClick={() => jump(c.id)}
                    data-active={c.id === active ? "true" : undefined}
                    title={previewOf(c, locale)}
                    aria-label={translate(locale, "transcript.checkpoint", {
                      n: c.n,
                      text: previewOf(c, locale),
                    })}
                    className="checkpoint"
                  />
                </li>
              ))}
            </ol>
          </nav>
        )}
      </div>

      <Approvals locale={locale} />
      <Composer locale={locale} onPickModel={onPickModel} />
    </main>
  );
}
