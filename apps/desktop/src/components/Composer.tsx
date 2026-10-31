import { useEffect, useRef, useState } from "react";

import { translate, type Locale } from "../i18n";
import { toPictureDataUrl } from "../lib/image";
import type { ApprovalMode } from "../lib/ipc";
import { shortcutLabel } from "../lib/shortcuts";
import { useChat } from "../state/chat";
import { useNav } from "../state/nav";
import { useSettings } from "../state/settings";
import { useTranscript, type RunDiff, type RunRounds } from "../state/transcript";
import { Icon } from "./icons";
import { ModelPicker } from "./ModelPicker";

/**
 * The three modes a press of the dial cycles through, in order. `custom` is deliberately absent: it is
 * a per-tool table only Settings can fill, so a press leaves that mode rather than landing in it.
 */
const DIAL: ApprovalMode[] = ["auto_approve", "ask_always", "plan_only"];

/**
 * What the run's pill says: the spinner while calls are in flight, the budget it is spending, and the
 * ledger it is leaving. The dot is the reference's separator between the two readings; the spinner is
 * the same arc the header's step glyph draws, turning only while there is something to wait for.
 */
function PillBody({
  locale,
  running,
  runRounds,
  runDiff,
}: {
  locale: Locale;
  running: boolean;
  runRounds: RunRounds | null;
  runDiff: RunDiff | null;
}) {
  return (
    <>
      <Icon
        name={running ? "step" : "review"}
        size={13}
        className={`shrink-0 ${running ? "animate-spin" : ""}`}
        aria-hidden
      />
      {runRounds && (
        <span className="tabular-nums">
          {translate(locale, "composer.step", {
            done: runRounds.done,
            max: runRounds.max,
          })}
        </span>
      )}
      {runRounds && runDiff && (
        <span className="run-pill-sep" aria-hidden>
          ·
        </span>
      )}
      {runDiff && (
        <>
          <span className="whitespace-nowrap">
            {translate(
              locale,
              runDiff.files === 1 ? "transcript.diff.fileChanged" : "transcript.diff.filesChanged",
              { count: runDiff.files },
            )}
          </span>
          <span className="font-mono tabular-nums">
            <span className="text-success">+{runDiff.added}</span>{" "}
            <span className="text-danger">−{runDiff.removed}</span>
          </span>
        </>
      )}
    </>
  );
}

/**
 * The composer's approval selector is the agent loop's permission dial, and it edits the one value the
 * loop reads: `approvals.mode` in the settings the host loads when a prompt is sent, which is the same
 * document the Settings page writes. A call the mode settles on its own comes back as a row naming the
 * policy it settled under; a call it cannot settle stops the run on a card over this box, and waits for
 * the press. Hover spells both out per mode, because the difference between the two dials is precisely
 * how often the run stops in front of you.
 */
export function Composer({ locale, onPickModel }: { locale: Locale; onPickModel: () => void }) {
  const running = useTranscript((s) => s.running);
  const runDiff = useTranscript((s) => s.runDiff);
  const runRounds = useTranscript((s) => s.runRounds);
  const send = useChat((s) => s.send);
  const stop = useChat((s) => s.stop);
  const error = useChat((s) => s.error);
  // The diff pill and the right column's review are one control at either end of the same press, so
  // the pill reads the column's state rather than holding its own copy of it.
  const review = useNav((s) => s.panel) === "review";
  const toggleReview = useNav((s) => s.toggleReview);
  const [draft, setDraft] = useState("");
  const mode = useSettings((s) => s.doc?.approvals.mode);
  // The shell's switch changes what the agent's title may promise: with it on, a run can also issue
  // one command, and that is the one call the tooltip's "nothing else reaches past this machine" would
  // otherwise be wrong about.
  const shell = useSettings((s) => s.doc?.approvals.shellEnabled ?? false);
  // A delegation is a second model run, so the tooltip has to name it whenever the switch that pays
  // for it is on — the base sentences price a run in tool results and would leave it out.
  const subagents = useSettings((s) => s.doc?.approvals.subagentsEnabled ?? false);
  // A free model is answered by the OpenCode service, which now works in the project folder and has its
  // writes gated and rowed like a local run's — so the badge may promise a run that changes files. What
  // that route cannot give is a before-image, and the title says so under the picker's name.
  const free = useSettings((s) => s.doc?.freeModel ?? null);
  // The agent's badge: what every prompt of this app is allowed to do, and what it costs. It is not a
  // control — the mode is the app, not a per-question opt-in — so the glyph stays lit and the title
  // carries the price. What a run may *change* is still the approval dial's answer beside it, and the
  // shell and the subagents stay Settings switches, named here only because they widen what this badge
  // is promising.
  const agentTitle = [
    translate(locale, shell ? "composer.agent.onShell" : "composer.agent.on"),
    ...(subagents ? [translate(locale, "composer.agent.subagents")] : []),
    ...(free ? [translate(locale, "composer.agent.proseRoute")] : []),
  ].join(" ");
  const [picture, setPicture] = useState<string | null>(null);
  const [preparing, setPreparing] = useState(false);
  const [attachError, setAttachError] = useState<string | null>(null);
  const picker = useRef<HTMLInputElement>(null);
  const notInBuild = translate(locale, "common.notInBuild");

  // The picture is shrunk and re-encoded here, before it ever crosses to the host, because a phone
  // photo's bytes are far past what a vision projector samples — and the host charges the attached
  // picture for the engine's whole image-token budget either way.
  const pickPicture = async (picked: File | undefined) => {
    if (!picked) return;
    setPreparing(true);
    setAttachError(null);
    try {
      setPicture(await toPictureDataUrl(picked));
    } catch {
      // A file the browser cannot decode as an image is the one way this fails without the engine
      // being involved, so it is said here rather than left as a picture button that did nothing.
      setPicture(null);
      setAttachError(translate(locale, "composer.picture.failed"));
    } finally {
      setPreparing(false);
      // Cleared so choosing the same file twice fires `change` again, which the browser otherwise
      // suppresses and the user reads as the button having stopped working.
      if (picker.current) picker.current.value = "";
    }
  };

  // The dial names a value the host owns, so it is read at launch rather than after the first press:
  // a pill that held nothing until the Settings page was opened would be a control with no state.
  useEffect(() => {
    const store = useSettings.getState();
    if (!store.loaded) void store.load();
  }, []);

  // A press writes the same document the Settings page edits, and the host reads it when the next
  // prompt is sent — so the run already on screen is unaffected, and the one after it is not.
  const cycleApproval = async () => {
    if (!useSettings.getState().loaded) await useSettings.getState().load();
    const current = useSettings.getState().doc?.approvals.mode ?? "auto_approve";
    // `custom` is not in the cycle, so its index is -1 and the next press lands on the first mode.
    const next = DIAL[(DIAL.indexOf(current) + 1) % DIAL.length] ?? "auto_approve";
    await useSettings.getState().update((doc) => void (doc.approvals.mode = next));
  };

  const submit = () => {
    if (!draft.trim() && !picture) return;
    // A picture with no words is a question — "what is in this?" needs no saying.
    void send(draft, picture ?? undefined);
    setDraft("");
    setPicture(null);
  };

  return (
    <div className="app-composer shrink-0 px-[var(--space-6)] pb-[var(--space-3)]">
      {(runRounds || runDiff) && (
        // One capsule above the box, as the reference draws it: the spinner, the round counter, a
        // dot, then what the run has changed. The two readings were separate pills, which read as two
        // unrelated facts; they are one line about the same run. The pill doubles as the way into
        // Review, because a count that cannot show what it counts is a number to go and find yourself.
        <div className="mx-auto mb-[var(--space-2)] flex w-full max-w-[var(--content-width)] justify-center">
          {runDiff ? (
            <button
              type="button"
              onClick={toggleReview}
              aria-expanded={review}
              title={translate(locale, review ? "composer.review.close" : "composer.review.open")}
              className="run-pill"
            >
              <PillBody locale={locale} running={running} runRounds={runRounds} runDiff={runDiff} />
            </button>
          ) : (
            <span className="run-pill">
              <PillBody locale={locale} running={running} runRounds={runRounds} runDiff={null} />
            </span>
          )}
        </div>
      )}
      <div className="app-composer-box mx-auto w-full max-w-[var(--content-width)] rounded-[var(--radius-lg)] border border-line bg-elevated shadow-raised transition-colors focus-within:border-accent-line">
        <label className="sr-only" htmlFor="composer-input">
          {translate(locale, "composer.placeholder")}
        </label>
        <textarea
          id="composer-input"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            // Enter sends, Shift+Enter adds the newline a multi-line task needs.
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
          rows={2}
          placeholder={translate(locale, "composer.placeholder")}
          className="app-composer-field w-full resize-none bg-transparent px-[var(--space-4)] pt-[var(--space-3)] text-fg outline-none placeholder:text-faint"
        />
        {error && (
          <p role="alert" className="px-[var(--space-4)] pb-1 text-fine text-danger">
            {error}
          </p>
        )}
        {attachError && (
          <p role="alert" className="px-[var(--space-4)] pb-1 text-fine text-danger">
            {attachError}
          </p>
        )}
        {picture && (
          <div className="flex items-center gap-2 px-[var(--space-4)] pb-2">
            {/* The shrunk bytes, not the file the user picked: what is on screen is exactly what the
                model will be given, so a picture that resized badly is visible before it is sent. */}
            <img
              src={picture}
              alt=""
              className="size-12 rounded-[var(--radius-md)] border border-line object-cover"
            />
            <span className="text-fine text-muted">{translate(locale, "composer.picture")}</span>
            <button
              type="button"
              onClick={() => setPicture(null)}
              className="icon-btn"
              aria-label={translate(locale, "composer.picture.remove")}
            >
              <Icon name="close" size={12} />
            </button>
          </div>
        )}
        <div className="app-composer-actions flex items-center gap-2 px-[var(--space-3)] pb-[var(--space-3)]">
          {/* A plain file input, because the webview answers one with the native open panel — no
              dialog plugin and no filesystem capability handed to the interface. Only a picture is
              asked for: the engine reads images, and nothing else has a path to an answer. */}
          <input
            ref={picker}
            type="file"
            accept="image/*"
            className="sr-only"
            id="composer-picture"
            onChange={(e) => void pickPicture(e.target.files?.[0])}
          />
          {/* Still nothing to add a prompt but a picture: context attachments wait on the retrieval
              path, voice on the input it would transcribe into. Named on hover, like the sidebar's
              unbuilt rows. The reference draws this one as an outlined square rather than a bare
              glyph — the border is what separates the row's first control from the box it sits in,
              and a disabled control still keeps its shape. */}
          <button
            type="button"
            className="icon-btn border border-line"
            aria-label={translate(locale, "composer.add")}
            aria-disabled
            title={notInBuild}
          >
            <Icon name="plus" size={14} />
          </button>
          <button
            type="button"
            onClick={() => picker.current?.click()}
            className="icon-btn"
            aria-label={translate(locale, "composer.attach")}
            aria-busy={preparing || undefined}
            title={
              preparing
                ? translate(locale, "composer.picture.busy")
                : translate(locale, "composer.attach")
            }
          >
            <Icon name="attach" size={14} />
          </button>
          {/* The agent's own glyph, lit and permanent: the reference draws the mode as one line icon in
              this row, and here it is the app's name for itself rather than a state to enter. */}
          <span
            className="icon-btn bg-accent-soft text-fg-strong"
            role="img"
            aria-label={translate(locale, "composer.agent")}
            title={agentTitle}
          >
            <Icon name="coding" size={14} />
          </span>
          {/* The reference marks this dial with the warning triangle and colours the whole pill when
              the policy is the wide one: auto approve is the mode that lets the run change files and
              run a command without being asked, which is the one state the row should not whisper.
              The narrower modes keep the same glyph in the muted ink, because the shape names the
              kind of control and the colour names the risk. */}
          <button
            type="button"
            onClick={() => void cycleApproval()}
            // Until the host's value arrives the pill names the control rather than guessing at what it
            // holds, the way the model pill carries its own title before the engine's answer does.
            title={translate(
              locale,
              mode ? `composer.approve.${mode}` : "settings.row.approvalMode",
            )}
            className={`flex items-center gap-1.5 rounded-[var(--radius-pill)] px-2 py-1 text-fine ${
              mode === "auto_approve" ? "text-danger" : "text-muted hover:bg-hover hover:text-fg"
            }`}
          >
            <Icon name="warning" size={14} />
            {translate(locale, mode ? `settings.opt.${mode}` : "settings.section.approvals")}
            <Icon name="chevronDown" size={12} />
          </button>

          <span className="flex-1" />

          <ModelPicker locale={locale} onOpenSettings={onPickModel} />
          <button
            type="button"
            className="icon-btn"
            aria-label={translate(locale, "composer.mic")}
            aria-disabled
            title={notInBuild}
          >
            <Icon name="mic" size={14} />
          </button>
          <button
            type="button"
            onClick={() => (running ? void stop() : submit())}
            disabled={!running && !draft.trim() && !picture}
            aria-label={translate(locale, running ? "composer.stop" : "composer.send")}
            // The key the interrupt answers to, named only while there is a reply to interrupt.
            title={running ? shortcutLabel("escape") : undefined}
            className="grid size-7 place-items-center rounded-[var(--radius-md)] bg-accent text-accent-contrast disabled:opacity-60"
          >
            {/* Interrupting a reply cuts its stream and keeps the server, so the next prompt is
                answered without reloading the weights. */}
            <Icon name={running ? "stop" : "send"} size={14} />
          </button>
        </div>
      </div>
    </div>
  );
}
