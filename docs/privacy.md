# Privacy

Short version: this app has no account, no analytics and no server of its own.
It cannot see what you type, because nothing you type ever leaves your machine.
This page is the long version, written so you can check each claim against the
code.

## What leaves the machine

Only a download you started. There are exactly two kinds of outbound request,
and both are aimed at the Hugging Face endpoint in your settings
(`https://huggingface.co` unless you changed it):

| When | Request | Why |
| --- | --- | --- |
| You press **Download**, or the CLI resolves a catalog entry | `GET {endpoint}/api/models/{repo}/resolve/{revision}/{file}` | size, commit and digest, so a resumed or verified file is the file the registry still serves |
| The transfer itself | `GET {endpoint}/{repo}/resolve/{revision}/{filename}` | the bytes; the registry may redirect to its CDN, and the redirect is followed without any credential |

Nothing else. There is no update pinger — the **Settings → Data and privacy**
switch is stored but unread in this build, and the page says so. There is no
crash reporter, no metrics, no "improve the model" upload, and no phone-home in
the dependency tree; the webview additionally runs under a Content-Security-
Policy whose `connect-src` allows only its own origin and the local IPC bridge,
so a script that tried to reach the network from the UI would be blocked by
WebKit rather than by polite convention.

What the registry sees about a download is what any web request reveals: your IP
address, the repository and file you asked for, the time, and the standard
headers the HTTP client sends. This app sets no User-Agent and no identifier of
its own. Hugging Face's treatment of that access log is governed by
[its own privacy policy](https://huggingface.co/hardware), not by this file.

Inference is local. The mock engine and the `llama-cpp` engine both run in this
process; prompts, documents and answers are never serialised toward a network
client. The OpenAI-compatible server is a socket you start yourself, bound to
loopback, and a non-loopback `host` is refused at startup.

## Your Hugging Face token

If you paste a gated-model token into **Settings → Models**, it goes to the OS
credential store — the macOS Keychain or Windows Credential Manager — and to
`settings.json` never. Logs record only that a credential exists
(`configured=true`), never its value. The bearer header is attached only after
the request's host is compared against the configured endpoint, so a CDN
redirect in the middle of a download carries no credential with it. Delete it
from Settings, or from the Keychain directly, and the app has nothing left to
find.

## What is written to disk, and where

The data folder is `~/Library/Application Support/SmolLLM Studio` on macOS,
`%APPDATA%\SmolLLM Studio` on Windows, `~/.local/share/smollm-studio` on Linux,
and `SMOLLM_STUDIO_DATA_DIR` overrides all of it.

| Path | Contents | Sensitive? |
| --- | --- | --- |
| `settings.json` | theme, model folder, default model, context length, GPU layers, backend, decode threads, sampling sliders and seed, server host and port, chat preset and stop sequences | only in that your default model and sampling taste are yours; no token ever appears here |
| `sessions/*.json` | one file per transcript: your messages, the model's replies, timestamps, the model used | yes, if your prompts are — plaintext, like a document you saved |
| `logs/smollm-<date>.log` | daily-rotating info-and-above lines from the app, engine, downloads and server: hardware numbers, transfer progress, model names, errors | prompts and message contents are not logged; the transcript of a chat is on disk only in `sessions/` |
| `models/*.gguf`, `*.gguf.part` | the weights you downloaded, and the resume file of an interrupted transfer | a model file is a file; nothing about you is in it |
| `~/Library/Application Support/studio.smollm.app/.window-state.json` | the window's size, position and maximised state, so the app comes back the way you left it | no |

Tauri's own app data — nothing user-visible, but worth knowing it exists — sits
next to that last file.

## Getting it off your machine

**Settings → Data and privacy → Reset** clears settings, the in-memory log and
every saved transcript, and removes partial downloads. It deliberately keeps
your model files; delete those from the Library page, one at a time, or delete
the folder. For the whole install at once: quit the app, then remove the data
folder and the Tauri config folder named above, and forget the Keychain item if
you stored a token. There is no second copy anywhere, because there is no
backend to hold one.

Exports are yours to place: **Chat → Export** writes a transcript to a path you
pick, and that file is then governed by wherever you put it.

## Honest limits

- **Disk is the threat model you have to cover.** Transcripts and logs are
  plaintext with your user's file permissions. FileVault, BitLocker or an
  equivalent is the protection.
- **Anyone on the machine can use the local API.** The served port has no
  authentication, because a loopback endpoint on your own machine was not the
  threat the design targets — see [SECURITY.md](../SECURITY.md) before you point
  another program at it on a shared login.
- **The catalog's third-party metadata can be wrong.** A size, license id or
  parameter count comes from the registry at the time the entry was curated; the
  app re-checks the byte length and digest before it accepts a file, not the
  licence text.
- **A model's own behaviour is not covered by this document.** Nothing the model
  generates is sent anywhere by this app, but a prompt you paste into it may
  contain someone else's information; that is your call to make, not the app's.
