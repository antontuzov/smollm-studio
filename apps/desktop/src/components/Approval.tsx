// The card a run is parked behind: what it is about to do, the line behind that, and the two buttons
// that let it go on.
//
// It sits over the composer rather than in the transcript because a question the run is waiting on is
// not a fact about the past — the answer cannot be read until it is acted on, and a card at the bottom
// of a column the user has scrolled away from is a run that stops for no visible reason. The call
// already has its row above; this adds only what a row cannot hold.
import { useEffect, useState } from "react";

import { translate, type Locale } from "../i18n";
import { secondsLeft, useApproval, type Prompt } from "../state/approval";

/**
 * Every prompt the run is held on, oldest first.
 *
 * A round's calls go out together, so two of them can be waiting at once — a run that wants to write
 * one file and delete another asks twice. Each gets its own card, because each is its own decision and
 * answering one must not settle the other.
 */
export function Approvals({ locale }: { locale: Locale }) {
  const prompts = useApproval((s) => s.prompts);
  // One timer for the whole stack rather than one per card: the countdowns are all read off the same
  // instant, and a card added mid-tick would otherwise restart the beat the others are on.
  const [, setBeat] = useState(0);
  useEffect(() => {
    if (prompts.length === 0) return;
    const id = window.setInterval(() => setBeat((n) => n + 1), 1000);
    return () => window.clearInterval(id);
  }, [prompts.length]);
  if (prompts.length === 0) return null;
  const now = Date.now();
  return (
    <div className="shrink-0 px-[var(--space-6)] pb-[var(--space-2)]">
      <div className="mx-auto flex w-full max-w-[var(--content-width)] flex-col gap-[var(--space-2)]">
        {prompts.map((prompt) => (
          <Card key={prompt.callId} locale={locale} prompt={prompt} now={now} />
        ))}
      </div>
    </div>
  );
}

function Card({ locale, prompt, now }: { locale: Locale; prompt: Prompt; now: number }) {
  const answer = useApproval((s) => s.answer);
  const left = secondsLeft(prompt, now);
  // Downed the moment it is pressed, rather than when the host confirms it: the wait ends as soon as the
  // answer is taken, and a live-looking button over a settled question is an invitation to answer twice.
  const busy = prompt.pressed !== null;
  return (
    <div className="approval-card">
      <div className="approval-head">
        <span className="approval-label">
          <span className="dot-running" aria-hidden />
          {translate(locale, "approval.waiting")}
        </span>
        <span className="approval-class">
          {translate(locale, `extensions.class.${prompt.permission}`)}
        </span>
      </div>
      <p className="approval-headline">{prompt.headline}</p>
      {prompt.command && <pre className="approval-command">{prompt.command}</pre>}
      <div className="approval-foot">
        <span className={`approval-count${left <= 10 ? " approval-count--short" : ""}`}>
          {translate(locale, "approval.left", { seconds: left })}
        </span>
        <div className="approval-actions">
          {/* Two plain buttons rather than one form with two submitters: a focused button already
           * answers Enter, and a submit would have to work out which press meant which answer. */}
          <button
            type="button"
            onClick={() => void answer(prompt.callId, false)}
            disabled={busy}
            className="approval-deny"
          >
            {translate(locale, "approval.deny")}
          </button>
          <button
            type="button"
            onClick={() => void answer(prompt.callId, true)}
            disabled={busy}
            className="approval-allow"
          >
            {translate(locale, "approval.allow")}
          </button>
        </div>
      </div>
    </div>
  );
}
