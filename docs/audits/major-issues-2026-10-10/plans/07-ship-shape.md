# Batch 7 — Ship shape (P1-21, P1-22, P1-14 full guided install)

- Branch: `stack/07-ship-shape`, stacked on the previous batch branch, base `stack/00-audit-plan` on `audit/major-issues`. Slice 1 (CI packaging) is independent of the stack and may be cherry-picked to `main` first.
- Audit sources: `REPORT.md` (P1-14, P1-21, P1-22), `09-build-release-devex.md` F1–F4/F8–F10, `01-ux-flows.md` F2, `11-crosscheck.md:45/53–55`. Every `file:line` below was opened at HEAD `b6d6815`.
- `DECISIONS.md` overrides `REPORT.md` where they differ. This plan edits no product code; it is the implementation contract for the branch.

## Goal

- A non-developer can download one notarized DMG, open Inlay, and reach first dictation without a shell: the app installs its bundled local server as a per-user LaunchAgent, downloads and verifies the pinned ~3.5 GB of models, generates the local credential, starts the server, and reports readiness from the existing health poll.
- Every shipped binary identifies its own build: the client from its `v*` tag, the server from its `server-v*` tag, the API integer remaining the only compatibility contract.
- Releases are permanent GitHub Releases with checksums; expiring Actions artifacts remain only for PR/manual CI builds.
- Packaging breakage is caught in CI before release day: `build-app.sh` + `codesign --verify` runs on every push/PR. This slice lands first.
- Shell users keep a supported path: one `setup.sh` replaces README curl loops, `install-service.sh` gives macOS the launchd story Linux already has, and `migrate.sh` performs the manual upgrade checklist.

## Decisions applied

- **Audience: shareable product.** P1-14, P1-21, P1-22 are real P1s; signing/notarization/setup work is funded (`DECISIONS.md` "Audience").
- **First run: guided install.** "The app installs/starts the server and downloads models itself." Batch 2's S slice (first-run routing + token-hint copy, `REPORT.md` Batch 2) lands earlier in this stack; this batch extends that page, it does not add a second onboarding surface.
- **Versions independent per component, API integer is the contract; real GitHub Releases** (`DECISIONS.md` defaults). This overrides `09` F3's single-version reading: there is no unified Inlay version. Client tags stay `v*`, server tags stay `server-v*` (`.github/workflows/server-release.yml:6`); each build stamps only its own component from its own tag, and compatibility stays `API_VERSION = 2` (`Server/src/api.ts:71`, `x-inlay-apiVersion: 2` at `Server/api/openapi.yaml:10`, client gate `isServerReady` in `Sources/Inlay/InlayController.swift:151`).
- **Auth: per-device token** (Batch 5, earlier in the stack). Guided install mints the local credential through Batch 5's mechanism and stores it via the existing `ClientPreferencesStore.save(endpoint:token:deviceName:)` (`Sources/Inlay/ClientPreferences.swift:47`, Keychain write at `:66`). This batch must not invent a second token format; if Batch 5's mint API is not yet in the stack when Slice 8 starts, fall back to the existing 32-byte hex token-file pattern (`Server/scripts/dev.ts:21`, mode `0600`) behind one function so Batch 5 can replace its internals.
- **Remote transport: Tailscale only, no built-in TLS.** The app-managed server binds `127.0.0.1` only. Remote servers remain the shell/systemd path in `Server/README.md:144`; guided install never offers a LAN bind.
- **Swift reference server: deleted** (Batch 8). Nothing in this batch packages or signs `Sources/InlayServer` / the Swift `inlay-server` product (`Package.swift:6`); the bundled server payload is exclusively the Bun/TypeScript output of `scripts/build-server.sh`.
- **Checks: parity checks allowed in `bun run check`; behavioral suites banned** (`AGENTS.md:4`). This batch adds no parity check of its own (model-pin generation is Batch 8); new shell downloaders copy the pins and are listed as generation debt there.
- **Layout: flat + dividers** (Batch 6 scaffold). The guided-install UI is a flat section in the existing This Mac page, no wizard window, no cards.
- Chosen defaults where the audits leave a fork (not open questions; change on request):
  - **Bundle the server in the app, download only the models.** The darwin-arm64 server payload (`build/server/` from `scripts/build-server.sh`: `inlay-server`, `helpers/`, `resources/`) ships inside `Inlay.app/Contents/Resources/Server/`. The alternative — downloading binaries on first run — needs a second trust root (release checksums), quarantine handling for downloaded executables, and an update channel for the server alone. Bundling keeps one signature covering client + server + helpers, works offline after the DMG, and makes client/server versions move together for the same-Mac case, which is the guided-install audience. Cost: larger DMG; accepted.
  - **App-written LaunchAgent plist + `launchctl`, not `SMAppService.agent`.** `SMAppService.mainApp` is the existing login-item pattern (`Sources/Inlay/InlayController.swift:1357`, register/unregister at `:1367`), but an agent plist baked into the bundle cannot carry per-user absolute paths for data dir, token file, and models (launchd does not expand `~` in `ProgramArguments`). The app therefore renders a plist with absolute paths into `~/Library/LaunchAgents/` and drives `launchctl bootstrap/bootout/kickstart` via `Process`, returning `Result` (errors-as-values, matching `TextModel.verify`/`ModelIntegrity.verify`). The shell `install-service.sh` writes the same plist shape.
  - **Launch label is `dev.davis.inlay.server-agent`** (Dev: `dev.davis.inlay.dev.server-agent`), derived as `InlayBuild.current.bundleIdentifier + ".server-agent"`. `bundleIdentifier + ".server"` is already the Keychain credential service (`Sources/InlayCore/InlayBuild.swift:16`) and must not be reused as a launchd label.
  - **Update story is a manual Releases link**, shown next to the version row that already exists (`Sources/Inlay/Views/PreferencesPage.swift:99`). Sparkle/auto-update is OUT (below).
  - **Model downloads restart per file, never per byte-range.** Completed, verified files are kept (Qwen is six files, so at most one file redownloads); no resume-data persistence in v1.

## IN scope commit slices

Ordered. Each slice is one commit (Slice 9 is three commits, 9a–9c). Sizes: S ≈ ≤1 file cluster / under an hour of review, M ≈ a few files, L ≈ new subsystem.

### Slice 1 (DO FIRST, stack-independent) — CI exercises client packaging — S

- Files + symbols:
  - `.github/workflows/server.yml:30` (`swift` job, `swift build` only at `:49`) — add a sibling job `client-package` on `macos-26` (same runner as `:32`; do not use the `macos-15` coordinator runner at `:58`).
  - `scripts/build-app.sh` — no change in this slice; CI runs it as-is. Steps it must exercise: Swift release build (`:28`), bundle assembly (`:36`–`:40`), icon generation (`:41`–`:42`), identity fallback to ad-hoc (`:44`–`:48`, correct in CI where no identity exists), `codesign` (`:50`), `codesign --verify --deep --strict` (`:52`), bundle-ID overwrite guard (`:53`–`:58`).
- Change: job steps are `actions/checkout` (same pinned SHA as `:21`), `./scripts/build-app.sh`, then explicit `codesign --verify --deep --strict "build/Inlay.app"` (re-verify outside the script so a future script edit cannot silently drop `:52`), then a `PlistBuddy` sanity read of `CFBundleIdentifier` (expect `dev.davis.inlay`) and `CFBundleShortVersionString` (non-empty) from the built app, then `actions/upload-artifact` (same pin as `:71`) of `build/Inlay.app`. No Bun, no Metal toolchain: the client build needs neither.
- Why: P1-22 / `09` F2 — today green CI says nothing about whether an `.app` can be built; packaging rot is found by users. `AGENTS.md:9` explicitly allows build checks. Landing it first protects every later slice in this batch.
- Size: S.

### Slice 2 — De-hardcode the personal dev host — S

- Files + symbols:
  - `Server/scripts/dev.ts:50` — `hostname() === "siva" ? "siva.otter-hawksbill.ts.net" : hostname()`; printed at `:51`.
  - `Server/README.md:178` ("Development and verification" section) and `docs/development.md:17` — mention the new override once.
- Change: `displayHost = process.env.INLAY_DEV_DISPLAY_HOST ?? "localhost"`. Drop the `hostname` import use if it becomes unused (`Server/scripts/dev.ts:3`); keep `homedir`. The model-path defaults at `Server/scripts/dev.ts:34`–`:42` stay: they are the documented installed-model locations (`Server/README.md:142`), not personal data in the same sense, and `scripts/run-dev.sh:136` depends on the same convention. Removing them is OUT (below).
- Why: `09` F8 — the owner's tailnet name ships in source and every other developer gets a wrong "Development URL".
- Size: S.

### Slice 3 — Per-component version stamping (server + client) — M

- Files + symbols:
  - New `Server/src/version.ts` — `export const SERVER_VERSION = process.env.INLAY_SERVER_VERSION ?? "0.0.0-dev"`.
  - `Server/src/generation-service.ts:362` — `serverVersion: "0.1.0"` inside `GenerationService.health()` (starts `:337`) becomes `serverVersion: SERVER_VERSION`. No schema change: `serverVersion` is already a required string (`Server/api/openapi.yaml:679`).
  - `Server/scripts/standalone-build-settings.ts:13` — extend the existing `define` map with `"process.env.INLAY_SERVER_VERSION": JSON.stringify(process.env.INLAY_SERVER_VERSION ?? "0.0.0-dev")` so the compiled binary bakes the value (the same mechanism already used for `NODE_ENV`); `Server/scripts/build.ts:49` spreads these settings unchanged.
  - `scripts/build-server.sh` — before the `bun run --cwd Server build` call at `:83`, resolve `INLAY_SERVER_VERSION` if unset: `git describe --tags --match 'server-v*' --always` with the `server-v` prefix stripped; if git/tag lookup fails (tarball build), `0.0.0-dev`. Export it for the build step only.
  - `Sources/InlayAPI/API.swift:123` — the hand-written facade default `serverVersion: String = "0.1.0"` becomes `"0.0.0-dev"` (default only; the wire value always comes from the server).
  - `scripts/build-app.sh:38` — after `cp "$info_plist" "$staged_app/Contents/Info.plist"`, stamp the **staged copy only** when `INLAY_CLIENT_VERSION` is set: `PlistBuddy -c "Set :CFBundleShortVersionString $INLAY_CLIENT_VERSION"` and, when `INLAY_CLIENT_BUILD` is set, `Set :CFBundleVersion`. Source plists keep `0.14.1` / `28` (`Resources/Info.plist:15`–`:18`, `Resources/Info-Dev.plist:15`–`:18`) as the local-dev fallback and are never edited by the build.
  - `Sources/Inlay/Views/PreferencesPage.swift:176` — in `ServerPreferencesPage`'s existing "Server models" section, add one row showing `controller.serverHealth?.serverVersion` (health is already fetched; `serverHealth` is set in `checkServer` at `Sources/Inlay/InlayController.swift:292`). Client version display already exists at `PreferencesPage.swift:99`.
- Change summary: version strings are build-time inputs, never hand-edited in two places again. `README.md:81`'s ad-hoc/TCC warning is documentation, not a version site; it is updated in Slice 11.
- Why: P1-21 / `09` F3 — `/v1/health` currently reports `0.1.0` forever while the client is `0.14.1`; an operator facing the hard API-version rejection (`README.md:46`) has no build evidence. Per DECISIONS the versions stay independent; only the stamping is unified.
- Size: M.

### Slice 4 — Real GitHub Releases for the server — M

- Files + symbols:
  - `.github/workflows/server-release.yml:8` (`permissions: contents: read`), triggers at `:5`–`:6` (`server-v*` tags + `workflow_dispatch`), package matrix (`:19` darwin-arm64, `:22`/`:24` Linux CPU), checksum step (`:48`–`:52`), `upload-artifact` steps (`:53`, CUDA `:76`).
  - `Server/README.md:12` — "Download the artifact from the Actions run and unzip it" becomes a Releases permalink; `Server/README.md:106` updated in Slice 10/11.
- Change: keep `package` and `cuda-package` jobs byte-identical through their `upload-artifact` steps. Add a `publish` job: `needs: [package, cuda-package]`, `runs-on: ubuntu-24.04`, job-level `permissions: contents: write` (workflow-level stays `read`), `if: startsWith(github.ref, 'refs/tags/server-v')`. It downloads all four artifacts and runs `gh release create "$GITHUB_REF_NAME" --verify-tag --title "$GITHUB_REF_NAME" --notes "Server $GITHUB_REF_NAME"` (or `gh release upload … --clobber` if the release already exists, so a re-run is idempotent) attaching every `*.tar.gz` + `*.sha256`. `workflow_dispatch` runs keep today's artifact behavior and never publish. Set `INLAY_SERVER_VERSION` from the tag in the package jobs so Slice 3's stamping matches the release name (strip `server-v`).
- Why: P1-21 / `09` F3 — a tag today produces expiring artifacts, not a permalinkable release. DECISIONS: "real GitHub Releases instead of expiring artifacts."
- Credential seam: uses the built-in `GITHUB_TOKEN` only; no new secret. Repository Settings → Actions permissions must allow it — an owner toggle outside the checkout (see Open questions; it gates the first publish run, not this code).
- Size: M.

### Slice 5 — Verified text-model downloader script — S

- Files + symbols:
  - New `scripts/download-text-model.sh`, mirroring `scripts/download-model.sh` structure: verify-then-skip (`:16`), `verify_model` via `shasum -a 256` (`:10`–`:14`), `.download` temp + verify + atomic `mv` (`:22`–`:30`), `INLAY_MODEL_DIR` override (`:4`).
  - Pins copied verbatim from `Sources/InlayCore/TextModel.swift:45` (`TextModel.qwen`: base URL at `:46`, six `(filename, byteCount, sha256)` entries at `:47`–`:54`, directory name `Qwen3-4B-Instruct-2507-MLX-4bit` at `:57`).
  - `Server/README.md:58`–`:68` — replace the copy-paste curl loop with one script invocation; keep the exact-six-files warning at `Server/README.md:70`.
  - `Server/README.md:74`–`:81` (Linux GGUF) — same script gains a `--linux` mode (or platform detect via `uname -s`, matching `scripts/build-server.sh:7`) downloading the single GGUF and verifying size + SHA-256 from `Server/src/inference/model-pins.ts:7` (`linuxProofModelPin`, already restated as prose at `Server/README.md:81`).
- Change: per-file verify-then-skip; after download, list the directory and fail naming any extra/missing file (the MLX helper rejects extra files at load — `TextModel.verify` set-equality at `TextModel.swift:71` — so the downloader fails at download time instead, per `09` F4). `curl --fail --location --retry 3`, same flags as `download-model.sh:22`.
- Why: `09` F4 — Qwen setup is the only model with no installer; a wrong file set surfaces later as opaque `ready: false`. This script is also the behavioral spec the Swift downloader (Slice 9a) must match.
- Debt note: the six hashes are now in two more homes (script + Swift, joining `model-pins.ts:4` and `download-model.sh:6`). Batch 8 generates all pin sites from one manifest; this slice adds its site to that list rather than blocking on the generator.
- Size: S.

### Slice 6 — `setup.sh`: one front door for build-from-source — M

- Files + symbols:
  - New `scripts/setup.sh`.
  - Reuses, does not reimplement: `scripts/download-model.sh`, `scripts/download-text-model.sh` (Slice 5), `scripts/build-server.sh` (which already auto-inits submodules at `:29`–`:31` and downloads VAD at `:69`), Metal check pattern from `scripts/build-text-engine.sh:9`–`:12`, Swift-floor check pattern from `.github/workflows/server.yml:41`–`:45`.
  - `README.md:9` (prerequisites), `README.md:16`–`:22` (model + `run-dev.sh` flow), `Server/README.md:85` (build prerequisites) — point at the script instead of restating steps.
- Change: fail-fast preflight in order, each naming the fix: `uname -s`/`uname -m` supported (mirror `build-server.sh:9`–`:17` and the Apple-Silicon requirement at `:36`–`:39`); `bun --version` == `1.4.2` (pinned at `package.json:4`, `setup-bun` at `server.yml:24`); `cmake`, `git` on PATH; on macOS, Swift ≥ 6.2 and `xcrun metal --version`. Then: `git submodule update --init --recursive`, `bun install --frozen-lockfile`, `./scripts/download-model.sh`, `./scripts/download-text-model.sh`, `./scripts/build-server.sh`. Final output prints the two supported next steps verbatim: `./scripts/run-dev.sh` (dev pair) or `./scripts/install-service.sh` (Slice 7). It never writes outside the repo, `INLAY_MODEL_DIR`, and the default model dir; it never starts a server.
- Why: `09` F4 — time-to-first-dictation on a clean machine is hours of README archaeology across three files; there is no doctor that checks the chain up front.
- Size: M.

### Slice 7 — macOS launchd story + `migrate.sh` (shell path) — M

- Files + symbols:
  - New `Server/dev.davis.inlay.server-agent.plist` — template mirroring `Server/inlay-server.service` (the Linux unit: `ExecStart` + `Environment=` lines for host/port/data/token/helpers/models, `Restart=on-failure`, hardening). Plist keys: `Label` = `dev.davis.inlay.server-agent`; `ProgramArguments` = absolute server path plus the six required arguments exactly as `Server/README.md:117`–`:125` and `Server/src/configuration.ts:20` (`--host 127.0.0.1 --port 8391 --data-dir --speech-helper --speech-model --vad-model --proof-helper --proof-model --token-file`); `RunAtLoad` true; `KeepAlive` true; `ThrottleInterval` 10; `StandardOutPath`/`StandardErrorPath` under `~/Library/Logs/Inlay/`; `WorkingDirectory` = the server package dir. Placeholders (`__SERVER_DIR__`, `__DATA_DIR__`, `__MODELS_DIR__`, `__TOKEN_FILE__`, `__LOG_DIR__`) are substituted by the installer, never by hand.
  - New `scripts/install-service.sh` — renders the template into `~/Library/LaunchAgents/dev.davis.inlay.server-agent.plist` (0600 dirs where needed), creates data/log dirs, `launchctl bootout` any previous instance of the label, `launchctl bootstrap gui/$(id -u)`, then waits for `curl http://127.0.0.1:8391/v1/health` with the same bounded loop shape as `scripts/run-dev.sh:155`–`:167`. `--uninstall` boots out and removes the plist; data and models are kept. Defaults target an extracted release package (`build/server` locally); every path is overridable by the same `INLAY_*` env names `Server/README.md:133` already documents. It refuses to bootstrap while the dev server holds the port (reuse the PID-command verification idea from `run-dev.sh:42`–`:53` in prose-level form: if health already answers, say so and stop).
  - `scripts/build-server.sh:114` — the Linux-only `cp Server/inlay-server.service` gains a Darwin twin: copy the plist template into the package root so a downloaded Mac tarball carries its service definition.
  - `Server/README.md:129` — "the scripts do not install a service" is replaced by the `install-service.sh` flow + log location.
  - New `scripts/migrate.sh` — performs `README.md:43`–`:50` mechanically: refuses to run while `Inlay`/`Inlay Dev` processes are alive (both clients must be quit, per `README.md:47`); copies `config.json`/`client.json` from the legacy client dir to `~/Library/Application Support/Inlay` (and `Inlay Dev`) with timestamped backups, never overwriting a newer destination; verifies the chosen `--data-dir` (or `INLAY_SERVER_DATA_DIR`) is non-empty when a legacy archive exists, refusing to bless an empty archive (the lock at `Server/src/data-lock.ts` via `Server/src/main.ts` `acquireDataDirectoryLock` prevents corruption, not wrong-directory starts); runs `scripts/download-model.sh` for the Whisper→Parakeet swap (`README.md:50`). Steps it cannot automate — token re-entry (new Keychain services, `README.md:48`), TCC re-grants, launch-at-login — are printed as a checklist at the end. It deletes nothing.
- Why: `09` F9 (no macOS service story; the primary platform is `nohup` from `run-dev.sh:152` or nothing) and `09` F10 (six-step manual upgrade with data-loss-shaped failure modes). The plist shape decided here is reused verbatim by the app in Slice 9b.
- Size: M.

### Slice 8 — Bundle the server payload in the app build — M

- Files + symbols:
  - `scripts/build-app.sh` — argument parsing at `:10`–`:20` gains `--with-server` (combinable with `--dev`); staging at `:36`–`:40` gains `ditto build/server "$staged_app/Contents/Resources/Server"` after asserting `build/server/inlay-server`, `build/server/helpers/inlay-engine`, `build/server/helpers/inlay-text-engine`, `build/server/helpers/mlx.metallib`, `build/server/resources/silero-vad.bin` exist (fail with "run ./scripts/build-server.sh first", mirroring `run-dev.sh:126`–`:129`). Signing order becomes inside-out: bundled helpers and `inlay-server` first (Slice 10 owns the identity rules; this slice signs them with the same `$signing_identity` at `:50` and `Server/entitlements.plist` for the Bun binary, replacing the ad-hoc re-sign `build-server.sh:92`–`:95` already applied), then the app bundle with `Resources/Inlay.entitlements`, then the existing `codesign --verify --deep --strict` at `:52`.
  - `scripts/build-server.sh:92`–`:95` — parameterize the ad-hoc sign as `INLAY_SIGNING_IDENTITY` with `-` default, so a dist build signs helpers once with the right identity instead of sign-then-resign; `--timestamp` only when the identity is not `-` (ad-hoc + timestamp is invalid).
  - `.github/workflows/server.yml` Slice 1 job — in this slice, extend it to also run `./scripts/build-server.sh` (with the Metal step from `server-release.yml:37`) then `./scripts/build-app.sh --with-server`, so the bundled layout is CI-covered from the day it exists. This makes the job slower; that is the point of P1-22.
- Change: `--with-server` is opt-in so the fast client-only dev loop (`scripts/build-dev-app.sh`, which execs `build-app.sh --dev`) is unchanged. Release builds (Slice 10) always pass it.
- Why: guided install needs a server to install. Bundling (chosen default above) puts it under the app signature; `09` F1's "build the client and server and two helpers from source" becomes one download.
- Size: M.

### Slice 9a — Verified model downloads in the app — M

- Files + symbols:
  - New `Sources/Inlay/LocalServer/ModelDownloadManager.swift` (SwiftPM's `Inlay` executable target globs `Sources/Inlay/**`; no `Package.swift` edit). An `@MainActor ObservableObject` in the style of `ClientPreferencesStore` (`Sources/Inlay/ClientPreferences.swift:10`).
  - Reuses, never forks: `SpeechModel.parakeet` (`Sources/InlayCore/SpeechModel.swift:22`, `downloadURL` at `:28`, `byteCount`/`sha256` at `:26`–`:27`), `ModelIntegrity.verify(_:model:)` (`SpeechModel.swift:49`, returns `Result<Void, ModelIntegrityError>`), `TextModel.qwen` and its six `File` entries (`Sources/InlayCore/TextModel.swift:45`–`:61`, per-file `downloadURL`), `TextModel.verify(_:)` (`TextModel.swift:63`, exact-set + per-file digest, also `Result`-typed).
  - `Sources/Inlay/AppState.swift:14` — wire the dead `ModelStatus` (`missing/downloading/verifying/installed/failed`) as the manager's per-model status; publish `downloadFraction: Double` and `currentFile: String` beside it instead of growing associated values. Delete the equally dead `EngineStatus` (`AppState.swift:18`) in this commit — zero call sites, confirmed by `01` F2 / `11-crosscheck.md:45`.
- Change: installs into `InlayBuild.current.dataDirectory/Models/` (`InlayBuild.swift:18` → `~/Library/Application Support/Inlay/Models`, Dev gets `Inlay Dev`). Flow per model: `verify` first — `.success` means `installed`, skip network entirely. Otherwise preflight free space ≥ `parakeet.byteCount + qwen.byteCount` + 1 GiB headroom (volume capacity via `URLResourceValues`), failing with required/available numbers. Each file: `URLSession` download to `<filename>.download` in the target dir (Qwen files into a staging sibling dir `Qwen3-4B-Instruct-2507-MLX-4bit.download/` so a partial set can never be seen by `TextModel.verify`'s exact-set check), check byte count, then SHA-256 via the existing verify path, then atomic rename. Parakeet is one file; Qwen promotes the staging dir by rename only when all six verify. Failures keep completed verified files and delete only the failed `.download`; cancel keeps completed files for next time. All errors surface as `ModelIntegrityError`/localized strings — no `try!`, no force unwrap, no `as any` anywhere in this batch (TypeScript slices: `oxlint` bans explicit `any` already, `package.json` lint script).
- Why: P1-14 full half — ~4 GB of models are terminal-only today (`Server/README.md:42`, `:58`); `ModelStatus` existing but dead proves the UI was planned and never landed.
- Size: M.

### Slice 9b — App-managed local server (install, LaunchAgent, lifecycle) — L

- Files + symbols:
  - New `Sources/Inlay/LocalServer/LocalServerManager.swift` — `@MainActor ObservableObject`, owned by `InlayController` as a child object whose `objectWillChange` is forwarded exactly like `microphones`/`preferences` at `Sources/Inlay/InlayController.swift:239`–`:240` (the `sink` forwarding in `init`).
  - New `Sources/Inlay/LocalServer/LocalServerPaths.swift` — pure path math off `InlayBuild.current.dataDirectory` (see "API/data changes" for the full layout).
  - Existing seams it calls: `ClientPreferencesStore.save(...)` (`ClientPreferences.swift:47`) to persist endpoint + token after minting; `InlayController.checkServer` health poll (`InlayController.swift:290`–`:294`, 5 s `monitorTask`) as the sole readiness authority — the manager never declares "ready", it only starts the agent and lets `isServerReady` (`:151`) speak; `ServerEndpoint` validation (`Sources/Inlay/ServerEndpoint.swift:25`) already permits loopback HTTP (`:44`–`:49`).
- Change, in order:
  1. **Detect, never clobber.** If `preferences.endpoint` is non-loopback, or health already answers with a matching `apiVersion`, guided install is inert and the UI says an existing server is in use (covers `run-dev.sh`, hand-run, and remote servers — the owner's current setup keeps working untouched). Guided install is offered only when the endpoint is the loopback default (`ClientPreferences.swift:30`, `http://127.0.0.1:8391`) and health fails, or a marker from a previous guided install exists.
  2. **Install payload.** Copy `Bundle.main.resourceURL/Server/` (Slice 8) to `<dataDirectory>/ServerApp/` via a staging dir + rename swap; write marker `local-server.json` `{installedClientVersion, installedAt}`. Re-copy only when the marker's client version differs from `CFBundleShortVersionString`, and only after booting the agent out — a running server holds its data-dir lock (`Server/src/data-lock.ts`, acquired in `startServer`), and replacing binaries under a live process is never attempted.
  3. **Mint credential.** Create `<dataDirectory>/server-token` (0600, parent 0700 — same modes as `ClientPreferencesStore.persist` at `ClientPreferences.swift:84`–`:90`) via Batch 5's per-device mint (fallback: 32-byte hex, `Server/scripts/dev.ts:21` pattern), then `preferences.save(endpoint: "http://127.0.0.1:8391", token: …, deviceName: preferences.deviceName)`. Never log or display the token; the existing `SecureField` (`PreferencesPage.swift:27`) remains the only place it can be viewed/edited.
  4. **Ensure models** via Slice 9a's manager (verify-first; download only what's missing).
  5. **Render + bootstrap agent.** Write `~/Library/LaunchAgents/<label>.plist` with the Slice 7 shape and absolute paths (`ProgramArguments` mirroring `configuration.ts:114`–`:120`'s five path options plus `--token-file`), `launchctl bootstrap gui/<uid> <plist>`, then poll health until `ready` or a 60 s budget expires naming the failing step and the log path. `Process` calls return `Result<String, LocalServerError>` with captured stderr — errors-as-values throughout.
  6. **Stop / restart / remove.** Stop = `launchctl bootout`; restart = `launchctl kickstart -k`; remove = bootout + delete plist + delete `ServerApp/` + delete marker. `ServerData/` (history) and `Models/` are kept on remove; deleting them is a separate, explicit future action, not this batch.
  7. **Port conflict.** If `8391` answers but is not Inlay (health shape mismatch) or is occupied without answering, fail with that fact named. No port hunting in v1.
- Dev isolation: Dev builds use label `dev.davis.inlay.dev.server-agent`, port stays 8391 only if free — the existing dev flows (`bun run dev` on 8392, `run-dev.sh` on 8391) are never adopted or killed by the manager; the detect step makes them "existing server" cases.
- Why: P1-14's remaining half and `09` F9's product form — the same-Mac server, the primary platform (`README.md:7`), currently does not survive reboot unless the user hand-writes this plist with six arguments.
- Size: L.

### Slice 9c — Guided-install UI + first-run routing — M

- Files + symbols:
  - `Sources/Inlay/Views/PreferencesPage.swift:13` (`DevicePreferencesForm`, the This Mac page) — add one flat "Local server" section above the existing Connection section (`:23`–`:41`), visible only for loopback endpoints. Contents, driven by `LocalServerManager` state: not installed → one line ("Inlay can install its dictation server on this Mac…", required disk size from the model byte counts) + Install button; working → current step text (Installing server / Downloading Parakeet… 42% / Verifying / Starting server) + Cancel; installed → status line (Running/Stopped from health, not from the manager), Start/Stop/Restart, Remove server, Reveal log; failed → the named failed step + Retry, reusing `InlayActionMessage` (`PreferencesPage.swift:40`). No headings beyond the section header, no explanatory sub-bullets per button (house style: less is more); controls sit in fixed-height rows so state changes cause no layout shift.
  - `Sources/Inlay/Views/InlayWindowView.swift:25` — `page` default `.dictation` becomes computed at init: `.device` when (endpoint is the loopback default AND no local install marker AND `serverHealth == nil`), else `.dictation`. Synchronous file checks only; no network on the launch path. Batch 2's routing/copy slice lands first in the stack — extend its condition, do not duplicate its token-hint text.
  - `Sources/Inlay/InlayApp.swift:56`–`:71` (`finishLaunching`, first-launch `showWindow` condition at `:67`) — unchanged; the window already opens on first launch, it just lands on the right page now.
  - `Sources/Inlay/Views/PreferencesPage.swift:103` — "Quitting this app leaves your server running" gains a managed-server variant ("…your local server keeps running; stop it above") chosen by manager state.
- Change: after Install completes and health reports ready, the section collapses to the installed status rows and the user grants Microphone/Accessibility in the existing Permissions section (`PreferencesPage.swift:80`–`:90`) — first dictation needs no further setup.
- Why: `01` F2 — first launch lands on Dictation showing "Server offline" with no path forward. This makes the dead end the front door.
- Size: M.

### Slice 10 — Developer ID signing, notarization, DMG (credential seams) — M code, credential-gated run

- Implementable without Ben's credentials (all of the code):
  - `scripts/build-app.sh:44`–`:52` — add `--dist` flag. With `--dist`: require `INLAY_SIGNING_IDENTITY` to match `Developer ID Application:*` (fail closed with instructions; never fall back), sign inside-out (helpers → `inlay-server` with `Server/entitlements.plist` → app with `Resources/Inlay.entitlements`) with `--options runtime --timestamp`, `codesign --verify --deep --strict`. Without `--dist`: today's behavior, plus a loud stderr warning when the identity resolves to `-` that TCC grants will not survive (productizing `README.md:81`).
  - New `scripts/make-dmg.sh` — `hdiutil create -volname Inlay -srcfolder <staging with .app + /Applications symlink> -ov -format UDZO`, `codesign` the DMG, and when `INLAY_NOTARY_PROFILE` is set: `xcrun notarytool submit --keychain-profile "$INLAY_NOTARY_PROFILE" --wait`, `xcrun stapler staple`, `spctl -a -t open --context context:primary-signature`. When the profile is unset it stops after a signed-but-unnotarized DMG and says so; `--require-notarized` makes a missing profile an error. It never prints success it did not perform.
  - New `.github/workflows/client-release.yml` — on `v*` tags + `workflow_dispatch`, `macos-26`: checkout with submodules, Metal step (from `server-release.yml:37`), `bun install --frozen-lockfile`, `INLAY_SERVER_VERSION` from the latest `server-v*` tag (or `0.0.0-dev`), `./scripts/build-server.sh`, `INLAY_CLIENT_VERSION` from the tag (strip `v`), `INLAY_CLIENT_BUILD=$GITHUB_RUN_NUMBER`, `./scripts/build-app.sh --with-server --dist`, `./scripts/make-dmg.sh --require-notarized`, `shasum -a 256`, then a `publish` step mirroring Slice 4 (`gh release create/upload`, job-level `contents: write`, tags only). Secrets consumed, documented in the workflow header comment: `MACOS_CERTIFICATE_P12_BASE64`, `MACOS_CERTIFICATE_PASSWORD`, `KEYCHAIN_PASSWORD`, `NOTARY_APPLE_ID`, `NOTARY_TEAM_ID`, `NOTARY_APP_PASSWORD` (imported into a throwaway keychain; `notarytool store-credentials` creates the profile `INLAY_NOTARY_PROFILE` points at). If the certificate secret is absent, the workflow builds an ad-hoc DMG as an artifact, skips publish, and warns — an unsigned build is never published as a Release.
  - `Server/README.md:106` — "draft Mac build is ad-hoc signed" updated to describe when the Mac tarball is Developer ID signed (release workflow with secrets) vs ad-hoc (local/PR).
- Credential-gated (documented seams, never faked): enrolling in the Apple Developer Program, creating the Developer ID Application certificate, generating the app-specific password / API key, adding the six CI secrets, and the first real `notarytool --wait` turnaround. Until Ben does these, `--dist` exits with the missing-credential message and CI publishes nothing for the client. No step stubs, mocks, or simulates notarization.
- Why: P1-21 / `09` F1 — an Apple Development identity cannot be notarized and ad-hoc rebuilds break TCC grants; there is no DMG or update path at all today.
- Size: M (code). First successful run also needs the Risks items below (Bun JIT entitlements, metallib) cleared with Apple's notary log in hand; budget one debug iteration there, it cannot be done from the checkout.

### Slice 11 — Docs: install, upgrade, release runbook — S

- Files + symbols:
  - `README.md:5` — "run build-app.sh and install" becomes: download DMG from Releases → open → Install button (guided) ; build-from-source moves below as `./scripts/setup.sh` + `./scripts/run-dev.sh`.
  - `README.md:43`–`:50` — the manual upgrade checklist shrinks to "run `scripts/migrate.sh`, then re-enter token / re-grant permissions"; `README.md:46`'s update-together rule stays, now naming `v*`/`server-v*` compatibility via API version.
  - `README.md:81` — ad-hoc permission warning kept, scoped to source builds; downloaded builds note stable identity.
  - `Server/README.md:12` (artifact prose), `:42` ("does not download large weights automatically" gains "…the Mac app's guided install does"), `:129` (install-service), new short "Releasing" section: tag conventions (`vX.Y.Z`, `server-vX.Y.Z`), which workflow fires, secret names, how to verify a DMG (`codesign --verify`, `spctl`, checksum), and the never-publish-unsigned rule.
  - `docs/development.md` — personal-host override note (Slice 2) and a pointer that guided-install verification uses Inlay Dev with an isolated `INLAY_CLIENT_DATA_DIR` (`:24`–`:30` pattern already documented).
- Why: `09` F4/F10 — the docs currently are the installer; after this batch they must describe one instead.
- Size: S.

## API/data changes + migration

- **HTTP API: no change.** No OpenAPI edit, no regenerated bindings, no `apiVersion` bump. `serverVersion` keeps its existing required-string wire shape (`Server/api/openapi.yaml:679`); only its value stops being the constant `"0.1.0"` (`Server/src/generation-service.ts:362`). Old clients ignore the value; new clients only display it. `bun run generate:api --check` is still run in Verification because Swift changed, and must pass unmodified.
- **Server data/archive format: no change.** Guided install points `--data-dir` at a new directory; `GenerationService.open` sees the same on-disk records it does today. The archive lock (`Server/src/data-lock.ts`) still guarantees one owner.
- **New on-disk state (client side, all under `InlayBuild.current.dataDirectory`, 0700/0600 as in `ClientPreferences.swift:84`–`:90`):**
  - `ServerApp/` — copied bundled payload (`inlay-server`, `helpers/`, `resources/`), replaceable wholesale; no user data lives here.
  - `ServerData/` — the managed server's archive (`--data-dir`). This is the user's history; it survives app updates, server removal, and uninstall of the payload.
  - `Models/` — `ggml-parakeet-tdt-0.6b-v3-f16.bin` + `Qwen3-4B-Instruct-2507-MLX-4bit/` (exact names from `SpeechModel.swift:25`, `TextModel.swift:57`).
  - `server-token` (0600) and `local-server.json` marker `{installedClientVersion: String, installedAt: String}` (ISO-8601; forward-compatible: unknown keys ignored, missing file = not installed).
  - `~/Library/LaunchAgents/dev.davis.inlay.server-agent.plist` and `~/Library/Logs/Inlay/server.log` (+ `.err.log` if split). Dev builds substitute the `.dev` label and `Inlay Dev` roots.
  - Existing client files are untouched in shape: `client.json` via `ClientPreferencesStore.persist` (`ClientPreferences.swift:88`), `config.json` via `ConfigurationStore` — guided install changes their values through existing save paths only.
- **Migration notes:**
  - Existing manual installs (`.local/server`, systemd, custom `--data-dir`, `run-dev.sh`) are detected as "existing server" and left completely alone; adopting one into guided management is OUT (below). `scripts/migrate.sh` covers the documented rename-era upgrade (`README.md:43`–`:50`) and nothing more.
  - App update with a managed server: on next launch, marker version ≠ bundle version triggers Slice 9b's bootout → payload swap → bootstrap sequence; `ServerData/` and `Models/` are not touched, so history and downloads survive. If the swap fails midway, the previous payload staging rename is atomic at the directory level and the UI offers Retry; the server is simply stopped until then, never half-copied while running.
  - Model pin bumps (future): changed `SpeechModel`/`TextModel` pins make `verify` fail on the old files, and Slice 9a re-downloads automatically on next guided check. No migration code needed; state this in `Server/README.md`'s Releasing section.
  - Rollback: every slice is independently revertible. Slices 1–7 touch no shipped behavior. Reverting 9b/9c leaves the LaunchAgent + `ServerData/` in place and the app behaving as today's bring-your-own-server client pointed at a working loopback server.

## OUT of scope / deferred

- **Sparkle / auto-update, in-app version checks beyond the Releases link.** Needs an appcast host and update-signing keys; revisit after the first notarized release exists.
- **App Store / sandboxing.** The event-tap + Accessibility design is unsandboxed by construction (`Resources/Inlay.entitlements` carries only audio-input); DECISIONS record direct distribution.
- **Guided install for remote/Linux servers.** The app manages only a same-Mac loopback server. Linux keeps tarball + systemd (`Server/README.md:30`–`:36`); a Mac client connecting remotely keeps manual endpoint/token entry.
- **Adopting an existing manual server into app management** (importing its data dir/token into the LaunchAgent). Detection says "existing server"; adoption is a follow-up once the greenfield path is proven.
- **Downloading server binaries from Releases instead of bundling** — rejected in favor of bundling (Decisions); revisiting it means a checksum trust root and quarantine handling designed from scratch.
- **Model management beyond the two pinned models**: no picker, no delete-models UI, no delta updates, no byte-range resume (v1 restarts per file).
- **Generating model pins from one manifest** — Batch 8. Slices 5/9a consume the pins where they already live.
- **CI speed/cache work** (`09` F7: Bun/SwiftPM/CMake caches, moving native packages off the PR path). Slice 8 knowingly makes the packaging job slower; caching is a separate follow-up, not a blocker.
- **Swift 6 language mode, `swift-tools-version` alignment** (`Package.swift:1` vs the 6.2 compiler floor, `09` F4) — Batch 8. This batch's Swift compiles under the current Swift 5 mode.
- **Changing dev-server model-path defaults** (`Server/scripts/dev.ts:34`–`:42`, `scripts/run-dev.sh:136`–`:137`) and the Swift reference server's fate (Batch 8 deletes it; CI keeps compiling it until then).
- **Windows/Intel Mac anything.** Apple Silicon + macOS 14+ is the floor (`Package.swift:29`, `Resources/Info.plist:21`).
- **Homebrew cask / `pkg` installer.** The DMG + drag-to-Applications is the distribution unit.

## Risks

- **Notarization may reject the bundled Bun binary's entitlements.** `Server/entitlements.plist` grants `allow-jit`, `allow-unsigned-executable-memory`, `disable-executable-page-protection`, `allow-dyld-environment-variables`, `disable-library-validation` — several are incompatible with hardened-runtime expectations and at least `disable-library-validation` + `allow-dyld-environment-variables` are candidates for trim-or-justify. This can only be settled with a real `notarytool` log (credential-gated). Mitigation path, in order: submit as-is and read the log; drop entitlements one at a time re-verifying the server still runs (Bun needs JIT; dyld env vars likely unneeded); if Bun cannot be notarized acceptably, the fallback is signing everything except keeping the server payload outside the app — a product decision for Ben at that point, not pre-built here.
- **MLX `mlx.metallib` and resource bundles inside `ServerApp/`** (`scripts/build-server.sh:87`–`:91`) must survive `codesign --deep` and the copy in Slice 9b without signature invalidation. Copy with `ditto` semantics and verify on the copied tree in owner testing; if deep-verify fails on bundles, sign each `.bundle` explicitly inside-out in Slice 8.
- **TCC identity churn during development of this batch.** Until Slice 10's Developer ID path is live with real credentials, every ad-hoc rebuild can reset Microphone/Accessibility grants (`README.md:81`). Owner verification should expect re-grants; the shipped identity ends this.
- **LaunchAgent vs the owner's running setup.** A bug in Slice 9b's detect step could try to manage a port the owner's `run-dev.sh` server already owns. The detect-first rule, Dev-label isolation, and "never stop a server we didn't start" (only `bootout` of our own label) are the guardrails; owner verification includes the existing-server case explicitly.
- **3.5 GB first-run download failures** (captive portals, dropped connections, full disk mid-download). Per-file verify + keep-completed bounds the loss to one file; the free-space preflight covers the common case but cannot reserve space.
- **DMG size and CI time grow.** Bundled helpers + metallib make the client download materially larger, and Slice 8 adds a full native build to the packaging job (Metal toolchain download, CMake whisper/llama builds). Accept for correctness; `09` F7 caching is the deferred remedy.
- **Batch 5 ordering.** Guided install's credential mint assumes Batch 5's per-device tokens have landed earlier in the stack. The single-function fallback (Decisions) keeps Slice 9b unblocked either way, but if Batch 5 changes token storage after 9b, one small reconciliation commit follows.
- **GitHub Release immutability.** Published assets can be replaced by maintainers; checksums are published alongside, but there is no additional signing of server tarballs in this batch. The DMG's notarization is the strong guarantee; Linux tarballs rely on GitHub account security, as today.

## Verification

Per `AGENTS.md:5`–`:9` and `docs/development.md:36`–`:42`. No automated behavioral tests are added or run. "Build checks are allowed" (`AGENTS.md:9`) covers `build-app.sh`/codesign/DMG assembly.

Commands (repo root of the worktree, after the final code edit of each slice; `bun run fmt` is mandatory after any TypeScript edit per `AGENTS.md:3`):

```sh
bun install --frozen-lockfile
bun run fmt            # Slices 2–4 touch TypeScript/YAML; run before responding
bun run fmt:check
bun run lint
bun run check
bun run generate:api --check          # Swift touched (Slices 3, 9); must pass with no API diff
swift build --force-resolved-versions # macOS, Swift 6.2+
bash -n scripts/setup.sh scripts/install-service.sh scripts/migrate.sh scripts/download-text-model.sh scripts/make-dmg.sh
./scripts/build-server.sh             # native helpers compile (Slice 8 prerequisite)
./scripts/build-app.sh --with-server
codesign --verify --deep --strict "build/Inlay.app"
/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "build/Inlay.app/Contents/Info.plist"
INLAY_CLIENT_VERSION=9.9.9 INLAY_CLIENT_BUILD=999 ./scripts/build-app.sh --with-server  # staged-plist stamping proof; re-read with PlistBuddy
./scripts/build-dev-app.sh            # Inlay Dev for computer-use verification
```

Inlay Dev manual steps for the owner (computer use, isolated dev world — never against the production server/data; keep existing servers and settings intact per `docs/development.md:8` and `AGENTS.md:8`):

1. Start from a clean isolated client: `INLAY_CLIENT_DATA_DIR` pointing at a fresh workspace-local dir, launch `build/Inlay Dev.app` (`docs/development.md:24`–`:30`). Confirm first launch opens the window on **This Mac** with the Local server section offering Install, and Dictation is not the landing page.
2. Click Install. Observe, in order: payload install, Parakeet download with live percent, Qwen six-file download, Verifying, Starting server, then `Server online` from the existing status row. No layout shift in the section as states change. Total new disk use ≈ 3.5 GB under `~/Library/Application Support/Inlay Dev/Models`.
3. Interrupt once: cancel mid-Qwen-download, relaunch the app, Install again — Parakeet and completed Qwen files are kept (verify-first), only the interrupted file restarts.
4. Grant Microphone + Accessibility to Inlay Dev, dictate into a separate editable app with the real models and confirm insertion; confirm the take appears in History. (`ready: false` or fake inference is not verification, `AGENTS.md:7`.)
5. Quit Inlay Dev; confirm dictation server still answers (health via browser on the dev instance only proves reachability). Log out/in (or reboot if convenient) and confirm the LaunchAgent restarted the server and the app reconnects with history intact.
6. In This Mac → Local server: Stop (health fails, status shows offline), Start (recovers), Restart, Reveal log (Finder shows `~/Library/Logs/Inlay/`). Then Remove server: agent gone (`launchctl list | grep server-agent` empty), but `ServerData/` and `Models/` still on disk; Install again and confirm history is still there.
7. Existing-server case: with the owner's normal server already running on 8391, launch the release-variant client pointed at it and confirm guided install stays inert ("existing server") and nothing is installed, stopped, or re-tokened.
8. Version proof: `curl http://localhost:8391/v1/health` shows the stamped `serverVersion` (not `0.1.0`); This Mac shows the client version from the tag-stamped plist; Server preferences → Server models shows the same server version.
9. Shell path on a spare checkout/user: `./scripts/setup.sh` from scratch (submodules absent) reaches the "next steps" output; `./scripts/install-service.sh` → health answers; `--uninstall` cleans the plist only. `./scripts/migrate.sh` against a copied legacy layout refuses while Inlay is running, then migrates with backups.
10. Credential-gated, only after Ben adds Developer ID + notary secrets: tag `v0.15.0`, confirm the client Release appears with DMG + `.sha256`; on a second Mac (or clean user account) with no Xcode/Bun, mount the DMG, confirm no Gatekeeper warning, drag to Applications, and repeat steps 1–4 end-to-end. Tag `server-v0.2.0` and confirm the server Release carries all four tarballs + checksums. Until this step runs, distribution is explicitly unverified — CI green and local ad-hoc success do not substitute (`REPORT.md` "What was not verified" discipline).

If a Mac, microphone, model assets, or signing credentials are unavailable at verification time, say which steps were not exercised rather than claiming them (`docs/development.md:41`).

## Open questions

Only items that block a specific slice's first run; everything above otherwise proceeds on the stated chosen defaults.

- **Developer ID + notarization credentials (blocks Slice 10's first real run and Verification step 10, nothing else).** Does Ben hold, or will he create, an Apple Developer Program membership with a Developer ID Application certificate and a notary app-specific password (or App Store Connect API key), and add the six CI secrets named in Slice 10? All Slice 10 code is written to fail closed with that exact shopping list until they exist.
- **GitHub repository settings (blocks Slice 4/10 first publish).** Actions `GITHUB_TOKEN` permissions and any release-protection rules live outside the checkout (`09` "What I could not verify"). The owner confirms Settings → Actions → Workflow permissions allows the job-level `contents: write` to create Releases on `davis7dotsh/inlay` (`README.md:12` remote).
