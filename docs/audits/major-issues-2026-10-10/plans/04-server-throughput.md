# Batch 4 — Server throughput (stack/04-server-throughput)

- Base: stacked on the Batch 3 branch (stack base `stack/00-audit-plan` on `audit/major-issues`). All `file:line` citations are at audit HEAD `b6d6815`; Batches 1–3 will shift `Server/src/generation-service.ts` substantially, so re-verify every line number against the rebased tree before editing. Symbol names are the stable anchor.
- Findings: P1-8, P1-9, P1-10 (`REPORT.md:45-47`), plus P2s cold start (`07 F9`), fixed speech deadline (`07 F8`), events re-send (`05 F5`). Audits read in full: `05-server-api-bugs.md`, `07-audio-pipeline-perf.md`, `11-crosscheck.md`.

## Goal

- Concurrent takes no longer serialize behind each other's disk I/O: chunk uploads, WAV sealing, history, and health stop sharing one global promise chain (`mutate`, `Server/src/generation-service.ts:196`).
- Overload fails at key-press with a clear capacity error instead of after speaking; a queued user can see their queue position; transcription of take N+1 overlaps proofreading of take N.
- A pro audio interface (>192 kHz, or any format whose 180 s archival copy would exceed the server cap) can never kill the inference take; the original-audio copy is dropped for that take with a recorded reason.
- Cold start warms speech and proof models in parallel; the speech deadline scales with audio duration so a 180 s take on slow hardware is not killed by a fixed 180 s wall clock.

## Decisions applied

- **Shareable product** (`DECISIONS.md:8`): the admission cap and its error copy are product behavior, not owner-only tuning. The client already maps HTTP 429 to "The server has reached its capacity. Try again shortly." (`Sources/Inlay/ServerClient.swift:102`).
- **Cancel keeps history** (`DECISIONS.md:14`, overrides P1-18): the lock refactor must not change `cancelRecord` retention semantics (`Server/src/generation-service.ts:798`); sealed audio of cancelled takes stays.
- **Swift server is deleted** (`DECISIONS.md:18`, Batch 8): do not mirror any of these changes into `Sources/InlayServerKit/`; TypeScript server only.
- **Interruption: save the speech** (Batch 1, `DECISIONS.md:9`): Batch 1 lands before this batch and may add chunk retry / receiving-take recovery. Slice 3's "fsync at seal only" must be reconciled with whatever Batch 1 chose for partial-upload durability (see Risks).
- **Checks** (`DECISIONS.md:21`): no behavioral tests; no new parity checks in this batch (Batch 8 owns those). `generate:api` regeneration is the only generated-code gate touched.
- **Logging is Batch 3's job**: this batch adds no logging. Existing per-record `processingSeconds` fields are the manual-verification signal.

## IN scope commit slices (ordered)

### Slice 1a — Duration-scaled speech deadline (S)

- Files/symbols:
  - `Server/src/inference/native-inference.ts:43` `createInferenceConfiguration` (`speechTimeout: deadline(input.speechTimeout, 180)` at `:49`); `:79` `InferenceBackend.transcribe` interface; `:254` `NativeInference.transcribe` (timeout consumed at `:302`).
  - `Server/src/generation-service.ts:1047` — the single `transcribe` call site in `process()` (confirm with a `\.transcribe(` search; no other TS caller exists at `b6d6815`).
- Change: add `audioSeconds?: number` as the final optional parameter of `transcribe` on the interface and implementation. Compute the effective timeout once, next to `deadline()` (`native-inference.ts:37`): `effective = audioSeconds === undefined ? speechTimeout : min(900, max(speechTimeout, ceil(60 + 2 × audioSeconds)))`. A 180 s take gets 420 s; short takes keep today's 180 s floor. The caller passes `record.inferenceAudio.frameCount / 16000` (the artifact is set by `seal` before `process` runs, `generation-service.ts:687`).
- Why: `07 F8` — a fixed 180 s kill deadline means a max-length take must transcribe faster than real time or always fail, and the kill also evicts the warm model (`HelperProcess.setTimeout` → `reset`, `Server/src/inference/helper-process.ts:323`).
- No config surface change: `Server/src/configuration.ts:114` never passes `speechTimeout`, so the default formula is the behavior. `proofTimeout` untouched.

### Slice 1b — Parallel warmup (S)

- Files/symbols: `Server/src/inference/native-inference.ts:231` `NativeInference.warmUp` (sequential verify→load speech at `:233-247`, then verify→load proof at `:248-251`). Collaborators opened: `ModelVerifier.verify` (`Server/src/inference/model-verification.ts:56`, per-path pending map, safe for two distinct paths concurrently) and `HelperProcess.ensureLoaded` (`Server/src/inference/helper-process.ts:106`, distinct `speech`/`proof` instances).
- Change: run the speech branch and proof branch concurrently with `Promise.allSettled`, then rethrow the speech failure first, else the proof failure. Keep the exact speech-pin error rewrite at `:240-244` ("The speech model must be Parakeet v3…") inside the speech branch. When `proofreadingEnabled` is false, behavior is identical to today.
- Why: `07 F9` — sequential hashing of ~1.26 GB speech + (Linux) ~2.5 GB proof models, then sequential loads, gates the whole post-restart window. `allSettled` (not `all`) preserves today's error precedence and never orphans a still-loading branch behind a fast failure.

### Slice 2 — De-queue synchronous reads (S)

- Files/symbols (all `Server/src/generation-service.ts`): `get` `:707`, `history` `:710`, `getPreferences` `:384`, `health` `:337` (mutate wrapper at `:346`), `tick` heartbeat half `:297-303`.
- Change: remove the `mutate()` wrapper from bodies that never `await` between reading and copying state. `getInternal` (`:204`) already returns a `structuredClone`; `history`'s sort/filter/clone is synchronous; `health`'s tail (read `speechLoadError`/`warming`/`stopping`/`preferences`, call `beginWarmup` `:1244`) is synchronous — keep the two real awaits (`readiness` `:338`, disk check `:341`) before it, as today. The tick heartbeat loop (`yieldTo` `:1223` is synchronous) likewise runs unwrapped; `imports.expireImportStages()` stays queued until Slice 3b.
- Why: instant, low-risk half of P1-8 — health/history stop stalling behind another take's seal even before the lock refactor lands. JavaScript's single thread makes a no-await read-copy atomic against queued mutations; that invariant is the whole safety argument, so no `await` may be introduced into these bodies later without re-queueing (add a one-line comment stating this at each site).

### Slice 3a — Per-generation locks, I/O out of the global queue, fsync at seal (L) — core of P1-8

- Files/symbols (`Server/src/generation-service.ts` unless noted):
  - Lock infrastructure next to `mutate` `:196` / `queue` field `:176`: add `generationQueues = new Map<string, Promise<unknown>>()` and `withGenerationLock<T>(id, operation)` mirroring `mutate`'s chain-and-swallow pattern; delete the map entry when the settled tail is still the stored promise (bounds map growth).
  - `appendAudio` `:482`: move the whole body from `mutate` to `withGenerationLock(record.id)`. Delete per-chunk `await file.sync()` (`:614`) and per-chunk `requireDiskSpace` (`:590`, a `statfs` in `Server/src/storage.ts:95`); keep the disk check at `create` (`:445`), on the first chunk of each stream (`upload.bytes === 0`), and before sealing (below). Keep the finite-float scan (`:531`), replay verification (`:551`), sequence and cap checks (`:581-589`) unchanged.
  - `finish` `:630`: move to the generation lock. Inside it, before sealing, `requireDiskSpace` once. Seal order and failure path (`:686-701`) unchanged semantically.
  - `seal` `:928`: already `output.sync()`s at `:977`; add a directory sync after the `rename` at `:984` so the WAV name itself is durable — copy the exact pattern from `atomicPrivateWrite` (`Server/src/storage.ts:76-84`) into a small exported `syncDirectory(path)` helper in `storage.ts` and use it in both places.
  - `cancel`/`cancelRecord` `:795/:798`, `delete` `:856`, `recordDelivery` `:810`, `artifact` `:871`: generation lock instead of `mutate`. `delete` additionally removes the record from `records`/`imports` (`:867-868`) — synchronous map edits, safe under the generation lock alone. `artifact`'s open/stat I/O stays inside the generation lock (short, and only blocks that one generation).
  - `process` transitions `:1037`, `:1074`, `:1118`, `:1125` and the progress callback `:1052-1054`: record mutations move to the generation lock; `progress()` (`:1228`) is already synchronous — call it directly, dropping the `mutate` round-trip per helper progress tick.
  - `tick` expiry half `:304-313` and `shutdown` `:320-323`: cancel each record through its own generation lock (sequential loop is fine; neither is hot).
  - What stays on the global `mutate` queue: `create` `:412` (idempotency scan + preferences snapshot at `:475` + `mkdir`/`save` must be atomic against `updatePreferences`), `updatePreferences` `:387`, `events` subscribe/unsubscribe bookkeeping `:748/:784` (subscribers map), shutdown's subscriber teardown `:328`.
  - Lock-ordering rule (state it in a comment on `withGenerationLock`): never await a generation lock while holding the global queue; global-queue operations never call generation-locked methods. `save()` (`:1199`, an `atomicPrivateWrite` with file+directory fsync) is henceforth called only under a generation lock or the global queue, never both.
- Why: P1-8 (`05 F1 = 07 F4`, confirmed `11-crosscheck.md:20`). One take's seal (whole-file copy at `:964`) and hundreds of per-chunk fsyncs currently stall every other take's chunk acknowledgements, which can overflow the client's 512-chunk pipe (`Sources/Inlay/ServerClient.swift:132`) and lose the second take. Per-chunk fsync buys nothing today: a crash mid-upload discards the take anyway (`cleanPartial` `:1234`).

### Slice 3b — Wispr imports off the global queue (M)

- Files/symbols: wrappers `knownWisprFlowIDs`/`beginWisprFlowImport`/`uploadWisprFlowArtifact`/`completeWisprFlowImport`/`cancelWisprFlowImport`/`archiveWisprFlowDictionary` (`Server/src/generation-service.ts:906-927`); heavy bodies in `Server/src/imports.ts` (`uploadWisprFlowArtifact:551`, `completeWisprFlowImport:643`, `expireImportStages:844`); callback surface `getPreferences`/`getRecord`/`publish`/`requireDiskSpace` wired at `generation-service.ts:183-189`.
- Change: replace `mutate` in the six wrappers and in `tick`'s `expireImportStages` call with one dedicated `importQueue` promise chain (same pattern as `mutate`). Imports stay serialized against each other (they share the source-ID index) but no longer block audio. The callbacks they invoke are synchronous (`publish` `:1210`, `getInternal` `:204`), so no global-queue re-entry is needed; audit `imports.ts` for any `save`-like awaited write while implementing and keep those on the import queue only.
- Why: `05 F1` — import completion does multi-file reads/writes and directory swaps under today's global queue, stalling live dictation uploads for its whole duration.

### Slice 4 — Events stream: dedupe heartbeats, coalesce progress (S–M)

- Files/symbols (`Server/src/generation-service.ts`): `Watcher` interface `:156`, `tick` heartbeat `:298-301`, `yieldTo` `:1223` (deep-clones per watcher, silently drops beyond 8 queued), `publish` `:1210`, `progress` `:1228`. Wire contract at `Server/api/openapi.yaml` events operation; client reader `readEvents` (`Sources/Inlay/ServerClient.swift:330`, skips empty lines at `:340`).
- Change: add a per-record version counter (`recordVersions = new Map<string, number>`, bumped in `publish` and in `progress` when the value actually changes) and `lastVersion`/`lastSentAt` on `Watcher`. `yieldTo` skips when the version is unchanged and the last send was under 10 s ago, so the 2 s tick degrades to a ≤10 s keepalive of the full record (wire format unchanged — still complete `GenerationRecord` NDJSON lines; no blank-line frames, so older clients are unaffected). Coalesce progress: `progress()` yields at most every 250 ms per record unless the value crosses to `1` or the record became terminal via `publish`. Slice 5's `queuePosition` is part of the comparison (see there).
- Why: `05 F5` — a queued take currently re-sends its entire record (settings snapshot included) ~30×/minute per watcher over possibly remote links, cloning inside what was the mutation queue.

### Slice 5 — Admission cap + queue position (M) — first half of P1-9

- Server (`Server/src/generation-service.ts`):
  - Add `MAX_ACTIVE_GENERATIONS = 12` beside `MAX_CHUNK_BYTES` (`:53`). In `create` (`:414`, inside the existing global-queue body, after the idempotency replay return at `:433` so replays never count twice): if non-terminal records (`receiving|queued|transcribing|proofreading`) number ≥ 12, throw `ServiceError(429, "server_busy", "The server is already processing the maximum number of recordings. Try again shortly.")` (`ServiceError` shape: `Server/src/errors.ts:1`). Not env-configurable in this batch.
  - Track FIFO order in memory: `processingOrder: string[]` appended in `enqueueProcessing` (`:1014`), removed when that job's processing finishes (the existing `.finally` at `:1026`) or on cancel/delete. Position = 1-based index in this list among records still `queued`.
- API: add optional `queuePosition` (integer, minimum 1) to `GenerationRecord` in `Server/api/openapi.yaml:1122` (beside `progress` at `:1184`). Regenerate (`bun run generate:api`), which updates `Server/src/generated/api.ts` and the Swift wire bindings; the TS alias at `Server/src/api.ts:60` needs no edit. Hand-written Swift facade: add `public var queuePosition: Int?` to `GenerationRecord` (`Sources/InlayAPI/API.swift:327`, beside `progress` at `:354`) — a missed facade field silently defaults, so this edit is mandatory in the same slice.
- Computation point: one private `decorate(record)` in `generation-service.ts` that sets/clears `queuePosition` on the **copy** returned by `getInternal` and on copies pushed through `yieldTo`/`history`. Never persist it: `save()` (`:1199`) deletes the field before `JSON.stringify`, so `metadata.json` and startup validation (`:244`) never see a stale position. Slice 4's dedupe key becomes `(version, queuePosition)` so position changes propagate on the next tick.
- Client: `applyProgress` (`Sources/Inlay/InlayController.swift:1081`) — `.queued` case at `:1085` becomes `"Queued on server… (#\(position))"` when `queuePosition` is present, else today's string. No view changes; Batch 2 owns surfacing `statusMessage` in the HUD.
- Why: P1-9 — no cap means overload is discovered after speaking; no position means "queued behind 4 takes" is indistinguishable from "stuck".

### Slice 6 — Stage pipelining across jobs (M) — second half of P1-9

- Files/symbols (`Server/src/generation-service.ts`): `processingQueue` field `:168`, `enqueueProcessing` `:1014`, `process` `:1031` (speech `:1047`, deterministic text stages `:1068-1070`, proof transition `:1074-1083`, `proofread` call `:1084`, completion save `:1118`), `proofread` `:1142` (internally awaits `inference.correct` and `evaluateCorrectionInWorker`, `Server/src/domain/correction-runtime.ts:18`). Load-bearing constraint opened: `HelperProcess.request` rejects with `busy` if a request is already outstanding (`Server/src/inference/helper-process.ts:140`) — per-stage chains, never parallel calls into one helper.
- Change: replace the single chain with `speechQueue` and `proofQueue` promise chains. `enqueueProcessing` chains the speech stage (status→`transcribing`, `transcribe`, `cleanTranscript`/`applyDictionary`/`formatSpokenList`) onto `speechQueue`; on success it chains the proof stage (status→`proofreading` **at stage entry**, moving the `:1074` block, then `proofread`, composition, terminal save) onto `proofQueue`. `processingControllers`/`AbortSignal` plumbing, `signal.throwIfAborted()` checkpoints, and the failure/cancel save (`:1125`) stay per job across both stages; each stage re-checks `terminal()` under the generation lock before mutating. `beginWarmup` fires from the proof stage's `finally` and from a speech-stage failure, not between stages. `shutdown` awaits both chains (today `:326`).
- Preserved invariants (state in the commit message): completion order still equals seal order (both chains are FIFO and speech feeds proof in order); the list-continuation snapshot is captured at `finish` (`:685`, `continuation()` `:995`), not re-read during processing, so overlap cannot corrupt it; client delivery ordering is separately serialized by `deliveryTail` (`Sources/Inlay/InlayController.swift:1066`). `queuePosition` from Slice 5 now means speech-queue position; a record in `proofreading` reports none.
- Why: `07 F3` — speech and proof are different helper processes; serializing proof of take N ahead of speech of take N+1 wastes the idle Parakeet helper on every back-to-back dictation.

### Slice 7a — Server: original audio becomes best-effort at finish (M)

- Files/symbols: `finish` (`Server/src/generation-service.ts:630`, original-required branch `:662-684`); `appendAudio` caps (`:584`, unchanged); `GenerationRecord` in `Server/api/openapi.yaml:1122`.
- Change:
  1. Add optional `originalAudioOmittedReason` (string) to `GenerationRecord` in the OpenAPI schema; regenerate bindings; add the matching optional field to the Swift facade (`Sources/InlayAPI/API.swift:327`, beside `originalAudio` at `:338`).
  2. In `finish`, when `keepOriginalAudio` is on but `request.originalFrames === undefined`: discard any partial `original.raw` (single-file `rm`, same pattern as `cleanPartial` `:1234`), set `record.originalAudioOmittedReason = "Original audio was not kept for this recording because its format is too large to archive."`, and seal inference only. When `originalFrames` **is** present, today's completeness and ±0.075 s interval checks (`:663-678`) apply unchanged — a truncated upload still 409s, only a deliberate omission is accepted. The `unexpected_original` branch (`:679`) still rejects originals sent when the preference is off.
- Why: P1-10 server half — today the archival copy is _required_ at finish, so any client-side decision to skip it would 409 the whole take.

### Slice 7b — Client: decide original-audio eligibility at capture, never throw over archival (M)

- Files/symbols:
  - `Sources/Inlay/System/InputOnlyAudioUnit.swift:85` `prepare` admission (≤768 kHz / ≤8 ch): **no behavior change**. Narrowing it would block the inference path (which downmixes/resamples any admitted format, `AudioRecorder.swift:354-375`) — the opposite of the goal. Add a one-line comment pointing at the eligibility predicate below.
  - `RecordingWriter.init` (`Sources/Inlay/System/AudioRecorder.swift:345`, `originalURL` gate at `:379`): this is the first point where the negotiated `inputFormat` is known. Add one shared predicate, e.g. `OriginalAudioPolicy.isArchivable(sampleRate:channels:)` in `AudioRecorder.swift`: archivable iff `sampleRate ≤ 192_000 && sampleRate × channels × 4 × 180 ≤ 268_435_456` (mirrors the server's per-stream caps at `generation-service.ts:584` and format ceiling at `:515`/`openapi.yaml:712-726`; cross-reference both in a comment — Batch 8 single-sources these constants). If not archivable, treat as `preserveOriginalAudio == false`: no local `original.wav`, no `emitOriginal` (`:486,:589`). This also closes the newly-reachable multi-GB temp file: once the session survives (next bullet), a 768 kHz × 8 ch × 180 s local WAV would be ~4.4 GB (writer is duration-capped only, `:481`; cross-check `11-crosscheck.md:63` refuted reachability only because the session died first).
  - `Sources/Inlay/ServerClient.swift`: `UploadBuffer.append` (`:407`) drops the `sampleRate ≤ 192_000` session-killing throw for the **original** buffer — instead, `upload(_:to:preserveOriginal:)` (`:356`) applies the same predicate on the first original chunk and, if ineligible, ignores original chunks for the rest of the take. Belt-and-braces: if an original `flush` (`:382`) is rejected (413/400), drop the original stream and continue; inference flush errors keep today's throw (Batch 1 owns retry). At the end, `originalFrames` is sent only when the original was kept **and** `original.frames > 0`; otherwise `nil`, which Slice 7a accepts. `InlayController` call sites (`:906` upload, `:920` recorder start) are unchanged — both still pass the server preference; each layer downgrades independently, and the `nil`-frames finish reconciles them.
  - `Sources/Inlay/Views/HistoryPage.swift:232`: where the "Open original" button is conditionally shown, add a caption `Text` of `originalAudioOmittedReason` when present and `originalAudio == nil` (one line, `InlayPalette.muted`, matching the provenance caption at `:258-262`). No layout change otherwise.
- Worked examples for the predicate (180 s worst case): 48 kHz stereo ≈ 69 MB — kept. 192 kHz stereo ≈ 276 MB > cap — dropped. 96 kHz × 8 ch — dropped. 192 kHz × 8 ch hits the server byte cap at ~43 s today (`11-crosscheck.md:39`) — dropped at admission instead.
- Why: P1-10 (`07 F5` mechanism confirmed) — the transcript needs the 16 kHz stream only; the archival stream must never be able to fail it.

## API/data changes + migration

- **Two additive optional fields** on `GenerationRecord` (`Server/api/openapi.yaml:1122`): `queuePosition` (Slice 5, computed, never persisted) and `originalAudioOmittedReason` (Slice 7a, persisted). `API_VERSION` stays 2 (`Server/src/api.ts:71`); `schemaVersion` stays 1. Regenerate TS + Swift bindings in the same slice that edits the schema (`bun run generate:api`), and hand-edit the Swift facade (`Sources/InlayAPI/API.swift:327`) in the same commit.
- **New error**: `429 server_busy` from `create` (Slice 5). The error envelope is the existing `APIErrorResponse` (`openapi.yaml:1232`); the client already has 429 copy (`ServerClient.swift:102`) and treats 429 as transient for retryable calls (`:323`) — `create` itself is not retried, so the user sees the capacity message at key-press.
- **Semantics change, no shape change**: `finish` with `keepOriginalAudio=true` and omitted `originalFrames` now succeeds (Slice 7a). `FinishGenerationRequest` (`openapi.yaml:743`) already makes `originalFrames` optional.
- **Stored data**: old `metadata.json` records validate unchanged (both fields optional; startup `validateBody` at `generation-service.ts:244` passes). No migration script. Deploy order within a release: server before client, or same release — a new client omitting `originalFrames` against an old server gets `409 incomplete_original`; an old client against the new server behaves exactly as today (its >192 kHz throw is client-side). The exact-`apiVersion` client gate already forces lockstep upgrades, so this is a release-notes line, not code.

## OUT of scope/deferred

- **Let `create` accept (and the client record) while models warm** (`07 F9` second half; touches `create` readiness gate `generation-service.ts:434` and client refusal `InlayController.swift:852`). Deferred: it redefines the admission contract Batch 1 builds on; parallel warmup (1b) lands without it.
- **Per-device fairness / priority queues** (`07 F3` question): single global FIFO order is kept; only count and overlap change.
- **Chunked/streaming transcription and cooperative helper abort** (`07 F8` L-half, `Engine/worker.cpp`): product decision, DECISIONS default keeps whole-utterance decode. Only the deadline scales here.
- **Correction-Worker pooling** (`correction-runtime.ts:36` spawns per job): untouched; pipelining does not increase its concurrency (proof chain is serial).
- **History payload projection, size-limit realignment** (`05 F3/F11`, incl. the 180.1 s append vs 180.0 s finish mismatch at `generation-service.ts:584/:655`): Batch 8 contract work. Slice 7 does not rely on changing them.
- **Mirroring anything into the Swift reference server** (`Sources/InlayServerKit/`): deleted in Batch 8 per DECISIONS.
- **Config knobs** (env/CLI for cap, deadline factor, keepalive cadence): named constants only; less is more.
- **Writer-queue bounding / buffer-pool work** in the realtime capture path (`07 F10`, `AudioRecorder.swift:406-424`): separate slice; Slice 7b only gates the original file at writer init.

## Risks

- **Lock-ordering deadlock (Slice 3a)** is the batch's main hazard: any path that awaits a generation lock from inside the global queue (or vice versa) hangs the server. Mitigation is the stated ordering rule plus keeping `save()` call sites auditable in one search; manual verification must include overlapping takes (below).
- **Cancel vs seal race**: with per-generation locks, a cancel issued mid-seal now _waits_ for the seal, then cancels a `queued` record (controller abort at `:805`) instead of interleaving. That matches the DECISIONS keep-in-history semantics, but a cancel during a 256 MiB seal feels slower to the user. Accepted; do not add a seal-abort path.
- **Durability window widens**: dropping per-chunk fsync/statfs means an OS crash can lose recent `.raw` bytes, and disk-full is now discovered at `finish`/seal instead of mid-take. At `b6d6815` a seal failure marks the record failed and `cleanPartial` deletes the raw audio (`:693-698`) — Batch 1's seal-keep-raw work must land first (it is earlier in the stack); verify its final shape before Slice 3a. If Batch 1 instead adds restart-resume of `receiving` takes from `.raw` files, re-check this slice: the size guards at `:559/:601` will trip `invalid_archive` on a torn tail rather than resuming, and the plan's default becomes "resume only from the last acknowledged chunk boundary" — flag to the Batch 1 owner rather than silently re-adding fsync.
- **Pipelining resource contention (Slice 6)**: both model helpers active simultaneously raises peak unified-memory/CPU pressure; a slow proof stage can now also hold one finished transcript in memory while speech continues. Bounded by the Slice 5 cap. If manual verification shows proof starving speech on the dev Mac, the fallback is landing Slice 5 without Slice 6 (the REPORT's "cap + position (S)" minimum).
- **Synchronous-read invariant (Slice 2)** is fragile to future edits: one added `await` in `history`/`health` reintroduces torn reads. The per-site comment is the mitigation; there is no checker for it (and none may be added in this batch).
- **Queue-position accuracy**: position is computed from the in-memory FIFO list, which is empty after a restart — requeued-by-Batch-1 records get positions only once `enqueueProcessing` runs for them. `history` rows for old `queued` records simply omit the field. Acceptable.
- **Original-audio predicate duplication (Slice 7b)**: the 192 kHz / 256 MiB / 180 s constants now live in TS service code, OpenAPI, and Swift. Wrong-but-safe direction: Swift stricter → an original is dropped unnecessarily (visible via `originalAudioOmittedReason`); server stricter → Slice 7b's flush-failure downgrade still saves the take. Batch 8's constant generation removes the duplication.
- **Warmup parallelism (Slice 1b)**: two multi-GB hashes now contend for one disk. On slow storage the _speech_ ready time could regress slightly even as total warm time drops. Acceptable; do not add I/O throttling.

## Verification (exact commands + Inlay Dev manual steps for owner)

Per `AGENTS.md` — no automated behavioral tests anywhere in this batch.

Static/build gates, from the worktree root, after the final edit of any slice:

- `bun run fmt` (required after any TypeScript edit, `AGENTS.md:3`)
- `bun run check` (strict TS + generated-API consistency)
- `bun run lint`
- `bun run fmt:check`
- Slices 5 and 7a only (OpenAPI changed): `bun run generate:api --check` (full check incl. Swift bindings; the Swift half may need network for the generator — if offline, run `bun run check`'s `--typescript-only` gate and report the Swift half unverified, do not claim it passed)
- Slices 5 and 7b only (Swift changed): `swift build --force-resolved-versions`
- No native helper rebuild: this batch touches no `Engine/`/`TextEngine/` code, so `./scripts/build-server.sh` is not required.

Runtime setup (isolated instance only; never disturb an existing server or archive):

- `bun install --frozen-lockfile`, then `bun run dev` (isolated data under `.local/dev-server/data`, port 8392) with real models configured per `docs/development.md` (`INLAY_SPEECH_MODEL`, `INLAY_TEXT_MODEL`, helpers built once). `ready: false` is not dictation verification.
- Launch **Inlay Dev** per `docs/development.md` (`./scripts/build-dev-app.sh`, separate `INLAY_CLIENT_DATA_DIR`), pointed at `http://localhost:8392` with the dev token.

Owner manual steps per slice, observed in Inlay Dev:

1. **Slices 1a/1b**: restart the dev server and immediately dictate — models should reach ready noticeably sooner than pre-change (compare against the pre-change build if unsure). Record one near-180 s take; confirm it completes and the history record's `speech.processingSeconds` is present. (A true >180 s wall-clock transcription needs CPU-only hardware; if unavailable, mark the deadline formula's long-take path unverified — do not substitute fake inference.)
2. **Slices 2/3a/3b**: dictate a long take, and _while it is sealing/processing_, immediately dictate a second take and open History. Pass = second take records and uploads without "connection cannot keep up", History loads promptly, both takes complete and insert in order. Start a Wispr import (if a source is available) and dictate during it; uploads must not stall.
3. **Slice 4**: queue a take behind a long one and watch it complete normally end-to-end via events (insertion still works). This only proves no regression by eye; the traffic reduction itself is not user-observable — state that in the commit/PR description rather than claiming a measurement.
4. **Slice 5**: fire 3–4 takes back-to-back; the status line shows `Queued on server… (#N)` counting down. Firing takes until the cap refuses (12 active) should show the capacity message at key-press, before speaking. If reaching 12 naturally is impractical in a session, say so and verify the countdown only.
5. **Slice 6**: two back-to-back medium takes — total wall time to second insertion should drop vs Slice 5 alone (proof of take 1 overlaps transcription of take 2); confirm both insert in spoken order and list-continuation ("next point…") across the pair still renumbers correctly.
6. **Slices 7a/7b**: with a USB interface set to 192 kHz (or >2 channels / any >192 kHz device), dictate ~30 s. Pass = transcript inserts normally; History shows the record with the omitted-original note and only "Open audio" (no "Open original"). Then dictate at 48 kHz stereo and confirm "Open original" returns and plays. If no such interface is available, mark 7b's admission path unverified — do not add a test hook or threshold override to force it.

## Open questions ONLY if truly blocking

None. Defaults chosen (change on request): admission cap = 12 non-terminal generations, one named constant, no config knob; speech deadline = `max(180, 60 + 2 × duration)` capped at 900 s; events keepalive = full record at most every 10 s when unchanged, progress coalesced at 250 ms; `proofreading` status is set when the proof stage actually starts (pipelining); original-audio eligibility assumes a full 180 s take (a 192 kHz stereo interface therefore loses its archival copy even for short takes — the visible `originalAudioOmittedReason` explains why); input-device admission in `InputOnlyAudioUnit` is deliberately left wider than archival admission.
