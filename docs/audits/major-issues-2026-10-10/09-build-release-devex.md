# Build / Release / DevEx / Docs — major issues (2026-10-10)

- Worktree: `.local/worktrees/major-issues-audit`, HEAD `b6d6815`. Scope: `README.md`, `docs/development.md`, `Server/README.md`, `scripts/`, `Server/scripts/`, CMake, `package.json`, `.github/`, signing/versioning/model setup. Every cited file body was opened and read; line numbers are against this HEAD. `vendor/`, `node_modules/`, `.build/`, `build/`, `.local/`, `.swiftpm/` excluded.
- Confidence key: **verified-in-code** = body read at the cited line; **inferred** = follows from read code/docs but not executed (no builds were run for this audit, and automated tests are prohibited by `AGENTS.md:4`).
- Top line: the server has real CI and release tarballs, but the Mac client — the thing every user touches — has no distributable release at all (Finding 1), and no CI ever exercises its packaging path (Finding 2). Everything else is friction that compounds those two.

---

## 1. No distributable Mac client: build-it-yourself, ad-hoc fallback, no notarization or auto-update

- **Severity:** P1 (P0 the moment anyone beyond the owner is meant to install Inlay)
- **Evidence:**
  - The only client install path is "run `./scripts/build-app.sh` and install `build/Inlay.app` in Applications" (`README.md:5`). There is no DMG, `.pkg`, Homebrew cask, GitHub Release asset, or update mechanism anywhere in `scripts/`, `.github/workflows/`, or the docs (searched for `notar`, `Developer ID`, `Sparkle`, `auto.update` — the only hit is the admission below).
  - Signing picks the sole Apple Development identity if exactly one exists, otherwise falls back to ad-hoc (`-`) (`scripts/build-app.sh:44`–`scripts/build-app.sh:48`), then signs with hardened runtime (`scripts/build-app.sh:50`). An Apple Development certificate is a _development_ identity: builds signed with it are not distributable to other Macs and cannot be notarized for Gatekeeper.
  - The docs acknowledge the cost: "ad-hoc rebuilds may require granting permissions again" (`README.md:81`) — i.e. Microphone + Accessibility TCC grants are tied to the signing identity, so every ad-hoc rebuild can silently break dictation/insertion until the user re-grants.
  - The Mac server tarball has the same problem: "Developer ID distribution still requires signing/notarization credentials; the draft Mac build is ad-hoc signed" (`Server/README.md:106`), with ad-hoc `codesign --sign -` for both helpers and the Bun binary (`scripts/build-server.sh:92`–`scripts/build-server.sh:95`) using broad JIT entitlements (`Server/entitlements.plist:1` includes `disable-library-validation`, `allow-dyld-environment-variables`).
- **What the user experiences:** a non-developer cannot install Inlay at all. A developer-user must install Xcode 26, Bun, CMake, initialize submodules, download ~4 GB of models, and build the client _and_ server _and_ two native helpers from source — then re-do permission grants whenever the ad-hoc identity changes, and manually rebuild for every update.
- **Confidence:** verified-in-code.
- **Fix direction:** add a Developer ID Application identity + `notarytool` + stapled DMG path to `scripts/build-app.sh` (secrets via CI), publish the client alongside the server tarballs, and add an update story (Sparkle or manual "check release" link). Ad-hoc should warn loudly that TCC grants will break, not silently succeed.
- **Effort:** L (credentials, CI secrets, notarization turnaround, update UX).
- **Owner-decision question:** Is Inlay personal software (build-it-yourself is the intended distribution) or a product others should install? This finding is P0 or informational depending entirely on that answer — see Questions for owner.

## 2. Client packaging is never exercised in CI — `build-app.sh` can rot undetected

- **Severity:** P1
- **Evidence:**
  - The only Mac CI job compiles Swift packages: `swift build --force-resolved-versions` (`.github/workflows/server.yml:49`). No workflow runs `scripts/build-app.sh`, so bundle assembly, `Info.plist` copying, icon generation (`scripts/build-app.sh:41`–`scripts/build-app.sh:42`), entitlements, codesign verify (`scripts/build-app.sh:52`), and the bundle-ID overwrite guard (`scripts/build-app.sh:53`) have zero automated coverage.
  - The release workflow packages only the server (`server-release.yml:12` matrix is server platforms; `server-release.yml:47` runs `scripts/build-server.sh`). There is no client release job at all.
  - The Bun coordinator job builds the darwin-arm64 server on `macos-15` (`.github/workflows/server.yml:58`) while the Swift and release jobs use `macos-26` (`.github/workflows/server.yml:32`, `.github/workflows/server-release.yml:19`) and the README requires Xcode 26+ (`README.md:9`). A future Bun/macOS incompatibility would surface in the job nobody is watching for client purposes.
- **What the user experiences:** the first machine to discover a broken bundle, missing entitlement, or icon failure is the user's (or the owner's, mid-release). A green CI badge currently says nothing about whether an `.app` can be built.
- **Confidence:** verified-in-code.
- **Fix direction:** add a `macos-26` CI job that runs `./scripts/build-app.sh` (ad-hoc is fine in CI) plus `codesign --verify` and a plist sanity check, and upload the `.app` as an artifact. Note `AGENTS.md:9` explicitly allows build checks; this does not violate the test ban (`AGENTS.md:4`).
- **Effort:** S.

## 3. Server "releases" are workflow artifacts, not releases; version numbers live in four disconnected places

- **Severity:** P1
- **Evidence:**
  - `server-release.yml` ends at `actions/upload-artifact` (`.github/workflows/server-release.yml:53`, `.github/workflows/server-release.yml:76`) — there is no `gh release` / `softprops/action-gh-release` step, so a `server-v*` tag (`.github/workflows/server-release.yml:6`) produces expiring Actions artifacts, not a GitHub Release. The docs tell operators to "Download the artifact from the Actions run and unzip it to get the tarball" (`Server/README.md:12`).
  - No single version source exists: client `0.14.1` / build `28` is hand-duplicated in both plists (`Resources/Info.plist:15`–`Resources/Info.plist:18` and `Resources/Info-Dev.plist` same values, verified by read), the API contract is `2.0.0` (`Server/api/openapi.yaml:5`), and the server's health response hardcodes `serverVersion: "0.1.0"` (`Server/src/generation-service.ts:362`, mirrored as a default in `Sources/InlayAPI/API.swift:123`). Nothing bumps or cross-checks these; `server-v*` tags are unconstrained by any file in the repo.
  - Consequence already visible: the architecture doc still says both processes use "the same OpenAPI v1 contract" (`docs/architecture.md:3`) while the contract and README are on API version 2 (`Server/api/openapi.yaml:11`, `README.md:46`).
- **What the user experiences:** an operator cannot tell from `/v1/health` which server build is running (`0.1.0` forever), cannot permalink a release, and a client/server mismatch (which hard-rejects dictation, `README.md:46`) gives no version evidence to debug with.
- **Confidence:** verified-in-code.
- **Fix direction:** pick one version source (git tag → injected at build time into plist, health response, and tarball name), publish real GitHub Releases from `server-release.yml`, and have `build-server.sh`/`build-app.sh` stamp — never hand-edit — version strings.
- **Effort:** M.

## 4. New-machine onboarding is a cliff: Xcode 26 + Metal toolchain + pinned Bun + submodules + ~4 GB of hand-fetched models

- **Severity:** P1
- **Evidence:**
  - Prerequisites: "Apple Silicon, macOS 14+, full Xcode 26+ with the Metal compiler, Bun 1.4.2, CMake, and Git" plus Swift 6.2+ (`README.md:9`). The Metal compiler is a separate download that fails only deep into the build (`scripts/build-text-engine.sh:9`–`scripts/build-text-engine.sh:12`; CI does it explicitly at `.github/workflows/server-release.yml:37`). `Package.swift:1` still declares `swift-tools-version: 5.10` while `TextEngine/Package.swift:1` declares `6.1` and builds require a 6.2 compiler — the manifests understate the real floor, so `swift build` diagnostics will mislead.
  - Parakeet has a real installer (`scripts/download-model.sh:16` verify-then-skip, pinned hash at `scripts/download-model.sh:6`), but Qwen on macOS is a copy-paste shell loop in the README (`Server/README.md:58`–`Server/README.md:68`) with no checksums in the loop itself, and Qwen on Linux is a raw `curl` whose expected hash the user must eyeball (`Server/README.md:74`–`Server/README.md:81`). Any wrong/extra file in the MLX directory fails readiness later (`Server/README.md:70`) with no pointer back to which file is wrong at download time.
  - Two different dev servers with different ports, data dirs, and token stories must be understood before anything works: source server on `0.0.0.0:8392` with `.local/dev-server/data` (`Server/scripts/dev.ts:28`–`Server/scripts/dev.ts:29`) vs packaged runner on `8391` with `.local/server` (`scripts/run-dev.sh:13`). This is documented (`docs/development.md:14`) but it is still the first thing a new dev can get wrong, and dictation additionally requires `INLAY_SPEECH_MODEL`/`INLAY_TEXT_MODEL` exports to be set in the right shell (`README.md:19`, `scripts/run-dev.sh:131`).
- **What the user experiences:** time-to-first-dictation on a clean Mac is measured in hours and failure modes (missing MetalToolchain, wrong Qwen file set, wrong port/token, helpers not built so `ready: false`) that all present as "it doesn't work." There is no `doctor`/`setup` command that checks the whole chain up front.
- **Confidence:** verified-in-code (failure presentation inferred from the readiness model in `docs/development.md:18`).
- **Fix direction:** one `scripts/setup.sh` (or `bun run setup`) that checks Xcode/Swift/Metal/Bun/CMake versions up front, initializes submodules, and downloads+verifies _both_ models using the existing pins; replace the README curl loops with it. Align `swift-tools-version` declarations with the actual 6.2 floor or document why they differ.
- **Effort:** M.

## 5. Model pins are duplicated across three codebases plus prose, with no consistency gate

- **Severity:** P2 (P1 the day a model is ever bumped)
- **Evidence:** the Parakeet SHA-256 `833bffc9…` appears verbatim in `scripts/download-model.sh:6`, `Sources/InlayCore/SpeechModel.swift:27`, and `Server/src/inference/model-pins.ts:4`; the Linux Qwen hash appears in `Server/src/inference/model-pins.ts:9` and as prose in `Server/README.md:81`; the MLX six-file manifest lives in `Sources/InlayCore/TextModel.swift` (referenced from `Server/README.md:70`). The root gate `bun run check` only verifies TypeScript + TS bindings (`package.json:16`), so nothing fails if one copy is edited and the others are not.
- **What the user experiences:** after a model bump, the downloader can fetch a file the server then rejects (or vice versa) — dictation dead with a digest error on a "correctly installed" model. This is exactly the class of break the upgrade section already warns about for Whisper→Parakeet (`README.md:50`).
- **Confidence:** verified-in-code.
- **Fix direction:** generate the three pin sites from one manifest (extending the existing `Server/scripts/generate-api.ts` pattern), or add a `--check`-style comparison to `bun run check`. Build/static check only — allowed under `AGENTS.md:9`.
- **Effort:** S–M.

## 6. The documented local gate (`bun run check`) silently skips Swift binding verification

- **Severity:** P2
- **Evidence:** root `check` is `tsc --noEmit` + `generate:api --check --typescript-only` (`package.json:16`). Full Swift verification requires remembering two extra commands from prose (`docs/development.md:42`, `AGENTS.md:9`), and the Swift generator itself `git clone`s `swift-openapi-generator` from the network into `.build/` on first use (`Server/scripts/generate-api.ts:62`–`Server/scripts/generate-api.ts:73`), uncached in CI. CI does run the full check (`.github/workflows/server.yml:47`), so drift is caught — but only after push, and a contributor following `docs/development.md:4` ("run `bun run check`") reasonably believes they already verified the contract.
- **What the user experiences:** (developer-facing) API changes pass locally, fail in CI minutes later, with the fix requiring a network clone + SwiftPM build of the generator. In an offline/airplane workflow the full check is impossible on a fresh checkout.
- **Confidence:** verified-in-code.
- **Fix direction:** make local `bun run check` match CI on macOS (full `--check` when `swift` is present, explicit "Swift bindings NOT checked" warning when it is not), and cache the generator checkout in CI.
- **Effort:** S.

## 7. CI is correct but slow and cacheless; every PR rebuilds the world

- **Severity:** P2
- **Evidence:** no `actions/cache` (or Bun/SwiftPM/CMake cache) appears in either workflow (searched both files; only `oven-sh/setup-bun` pins Bun). Every push/PR runs: TS checks, full `swift build` (`.github/workflows/server.yml:49`), three Bun coordinator compiles (`.github/workflows/server.yml:56` matrix), and two full native packages including vendored whisper.cpp/llama.cpp CMake builds (`.github/workflows/server.yml:97`–`.github/workflows/server.yml:102`). The CUDA release job allows 300 minutes (`.github/workflows/server-release.yml:64`). Builds also reach the network mid-flight: submodule auto-init (`scripts/build-server.sh:29`–`scripts/build-server.sh:31`), VAD download (`scripts/build-server.sh:69` → `scripts/download-vad.sh:20`), `bun install` inside the package script (`scripts/build-server.sh:77`), and `apt-get`/`xcodebuild -downloadComponent` in CI (`.github/workflows/server.yml:96`, `.github/workflows/server-release.yml:37`).
- **What the user experiences:** (developer/owner-facing) long PR feedback loops and flake surface from five independent network dependencies per build; a Hugging Face or apt outage reds CI for reasons unrelated to the change.
- **Confidence:** verified-in-code (no timing measured; this audit ran no builds).
- **Fix direction:** cache Bun install, SwiftPM (`.build`), CMake build dirs keyed on submodule SHAs, and the VAD file; consider moving the two full native packages off the per-PR path (nightly/manual) while keeping coordinator + checks per-PR.
- **Effort:** S–M.

## 8. Personal infrastructure is hardcoded in the dev launcher

- **Severity:** P2
- **Evidence:** `Server/scripts/dev.ts:50` special-cases one machine: `hostname() === "siva" ? "siva.otter-hawksbill.ts.net" : hostname()` — the owner's hostname and tailnet name are in the shipped source. The same file hardcodes the owner's model layout as defaults (`~/Library/Application Support/Inlay/Models/…` at `Server/scripts/dev.ts:34`–`Server/scripts/dev.ts:38`, `~/.inlay/models/…` at `Server/scripts/dev.ts:41`), duplicated in `scripts/run-dev.sh:136`–`scripts/run-dev.sh:137`.
- **What the user experiences:** any other developer (or future open-source reader) gets a printed "Development URL" and model defaults for a machine that is not theirs, and the repo permanently records the owner's tailnet name — a minor information leak with zero benefit outside one desk.
- **Confidence:** verified-in-code.
- **Fix direction:** print `localhost` plus the bound interfaces, or read an optional `INLAY_DEV_DISPLAY_HOST`; move personal model-path defaults behind the same env vars the docs already teach.
- **Effort:** S.

## 9. macOS has no service story: Linux gets systemd, the Mac server is `nohup` or nothing

- **Severity:** P2
- **Evidence:** Linux packages ship a hardened unit (`Server/inlay-server.service:1`, installed per `Server/README.md:30`–`Server/README.md:36`) and containers get a `HEALTHCHECK` + tini (`Server/Dockerfile:54`–`Server/Dockerfile:57`). On macOS the docs say "Use launchd, systemd, or container supervision…; the scripts do not install a service" (`Server/README.md:129`), and indeed no LaunchAgent plist or installer exists in `scripts/` — the only Mac supervisor is the dev runner's `nohup … &` + PID file (`scripts/run-dev.sh:152`–`scripts/run-dev.sh:154`), which is explicitly a _dev_ tool with `--dev` baked in (`scripts/run-dev.sh:143`).
- **What the user experiences:** the primary platform (same-Mac server, `README.md:7`) does not survive reboot/logout unless the user hand-writes a LaunchAgent with the six required helper/model/token arguments (`Server/README.md:117`–`Server/README.md:125`) — the easiest step in the whole setup to get subtly wrong, and the one that makes dictation "randomly stop working after restart."
- **Confidence:** verified-in-code.
- **Fix direction:** ship a templated `dev.davis.inlay.server.plist` + `scripts/install-service.sh` (Mac) mirroring the systemd unit's env, and document log location/rotation.
- **Effort:** S–M.

## 10. Upgrade/migration is a six-step manual checklist with data-loss-shaped failure modes

- **Severity:** P2
- **Evidence:** `README.md:43`–`README.md:50` requires the operator to, by hand: rebuild all four binaries, update service definitions, rename env prefix to `INLAY_`, update client+server atomically (version mismatch rejects dictation, `README.md:46`), copy `config.json`/`client.json` to new directories _with both clients quit_, re-enter the token (new Keychain services), re-grant TCC permissions, re-enable launch-at-login, retain the archive via `--data-dir`, back up before switching, stop the old server before opening the archive, replace the speech model, and keep container volumes mounted under the old name. No migration script or pre-flight check exists in `scripts/`. Design-history docs compound the confusion: `docs/typescript-server-plan.md`, `docs/typescript-server-validation.md`, and `docs/wispr-flow-import-plan.md` sit beside the canonical docs with no "historical" banner (inventory notes they must be verified against code, `docs/audits/major-issues-2026-10-10/00-inventory.md:22`).
- **What the user experiences:** one missed step = lost device identity, empty history (new empty archive), or a client that refuses to dictate — indistinguishable from data loss until debugged. The lock file prevents archive corruption (`Server/README.md:108`), but nothing prevents pointing the new server at a fresh empty directory.
- **Confidence:** verified-in-code.
- **Fix direction:** an idempotent `scripts/migrate.sh` that performs/verifies each checklist item and refuses to start against an empty archive when a legacy one exists; banner or `docs/history/` for plan docs.
- **Effort:** M.

---

## Questions for owner

- **Distribution intent:** Is Inlay personal software where build-from-source is the intended install (making Finding 1 informational), or should a non-developer be able to download, install, and auto-update it? This single answer re-grades Findings 1, 4, and 9.
- **Apple Developer Program:** Do you hold (or want) a Developer ID Application certificate + notarization credentials for CI? Without them, every Mac artifact stays ad-hoc/draft (`Server/README.md:106`).
- **Versioning policy:** Should client, server, and API share one version (single tag stamps everything), or stay independently versioned with the API integer (`Server/api/openapi.yaml:11`) as the only compatibility contract? Finding 3's fix differs either way.
- **Release home:** Are expiring Actions artifacts acceptable, or should `server-v*` tags publish permanent GitHub Releases (and should the client join them)?
- **Swift reference server:** It is still a shipped product (`Package.swift:6`) compiled in CI on every push (`.github/workflows/server.yml:49` builds all products) yet no script packages or launches it. Keep paying its CI/drift cost as a comparison oracle, or retire it? (Flagged in inventory caveats; graded here only as CI cost under Finding 7.)
- **Support matrix:** Is dropping pre-Xcode-26 / non-Apple-Silicon / SM 75 GPUs (`Server/README.md:12`) a deliberate permanent floor, or an accident of the current toolchain that onboarding (Finding 4) should soften?

## What I could not verify

- **No builds were executed** for this audit (read-only review per task; `AGENTS.md:4` also bars automated tests). Claims about CI duration/flakiness (Finding 7), Gatekeeper behavior of ad-hoc artifacts on a _different_ Mac (Finding 1), and CUDA package usability on real NVIDIA hardware are inferred from read configuration, not observed runs.
- **Notarization/Developer ID status outside the repo** (e.g. credentials or a manual release process the owner performs by hand) is invisible from this worktree; Finding 1 grades only what the repo itself automates.
- **Actual GitHub settings** (branch protection requiring the `server.yml` checks, artifact retention period, release usage) cannot be read from the checkout; Finding 3 assumes the workflow files are the whole story.
- **Vendored submodule contents** (`vendor/whisper.cpp`, `vendor/llama.cpp`) were excluded per instructions; CMake claims cover only Inlay's own `Engine/CMakeLists.txt`, `TextEngine/CMakeLists.txt`, and root `CMakeLists.txt:3`.
- **Generated files** (`Server/src/generated/api.ts`, `Sources/InlayAPIWire/*`) were not line-audited; generation claims cite `Server/scripts/generate-api.ts` and `Server/api/openapi.yaml` instead.
- Whether `docs/typescript-server-validation.md`'s claims still hold (it is 7 lines, read via search excerpt only) and whether the plan docs in Finding 10 misstate current behavior in specific details — flagged as stale-doc _risk_, not verified drift, except the "OpenAPI v1" line at `docs/architecture.md:3` which was read directly.
