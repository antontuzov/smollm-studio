import { useEffect, useRef, useState } from "react";

import { Extensions } from "./components/Extensions";
import { Inspector } from "./components/Inspector";
import { ModelStatus } from "./components/ModelStatus";
import { Onboarding } from "./components/Onboarding";
import { Palette } from "./components/Palette";
import { Preview } from "./components/Preview";
import { Review } from "./components/Review";
import { Settings } from "./components/Settings";
import { Sidebar } from "./components/Sidebar";
import { StatusBar } from "./components/StatusBar";
import { Terminal } from "./components/Terminal";
import { Transcript } from "./components/Transcript";
import { translate, type Locale } from "./i18n";
import { ipc } from "./lib/ipc";
import { useShortcuts } from "./lib/shortcuts";
import { useAppearance } from "./state/appearance";
import { useApproval } from "./state/approval";
import { useChat } from "./state/chat";
import { currentView, useNav } from "./state/nav";
import { useOnboarding } from "./state/onboarding";
import { useSettings } from "./state/settings";
import { useSessions } from "./state/sessions";
import { useShell } from "./state/shell";
import { FIXTURE_RUNS, useTerminal } from "./state/terminal";
import {
  FIXTURE_DIFF,
  FIXTURE_ROUNDS,
  FIXTURE_ROWS,
  builtPage,
  isTurn,
  useTranscript,
} from "./state/transcript";

export default function App() {
  const load = useAppearance((s) => s.load);
  const language = useAppearance((s) => s.language);
  const setRows = useTranscript((s) => s.setRows);
  const setContextUsed = useTranscript((s) => s.setContextUsed);
  const setRunDiff = useTranscript((s) => s.setRunDiff);
  const setRunRounds = useTranscript((s) => s.setRunRounds);
  const send = useChat((s) => s.send);
  const stop = useChat((s) => s.stop);
  const newTask = useChat((s) => s.newTask);
  // Which surface the window shows is the top of the navigation stack rather than a set of booleans:
  // the header's arrows move along it, a surface's own sub-page travels with its entry, and Esc leaves
  // one step at a time. The deep links the captures go through (`?settings=model`, `?extensions=1`)
  // seed the same stack in `state/nav.ts`, with the conversation underneath them.
  const view = useNav(currentView);
  const push = useNav((s) => s.push);
  const back = useNav((s) => s.back);
  // Which of the two reports the right column is holding, and the press that gives it back. The review
  // is not on the navigation stack: it is the same column answering a different question, and a Back
  // arrow that un-opened it would leave the conversation exactly as it was anyway.
  const panel = useNav((s) => s.panel);
  const showInspector = useNav((s) => s.showInspector);
  // The in-app browser, opened by the review column for a page the run built. Not on the navigation
  // stack — one press in, one press out, like the palette — so it closes into whatever surface asked
  // for it. Its tabs are the panel: the last one closing is the browser closing.
  const previewOpen = useNav((s) => s.previewTabs.length > 0);
  const openPreview = useNav((s) => s.openPreview);
  const closePreview = useNav((s) => s.closePreview);
  // The two strips the header's panel toggles fold. They live in the nav store rather than in local
  // state so the deep links can seed them — the native window takes no injected clicks, and a folded
  // column that only a press can reach is one no capture can show.
  const rightOpen = useNav((s) => s.rightOpen);
  const statusOpen = useNav((s) => s.statusOpen);
  // The bottom panel a command's output lives in. A command opening it, a press closing it, and
  // `?terminal=1` seeding it are the same switch: the panel is not a report on the run so much as a
  // place the run prints, so it takes the transcript's column rather than the right one.
  const terminalOpen = useTerminal((s) => s.open);
  const seedTerminal = useTerminal((s) => s.seed);
  // `?search=<query>` opens the palette with that query already typed — the deep link the panel is
  // captured through, since the window takes no injected keystrokes and a search nobody can type
  // into is a search nobody can see.
  const paletteQuery = query("search");
  const [paletteOpen, setPaletteOpen] = useState(paletteQuery !== null);
  // A command the palette lists is run from the palette itself; the same command pressed as a
  // shortcut runs with no palette in front of it. When it cannot run, the palette opens holding the
  // reason — the alternative is a key that did nothing, which is the one thing a shortcut must not be.
  const [paletteNote, setPaletteNote] = useState<string | null>(null);
  const showPalette = () => {
    setPaletteNote(null);
    setPaletteOpen(true);
  };
  const hidePalette = () => {
    setPaletteNote(null);
    setPaletteOpen(false);
  };
  const fixture = fixtureMode();
  const ask = query("ask");
  const askRepeat = Math.max(1, Number(query("ask_repeat")) || 1);
  const askAgain = query("ask_again");
  const forgetBetween = query("new_task_between") === "1";
  const stopAfter = Math.max(0, Number(query("stop_after")) || 0);
  const reopen = query("reopen") === "1";
  // `?pagecheck=1` runs the page bridge's development probe: the host writes a small page into the
  // project folder and drives it with the real browser calls, leaving a receipt for each one on the
  // page itself. The window takes no synthetic input, so this is how the bridge is proved end to
  // end; the command refuses unless the host was started with FREECODE_PAGE_PROBE=1, and the panel it
  // opens is where a capture of it lands.
  const pagecheck = query("pagecheck") === "1";
  const filed = useSessions((s) => s.items);
  const openFiled = useSessions((s) => s.open);
  const refreshFiled = useSessions((s) => s.refresh);
  // Setup reads the same document the Settings surface edits, so the window loads it whether or not
  // that page has been opened: the first launch has to know whether the flow was already answered.
  const loadSettings = useSettings((s) => s.load);
  const settingsDoc = useSettings((s) => s.doc);
  const setupOpen = useOnboarding((s) => s.open);
  const openSetup = useOnboarding((s) => s.openSetup);

  // The catalog language is the setting, so switching it in Settings repaints every label.
  // `?lang=` is the dev-only press of that menu row. It cannot ride the appearance levers in
  // state/appearance.ts, which work by painting CSS custom properties: a language is a catalog, not a
  // colour. It stays out of the store for the same reason those levers do — a window opened with a
  // seeded language must never write one nobody chose.
  const seeded = query("lang");
  const locale: Locale = seeded === "en" || seeded === "ru" ? seeded : language;

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!pagecheck) return;
    void ipc.agentPageProbe().catch((why) => console.error("page check:", why));
  }, [pagecheck]);

  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);

  // The setup flow opens on a first launch, and on every `?onboarding=<step>` capture. The deep links
  // that name another surface keep it shut: a card over the page a capture asked for would hide the
  // thing the capture exists to show.
  const forcedSetup = query("onboarding");
  useEffect(() => {
    if (forcedSetup === null) return;
    const asked = Number(forcedSetup);
    openSetup(Number.isInteger(asked) && asked >= 1 ? asked - 1 : 0);
  }, [forcedSetup, openSetup]);
  useEffect(() => {
    if (forcedSetup !== null || fixture) return;
    if (
      query("settings") !== null ||
      query("extensions") !== null ||
      query("ask") !== null ||
      query("approve") !== null ||
      query("preview") !== null ||
      query("pagecheck") !== null
    )
      return;
    if (!settingsDoc || settingsDoc.onboarding.completed) return;
    openSetup(0);
  }, [fixture, forcedSetup, settingsDoc, openSetup]);

  // The sidebar's rows are conversations the host filed on a previous run, so the window asks once it
  // opens — the event channel itself is wired at module scope in main.tsx, outside StrictMode's dev
  // double-mount. The fixture does not: it draws its own workspace list to compare against
  // docs/reference-ui, and a real list arriving underneath it would be a second truth.
  useEffect(() => {
    if (!fixture) void refreshFiled();
  }, [fixture, refreshFiled]);

  // `?fixture=1` renders the shell with a scripted run so the layout can be compared against
  // docs/reference-ui. Nothing sets it by default: an empty transcript is honest, an invented one
  // is not.
  //
  // `?batch=open` unfolds the scripted round's calls on top of it. The host declares a finished batch
  // folded, and the native window takes no injected clicks, so without a seed the expanded half of the
  // reference's "Ran N tools" — one check-circle per call — is a surface no capture can reach.
  useEffect(() => {
    if (!fixture) return;
    setRows(
      query("batch") === "open"
        ? FIXTURE_ROWS.map((r) => (r.kind === "toolBatch" ? { ...r, collapsed: false } : r))
        : FIXTURE_ROWS,
    );
    // The shot this is compared against has a context meter part-filled. Only the fixture can claim
    // a number; a real run reports none until the engine measures it.
    setContextUsed(40);
    // …and a "+N −M files changed" pill, which is the row a run leaves when it edits a file.
    setRunDiff(FIXTURE_DIFF);
    // …and the step pill beside it, which the scripted "Ran 3 tools" header earned.
    setRunRounds(FIXTURE_ROUNDS);
    // The terminal panel's scripted check, but only for the window that asked to see the panel:
    // `?fixture=1` alone is the reference shell, and a run count in the footer that no visible
    // surface explains would be a second difference from the shot it is compared against.
    if (terminalOpen) seedTerminal(FIXTURE_RUNS);
  }, [fixture, terminalOpen, seedTerminal, setRows, setContextUsed, setRunDiff, setRunRounds]);

  // `?ask=<prompt>` sends that prompt on mount; `?ask_repeat=<n>` sends it n times in a row. The
  // native window takes no injected clicks, so this is how a real answer from the local engine gets
  // verified in the real app — and how a second turn is verified, which is the one that has to reuse
  // the running server, or restart it after it died between the two.
  //
  // `?ask_again=<prompt>` sends it after the first one, and `&new_task_between=1` presses the
  // sidebar's New task between them — the only way to check that the control forgot anything, since
  // the second prompt asks what the first planted and an answer that remembers is a history the host
  // never dropped.
  //
  // Sent once per window: StrictMode runs this effect twice on a dev mount, and a second send would
  // ask the same question twice and file two runs of it.
  const asked = useRef(false);
  useEffect(() => {
    if (!ask || asked.current) return;
    asked.current = true;
    void (async () => {
      for (let i = 0; i < askRepeat; i += 1) await send(ask);
      if (!askAgain) return;
      if (forgetBetween) await newTask();
      await send(askAgain);
    })();
  }, [ask, askRepeat, askAgain, forgetBetween, newTask, send]);

  // `?shell=<command>` opens the panel's own shell and sends that line into it. The native window
  // takes no injected keystrokes, and a terminal nobody can type into is a surface no capture can
  // reach, so this is the dev-only press of the prompt row: with an empty value it only opens the
  // shell. A refusal — the Terminal switch off, Plan only, a denied command — lands in the panel as
  // its own sentence, which is the half of this feature a capture can prove.
  const shellLine = query("shell");
  useEffect(() => {
    if (shellLine === null) return;
    const panel = useTerminal.getState();
    panel.setOpen(true);
    panel.showShell();
    void (async () => {
      if (!(await useShell.getState().open())) return;
      const command = shellLine.trim();
      if (command) await useShell.getState().send(command);
    })();
  }, [shellLine]);

  // `?approve=<ms>` presses the card's Allow that many milliseconds after the first approval prompt
  // lands. The native window takes no injected clicks, so this is the dev-only press of the control —
  // and the only way a capture can show that a prompt the run waited on is answered and the write it
  // guarded then happens. A large value is the other half: the card sits on screen while the run is
  // parked behind it, which is the state a screenshot of this feature needs.
  const approveAfter = query("approve");
  const waiting = useApproval((s) => s.prompts.length > 0);
  const pressed = useRef(false);
  useEffect(() => {
    if (approveAfter === null || pressed.current) return;
    const prompt = useApproval.getState().prompts[0];
    if (!prompt) return;
    pressed.current = true;
    const ms = Math.max(0, Number(approveAfter) || 0);
    const id = window.setTimeout(() => void useApproval.getState().answer(prompt.callId, true), ms);
    return () => window.clearTimeout(id);
  }, [approveAfter, waiting]);

  // `?reopen=1` opens the newest filed conversation once the list arrives — the same call the
  // sidebar's row makes, and the only way that row's behavior gets verified rather than assumed,
  // since the native window takes no injected clicks. Waiting for the list is the point: the row
  // cannot be opened before the host has said what it filed.
  useEffect(() => {
    if (!reopen) return;
    const newest = filed[0];
    if (newest) void openFiled(newest.id);
  }, [reopen, filed, openFiled]);

  // `?preview=<path>` opens the in-app browser on that page — the dev-only press of the review's
  // open control, since the native window takes no injected clicks. The host answers with the
  // `preview://` address its scheme serves; a refusal is dev noise, so it is dropped.
  const previewPath = query("preview");
  useEffect(() => {
    if (previewPath === null) return;
    void ipc.previewUrl(previewPath).then(
      (url) => openPreview(url, previewPath),
      () => {},
    );
  }, [previewPath, openPreview]);

  // `?stop_after=<ms>` presses the composer's Stop that far into a run. Measured from the run
  // starting rather than from the window opening, because the engine spends seconds loading weights
  // and a timer from mount would press Stop before there was a reply to stop.
  const sending = useChat((s) => s.sending);

  // A run that built a page opens it: "build me a game" ends with the game on screen, rather than a
  // path the user has to go and find. Read out of the store the moment the send settles instead of
  // from a render, because the run's last events and the command's return race and the store is the one
  // place both have landed by then. A stopped run opens nothing — its page is half-written, and the
  // frame would be showing the user their own interruption.
  const wasSending = useRef(false);
  useEffect(() => {
    const settled = wasSending.current && !sending;
    wasSending.current = sending;
    if (!settled) return;
    const rows = useTranscript.getState().rows;
    const last = [...rows].reverse().find((row) => isTurn(row) && row.role === "assistant");
    if (last && isTurn(last) && last.stopped) return;
    const page = builtPage(rows);
    if (!page) return;
    void ipc.previewUrl(page).then(
      (url) => openPreview(url, page),
      () => {},
    );
  }, [sending, openPreview]);

  useEffect(() => {
    if (!stopAfter || !sending) return;
    const timer = setTimeout(() => void stop(), stopAfter);
    return () => clearTimeout(timer);
  }, [sending, stopAfter, stop]);

  // The keys the hints on the controls name. Settings is a full-window surface rather than a modal,
  // so while it is up it owns the keyboard: Esc gives the window back and the chat keys hold until
  // it does, rather than firing something the user cannot see.
  const running = useTranscript((s) => s.running);
  useShortcuts({
    newTask: () => {
      // The palette and the surfaces each own the keyboard the same way: a New task fired from under
      // one would empty the transcript the thing in front of the user describes. Setup owns it too —
      // it covers the window, and a shortcut that edits what is behind it is a shortcut into a hidden page.
      if (setupOpen) return;
      if (view.kind !== "chat" || paletteOpen) return;
      void newTask();
    },
    // ⌘, opens Settings over whatever is under it, and a second press does not bury the page the user
    // is reading under a fresh copy of Appearance.
    settings: () => {
      if (setupOpen) return;
      if (view.kind !== "settings") push({ kind: "settings", page: "appearance" });
    },
    // Twice closes it, because the second press is the user changing their mind.
    palette: () => {
      if (setupOpen) return;
      if (paletteOpen) hidePalette();
      else showPalette();
    },
    // The panel is a strip of the conversation's column, so it answers only while that column is the
    // page in front — over Settings or the setup flow it would open something behind them.
    terminal: () => {
      if (setupOpen) return;
      if (view.kind !== "chat" || paletteOpen) return;
      useTerminal.getState().toggle();
    },
    // The palette's open commands, at the keys the reference prints beside them. Each is the same
    // press the palettes row makes; the folder command has no row to leave open on a refusal, so it
    // opens the palette and puts the host's reason in it.
    files: () => {
      if (setupOpen) return;
      if (view.kind !== "chat" || paletteOpen) return;
      void ipc.revealWorkspace().catch((why) => {
        setPaletteNote(translate(locale, "palette.commandFailed", { why: String(why) }));
        setPaletteOpen(true);
      });
    },
    browser: () => {
      if (setupOpen) return;
      if (view.kind !== "chat" || paletteOpen) return;
      useNav.getState().openBrowser();
    },
    review: () => {
      if (setupOpen) return;
      if (view.kind !== "chat" || paletteOpen) return;
      useNav.getState().showReview();
    },
    // Esc leaves a step of the history when there is one to leave and stops the run when the window is
    // already back at the conversation. The one control that means two things takes three in hand: a
    // review panel the user opened is the nearest thing to close, and it is not on the stack. The
    // in-app browser is nearer still — it opened from that panel — so it closes first. While the
    // framed page has focus its own keys do not reach the window, which is what a game in it needs.
    escape: () => {
      if (previewOpen) {
        closePreview();
        return;
      }
      if (panel === "review") {
        showInspector();
        return;
      }
      if (!back() && running) void stop();
    },
  });

  // One overlay whatever surface is underneath it: the flow can be re-run from Settings, so its
  // "open a settings page" buttons push onto the same history the window's header moves along.
  const setup = setupOpen ? (
    <Onboarding locale={locale} onOpenPage={(page) => push({ kind: "settings", page })} />
  ) : null;

  if (view.kind === "settings") {
    return (
      <>
        <Settings
          locale={locale}
          page={view.page}
          onOpen={(page) => push({ kind: "settings", page })}
          onBack={back}
        />
        {setup}
      </>
    );
  }

  if (view.kind === "extensions") {
    return (
      <>
        <Extensions locale={locale} onBack={back} />
        {setup}
      </>
    );
  }

  return (
    <>
      <div className="app-shell flex h-full flex-col bg-bg text-fg">
        <div className="flex min-h-0 flex-1">
          <Sidebar
            locale={locale}
            fixture={fixture}
            onOpenSettings={() => push({ kind: "settings", page: "appearance" })}
            actions={{
              newTask: () => void newTask(),
              openPalette: showPalette,
              openExtensions: () => push({ kind: "extensions" }),
            }}
          />
          <div className="flex min-w-0 flex-1 flex-col">
            <Transcript
              locale={locale}
              onPickModel={() => push({ kind: "settings", page: "model" })}
              onOpenPalette={showPalette}
            />
            {terminalOpen && <Terminal locale={locale} />}
            {statusOpen && <StatusBar locale={locale} />}
          </div>
          {/* The column owns its width, not either report inside it: both are the same column's turn, and
            a height the pair has to share is one constraint the reports should not each re-declare. */}
          {rightOpen && (
            <div
              className={`flex min-h-0 shrink-0 flex-col ${
                previewOpen || panel === "review"
                  ? "w-[var(--review-width)]"
                  : "w-[var(--inspector-width)]"
              }`}
            >
              {!previewOpen && <ModelStatus locale={locale} />}
              {/* One column, three reports: the run's numbers, the change those numbers name, or the
                page that change built. Shot 06 has the diff here rather than under the answer,
                because a code line needs the width the transcript would have to give up to show it —
                and the game the diff produced gets the same column rather than the whole window,
                because it is the run's answer, not the app's. The model card steps aside for it: the
                page being played is the point, and the route it came from is the chat's business. */}
              {previewOpen ? (
                <Preview locale={locale} />
              ) : panel === "review" ? (
                <Review locale={locale} />
              ) : (
                <Inspector locale={locale} fixture={fixture} />
              )}
            </div>
          )}
        </div>
        {paletteOpen && (
          <Palette
            locale={locale}
            initial={paletteQuery ?? ""}
            notice={paletteNote}
            onClose={hidePalette}
          />
        )}
      </div>
      {setup}
    </>
  );
}

function fixtureMode(): boolean {
  return query("fixture") === "1";
}

/** `?settings=<page>` opens the Settings surface on that page — the dev-only deep link the
 * reference screenshots are captured from, since the native window takes no injected clicks. */
function query(name: string): string | null {
  return new URLSearchParams(window.location.search).get(name);
}
