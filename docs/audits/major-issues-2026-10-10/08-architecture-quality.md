# Architecture / maintainability — major issues (2026-10-10)

- Scope: worktree `.local/worktrees/major-issues-audit`, branch `audit/major-issues`, HEAD `b6d6815`. Read `AGENTS.md`, `README.md`, `docs/audits/major-issues-2026-10-10/00-inventory.md`, `docs/architecture.md` first. Excluded `vendor/`, `node_modules/`, `.build/`, `build/`, `.local/`, `.swiftpm/`. No product code edited, no tests added or run (per `AGENTS.md:4`).
- Method: search only located candidates; every file cited below was opened and read. Line numbers are HEAD `b6d6815`.
- Headline: the codebase is small and disciplined in places (strict TypeScript, no `as any`, no TODO/FIXME), but it pays for the same logic two to four times over. Almost every major finding is a variant of one problem: duplicated implementations and copied constants with no machine check that the copies still agree, in a repo that also forbids automated tests. That combination, not any single file, is what will slow future fixes.

## Findings

### 1. Two full servers and two text pipelines implement the same behaviour in parallel

- Severity: P1
- Evidence:
  - Shipped TypeScript server: `Server/src/generation-service.ts:163` (1,282 lines), text pipeline in `Server/src/domain/lists.ts` (897 lines), `Server/src/domain/correction.ts` (678), `Server/src/domain/dictionary.ts` (240), `Server/src/domain/composition.ts` (93), `Server/src/domain/cleaner.ts` (27).
  - Swift mirror of the same server: `Sources/InlayServerKit/GenerationService.swift` (1,210 lines), `Sources/InlayServerKit/NativeInference.swift` (410), `Sources/InlayServerKit/HelperProcess.swift` (332), `Sources/InlayServerKit/InlayHTTPServer.swift` (197), entry `Sources/InlayServer/ServerMain.swift:5` — 2,338 lines total for `InlayServerKit` + `InlayServer`.
  - Swift mirror of the same pipeline: `Sources/InlayDomain/SpokenListFormatter.swift` (695 lines), `Sources/InlayDomain/TextCorrectionAlignment.swift` (361), `Sources/InlayDomain/TextCorrectionPolicy.swift` (210), `Sources/InlayDomain/PersonalDictionary.swift` (202), `Sources/InlayDomain/DictationComposition.swift` (128), `Sources/InlayDomain/TranscriptCleaner.swift` (25).
  - The Swift server is documented as "Reference Swift server retained for source comparison" (`docs/architecture.md:16`) yet is still a shipped executable product with the same binary name as the TypeScript server: `.executable(name: "inlay-server", targets: ["InlayServer"])` at `Package.swift:6`. Both route tables implement the same 20 routes (`Server/src/http-server.ts:119` vs `Sources/InlayServerKit/InlayHTTPServer.swift:22`).
  - The two copies have already diverged structurally: TypeScript extracts Wispr import into `Server/src/imports.ts` (936 lines, delegated at `Server/src/generation-service.ts:906`), while Swift inlines import staging/indexing inside its `GenerationService.swift` (e.g. `Sources/InlayServerKit/GenerationService.swift:31`, `:69`, `:415`).
  - The Swift pipeline logic is not used by the Mac client for dictation: the only client-side reference to `InlayDomain` outside the server is a re-export, `@_exported import InlayDomain` at `Sources/InlayCore/DomainExports.swift:1` (the client needs the dictionary _types_ via `InlayAPI`, not list/correction _logic_). So the second pipeline exists almost solely to serve the second server.
- What the user experiences: nothing today if the TypeScript server is the only one deployed. Indirectly: every dictation-quality fix (list formatting, correction guard, dictionary rule) must be written, reviewed, and verified twice, or the "reference" silently becomes wrong and stops being a trustworthy comparison. If anyone ever deploys the Swift binary (it is buildable and identically named), users can get different transcripts for the same audio with no visible difference in setup.
- Confidence: verified-in-code (sizes, products, route tables, and the client re-export all read directly). Whether the Swift server is deployed anywhere is inferred — no script in `scripts/` builds or launches it (packaging always produces the Bun binary, `scripts/build-server.sh`).
- Suggested fix direction: pick one status and encode it. Either (a) delete `InlayServer`/`InlayServerKit` and the pipeline logic in `InlayDomain` that only it uses, keeping the shared wire/dictionary types; or (b) demote it explicitly — rename the product so it cannot be confused with the shipped `inlay-server`, remove it from the default `swift build` products, and state in `docs/architecture.md` that it is frozen/unmaintained. Do not keep it as a silently-rotting second implementation.
- Effort: M (deletion/demotion is mostly mechanical; the work is proving no client target needs the deleted logic).
- Owner-decision question: is the Swift server a live fallback someone might deploy, a comparison oracle you still consult, or migration residue? The fix differs completely by answer.

### 2. Two god-coordinators own nearly every responsibility on their side of the wire

- Severity: P1
- Evidence:
  - Server: `GenerationService` at `Server/src/generation-service.ts:163` (1,282 lines) simultaneously owns preferences with revision checks (`:387`), admission (`:412`), chunked upload (`:482`), seal-and-queue (`:630`, `:928`), history/artifacts/delete/delivery (`:710`, `:856`, `:871`), NDJSON events/subscribers (`:747`), serial FIFO processing and the proofread pipeline (`:1031`, `:1142`), expiry ticking (`:296`), model warmup (`:1244` area), startup reconciliation (`:209`), and Wispr delegate shims (`:906`). All durable mutations funnel through one promise chain, `mutate` at `Server/src/generation-service.ts:196`.
  - Client: `InlayController` at `Sources/Inlay/InlayController.swift:79` (1,380 lines, 62 `func`s) owns `@Published` UI state for every page (`:80`–`:131`), config `didSet` side-effects (`:94`–`:120`), server health (`:283`), history (`:340`), the whole Wispr import state machine (`:383`–`:648`, roughly 265 lines plus its gate type at `:36`), shared-preference saves (`:648`), and the dictation lifecycle (`beginDictation` `:849`, `finishDictation` `:938`, `deliver` `:1093`).
  - The Swift server repeats the pattern at `Sources/InlayServerKit/GenerationService.swift` (1,210 lines, 52 `func`s).
- What the user experiences: no direct symptom. The cost is change risk: a fix to, say, upload expiry or delivery ordering lands in a file where unrelated features share mutable state (`records`, `uploads`, `subscribers`, `pendingDictations`, a dozen `Task` handles), so reviewers must re-read ~1,300 lines to reason about a 10-line change, and merge conflicts concentrate in exactly the files everyone touches.
- Confidence: verified-in-code.
- Suggested fix direction: extract along existing seams, incrementally and without behaviour change — e.g. server-side: preferences store, upload/session store, and processing pipeline out of `GenerationService` (the `imports.ts` extraction at `Server/src/generation-service.ts:183` is the template); client-side: Wispr import driver and history loading out of `InlayController` into their own observable models. Do this only when next touching each area; a big-bang split would be riskier than the current state.
- Effort: L if done at once; S–M per extraction if done opportunistically.
- Owner-decision question: none blocking; this is sequencing, not direction.

### 3. Wire types exist three times: OpenAPI, generated bindings, and a hand-written facade that double-decodes

- Severity: P1
- Evidence:
  - Source of truth: `Server/api/openapi.yaml:5` (version `2.0.0`, ~1,254 lines). Generated output: `Server/src/generated/api.ts` (1,047 lines) and `Sources/InlayAPIWire/*` (5,617 lines across 8 files, largest `Types+Operations.swift` 2,900 and `Types+Components+Schemas.swift` 2,304), produced by `Server/scripts/generate-api.ts:44` (TypeScript) and `:58` (Swift, pinned generator 1.13.1 at `:14`).
  - On top of that, both sides hand-write the same models again: TypeScript re-aliases/narrows every schema in `Server/src/api.ts:5`–`:69`; Swift redeclares every struct by hand in `Sources/InlayAPI/API.swift` (375 lines, e.g. `ServerPreferences` at `:28`, `GenerationRecord` at `:327`).
  - The Swift facade is then bridged to the generated types by `Sources/InlayAPI/APIWire.swift:12`: `decodeWire` decodes the hand-written type from the original bytes _and_ decodes the generated `Wire` type from the same bytes, discarding the second result (`APIWire.swift:16`); `encodeWire` encodes the facade, decodes that as the generated type, and re-encodes (`APIWire.swift:21`). Every one of the ~40 model types needs a manual `APIWireModel` conformance line (`APIWire.swift:27`–`:65`). All client traffic pays this double pass (`Sources/Inlay/ServerClient.swift:72`, `:82`, `:341`, `:391`).
  - The shim's own comment records why the layers cannot simply be merged today: re-encoding the generated model "would turn explicit nulls into omitted fields and bypass custom dictionary validation or alter historical decoding defaults" (`Sources/InlayAPI/APIWire.swift:13`).
- What the user experiences: no visible symptom; a small per-request double encode/decode cost. The real impact is on every future API change: adding one field means editing the YAML, regenerating, updating the TypeScript narrowing in `api.ts`, updating the Swift struct + its custom `Codable`, and adding/keeping its `APIWire` conformance — five places, only some of which the compiler will catch if forgotten (a missed Swift field decodes silently to a default; see `proofreadingPrompt` defaulting at `Sources/InlayAPI/API.swift:69`).
- Confidence: verified-in-code.
- Suggested fix direction: decide which layer is the model. Long term, make the Swift client use the generated `Components.Schemas` types directly and move the handful of conveniences (validation, defaults) into extensions, deleting `API.swift`'s redeclarations and `APIWire.swift`'s double pass. If the facade must stay for historical-decoding reasons, say so in `docs/architecture.md` and accept the cost consciously rather than by default.
- Effort: L (touching every model type); can be done type-by-type.
- Owner-decision question: was the hand-written facade kept for the historical-decoding/null reasons in `APIWire.swift:13`, or is it pre-codegen residue that can now be retired?

### 4. Contract constants, the default prompt, language list, validation, and model pins are copied by hand — nothing checks the copies agree

- Severity: P1
- Evidence:
  - Limits exist in the contract as extensions — `x-inlay-apiVersion: 2`, `x-inlay-defaultPort: 8391`, `x-inlay-maximumRecordingSeconds: 180`, `x-inlay-maximumChunkBytes: 1048576` at `Server/api/openapi.yaml:11`–`:16` — but no generator consumes them; they are then re-typed in TypeScript (`Server/src/api.ts:71`–`:76`), in Swift (`Sources/InlayAPI/API.swift:5`–`:8`, `:35`–`:36`, `:224`–`:225`), _again_ as a file-local constant inside the server (`const MAX_CHUNK_BYTES = 1_048_576` at `Server/src/generation-service.ts:53`, used at `:525` while `Server/src/http-server.ts:143` uses the `api.ts` one), and _again_ for the recording limit in the client (`LifecyclePolicy.maximumRecordingSeconds = 180` at `Sources/InlayCore/LifecyclePolicy.swift:3`, used at `Sources/Inlay/InlayController.swift:1322`).
  - The entire default proofreading prompt is duplicated verbatim: `Server/src/generation-service.ts:64` and `Sources/InlayAPI/API.swift:37`. The supported-language list is duplicated: inline array at `Server/src/generation-service.ts:92`–`:110` vs `Sources/InlayAPI/API.swift:55`. Preferences validation is implemented twice with identical messages: `Server/src/generation-service.ts:90` vs `Sources/InlayAPI/API.swift:75`.
  - Model pins are copied with comments admitting the copy: `Server/src/inference/model-pins.ts:1` ("Immutable identities shared with SpeechModel.parakeet / TextModel.qwen in Swift"), `Sources/InlayCore/SpeechModel.swift:22`–`:28`, the Swift server re-types the same hashes at `Sources/InlayServerKit/NativeInference.swift:238`–`:260`, and `scripts/download-model.sh:5` carries the filename again.
  - The local gate does not catch drift on the Swift side: root `bun run check` runs `generate:api --check --typescript-only` (`package.json:16`); full Swift-binding verification only happens in the macOS CI job (`.github/workflows/server.yml`, `swift` job: `bun run generate:api --check` + `swift build`). None of these checks compare the hand-copied constants above at all.
- What the user experiences: the failure mode is a hard, confusing outage, not a subtle bug. If `API_VERSION` or a limit is bumped in one copy and missed in another, the client refuses to dictate because readiness requires an exact version match (`Sources/Inlay/InlayController.swift:151`: `serverHealth?.apiVersion == InlayAPI.version`), or client and server disagree on chunk size / prompt size and uploads start failing with 400/413. A prompt edited in one copy means the UI's "reset to default" (`Sources/Inlay/Views/PreferencesPage.swift:194`) no longer matches the server's actual default.
- Confidence: verified-in-code (all copies read; the `--typescript-only` scope read in `package.json` and `Server/scripts/generate-api.ts:17`).
- Suggested fix direction: generate the constants from the `x-inlay-*` extensions (or a single small JSON/YAML) into both `api.ts` and `API.swift` as part of `generate-api.ts`, delete the file-local `MAX_CHUNK_BYTES` and `LifecyclePolicy` duplicates in favour of the generated values, and emit the default prompt + language list from one text source. Model pins: generate `model-pins.ts` and the Swift pin literals from one manifest. Until then, at minimum make `bun run check` run the full `--check` on macOS.
- Effort: M.
- Owner-decision question: none on direction; only on whether the prompt/languages should live in the OpenAPI file or a separate shared data file.

### 5. Swift is compiled in Swift 5 language mode with pervasive `@unchecked Sendable`, despite requiring a Swift 6.2 compiler

- Severity: P2
- Evidence:
  - Root package: `// swift-tools-version: 5.10` at `Package.swift:1` and `swiftLanguageVersions: [.v5]` at `Package.swift:38`. CI nonetheless requires Swift ≥ 6.2 (`.github/workflows/server.yml`, `swift` job `#if compiler(<6.2) #error`), and the text-engine package already uses `// swift-tools-version: 6.1` (`TextEngine/Package.swift:1`).
  - 17 types opt out of concurrency checking with `@unchecked Sendable`, concentrated exactly where races would hurt most: audio capture (`Sources/Inlay/System/AudioCaptureSession.swift:7`, `:51`; `Sources/Inlay/System/AudioRecorder.swift:208`, `:320`), the upload pipe (`Sources/Inlay/ServerClient.swift:125`), insertion snapshots wrapping Accessibility objects (`Sources/Inlay/System/TextInserter.swift:44`, `:55`, `:80`), config watching (`Sources/Inlay/ConfigurationStore.swift:171`), and the Wispr reader/gate (`Sources/Inlay/WisprFlowSourceReader.swift:70`, `Sources/Inlay/InlayController.swift:36`).
- What the user experiences: nothing until a data race manifests as a rare crash, corrupted audio chunk ordering, or a stuck take that cannot be reproduced. In Swift 5 mode the compiler will not flag the next unsafe crossing either, so each new feature can quietly add another.
- Confidence: verified-in-code for the mode and the opt-outs; whether any specific `@unchecked` type is actually racy is inferred (their internal locking was not audited here — that belongs to the client-platform audit).
- Suggested fix direction: move `Package.swift` to Swift 6 language mode target-by-target (starting with `InlayDomain`/`InlayAPI`, which are value-type heavy), and require a one-line justification comment on each remaining `@unchecked Sendable`. The existing 17 then become the explicit, reviewable list instead of an ambient habit.
- Effort: M (mostly mechanical; the audio/AX types will need real thought).
- Owner-decision question: none.

### 6. Error handling mixes three conventions, and the few type-safety holes sit on trust boundaries

- Severity: P2
- Evidence:
  - TypeScript is otherwise strict: `strict: true`, `noUncheckedIndexedAccess`, `noUnusedLocals/Parameters` (`Server/tsconfig.json:8`–`:15`), `typescript/no-explicit-any: error` (`.oxlintrc.json:11`), and a search for `as any` / `: any` / `ts-ignore` across `Server/src` finds none outside generated code. The only two `as unknown` casts are `Server/src/inference/helper-process.ts:75` and `Server/src/domain/dictionary.ts:130`.
  - But errors travel three ways: thrown `ServiceError`/`InferenceError` (`Server/src/errors.ts:1`, `Server/src/inference/inference-error.ts:4`) for most of the server; errors-as-values in the domain layer (`decodePersonalDictionary` returns `{ value } | { error }` at `Server/src/domain/dictionary.ts:115`, explicitly "returning errors as values" at `:114`; validation functions return `string | undefined`, e.g. `Server/src/domain/dictionary.ts:110`); and the boundary between them is hand-glued (`if (dictionary.error) throw new ServiceError(...)` then `dictionary.value!` at `Server/src/generation-service.ts:138`–`:142`). Swift mirrors the split: most code `throws`, while model verification returns `Result` (`Sources/InlayCore/SpeechModel.swift:49`).
  - Silent swallowing is common at lifecycle edges: bare `catch {}` blocks throughout `Server/src/imports.ts:59`, `:604`, `:663`, `Server/src/generation-service.ts:343`, `Server/src/inference/helper-process.ts:45`; fire-and-forget `.catch(() => {})` on the mutation queue (`Server/src/generation-service.ts:198`), the expiry tick (`:292`), and cleanup paths (`:1242`). Some are correct (best-effort cleanup), but nothing distinguishes intentional silence from a dropped failure.
  - The riskiest single hole: helper stdout is "validated" by a hand-rolled shape check that returns `undefined` on mismatch and otherwise ends in `return response as unknown as HelperResponse` (`Server/src/inference/helper-process.ts:75`) — i.e. the one place untrusted subprocess output enters the typed world is the one place the type system is switched off. Swift has the analogous pattern at the Accessibility boundary with five `as!` casts (`Sources/Inlay/System/NativePasteCommand.swift:161`, `:190`; `Sources/Inlay/System/TextInserter.swift:411`, `:549`, `:557`), at least one guarded by `CFGetTypeID` (`TextInserter.swift:411`).
- What the user experiences: today, mostly invisible. The risk is a malformed helper response or an unexpected AX object type becoming a crash during dictation/insertion instead of a clean "try again", and future contributors not knowing whether a new function should throw, return `{error}`, or return a string — so error messages surface inconsistently (some become HTTP errors, some become `errorMessage`, some vanish).
- Confidence: verified-in-code.
- Suggested fix direction: document one rule in `docs/architecture.md` (e.g. "domain returns values; service boundary throws `ServiceError`; nothing catches silently without a comment saying why"), replace the helper-response cast with a real decoder that constructs `HelperResponse` field by field (the shape-check code at `helper-process.ts:60`–`:74` is 90% of it), and add a `// intentional: …` note to each bare `catch {}` that is genuinely best-effort.
- Effort: S–M.
- Owner-decision question: your stated preference is errors-as-values; do you want the server's thrown `ServiceError` treated as the sanctioned exception at the HTTP boundary only, with values everywhere else? (Currently the boundary leaks into storage/imports.)

### 7. A one-off migration feature (Wispr Flow import) is permanently embedded in the core modules

- Severity: P2
- Evidence:
  - Client: `Sources/Inlay/WisprFlowSourceReader.swift` is 1,149 lines — the second-largest client file — parsing a third-party app's local SQLite/artifacts; its driver/state machine occupies `Sources/Inlay/InlayController.swift:12`–`:74` (types + preparation gate) and `:383`–`:648` (prepare/start/run/cancel), interleaved with dictation state in the same controller.
  - Server: `Server/src/imports.ts` is 936 lines (second-largest server file), with its own staging/journal/merge logic; 6 of the ~20 routes exist only for it (`Server/src/http-server.ts:223`–`:261`); history filtering (`source=inlay` vs `wispr-flow`) is now part of the core contract and was the stated reason for the API v2 bump (`README.md:46`).
  - It is also duplicated into the Swift reference server (`Sources/InlayServerKit/GenerationService.swift:31`, `:69`, `:415`), compounding Finding 1.
- What the user experiences: users who never import still pay: bigger core files to reason about, history/API surface shaped around an import source, and any change to generation records must consider imported-record invariants (e.g. delivery receipts rejected for imported records). Users who do import depend on Wispr Flow's undocumented local formats, which can break with any Wispr update.
- Confidence: verified-in-code for size/coupling; the "one-off" character is inferred from `docs/wispr-flow-import-plan.md` being a migration plan and the import being a from-another-app move, not an ongoing sync.
- Suggested fix direction: isolate rather than necessarily remove — move the client import driver out of `InlayController` into its own type (it already has its own state enum), and consider feature-gating the server import routes. If the migration window has passed for the intended users, deleting the reader + routes (keeping already-imported history readable) removes ~2,100 lines plus their share of the god-objects.
- Effort: M to isolate; M to delete (plus a data-compat check for existing imported records).
- Owner-decision question: is Wispr import a permanent feature, or a migration bridge with an end date? That answer decides isolate-vs-delete, and whether it should keep being mirrored into the Swift server.

### 8. Model manifests live in the client library but are used by the server/helpers; hidden re-exports blur module ownership

- Severity: P2
- Evidence:
  - `Sources/InlayCore/SpeechModel.swift` (67 lines) and `Sources/InlayCore/TextModel.swift` define the pinned model manifests and integrity verification, yet the client "contains no model helpers" (`docs/architecture.md:21`) and a search finds no client caller of `SpeechModel.parakeet` / `ModelIntegrity.verify` — only a comment referencing it from the Swift server (`Sources/InlayServerKit/NativeInference.swift:238`). `TextModel.qwen` _is_ used, but by the helper: `TextEngine/Sources/InlayTextEngine/CorrectionEngine.swift:78`.
  - To reach that one manifest, the text-engine package depends on the entire root package (`.package(name: "Inlay", path: "..")` at `TextEngine/Package.swift:9`, product `InlayCore` at `:18`), which also drags in `InlayDomain` (`Package.swift:24`) and, on macOS, sits in the same manifest as the app and reference server.
  - Dependency hiding via re-export: `Sources/InlayCore/DomainExports.swift:1` (`@_exported import InlayDomain`) and `Sources/InlayAPI/API.swift:2` (`@_exported import InlayDomain`) mean `import InlayCore` or `import InlayAPI` silently surfaces the whole domain module, so the true dependency graph is not visible from imports.
  - Small dead-code residue in the same vein: `ModelStatus` and `EngineStatus` at `Sources/Inlay/AppState.swift:14`–`:20` have no references anywhere in `Sources/` (only their definitions); they describe a client-owned engine that no longer exists.
- What the user experiences: nothing directly. The costs are build and reasoning: helper builds resolve the whole client package graph, model-pin updates land in a "client core" library that client developers reasonably assume is UI-adjacent, and newcomers cannot tell from imports who really depends on `InlayDomain`.
- Confidence: verified-in-code (callers searched across `Sources/` and `TextEngine/`; package manifests read).
- Suggested fix direction: move `SpeechModel`/`TextModel` manifests into a tiny shared `InlayModelManifest` target (or into `TextEngine` + server-only Swift), delete `DomainExports.swift` and the `@_exported` in `API.swift` in favour of explicit imports, and delete the two dead enums.
- Effort: S–M (target reshuffle; no behaviour change).
- Owner-decision question: none, unless an in-client model download/verify feature is planned that would re-home these manifests in the app (the dead `ModelStatus.downloading/verifying` cases at `AppState.swift:15` hint one once was).

### 9. The no-automated-tests policy removes the only scalable check on all of the above duplication

- Severity: P2 (structural amplifier of Findings 1–4)
- Evidence:
  - `AGENTS.md:4` prohibits unit/integration/e2e/snapshot/regression suites, fixtures, mocks, runners, and "scripted behavioral assertion harnesses"; the lint config even bans importing `bun:test`/`node:test` (`.oxlintrc.json:14`–`:27`). Allowed verification is static (`bun run check`/`lint`/`fmt:check`, `AGENTS.md:5`) plus manual computer-use against Inlay Dev with real models (`AGENTS.md:6`–`:7`). CI (`.github/workflows/server.yml`) therefore only formats, typechecks, lints, generates, and _compiles_ — no job executes dictation, insertion, or the text pipeline.
  - This is precisely the repo shape where tests would carry the most weight: two implementations of a deterministic, pure text pipeline (`Server/src/domain/*` vs `Sources/InlayDomain/*`, Finding 1) whose parity is currently assertable only by reading both, and copied constants (Finding 4) whose equality a 20-line script could check.
- What the user experiences: regressions in list formatting, correction guards, or client/server constants are found by a human dictating into Inlay Dev — i.e. late, and only for the cases someone thinks to speak. Every refactor suggested above (splitting god-objects, retiring the facade, generating constants) is riskier and therefore less likely to happen.
- Confidence: verified-in-code (policy and CI read). This finding does not claim any specific bug exists because of it.
- Suggested fix direction: this is an owner policy, so no unilateral fix is proposed. If the ban stands, the cheapest compensating controls that are _not_ behavioural tests: (a) generation-based single-sourcing per Finding 4 so copies cannot drift; (b) reducing the duplicated surface per Findings 1 and 3 so there is less to keep in sync by eye; (c) extending `bun run check` to the full Swift generation check on macOS.
- Effort: S for (c); M for (a); see Finding 1 for (b).
- Owner-decision question: is the test ban absolute, or would narrow, deterministic parity/consistency checks (constant-equality and TS↔Swift pipeline fixture comparison, run as part of `bun run check`) be acceptable? The answer determines how much of Findings 1–4 must be solved by deletion/generation instead.

## Questions for owner

- Swift reference server: live fallback, comparison oracle, or migration residue? (`docs/architecture.md:16` vs `Package.swift:6`) — decides delete vs demote vs actively maintain (Finding 1).
- Hand-written Swift API facade: kept for the historical-decoding/null-handling reasons recorded at `Sources/InlayAPI/APIWire.swift:13`, or retireable in favour of the generated types? (Finding 3).
- Error convention: is thrown `ServiceError` the sanctioned exception at the HTTP boundary only, with errors-as-values everywhere else? (Finding 6).
- Wispr Flow import: permanent feature or migration bridge with an end date — isolate or delete? (Finding 7).
- Test policy: absolute ban, or are deterministic consistency/parity checks inside `bun run check` acceptable? (Finding 9).
- Model manifests in `InlayCore`: is an in-client download/verify feature planned (as the dead `ModelStatus` cases at `Sources/Inlay/AppState.swift:15` suggest was once intended), or can the manifests move to a server/helper-owned target? (Finding 8).

## What I could not verify

- Runtime behaviour of anything: per `AGENTS.md:4`–`:7` no tests were run and no Inlay Dev dictation/insertion was performed; all impact statements are code-path reasoning, not observed failures.
- Whether the Swift `inlay-server` (`Package.swift:6`) is built or deployed anywhere outside this repo's scripts/CI — no in-repo script launches it, but external deployment cannot be ruled out from code alone.
- Actual behavioural parity between the TypeScript and Swift text pipelines: both were read for structure/size, but no line-by-line equivalence diff of `lists.ts`↔`SpokenListFormatter.swift` or `correction.ts`↔`TextCorrection*.swift` was performed, and generated bindings (`Server/src/generated/api.ts`, `Sources/InlayAPIWire/*`) were sized and traced to their generator, not line-audited.
- Whether any specific `@unchecked Sendable` type (Finding 5) or `as!` Accessibility cast (Finding 6) is exploitable by real input — internal locking/guard completeness belongs to the client-platform and insertion-safety audits.
- Buildability claims (`bun run check`, `swift build`) were not executed in this audit; CI configuration was read (`.github/workflows/server.yml`) but no workflow run was inspected.
