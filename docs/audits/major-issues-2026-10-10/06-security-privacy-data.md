# Security, privacy, and data — major-issues audit (2026-10-10)

- Scope: token storage, local data isolation, audio/transcript storage and retention, permissions, secrets, network exposure, persistence integrity, deletion/history privacy. Worktree `audit/major-issues` @ `b6d6815`. Static code audit only — no runtime verification (automated tests are prohibited by `AGENTS.md:4`, and no Inlay Dev session was run).
- Headline: no P0. I found no unauthenticated _remote_ path to dictation content: non-loopback binds require a token, the client refuses non-Tailscale LAN HTTP, browsers are rejected, and on-disk writes are unusually careful. The major risks are defaults and trust shape: the default same-Mac server is unauthenticated to every local process, one shared token is all-or-nothing across every Mac's recordings, and the archive keeps all audio/text forever — including takes the user cancelled.
- Severity key: P1 = fix before relying on this with sensitive dictation beyond a single-user Mac; P2 = real exposure worth closing, narrower preconditions. Effort: S ≤ a day, M = a few days, L = a week+ or a contract change.

## Findings

### 1. P1 — The default same-Mac server has no authentication; any local process or user can read, change, and delete the whole archive

- Evidence (verified-in-code): auth is only enforced when a token exists; without one, the sole check is that the `Host` header names loopback (`Server/src/http-server.ts:72`). A token is only _required_ for non-loopback binds (`Server/src/configuration.ts:102`). Both default launch paths are tokenless: the packaged dev runner only adds `--token-file` when `INLAY_SERVER_TOKEN_FILE` happens to be set (`scripts/run-dev.sh:143`, `scripts/run-dev.sh:150`), and the server guide's run example passes no token file (`Server/README.md:117`).
- What the user experiences: on the recommended one-Mac setup, any app or process running on that Mac — under any user account, sandboxed or not, no permission prompt — can `GET /v1/generations` for every transcript, download `original.wav` for every take, `DELETE` history, and `PUT /v1/preferences` to rewrite the shared cleanup prompt/dictionary, which silently changes what future dictation inserts into the user's documents. The `Host`/`Origin` checks stop _websites_ (good), but they do nothing against local code, which can set any header it wants. Dictation is exactly the data class (draft messages, passwords read aloud, private notes) where "any local process can read it" hurts.
- Confidence: verified-in-code.
- Fix direction: make the token unconditional. The source dev runner already auto-generates a 0600 token file (`Server/scripts/dev.ts:19`); do the same for packaged/loopback runs, and let the same-Mac client discover it (token file next to the data dir, imported into Keychain on first connect — the client already scopes Keychain entries per endpoint+directory, `Sources/Inlay/ClientPreferences.swift:64`). Alternatively serve same-Mac traffic over a 0700 Unix socket. Keep an explicit `--allow-unauthenticated-loopback` escape hatch with a startup warning if friction is a concern.
- Effort: M.
- Owner decision: accept the "local process = trusted" model (the Ollama precedent), or pay the first-run pairing cost? This is P1 on a single-user Mac and P0-shaped on a shared/multi-user Mac or Linux host.

### 2. P1 — One shared bearer token is all-or-nothing, and "device" identity is self-asserted

- Evidence (verified-in-code): the server holds exactly one token (`Server/src/configuration.ts:14`) and one timing-safe compare guards every route (`Server/src/http-server.ts:80`). At admission, `device.id`/`device.name` are only shape-checked as labels (`Server/src/generation-service.ts:417`); nothing binds a request to a device. History returns all records regardless of origin (`Server/src/generation-service.ts:710`), delete works on any terminal record (`Server/src/generation-service.ts:856`), and artifacts — including imported Wispr `screenshot.png` — are served to any caller with the token (`Server/src/generation-service.ts:871`, artifact names at `Server/src/http-server.ts:41`).
- What the user experiences: every connected Mac fully trusts every other. A lost, sold, or compromised Mac (or a leaked token) exposes the entire archive — all devices' audio, transcripts, and imported screenshots — and can delete or impersonate the others (history entries under a spoofed device name). There is no way to revoke one Mac; rotation means touching every client and the server, with no overlap window.
- Confidence: verified-in-code.
- Fix direction: per-device credentials issued at pairing, stored server-side as hashes, with the device record binding `device.id` to the credential; then revoke-per-device and optional read-only scopes become possible. This is an API/contract change, so it wants an API-version bump conversation.
- Effort: L.
- Owner decision: is "any of my Macs can do anything" an intended product guarantee (shared history is a marketed feature, `README.md:87`), or should history _reading_ stay shared while delete/preferences/import get device-scoped?

### 3. P1 — The archive keeps everything forever, in plaintext: all audio, raw + final text, settings snapshots, imported screenshots

- Evidence (verified-in-code): "Inference audio is always retained for completed takes; original audio is optional and defaults on. The retention toggle does not remove existing files, and there is no automatic history expiry" (`docs/architecture.md:70`). Every record also persists a full settings snapshot — cleanup prompt, vocabulary, personal dictionary (`Server/src/generation-service.ts:475`, snapshot contents at `docs/architecture.md:70`) — plus `transcript.txt` next to `metadata.json` (`Server/src/generation-service.ts:1121`). "Inlay does not add filesystem encryption" (`docs/architecture.md:74`). Wispr Flow import additionally archives `screenshot.png` per session (`Sources/Inlay/WisprFlowSourceReader.swift:103`) into the same forever-store. Files are 0600/0700 (`Server/src/storage.ts:51`), which is the only barrier.
- What the user experiences: years of everything they have ever dictated — voice recordings at microphone quality plus raw ASR text of things they later edited away — accumulate in one directory that Time Machine/container volumes happily back up (backups are encouraged, `README.md:87`). One stolen backup drive, one over-shared folder, or one token leak discloses the user's entire spoken life, not last week's. Unbounded retention also guarantees eventual disk-full dictation failures; the only guard is refusing new audio below 100 MiB free (`Server/src/storage.ts:95`).
- Confidence: verified-in-code.
- Fix direction: retention controls with teeth — keep-days/keep-count for history, a separate shorter default for _audio_ (text-only history), a server-side usage/purge surface in the app, and applying "keep original audio: off" retroactively on request. Disk-usage visibility in Server preferences would prevent the surprise-outage half of this.
- Effort: M.
- Owner decision: what are the retention defaults? "Keep everything" is currently both the default and the only option; is local-disk-is-the-user's-problem the stance, or should audio default to short-lived?

### 4. P1 — Cancelling a take does not discard it: sealed audio from cancelled/failed takes is kept and shared in history

- Evidence (verified-in-code): cancel cleans only unsealed partials (`.raw`/`.wav.partial`, `Server/src/generation-service.ts:1234`) after marking the record cancelled (`Server/src/generation-service.ts:798`); a take cancelled after sealing, or failed during processing (`Server/src/generation-service.ts:1125`), keeps its WAV. Artifact download serves `inference.wav`/`original.wav` whenever present — only `transcript.txt` is gated on `completed` (`Server/src/generation-service.ts:881`). History has no status or mode filter (`Server/src/generation-service.ts:710`), and the History UI renders failed/cancelled rows (`Sources/Inlay/Views/HistoryPage.swift:114`). The architecture doc states the behavior plainly: "Failed takes can retain metadata and sealed audio" (`docs/architecture.md:72`). No exclusion for test-microphone mode was found in server history either (mode is stored on the record, `Server/src/generation-service.ts:471`).
- What the user experiences: they start dictating something sensitive, think better of it, hit cancel — and the recording is still on the server, playable from every connected Mac's shared history, forever (finding 3). Same for a mic-test utterance they assumed was ephemeral. "Cancel" reading as "discard" is a reasonable user model; the product currently implements "cancel = keep, labelled."
- Confidence: verified-in-code for server retention and history inclusion; inferred that test-mode takes render identically in the History UI (no mode filter was found in `HistoryPage.swift`).
- Fix direction: on cancel, delete audio artifacts (keep at most a tombstone with status); apply the same to restart/shutdown-cancelled takes (`Server/src/generation-service.ts:315`). If failed-audio retention exists for debugging, put it behind a server preference defaulting off, with short expiry. Decide explicitly whether test takes are history at all.
- Effort: S–M.
- Owner decision: is failed/cancelled audio retention a deliberate debugging feature? If yes, it needs to be visible as a setting, not a silent behavior.

### 5. P2 — Plain-HTTP serving on wide binds: the dev server defaults to `0.0.0.0`, and "Tailscale range" is treated as "encrypted"

- Evidence (verified-in-code): the source dev server binds all interfaces by default (`Server/scripts/dev.ts:28`) and prints an `http://<hostname>:8392` URL (`Server/scripts/dev.ts:51`). The product server "itself serves HTTP" on any bind; remote safety is delegated to a reverse proxy or tailnet (`Server/README.md:148`). The client accepts HTTP to any literal address in `100.64.0.0/10` or the Tailscale ULA range (`Sources/Inlay/ServerEndpoint.swift:44`) while its own comment admits "address validation does not attest network routing" (`Sources/Inlay/ServerEndpoint.swift:4`).
- What the user experiences: if Tailscale is down, or the route to a `100.64.x.x` address is actually an ISP CGNAT path (that range is shared address space, not Tailscale-exclusive — inferred, general networking knowledge), the client will still send the bearer token, live audio, and transcripts over it in cleartext, with no warning. And every `bun run dev` on a coffee-shop network publishes an HTTP server holding a dev archive to the whole LAN (token-gated, but see finding 6).
- Confidence: verified-in-code for the binds and client rule; the CGNAT/route-mismatch consequence is inferred.
- Fix direction: dev runner binds `127.0.0.1` unless `--host` is explicit; product server logs a loud warning (or requires an acknowledgement env/flag) when binding non-loopback over HTTP. Longer term, built-in TLS with a self-signed cert + client pinning removes the proxy-or-nothing cliff.
- Effort: S for the dev bind + warning; L for built-in TLS.

### 6. P2 — Token strength is length-only, and nothing is rate-limited

- Evidence (verified-in-code): non-loopback requires a token of ≥32 bytes but any characters pass (`Server/src/configuration.ts:99`); the install guide's literal example is `replace-with-a-token-of-at-least-32-characters` (`Server/README.md:27`) — a user who keeps it verbatim has a _published_ credential that passes validation. No rate limiting, lockout, or delay exists anywhere in the Fastify setup (`Server/src/http-server.ts:59`); the timing-safe compare (`Server/src/http-server.ts:31`) prevents timing oracles but not online guessing of a human-chosen passphrase. Creation of generations is likewise unbounded in count — each audio stream may reach 256 MiB (`Server/src/generation-service.ts:584`) — so a token holder (or any local process, per finding 1) can fill the disk; the 100 MiB free-space gate only stops the bleeding at the end (`Server/src/storage.ts:95`).
- What the user experiences: an internet- or LAN-exposed server with a memorable passphrase token can be brute-forced at full speed; a runaway or malicious client can exhaust the server disk and take dictation down for every Mac.
- Confidence: verified-in-code.
- Fix direction: ship a token-generation path in the documented flow (`openssl rand -hex 32`, or an `inlay-server --generate-token-file` mode like `Server/scripts/dev.ts:21` already implements privately), warn on low-entropy-looking tokens, and add sane ceilings: auth-failure throttling, max concurrent uploads, and a per-archive quota that fails _before_ the disk is nearly full.
- Effort: S–M.

### 7. P2 — `/v1/health` is unauthenticated and has a side effect: it starts model warmup

- Evidence (verified-in-code): health is exempt from bearer auth even when a token is set (`Server/src/http-server.ts:80`), returns API/server versions, model IDs, backends, readiness, and `isDev` (`Server/src/generation-service.ts:337`), and kicks off model loading when speech isn't loaded (`Server/src/generation-service.ts:359`).
- What the user experiences: anyone who can reach the port (whole LAN under finding 5, whole internet behind a careless proxy) can fingerprint the deployment and repeatedly trigger multi-GB model loads / GPU warm-up — a free resource-consumption and timing oracle that also announces when the owner is away from a loaded server. The Docker `HEALTHCHECK` depends on this endpoint (`Server/Dockerfile:54`), which is presumably why it's open.
- Confidence: verified-in-code.
- Fix direction: unauthenticated health should return a minimal liveness bit only, with the detailed readiness payload behind the token; move the warmup kick to authenticated calls or server start. Docker can healthcheck liveness just as well.
- Effort: S.

### 8. P2 — Token-file permissions are documented but never enforced

- Evidence (verified-in-code): the token file is opened `O_NOFOLLOW` and size-checked, but its owner/mode are never examined (`Server/src/configuration.ts:83`). The guide tells operators to install it `0600` owned by the service user (`Server/README.md:24`), and the systemd unit points at `/etc/inlay/token` (`Server/inlay-server.service`, `Environment=INLAY_SERVER_TOKEN_FILE=/etc/inlay/token`).
- What the user experiences: one sloppy `install`/`cp` leaves the token world-readable; every local account on the server host then has full API rights (finding 2), and the server starts silently. This is the classic way "we had a token" systems get owned.
- Confidence: verified-in-code.
- Fix direction: at startup, refuse or loudly warn when the token file is group/world-accessible (POSIX mode check on the already-`stat`ed file). Same check is cheap for the auto-created dev token.
- Effort: S.

### 9. P2 — The macOS server binary ships with code-injection-friendly entitlements

- Evidence (verified-in-code): `Server/entitlements.plist:5` grants `allow-jit`, `allow-unsigned-executable-memory`, `disable-executable-page-protection`, `allow-dyld-environment-variables`, and `disable-library-validation`; the package is "ad-hoc signed with Bun's executable entitlements" (`Server/README.md:106`).
- What the user experiences: any same-user process can `DYLD_INSERT_LIBRARIES` code into the one process that holds the token in memory and reads every recording — defeating the file-permission care elsewhere. This is a Bun-runtime requirement, not carelessness, but it means the server's process boundary on macOS is weaker than its storage boundary. (The client app, by contrast, requests only `com.apple.security.device.audio-input`, `Resources/Inlay.entitlements`.)
- Confidence: verified-in-code (file contents); exploitability under the current signing setup is inferred, not exercised.
- Fix direction: test which entitlements the compiled Bun binary actually needs under hardened runtime and drop the rest (dyld-env/library-validation are the two that matter); document the residual requirement so it reads as a decision. Developer ID signing for distributed builds would also let TCC/Keychain ACLs bind to a stable identity.
- Effort: M (mostly build verification).

### 10. P2 — Deletion is per-item unlink, with quiet survivors: backup/snapshot copies and the Wispr dictionary archive

- Evidence (verified-in-code): deleting a generation recursively removes its directory (`Server/src/generation-service.ts:866`) — correct as far as it goes, but nothing overwrites data, and the docs steer users to back up the whole data directory (`docs/architecture.md:74`, `README.md:49`) without noting that "deleted" dictation persists in those backups and APFS/volume snapshots. Separately, the archived Wispr Flow dictionary (`dictionary.json` plus `dictionary-versions/`, written at `Server/src/imports.ts:868`) has no delete route in the API surface (`Server/src/http-server.ts:223` import routes are create/complete/cancel/archive only), so imported data has a permanent residue even after deleting every imported generation. Client-side history-audio previews are downloaded to a temp directory (`Sources/Inlay/ServerClient.swift:261`); that directory is removed at specific controller points (`Sources/Inlay/InlayController.swift:249`, `Sources/Inlay/InlayController.swift:816`) but downloaded audio/screenshots sit there in between (0600 in a 0700 directory, `Sources/Inlay/ServerClient.swift:266`).
- What the user experiences: "I deleted that recording" is true in the app and false on their backup drive; and a user who imports Wispr Flow, regrets it, and deletes every imported item still leaves their full Wispr dictionary on the server with no in-app way to remove it.
- Confidence: verified-in-code for the unlink and missing dictionary-delete route; backup/snapshot survival is standard platform behavior (inferred per-user setup).
- Fix direction: say what delete means in the history UI/docs ("removes from the server; copies in your backups remain"); add a delete affordance for the archived Wispr dictionary; consider wiping the preview-temp directory on quit if not already covered by the two removal points above.
- Effort: S.

## What is already done well (verified, keep)

- Timing-safe token compare (`Server/src/http-server.ts:31`); browsers rejected via `Origin` (`Server/src/http-server.ts:87`); tokenless mode additionally pins the `Host` header to loopback (`Server/src/http-server.ts:24`) — the Swift reference server mirrors all three (`Sources/InlayServerKit/InlayHTTPServer.swift:142`).
- Storage: atomic temp+rename writes at 0600 in 0700 directories, `O_NOFOLLOW`/`O_EXCL` throughout, symlink refusal, fsync of file and directory (`Server/src/storage.ts:51`); artifact serving is an allowlist through a no-follow open (`Server/src/generation-service.ts:871`); imports verify per-artifact SHA-256 against the manifest before publish (`Server/src/imports.ts:561`).
- Client: tokens in Keychain (`AfterFirstUnlockThisDeviceOnly`) scoped to endpoint+data-dir so one server's token can't be sent to another (`Sources/Inlay/ClientPreferences.swift:38`, `Sources/Inlay/ClientPreferences.swift:115`); endpoint validation runs before credentials load (`Sources/Inlay/ServerEndpoint.swift:25`); ephemeral, cache-less, redirect-refusing URLSession (`Sources/Inlay/ServerClient.swift:42`, `Sources/Inlay/ServerClient.swift:114`); ATS limited to local networking (`Resources/Info.plist`, `NSAllowsLocalNetworking`).
- No transcript logging found: Fastify logger is off (`Server/src/http-server.ts:60`), helper stderr is drained and discarded explicitly to avoid retaining content (`Server/src/inference/helper-process.ts:250`), server console output is limited to the listen address and startup errors (`Server/src/main.ts:49`). Clipboard delivery uses `currentHostOnly` pasteboard leases with restore (`Sources/Inlay/System/TextInserter.swift:655`, `Sources/Inlay/System/TextDeliveryTransaction.swift:253`), so transcripts don't ride Universal Clipboard.
- Model supply chain: pinned size+SHA-256 verified before load (`Server/src/inference/model-verification.ts:31`); no tracked secrets in the repo (`.gitignore` covers `.env*`, `*.p12`/`*.pfx`, model binaries; `git ls-files` shows no token/env/key files). Linux deployment is genuinely hardened (dedicated user, `ProtectSystem=strict`, `PrivateTmp`, loopback bind in `Server/inlay-server.service`; non-root UID 10001 + secrets-mount token in `Server/Dockerfile:51`).

## Questions for owner

- Finding 1: is "any local process is trusted" an accepted threat model for the default install, or should same-Mac pairing get a real token? Would you rather pay Unix-socket complexity or first-connect token import?
- Finding 2: is full mutual trust between all connected Macs a product promise? If a Mac is lost today, what is the intended user recovery story?
- Finding 3: what should default retention be — for original audio, inference audio, and text respectively? Is indefinite retention a feature ("your complete spoken archive") that should be marketed rather than changed?
- Finding 4: is cancelled/failed audio retention deliberate (debugging)? Should test-microphone takes appear in shared history at all?
- Finding 5: is Tailscale the only supported remote transport you want to bless, or is built-in TLS (Let's Encrypt / self-signed + pinning) on the roadmap? The answer decides whether the HTTP-range exception is a stopgap or the design.
- Finding 7: does anything outside Docker healthchecks depend on detailed unauthenticated health (e.g. the client's pre-token "is a server there?" probe during first setup)?
- Finding 10: what does "delete" promise the user — removed from the app, removed from the server disk, or removed everywhere including backups the docs told them to make?

## What I could not verify

- Runtime behavior of any of this: per `AGENTS.md:4` no automated tests were run and no Inlay Dev session was performed, so e.g. Fastify's actual header handling under Bun, Keychain ACL prompts on ad-hoc rebuilds, and TCC behavior are code-reads, not observations.
- Full security parity of the Swift reference server: only its auth middleware was spot-checked (`Sources/InlayServerKit/InlayHTTPServer.swift:138`); its storage/import paths were not re-audited. If it is ever deployed in place of the TypeScript server (same binary name, per `docs/audits/major-issues-2026-10-10/00-inventory.md`), findings 1/2/6/7/8 need re-verification there.
- Whether the History UI offers any bulk-delete/"purge audio" affordance — only the server API surface and `HistoryPage.swift` rendering were inspected; no client-wide search for a purge flow was done.
- Dependency vulnerability state (Bun 1.4.2, Fastify, vendored whisper.cpp/llama.cpp parsing of model files): no CVE/advisory sweep was performed; helpers parse only operator-supplied pinned models, but the HTTP/WAV/import parsers' robustness against a malicious _authenticated_ client was only reviewed at the validation layer, not fuzzed or traced into native code.
- Real-world operator configurations: actual token-file modes, reverse-proxy setups, tailnet ACLs, and backup tooling on deployed installs are outside the repo and were not inspected.
- The `archiveWisprFlowDictionary` overwrite semantics for `dictionary.json` beyond the write site (`Server/src/imports.ts:868`): version-retention count and growth bounds of `dictionary-versions/` were not fully traced.
