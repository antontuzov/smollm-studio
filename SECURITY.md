# Security policy

SmolLLM Studio runs models on your own machine and serves them over loopback.
That design is the security model, so this page says what is actually protected,
what is deliberately not, and how to report a problem.

## What this app guarantees

- **Nothing leaves the machine except a download you start.** No telemetry, no
  analytics, no crash reporting, no update pinger, no background request of any
  kind. The only outbound traffic is a model fetch from the registry you chose,
  plus the metadata request that checks its size and digest.
- **The HTTP server binds to loopback, and refuses to do otherwise.** A
  `host` other than `127.0.0.1`, `localhost` or `::1` is rejected at startup, so
  the app cannot accidentally become an open endpoint on a network.
- **A Hugging Face token lives in the OS credential store** — the macOS Keychain
  or the Windows Credential Manager — never in `settings.json`, never in a log
  in full, and its bearer header is attached only after the request's host is
  compared with the configured endpoint. A CDN that a download redirects to is
  served without it.
- **The webview gets a Content-Security-Policy** (`default-src 'self'`, no
  remote script, style, font or connect source, no `object-src`, no framing),
  and the app's capabilities are limited to the window it ships: the frontend
  can open native dialogs and call the commands it calls, nothing else.
- **Downloaded files are validated before they are used.** A `.gguf` is checked
  for a parseable header, a plausible tensor layout and, where the registry
  publishes one, the exact byte length and SHA-256 — and an import or a resume
  that fails any of those never enters the library.

## What this app does not protect

Read this part before pointing another program at the server.

- **The local API has no authentication.** Anyone or anything that can open a
  TCP connection to the port can send prompts and read answers. That is safe
  against remote hosts because of the loopback bind, and not safe against other
  users or other processes on the same machine. If you share a login session,
  treat the port as open.
- **Model output is untrusted text.** A small local model can be steered by
  instructions embedded in a document you feed it, exactly like a hosted model,
  and there is no provider-side moderation in between. Nothing in this app
  executes model output; if you wire it into something that does, that is your
  boundary to defend.
- **Transcripts and logs are plaintext on disk**, under the app's data folder,
  with the file permissions your user account gives them. Disk encryption is
  the protection, as it is for any document you save.
- **Model weights are third-party artifacts** under their own licences, which
  this app neither audits nor sandboxes.
- **Releases are not yet signed or notarized.** The published build artifacts are
  produced by CI and are currently unsigned for macOS and Windows, so first
  launch needs the manual approval your OS asks for, and "this binary came from
  this project" is not yet something you can verify from the file itself. See
  the README's status section; closing this gap is the next shipping milestone.

## Reporting a vulnerability

Open a **private security advisory** rather than a public issue, so nobody is
handed an exploit before there is a fix:

<https://github.com/antontuzov/smollm-studio/security/advisories/new>

If you would rather not use GitHub, email the maintainer address on the
repository's commits and put "SmolLLM Studio security" in the subject.

Please include: the app version from **Settings → About** (or `smollm --version`),
the OS and architecture, what you expected, what happened, and how you triggered
it. A `cURL` invocation or a request body that reproduces the problem is worth
more than a description.

There is no bounty programme and no formal embargo period, because this is not a
company. What you can expect is a reply, a fix, and credit in the release notes
if you want it.

## Scope

Issues in the underlying libraries — llama.cpp, Tauri, Rust crates, or a model's
own behaviour — belong to those projects first. File them here when this app uses
them in a way that creates the problem, or when you are not sure; forwarding a
report is cheap, missing one is not.
