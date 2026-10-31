# FreeCode

**A desktop coding agent that runs on free models.** No account, no API key, no bill.

[![Release](https://img.shields.io/github/v/release/antontuzov/FreeCode?label=release)](https://github.com/antontuzov/FreeCode/releases/latest)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![macOS](https://img.shields.io/badge/platform-macOS%20%7C%20Apple%20silicon-lightgrey)](#requirements)

FreeCode is a native macOS workbench — Rust host, Tauri v2, React interface — pointed at the free
tier [OpenCode](https://opencode.ai) publishes. It is a real agent loop: it reads your project,
writes files, runs your checks, drives a browser, and shows every step it took.

**[Download for macOS →](https://github.com/antontuzov/FreeCode/releases/latest/download/FreeCode.dmg)**
· [Landing page](https://antontuzov.github.io/FreeCode/) · [中文](https://antontuzov.github.io/FreeCode/zh/) · [Issues](https://github.com/antontuzov/FreeCode/issues)

> The build is not signed or notarised yet. On first launch, right-click the app → **Open**, then
> confirm once; Gatekeeper leaves it alone after that.

## What it is

A three-column workbench: conversations on the left, the transcript in the middle, the run's report
on the right. The transcript is not a chat log — every tool call is a row with its own result, every
refusal names the reason in your interface language, and the inspector reports only numbers the
service or the host actually measured.

- **A real agent loop.** The model reads and searches the project, creates and edits files, applies
  patches, and runs the workspace's own tests, build and linter through argv. Each round is narrated
  in the transcript.
- **A browser the agent can use.** It opens the page it built in a panel beside the chat, clicks and
  types in it, screenshots it, and can look at that screenshot with a vision model.
- **A terminal beside the chat.** Watch every shell command a run executes, or open your own shell in
  the project folder and re-run the check by hand. Commands pass the same approval policy, and the
  panel can run a Python REPL in the project folder as well.
- **Review before you trust.** Every change lands in a diff panel with line numbers and a find
  control. A run keeps a before-image journal, so revert puts the folder back the way it was —
  including files the run created.
- **Plan mode.** The same run as a read-only proposer, marked so it cannot be mistaken for a change
  that already happened.
- **Your own rules, from your own files.** `~/.freecode/AGENTS.md` for the machine, `AGENTS.md` and
  `.freecode/AGENTS.md` for the folder are read before a request goes out and carried in the run's
  system turn. The transcript names which files were read, and says so when one was cut for length.
  They say how the work should be done; they grant no access the approval dial withheld.
- **Light and dark, eight interface styles, English and Russian.**

## Free models

The free models are served only to OpenCode's own runtime. FreeCode is that runtime's client, not a
way around it: a turn is relayed through the daemon the OpenCode app keeps on loopback, using the
handshake its gateway expects. Nothing is scraped and no credential is invented. The app does not
bundle OpenCode — it uses the copy already on your Mac and starts its service when it is not up.

| Model id | Label | Carrier |
| --- | --- | --- |
| `big-pickle` | Big Pickle | FreeCode — the default this build starts on |
| `glm-5.3-flash-free` | GLM-5.3-Flash | GLM |
| `deepseek-v4.1-flash-free` | DeepSeek V4.1 Flash | DeepSeek |
| `beam-free` | Beam | Beam |
| `flint-40b-free` | Flint 40B | Flint |
| `mimo-v2.6-flash-free` | MiMo-V2.6-Flash | MiMo |
| `nemotron-3.5-lightning-free` | Nemotron 3.5 Lightning | NVIDIA |
| `nemotron-3.5-super-vl-free` | Nemotron 3.5 Super VL | NVIDIA |

The list is what OpenCode's service publishes, not a list baked into this repo, so it moves without
an app update: when a model stops being free its row stops answering rather than quietly billing you.
Switch models mid-task — the run keeps its context and the report says which one answered.

**Free means cloud.** A free-model prompt leaves your Mac and is answered by OpenCode's provider under
their terms. The optional remote route is the only other way to answer a prompt. There is no local
model in this build — no weights to download, no server to start, no engine to install.

### Remote providers (optional)

Settings → Model → Remote provider can send prompts to any OpenAI-compatible endpoint instead. It is
opt-in twice: the *Allow remote providers* switch must be on, and the source must be set to Remote.
The key lives in the macOS Keychain — never in the settings file, IPC answers, logs or process
arguments — and is sent as `Authorization: Bearer` only while a request is being built. The base URL
must be `https://…` (plain `http://` only on loopback). No sampler values or reasoning field are sent;
an arbitrary endpoint has neither, so its own defaults apply.

## Install

**Download** the `.dmg` from [Releases](https://github.com/antontuzov/FreeCode/releases/latest),
drag FreeCode to Applications, right-click → Open on first launch.

**Or build it:**

```sh
pnpm install
pnpm app:dev        # the app with a live dev frontend
pnpm app:build      # a .app and a .dmg under target/release/bundle/
```

Both routes need [OpenCode](https://opencode.ai) installed for the free models. FreeCode starts its
service itself and says plainly when OpenCode is missing.

## Requirements

- macOS 12 or newer on Apple silicon. Every native build and capture was made here. Windows and
  Linux are not built or tested; the Windows button says "in the works" rather than linking a file
  that does not exist.
- [OpenCode](https://opencode.ai) — the app whose local service answers the free models.
- Rust stable with `rustfmt` and `clippy`, Node 22 or newer, and pnpm 11 through corepack
  (`packageManager` is pinned in the root manifest).

## Where things live

| What | Where |
| --- | --- |
| Settings | `~/Library/Application Support/freecode/settings.json` |
| Remote provider key | macOS Keychain, service `studio.freecode.desktop.remote-provider` |
| Filed conversations | `sessions/` beside the settings file, one JSON file each |
| Deleted conversations | `sessions/trash/`, until Settings → Storage erases one for good |

A checkout reads its data from the repo root. An installed `.app` uses the directory it owns;
`FREECODE_ROOT` overrides the root strictly, so a wrong override fails loudly instead of verifying
the wrong tree.

## Development

| Command | What it does |
| --- | --- |
| `pnpm app:dev` | the native app against the Vite dev server |
| `./scripts/verify-precommit.sh` | the gate: fmt, clippy `-D warnings`, Rust tests, prettier, eslint, tsc, store tests, build, landing root copy |
| `./scripts/verify-tauri-ui.sh` | boots the real window |
| `./scripts/verify-studio-natively.sh` | the full native set |
| `./scripts/render-mascot-logos.py <sheet.png>` | cuts the twelve duck app icons from the sheet |
| `./scripts/render-wave-marks.py ~/Desktop/wave.png` | cuts the two circular wave app icons |

Four checks exist because a claim that is not enforced rots within a commit:

- `check-i18n.mjs` — every key defined in both catalogs, and every literal lookup defined.
- `check-contrast.mjs` — every grey and style ink at 4.5:1 on every surface each style paints.
- `check-shortcuts.mjs` — every binding answered by a handler, every hint reading its label.
- `check-settings-index.mjs` — the search index matching the rows each Settings page renders.

### Looking at the UI without clicking

The native window takes no injected clicks on a developer machine, and several states are one press
deep. The dev URL accepts seeds no shipped control sets:

| Seed | Opens |
| --- | --- |
| `?fixture=1` | a scripted run, so the transcript and inspector render with nothing to show yet |
| `?ask=<prompt>` | a real agent run, started from the URL |
| `?settings=<page>`, `?q=<query>` | a Settings page, and the search field |
| `?row=<control>` | a Settings control scrolled into view, whether it is a row or a grid of tiles |
| `?terminal=1`, `?session=shell\|python`, `?input=<line>` | the terminal panel, its own shell or Python session, and the lines typed into it |
| `?review=1`, `?find=<text>`, `?scope=all`, `?outline=1` | the review panel, its find field, its scope and its fold |
| `?plan=1`, `?collapsed=<groups>`, `?extensions=1` | the plan panel, the inspector's lower groups, the Extensions inventory |

`pnpm app:dev:fixture`, `app:dev:settings` and `app:dev:chat` are three pre-wired ones.

## What this build does not do

Said plainly, because the interface says it too — a control that is not built is disabled and names
the reason on hover:

- **No local model.** The local route, its weights and its engine were removed; every build of this
  app answers over the network.
- **Not signed or notarised.** The bundle is ad-hoc signed, so a copy downloaded from the internet
  needs one right-click → Open.
- **No updater.** It needs a published update endpoint and a signing key pair that does not exist
  yet; the app tells you when a newer release is published but does not install it.
- **No telemetry.** Nothing in this build collects or sends usage data.
- **No file attachments.** The composer's add control is dead until there is a retrieval path.

## Layout

```
crates/freecode-config    settings document, privacy and approval policy
crates/freecode-tools     the tool registry, the sandbox, exec/web/page/pixel helpers
crates/freecode-core      the transcript model, the agent loop, approvals
crates/freecode-models    the retired local route's catalog contract, kept in-tree, not wired in
crates/freecode-llama     the fork's remote streaming types, kept in-tree, not wired in
apps/desktop              the Tauri v2 host, the OpenCode relay and the React interface
site                      the landing page published at antontuzov.github.io/FreeCode
docs/reference-ui         the screenshots the interface is matched against
docs/KNOWN_ISSUES.md      every model limit the code defends against, and where the defence is
```

## Third-party

- **OpenCode** — the free tier and the loopback service the free route is relayed through. FreeCode
  is not affiliated with OpenCode; it uses the copy you installed and the list that service publishes.
- **Simple Icons (CC0)** — path data for the marks the app wears: GitHub, Hugging Face, Wikipedia and
  four maker signs on the model rows. The signs remain their owners' trademarks; only the icon
  geometry is shared.
- **KaTeX** fonts and stylesheet, bundled with the frontend build.

FreeCode is MIT licensed; see [`LICENSE`](LICENSE).
