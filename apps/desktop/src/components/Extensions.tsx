// The Extensions page: what the agent is offered, and which of those this build actually runs.
//
// "Extensions" in a cloud app means an add-on store. This app has no add-on format and nothing to
// install from, so the row's local meaning is the one thing that occupies the same place: the list of
// capabilities the model is handed when it is allowed to act. That list exists in the host already —
// `freecode_tools::registry` is what the Coding loop offers — and this page reads it rather than
// restating it, so a tool added there shows up here without a second edit.
//
// The two facts worth a page are the ones no other surface shows. Which calls reach a runner in
// this build (the four reads and four writes inside the workspace, the nine page calls, the two web
// calls the privacy switch has to be on for, the command and its three runners, the repository's
// eight and the one delegation — the registry holds nothing else, so a row here is a capability
// rather than an intention), and what the run would do with each one right now. The second is
// computed by `freecode_core::approvals::evaluate` against the settings and sandbox in force — the
// same engine that decides it mid-run, so the page cannot tell a friendlier story than the loop acts.
import { useEffect, useState } from "react";

import { Row, Section } from "./controls";
import { Icon } from "./icons";
import { translate, type Locale } from "../i18n";
import { ipc, type ExtensionInventory, type ExtensionTool } from "../lib/ipc";
import { shortcutLabel } from "../lib/shortcuts";

type T = (key: string, params?: Record<string, string | number>) => string;

/** A label and its value, right-aligned. The nav column is a list of these, so the figures the page
 * leads with read as measurements rather than prose. */
function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-[var(--space-2)] py-1 text-quiet">
      <span className="text-muted">{label}</span>
      <span className="text-right font-medium tabular-nums text-fg-strong">{value}</span>
    </div>
  );
}

/**
 * What a run would do with this call — the approval answer, not the execution answer, which is the
 * section the row sits in. Only "Auto-approve" gets the accent's soft fill: a call that happens
 * without interrupting anyone is the thing this app grants narrowly, and the other two are the row
 * saying it has not been granted.
 */
function Decision({ label, granted }: { label: string; granted: boolean }) {
  return (
    <span
      className={`shrink-0 rounded-[var(--radius-pill)] px-2 py-0.5 text-fine ${
        granted ? "bg-accent-soft text-fg-strong" : "bg-sunken text-muted"
      }`}
    >
      {label}
    </span>
  );
}

/**
 * The row's two labels are looked up by the host's own wire spellings — the same convention the
 * refusal cards and the Settings options use, and the reason there is no copy of either vocabulary
 * here to drift. `extensions::tests` asserts the spellings that appear on the wire.
 */
function Tool({ tool, t }: { tool: ExtensionTool; t: T }) {
  return (
    <Row title={tool.title} description={`${tool.id} — ${tool.description}`}>
      <span className="flex shrink-0 items-center gap-[var(--space-2)]">
        <span className="pill shrink-0">{t(`class.${tool.permission}`)}</span>
        <Decision label={t(`decision.${tool.decision}`)} granted={tool.decision === "auto"} />
      </span>
    </Row>
  );
}

export function Extensions({ locale, onBack }: { locale: Locale; onBack: () => void }) {
  const [inv, setInv] = useState<ExtensionInventory | null>(null);
  const [error, setError] = useState<string | null>(null);
  const t: T = (key, params) => translate(locale, `extensions.${key}`, params);

  useEffect(() => {
    let live = true;
    void ipc
      .extensionsInventory()
      .then((found) => {
        if (live) setInv(found);
      })
      .catch((e: unknown) => {
        if (live) setError(String(e));
      });
    return () => {
      live = false;
    };
  }, []);

  // `?tool=<id>` is the deep link to one row of this list, the way `?row=` is the deep link to a
  // settings control: the list is long and this window cannot be scrolled or clicked by the harness.
  // Dev-only, and it moves the page rather than changing it — the row it finds is the row the
  // engine already decided.
  useEffect(() => {
    if (!inv) return;
    const wanted = import.meta.env.DEV
      ? new URLSearchParams(window.location.search).get("tool")
      : null;
    if (!wanted) return;
    const rows = [...document.querySelectorAll<HTMLElement>(".card-row")];
    const hit = rows.find((row) =>
      row.querySelector(".row-desc")?.textContent?.trim().startsWith(`${wanted} —`),
    );
    if (!hit) return;
    hit.scrollIntoView({ block: "center" });
    hit.dataset.flash = "true";
    const id = window.setTimeout(() => delete hit.dataset.flash, 1400);
    return () => window.clearTimeout(id);
  }, [inv]);

  const blocked = inv?.tools.filter((r) => r.decision === "blocked").length ?? 0;
  // The registry holds only calls that reach a runner, so this is empty today. The section stays
  // because it is what an unimplemented declaration would read as — a row that says so out loud
  // rather than a capability the page implies.
  const declaredOnly = inv?.tools.filter((r) => !r.runnable) ?? [];

  return (
    <div className="flex h-full min-h-0 bg-bg text-fg">
      <nav
        aria-label={t("title")}
        className="flex h-full w-[var(--inspector-width)] shrink-0 flex-col border-r border-line bg-panel"
      >
        <div
          className="pt-[var(--space-3)] pb-1 pr-[var(--space-3)]"
          style={{ paddingLeft: "max(var(--space-4), var(--traffic-light-inset))" }}
        >
          <button
            type="button"
            title={shortcutLabel("escape")}
            className="nav-item"
            onClick={onBack}
          >
            <Icon name="back" />
            <span>{t("back")}</span>
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-[var(--space-3)] pb-[var(--space-3)]">
          {!inv && !error && (
            // The column holds its shape through the round trip rather than arriving empty and
            // filling, the way the status card does for the same reason.
            <div className="flex flex-col gap-1">
              {[0, 1, 2].map((i) => (
                <span key={i} className="skeleton-line block h-7" />
              ))}
            </div>
          )}
          {inv && (
            <>
              <p className="section-label">{t("thisBuild")}</p>
              <Stat label={t("declared")} value={String(inv.tools.length)} />
              <Stat label={t("runnable")} value={String(inv.runnable)} />
              <Stat label={t("blocked")} value={String(blocked)} />

              <p className="section-label mt-[var(--space-4)]">{t("policy")}</p>
              {/* The approval dial's own option strings, from Settings: one vocabulary for the value,
                  so the page and the control it reports cannot word it differently. */}
              <Stat
                label={t("mode")}
                value={translate(locale, `settings.opt.${inv.approvalMode}`)}
              />
              <Stat
                label={t("network")}
                value={t(inv.networkAllowed ? "network.on" : "network.off")}
              />
              <p className="mt-[var(--space-3)] text-fine text-faint">
                {t("workspace")}
                <br />
                <span className="font-mono break-words">{inv.workspaceRoot}</span>
              </p>
            </>
          )}
        </div>
      </nav>

      <main className="min-w-0 flex-1 overflow-y-auto">
        <div className="mx-auto w-full max-w-[980px] px-[var(--space-6)] py-[var(--space-6)]">
          <h1 className="text-page font-semibold text-fg-strong">{t("title")}</h1>
          <p className="mt-1 text-muted">{inv ? t("about") : t("loading")}</p>

          {error && (
            <p
              role="alert"
              className="mt-[var(--space-4)] rounded-[var(--radius-md)] border border-danger px-[var(--space-3)] py-[var(--space-2)] text-quiet text-danger"
            >
              {t("loadFailed", { error })}
            </p>
          )}

          {inv && (
            <>
              <Section label={t("runs")} description={t("runs.desc", { count: inv.runnable })}>
                {inv.tools
                  .filter((r) => r.runnable)
                  .map((r) => (
                    <Tool key={r.id} tool={r} t={t} />
                  ))}
              </Section>

              {declaredOnly.length > 0 && (
                <Section
                  label={t("declaredOnly")}
                  description={t("declaredOnly.desc", { count: declaredOnly.length })}
                >
                  {declaredOnly.map((r) => (
                    <Tool key={r.id} tool={r} t={t} />
                  ))}
                </Section>
              )}

              <p className="text-fine text-faint">{t("note")}</p>
            </>
          )}
        </div>
      </main>
    </div>
  );
}
