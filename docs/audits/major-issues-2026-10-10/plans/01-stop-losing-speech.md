# Batch 1 plan — Stop losing speech

- Branch: `stack/01-stop-losing-speech` (stacked on `stack/00-audit-plan`, base `audit/major-issues` @ `b6d6815`).
- Sources read in full: `REPORT.md`, `DECISIONS.md`, `AGENTS.md`, `docs/development.md`; audit sections in `01-ux-flows.md` (F3), `04-swift-bugs.md` (F1, F2), `07-audio-pipeline-perf.md` (F1, F6, F7), `10-reliability-errors.md` (F1, F2, F3, F8), `11-crosscheck.md` (merged P1-4 at `:62`, under-graded expiry at `:79`).
- Every code body named below was opened at the cited lines. Line numbers are as of `b6d6815`; re-check on rebase.

## Goal

- Recorded speech is never silently destroyed by a transient failure at any stage: capture, chunk upload, seal, queue, processing, or delivery tracking (REPORT P1-1..P1-4 plus the seal/expiry P2s).
- Concretely: a health-poll blip, a stalled chunk upload, sleep/lock/device-loss, a slow uplink, a full disk at seal time, a server restart, or an app relaunch each leave the take either completed, sealed and recoverable, or honestly reported — never cancelled-and-deleted while durable audio exists on either side.
- Success is judged per slice by the Inlay Dev manual steps in Verification, not by automated tests (`AGENTS.md:4` ban; Batch 8 parity checks do not apply here).

## Decisions applied

- DECISIONS "Interruption: save the speech" (`DECISIONS.md:9`): retry chunk uploads, partial-finish on interruption, requeue sealed takes on restart. No total loss for recorded speech. This retires the old "interruption = total loss" contract (REPORT Q3).
- DECISIONS "Undelivered results: badge + copy" (`DECISIONS.md:11`): finished-but-undelivered text shows as ready to copy on relaunch. **Never auto-paste** — reconciled takes have no captured insertion destination, so `deliver()`/`TextInserter` must not run for them.
- DECISIONS default (`DECISIONS.md:28`): client deadline for a wedged generation is **15 minutes**, then the badge+copy path.
- DECISIONS default (`DECISIONS.md:27`): failed-with-audio records get "transcribe again".
- DECISIONS "Cancel/failure: keep in history" (`DECISIONS.md:14`) overrides REPORT P1-18: **no slice here deletes sealed audio on cancel/failure**, and `cancelRecord`'s keep-sealed behavior (`Server/src/generation-service.ts:798`, `cleanPartial:1234` removes only `.raw`/`.partial`) is relied on, not changed. Labeling kept cancelled audio is Batch 5.
- Swift reference server is deleted in Batch 8 (DECISIONS `:18`): this batch changes **only** the Bun/TypeScript server and the Mac client. `Sources/InlayServerKit/*` is not touched and does not need equivalent fixes.
- Audience is a shareable product (`DECISIONS.md:8`): messages written in this batch must be user-honest (no "check shared history" promise for a take that cannot be there — `InlayController.swift:1233` today).

## Key facts the plan relies on (verified in code)

- Server chunk replay is exact and safe: `appendAudio` at `Server/src/generation-service.ts:551` re-reads the stored bytes, 409s on mismatch (`conflicting_chunk:568`), and returns `{ nextSequence: upload.chunks.length, frameCount: totalFrames }` (`:576`). Because the client flushes strictly sequentially (`ServerClient.upload:356`), a lost response replays to `nextSequence == sequence + 1` and `frameCount == UploadBuffer.frames` (cumulative), so the existing receipt validation at `ServerClient.swift:392` passes unchanged on replay.
- `finish` is already retry-safe: server `finish:634` returns the sealed record when frame counts match; client `ServerClient.finish:236` already retries transient errors 3 times. Only audio chunks lack retry.
- The local capture file is **not** a resend source today: `finishDictation` awaits `stopped.value` (`InlayController.swift:984`), then `audio.cleanup()` (`:990`) without ever reading samples back; upload happens only through `AudioChunkPipe`. Salvage in this batch therefore means _pipe + server-acknowledged audio only_. A durable local outbox is OUT (below).
- `UploadBuffer.frames` (`ServerClient.swift:404`) counts _appended_ frames, not acknowledged frames. Partial-finish needs new acknowledged-frame tracking (Slice 3).
- Server `finish:630` requires: inference frames exact (`:648`), duration 0.25–180 s (`:655`), and when `keepOriginalAudio` (default on, `generation-service.ts:87`) original frames exact plus matching interval within 0.075 s (`:662`, `:669`). Partial-finish can therefore only seal at points where **both** streams' acknowledged intervals align; otherwise the server correctly 400s `audio_mismatch` and the client falls back to cancel + honest message. This is expected, not a bug to work around server-side.
- Continuation (list) context is not persisted: `finish` derives `previous` at seal time (`:685`) and passes it to `enqueueProcessing:1014`. Any requeue/reprocess path must pass `undefined`; the take transcribes standalone. Acceptable and noted per slice.
- `record.updatedAt` advances on chunk append in memory only (`:622`, no `save()`), and the replay early-return (`:576`) does not touch it. The receiving-expiry tick (`:304`) reads the in-memory value, so both facts matter in Slice 2.

## IN scope — commit slices (ordered)

Each slice is one commit on `stack/01-stop-losing-speech`, in this order. Slices 1–2 are independent quick wins; 3 enables 5; 8 enables 9. Do not reorder 5 before 2–3.

### Slice 1 — Health poll no longer kills a live recording (P1-2) · S

- Files/symbols:
  - `Sources/Inlay/InlayController.swift:283` `checkServer(refreshData:)`, catch block `:315`.
- Change: delete the `if isCapturing { failSession(serverStatusMessage, cancelServer: true) }` branch at `:319`. The catch keeps `serverHealth = nil` + `serverStatusMessage = Self.connectionMessage(error)` (`:317`) and the idle-status update (`:320`); while capturing it updates status state only. The upload task remains the sole authority on take health (its own failure path is `InlayController.swift:905` `uploadTask` catch, plus `AudioChunkPipe` `onFailure` at `:895`).
- Also covered by this one-line removal: the same `do` block wraps the post-health `preferences()`/`history()` fetches (`:296`), which `11-crosscheck.md:13` confirmed take the same kill path today.
- Why: REPORT P1-2 / `04 F1`. Health and upload are independent `URLSession` tasks on independent `ServerClient` values; a 5 s poll timeout says nothing about the upload.
- No API/data change. `failSession` (`:757`) and `resetSession` (`:738`) unchanged.

### Slice 2 — Receiving expiry stops killing slow/uplink-stalled takes (P1-1 server half) · S

- Files/symbols:
  - `Server/src/generation-service.ts:296` `tick()`, expiry predicate `:307`.
  - `Server/src/generation-service.ts:551` `appendAudio` replay branch.
- Change:
  - Widen the idle bound 45 s → **120 s** and the total bound 300 s → **900 s** (named constants next to `MAX_CHUNK_BYTES:53`, e.g. `RECEIVING_IDLE_MS`, `RECEIVING_TOTAL_MS`). 900 s = 180 s max recording (`LifecyclePolicy.maximumRecordingSeconds`, `Sources/InlayCore/LifecyclePolicy.swift:4`) + drain/retry allowance, and matches the 15 min client deadline (Slice 8).
  - In the replay branch, before returning the receipt (`:576`), set `record.updatedAt = now()` and `this.records.set(id, record)` exactly as the fresh-append path does (`:622`), so a client retrying a lost-ack chunk (Slice 3) keeps its session alive instead of idling out mid-retry.
- Why: `07 F7`/`05 F6`, regraded as same-loss-class in `11-crosscheck.md:79`. With Slice 3's retry budget (≤60 s) the 120 s idle bound cannot expire a session whose client is actively retrying. Abandoned `receiving` records now linger up to 15 min before `cancelRecord` cleans partials — bounded by the existing 256 MiB/stream cap (`:584`); accepted cost.
- No API/data change (tick behavior only). No heartbeat endpoint — explicitly not added (see OUT).

### Slice 3 — Chunk-flush retry with backoff + acknowledged-frame tracking (P1-1 client half) · M

- Files/symbols:
  - `Sources/Inlay/ServerClient.swift:382` `flush(_:to:)`, `:356` `upload(_:to:preserveOriginal:)`, `:400` `UploadBuffer`, `:315` `isTransient(_:)`.
  - Pattern to mirror: `ServerClient.finish:236` retry loop.
- Change:
  - `UploadBuffer` gains `acknowledgedFrames: Int64 = 0`. In `flush`, after receipt validation (`:392`) set `buffer.acknowledgedFrames = receipt.frameCount` (server returns cumulative frames, `generation-service.ts:578`/`:626`).
  - Wrap the single attempt in `flush` in a retry loop over an immutable snapshot (`data`, `sequence`, `format`, expected `frames`): on error, `try Task.checkCancellation()`, retry only when `Self.isTransient(error)` (`:315` — transient `URLError`s, `.disconnected`, HTTP 408/429/502/503/504), with backoff 0.5 s-doubling capped at 8 s (same cap as `events:311`), until a **60 s total flush deadline** (wall clock from first attempt) — comfortably under Slice 2's 120 s idle expiry. Non-transient errors (`400 invalid_chunk`, `409 missing_chunk`/`conflicting_chunk`/`format_changed`, `413`) throw immediately: replaying them cannot succeed and must surface.
  - `buffer.sequence`/`buffer.data` mutate only after a validated receipt, exactly as today (`:395`), so a retry re-sends byte-identical content — which is what makes the server replay at `generation-service.ts:551` safe. Receipt validation at `:392` is unchanged (see Key facts).
  - `upload()` otherwise unchanged; its final return (`:378`) still reports total appended frames. The acknowledged counts are consumed by Slice 5, so `upload`'s failure path must expose them: change the throw site contract minimally — add `ServerClientError.uploadInterrupted(acknowledged: FinishGenerationRequest, underlying: String)`? No: prefer errors-as-values **without** a new public error case by having `upload` return the acknowledged `FinishGenerationRequest` and record the interruption in a new `UploadOutcome` struct `{ finish: FinishGenerationRequest, complete: Bool }` (internal to `ServerClient.swift`). Chosen default: `upload` returns `UploadOutcome`; `complete == true` on clean drain (today's behavior), `complete == false` with acknowledged counts when a flush ultimately throws non-cancellation. Update the sole caller `InlayController.swift:905` `uploadTask` type (`Task<FinishGenerationRequest, Error>` at `:173`, `:190`, `:960`) to `Task<UploadOutcome, Error>` in this slice, treating `complete == false` while still capturing as today's `failSession` until Slice 5 replaces that handling. Keep `CancellationError` as a throw (cancel must stay cancel — user intent, DECISIONS keep-in-history notwithstanding).
- Why: REPORT P1-1 (`01 F3 = 07 F1 = 10 F1`, confirmed `11-crosscheck.md:12`). The server already fsyncs each chunk (`generation-service.ts:614`); one Wi-Fi stall should cost a retry, not 180 s of speech.
- Risk note inside the slice: during a long flush retry, capture keeps filling `AudioChunkPipe` (512-buffer, `ServerClient.swift:132`); overflow still fails via `.uploadBacklog` (`:139`). That is pre-existing backpressure behavior and stays (see OUT); the 60 s budget bounds how long it can be exercised by one flush.
- No API/data change. No `as any`, no new dependencies.

### Slice 4 — Seal space pre-check; seal failure keeps the raw audio (P2 seal) · S–M

- Files/symbols:
  - `Server/src/storage.ts:95` `requireDiskSpace(dataDirectory)`.
  - `Server/src/generation-service.ts:630` `finish()`, seal calls `:687`, failure catch `:693`; `seal():928`; `cleanPartial():1234`.
- Change:
  - `storage.ts`: add `requireDiskSpaceFor(dataDirectory, additionalBytes: number)` — same `statfs` read, throws the existing `ServiceError(507, "storage_full", …)` when `bavail * bsize < 100 MiB + additionalBytes`. Reimplement `requireDiskSpace` as `requireDiskSpaceFor(dir, 0)` so create/append call sites (`:445`, `:590`) are untouched.
  - `finish`: after all frame/duration/original validation (`:648`–`:684`) and **before** the first `seal` (`:687`), call `requireDiskSpaceFor(dir, speech.bytes + 44 + (original ? original.bytes + 44 : 0))`. On 507 nothing has been written or deleted; the record stays `receiving` with `.raw` intact.
  - Seal-failure catch (`:693`): stop destroying the only copy. Replace mark-failed + `cleanPartial` with: clear the in-memory `record.inferenceAudio`/`record.originalAudio` assignments from `:687`, leave `record.status == "receiving"`, remove **only** `inference.wav.partial`/`original.wav.partial` (add a `cleanSealPartials(id)` helper or a parameter on `cleanPartial` — chosen default: small separate helper next to `cleanPartial:1234`), `save()`/publish, and rethrow `ServiceError(500, "audio_storage_failed", …)` as today. A later `finish` with the same counts can then succeed (frame validation at `:648` reads the still-present `uploads` map + `.raw` files). One subtlety to handle in the same edit: if inference sealed but original failed, `inference.wav` exists while `inference.raw` was already removed by `seal:985`; the retry must not re-seal a missing raw. Handle by checking in `finish` before sealing each kind: if `record.inferenceAudio` was restored from a prior partial attempt… — simpler chosen default: on catch, also delete any completed `.wav` produced **by this attempt** only if its `.raw` still exists; if the raw is gone (seal fully succeeded for that kind), keep the artifact assignment for that kind and make the retry path skip kinds whose `record.<kind>Audio` is already set. Implementer must cover both orders; keep the logic inside `finish`, do not change `seal()` itself.
- Why: `10 F8` seal half (REPORT P2, `:66`). Sealing needs a second full copy far beyond the 100 MiB floor; today a transient full disk deletes a 100%-uploaded take (`:693` + `cleanPartial`).
- Client half of 507 handling (do not server-cancel; retry on relaunch) lands in Slice 9, where persistence exists. Until then, a 507 behaves like other `finish` failures client-side but the server no longer destroys the raw — strictly better, and Slice 9 closes the loop.
- No API/data change.

### Slice 5 — Partial-finish on interruption (sleep / lock / device-loss / upload death) · M–L

- Files/symbols:
  - `Sources/Inlay/System/AudioRecorder.swift:190` `interrupt(id:message:)`, `:181` `cancel()`, `:172` `stopCapture()`, `:158` `stopAcceptingAudio()`; sleep observer `:112`.
  - `Sources/Inlay/InlayController.swift:822` `recorder.onInterruption` binding, `:1229` `restForSystem()`, `:938` `finishDictation(atLimit:)`, `:186` `PendingDictation`, `:757` `failSession(_:cancelServer:)`, `:1165` `stopPendingDictations()`.
  - Consumes Slice 3's `UploadOutcome`.
- Change:
  - **Recorder:** `interrupt(id:message:)` no longer calls `cancel()` (which discards the capture via `worker.cancel`, `AudioRecorder.swift:187`). It only forwards `onInterruption?(message)`. The controller's salvage path then owns teardown through the existing `stopCapture()` route, exactly like a key release. `cancel()` remains for explicit user cancel and `resetSession` (`InlayController.swift:746`).
  - **Controller:** add `func salvageDictation(reason: String)` and route three callers to it:
    1. `recorder.onInterruption` (`:822`): `failSession(…, cancelServer: true)` → `salvageDictation(reason: message)`.
    2. `restForSystem()` (`:1229`): the `isCapturing` branch (`:1232`) → `salvageDictation(…)`; and remove the unconditional `stopPendingDictations()` (`:1235`) — sealed pendings are no longer cancelled/disowned on sleep; their `events` recovery (Slice 8) + persistence (Slice 9) own them across the sleep. Keep `continuationAnchors.removeAll()` and replace the false "Check shared history" copy (`:1233`, `:1237`) with outcome-accurate text (below).
    3. `uploadTask` completion with `UploadOutcome.complete == false` while still capturing (`:905`): instead of `failSession`, stop capture and attempt the same salvage finish with acknowledged counts (the already-fsynced server audio). Implement by funnelling into the same `PendingDictation` pipeline as `finishDictation`, not a second pipeline.
  - `salvageDictation` is `finishDictation` with three deltas — implement as a parameter, chosen default `finishDictation(atLimit:salvaged:)` or a small `FinishKind` enum, not a copied function:
    - **Forced clipboard destination:** `knownDestination` (`:963`) is `.clipboard` regardless of the captured target. After sleep/device-loss the original field/session cannot be trusted; the result surfaces via the existing copied/last-transcript path (`deliver:1093` clipboard branch) or, if the app relaunches first, Slice 9's badge. Never insert.
    - **Recorder-stop tolerance:** `stopped.value` (`:984`) may throw (`noAudio`/processing) because the device is gone. In salvaged mode, catch that specific failure, treat `capturedAudio` as `nil`, and continue to `pending.pipe.finish()` + upload drain — the streamed/acknowledged audio is the salvage, the local file never was (Key facts). Non-salvaged mode unchanged.
    - **Acknowledged-count finish:** when `upload` returns `complete == false`, build `finish` from `UploadOutcome.finish` (acknowledged frames only). If acknowledged inference < 0.25 s (4 000 frames at 16 kHz; server floor `generation-service.ts:655`), skip the server `finish` attempt, server-cancel, and report the take as too short to save. Otherwise attempt `connection.finish` as today (`:1007`); a 400 `audio_mismatch` (un-aligned original tail) falls into the existing catch (`:1044`), which already cancels only when unsealed — but the salvaged error copy must say the recording could not be saved, not "check history".
  - The existing `<0.25 s` release guard (`:948`) and `activity == .recording` guard (`:945`) apply to salvage too: an interruption during `.starting` (no generation/audio yet) keeps today's cancel behavior via `cancelDictation()` (`:945` path).
  - `PendingDictation` (`:186`) gains `let salvaged: Bool` so progress/error copy and Slice 9 persistence can distinguish it. No new activity state.
- Why: REPORT P1-1 interruption paths (`01 F3`, `10 F1`: lid close, AirPods death, route loss at second 170). DECISIONS `:9` makes "save what was said" the contract.
- Honest limit (state in commit message): on `willSleep` the finish request is best-effort — macOS may suspend the process before it lands. What survives is whatever the server already fsynced; Slice 2 keeps the `receiving` session alive for 15 min and Slice 9 retries the finish on wake/relaunch. Do not block sleep and do not claim otherwise in UI copy.
- No API/data change (server `finish` contract untouched). Swift-only slice.

### Slice 6 — Startup requeues sealed records; shutdown preserves them (P1-3) · M

- Files/symbols:
  - `Server/src/generation-service.ts:209` `initialize()`, non-terminal reconciliation `:272`; `:315` `shutdown()`; `:1014` `enqueueProcessing(id, previous)`; `:1031` `process(…)` catch `:1124`; `:1234` `cleanPartial`.
- Change:
  - `initialize`: split the `!terminal(record)` branch (`:272`) by sealed-ness:
    - `status == "receiving"` (never sealed): unchanged — mark `failed` "Server restarted before this generation completed.", `cleanPartial`, save.
    - `queued`/`transcribing`/`proofreading` **with** `record.inferenceAudio` set and `inference.wav` present as a regular file in `directory(id)` (check with the same `open(O_RDONLY|O_NOFOLLOW)` + `stat().isFile()` discipline as `artifact():890`): reset to `status = "queued"`, `progress = 0`, delete `error`, `updatedAt = now()`, save, and collect the id. After the directory scan completes (end of `initialize`), `enqueueProcessing(id, undefined)` for each collected id in `createdAt` order. Continuation context is lost (Key facts) — accepted.
    - Sealed-status but WAV missing/unreadable: mark `failed` with a naming error ("Sealed audio is missing…"), `cleanPartial`, save. Do not throw — one damaged record must not brick startup here (full quarantine is Batch 3; this slice only avoids _adding_ a new throw).
  - `shutdown`: stop cancelling sealed work. Reorder to: set `stopping`, clear timer, abort `warmController` and every `processingControllers` entry; `await inference.shutdown()`; await `processingQueue`/`warmTask` settle (existing `Promise.allSettled:325`); **then** one `mutate` that (a) `cancelRecord`s only `receiving` records, and (b) resets every remaining non-terminal sealed record to `queued` (delete `progress`, save). The reorder matters: `process`'s catch (`:1124`) marks aborted jobs `cancelled` — running the reset _after_ the queue drains makes the queued state the final write. Add the belt-and-braces guard in that catch anyway: if `this.stopping && record.inferenceAudio`, write `queued` instead of `cancelled`/`failed`.
  - `process` re-entry is already safe: it re-reads the record, sets `transcribing` (`:1041`), and re-transcribes from `inference.wav` (`:1048`) overwriting `rawText` etc. A take interrupted in proofreading redoes transcription — wasteful, correct, and the simplest correct thing. Do not add stage-resume.
- Why: REPORT P1-3 (`07 F6 = 10 F2`, confirmed `11-crosscheck.md:14`). The WAV was fsynced at seal (`seal:977`); restart must be a pause, not a deletion.
- Migration: records already marked `failed` ("Server restarted…") by previous builds are **not** auto-resurrected (their failure may be genuine); Slice 7 gives the user the manual path. No schema change (`schemaVersion` stays 1, `:259`).
- Server-only slice. No API change.

### Slice 7 — "Transcribe again" for failed/cancelled records with sealed audio · M

- Files/symbols:
  - `Server/api/openapi.yaml:222` (cancel path block — add sibling path after it); generated outputs via `bun run generate:api` only (`Server/src/generated/api.ts`, Swift wire — never hand-edit).
  - `Server/src/http-server.ts:178` (cancel route — add sibling route); `Server/src/generation-service.ts:795` `cancel(id)` (add sibling `reprocess(id)`); `:1014` `enqueueProcessing`.
  - `Sources/Inlay/ServerClient.swift:251` `cancel(_:)` (add sibling `reprocess(_:)`); `Sources/Inlay/InlayController.swift:664` `deleteGeneration(_:)` (add sibling `transcribeAgain(_:)`); `Sources/Inlay/Views/HistoryPage.swift:226` (audio actions block).
- API change: `POST /v1/generations/{id}/reprocess`, `operationId: "reprocessGeneration"`, returns the `GenerationRecord` with **202** (mirrors `finishGeneration`, `openapi.yaml:197`). No request body. No `apiVersion` bump: the route is additive and `InlayAPI.version` gating (`InlayController.swift:151`) is lockstep client+server by deployment; chosen default is additive-without-bump, consistent with DECISIONS `:33` (breaking changes may bump — this one is not breaking).
- Service `reprocess(id)` (inside `mutate`, mirroring `cancel:795`):
  - Reject: imported records (`record.importedSource` → 400, mirror `recordDelivery:813`); non-terminal records (409 `generation_active`, mirror `delete:859`); records without `inferenceAudio` or with missing `inference.wav` (404 `artifact_not_found` discipline as Slice 6); `stopping` (503, mirror `create:415`).
  - Accept `failed` **and** `cancelled` with sealed audio (DECISIONS keep-in-history `:14` + transcribe-again default `:27` — a post-seal cancel is exactly a take the user may want back). Completed records: reject 409 (re-transcribing a delivered take would rewrite delivered history text; out of scope).
  - Reset on the same record (no new generation id — history keeps one row): `status = "queued"`, `progress = 0`, delete `error`, clear `rawText`/`finalText`/`insertionText`/`previewText` to `""` (as `newRecord:476`), delete `speech`/`proofreading`/`textProcessing`/`continuation`/`delivery`/`detectedLanguage`/`recognitionHints`/`proofreadingHints`/`formattingRejectionReason`/`consumedListControls`, `updatedAt = now()`, `save()`, `enqueueProcessing(id, undefined)`. Settings snapshot (`record.settings`) is **kept** — reprocess with the preferences the take was recorded under; chosen default, note in commit message.
- Client: `ServerClient.reprocess(_:) async throws -> GenerationRecord` via existing `json(path:method:)` (`:68`). `InlayController.transcribeAgain(_ id:)` mirrors `deleteGeneration:664`: call, then `refreshServer()`, surface errors via `errorMessage`. It does **not** attach delivery — completion lands in history, and if it was a persisted pending id, Slice 9's badge picks it up. `HistoryPage`: in the selected-record actions, show a `Transcribe again` button when `(status == .failed || status == .cancelled) && inferenceAudio != nil && importedSource == nil`, next to `Open audio` (`:226`). Plain button, no new card/section (per repo UI preferences).
- Why: REPORT P1-3 second half ("no reprocess route exists", `10 F2`); also the manual recovery for every record Slice 6 cannot auto-requeue (pre-existing restart-failures, helper failures).
- Data: no schema/migration change; record fields reused.

### Slice 8 — Events gets a 15-minute wall-clock deadline (P1-4 wedge half) · S–M

- Files/symbols:
  - `Sources/Inlay/ServerClient.swift:286` `events(_:onUpdate:)`, recovery reset `:304`, give-up `:309`, `:5` `ServerClientError`; caller `Sources/Inlay/InlayController.swift:1013`.
- Change:
  - Capture `let deadline = ContinuousClock.now.advanced(by: .seconds(900))` (15 min per DECISIONS `:28`) at `events` entry. Check it at the top of the `while true` loop (`:288`) and cap the recovery sleep (`:311`) at the remaining time. On expiry throw a **new** `ServerClientError.processingTimedOut` whose `errorDescription` says the server is still processing and the text will be ready to copy — it must not read as "failed" and must not be classified transient by `isTransient:315` (new cases default to `false` there — verify the switch at `:320` when adding).
  - Keep both existing regimes intact inside the deadline: reachable-but-non-terminal re-fetches still reset `recoveryFailures` (`:304`, the wedge regime — now bounded), and 5 consecutive failed re-fetches still throw early (`:309`, the outage regime). This is the merged behavior `11-crosscheck.md:62` describes; do not "fix" either counter away.
  - Controller (`:1013` call site, catch at `:1044`): `pending.sealed` already prevents a server-cancel on this throw (`:1046`). Add one refinement in the same slice: when the thrown error is `.processingTimedOut`, set an informational status ("Still processing on the server") instead of `showError`'s failed treatment (`:1050`), and leave the take to Slice 9 persistence. The throw still unwinds the `PendingDictation` task, which is the point: `deliveryTail` (`:971`, `:1066`) unblocks so later takes deliver.
- Why: REPORT P1-4 wedge half (`04 F2` regime): a reachable server + stuck generation currently blocks the serialized delivery chain forever.
- No API/data change. `URLSession` timeouts (`ServerClient.swift:44`) unchanged.

### Slice 9 — Persisted pending IDs + badge-and-copy on relaunch (P1-4 orphan half) · M–L

- Files/symbols:
  - New `Sources/Inlay/PendingGenerationsStore.swift` (sibling of `ClientPreferencesStore`, `Sources/Inlay/ClientPreferences.swift:10`).
  - `Sources/Inlay/InlayController.swift:233` (store construction next to `preferences`), `:186` `PendingDictation`, `:960` (pending creation), `:1005` `pending.sealed = true`, `:975` (pending `defer` cleanup), `:703` `cancelDictation()`, `:1165` `stopPendingDictations()`, `:253` `monitorTask` 5 s loop, `:283` `checkServer`, `:766` `copyLastTranscript()`, `:225` `ServerClient.generation(_:)`, `:257` `ServerClient.delivery(_:receipt:)`.
  - Minimal UI surface: `Sources/Inlay/Views/MenuHUDViews.swift:39` (existing "Copy last message" menu item) and/or the Dictation page last-transcript block `Sources/Inlay/Views/InlayWindowView.swift:239`. Chosen default: menu item(s) + one plain row on the Dictation page; no new window, page, or card.
- Store design (mirror `ClientPreferencesStore.persist():82` discipline — atomic write, dir 0700, file 0600, errors-as-values `Bool`):
  - File `pending-generations.json` in the same root as `client.json` (`configuration.url.deletingLastPathComponent()`, `InlayController.swift:233`).
  - Entry: `{ id: UUID, endpoint: String, inferenceFrames: Int64, originalFrames: Int64?, isTest: Bool, sealedAt: Date }`. Frame counts are included **only** to retry a Slice-4 507 finish (below); endpoint scopes entries so switching servers (via `saveConnection:324`, which must clear the store) never badges another server's takes. No transcript text, no token, no destination is ever stored.
  - Write when `pending.sealed` becomes true (`:1005`, i.e. a finish was attempted and may have landed). Remove in the pending task `defer` (`:975`) on every terminal outcome (delivered, failed, cancelled, proven-unsealed `pending.sealed = false` at `:1009`), and on explicit cancel paths (`:703`, `:1165`). A crash/quit simply leaves the file — that is the feature. `shutdown():790` must **not** clear it for sealed pendings; today's `stopPendingDictations` server-cancel-only-if-unsealed rule (`:1168`) already encodes the right ownership split — keep it.
- Reconciliation (run after the first successful `checkServer` history load, then on each `monitorTask` tick while entries remain — reuse the existing 5 s loop, no new timer):
  - For each entry whose `endpoint == preferences.endpoint`, fetch `client().generation(id)`:
    - `completed` with `delivery == nil` → badge state: publish `@Published private(set) var undeliveredResults: [GenerationRecord]` on the controller. **Copy action only:** `DictationClipboard.copy(record.finalText)` (same helper as `copyLastTranscript:768`), then best-effort `delivery(id, receipt: DeliveryReceipt(status: "copied", message: "Copied after relaunch"))` (server validates completed + allowed status, `generation-service.ts:819`), then remove the entry. Never call `deliver()`/`TextInserter` (DECISIONS `:11`).
    - `completed` with `delivery != nil` → already delivered before quit (receipt landed, local cleanup didn't): remove entry silently.
    - `failed`/`cancelled` → remove entry; the row is visible in History (and transcribable via Slice 7). No badge, no error HUD.
    - Non-terminal non-`receiving` → keep entry, show in the same surface as "still processing"; the next tick re-checks. Do not re-attach `events()` in this slice (polling the durable record is sufficient and cannot wedge delivery).
    - Still `receiving` **and** the entry carries frame counts → the Slice-4 507 case (or a finish lost to sleep in Slice 5): retry `connection.finish(id, value:)` once per reconciliation pass; on success it flows to the non-terminal case next tick; on a 4xx rejection remove the entry (server says the audio is not finishable — history/error copy covers it). Bound this by the server's own 15 min receiving expiry (Slice 2): past that the fetch returns `cancelled` and the entry self-cleans.
    - Fetch throws (offline) → keep entry, try next tick. Entries older than 7 days are dropped unread (chosen default; prevents a permanently-offline server from badging stale text forever).
  - Test-mode entries (`isTest`) reconcile identically but copy shows the test transcript; do not special-case beyond the label. (Excluding test takes from shared history is a Batch 5 default, not this batch.)
- Slice 4/8 tie-ins completed here: in `finishDictation`'s catch (`:1044`), a `.rejected(507, _)` from `finish` must behave like sealed for cancel purposes — set/keep a persisted entry and skip the `connection.cancel(id)` at `:1046`, with copy telling the user to free server disk space. `.processingTimedOut` (Slice 8) likewise leaves its entry in place (already the case if removal only happens on terminal outcomes — implementer: the Slice 8 throw reaches the same `defer` at `:975`, so removal logic must key off outcome, not task end. Chosen default: `PendingDictation` gets `var resolved = false`, set true only on delivered/failed/cancelled/proven-unsealed; the `defer` removes the persisted entry only when `resolved`.)
- Why: REPORT P1-4 orphan half (`10 F3` regime) + quit-during-processing (`10 F6` sealed half). Results that complete while the app is gone currently land in history silently; DECISIONS `:11` requires badge+copy.
- Data: one new client-local JSON file (above). No server/API change. No migration (absent file = empty).

## API / data changes + migration (summary)

- API additions (Slice 7 only): `POST /v1/generations/{id}/reprocess` → 202 `GenerationRecord`, `operationId: reprocessGeneration`. Additive; no `apiVersion` bump; no new schemas. Regenerate with `bun run generate:api`, verify with `bun run generate:api --check`; commit the regenerated TS + Swift outputs in the same slice. Old client + new server: unaffected. New client + old server: `Transcribe again` gets a 404 surfaced via `errorMessage` — acceptable lockstep deployment (same posture as the Wispr 404 handling at `InlayController.swift:430`).
- Server data: **no format changes**. `GenerationRecord.schemaVersion` stays 1; `metadata.json` contents are only reset, never reshaped (Slices 6–7). Existing archives need no migration. Pre-existing restart-killed sealed records remain `failed` until the user hits Transcribe again (Slice 7) — deliberate.
- Client data: one new file `pending-generations.json` next to `client.json` (Slice 9), absent = empty, endpoint-scoped, cleared on connection change (`saveConnection:324`). `client.json`/`Settings` (`ClientPreferences.swift:11`) untouched.
- Behavior-contract changes to state in commit messages: interruption no longer means total loss (DECISIONS `:9`); server shutdown no longer cancels sealed takes (Slice 6) — operators restarting for upgrade will see queued takes resume, which is the point.
- Rollback: every slice is independently revertible; reverting Slice 6 reinstates restart-fails-sealed (records queued by Slice 6 would then fail on next old-build startup — they remain transcribable only if Slice 7 is present, so revert 7 before 6 if ever needed).

## OUT of scope / deferred

- Durable local outbox / local-WAV resend as a new generation (`01 F3` L-version, `10 F1` third suggestion): the local file is currently deleted unread (`InlayController.swift:990`); building a resend path is a new subsystem with its own privacy posture (audio at rest on the client). Not needed once Slices 3+5 salvage the streamed/acknowledged audio.
- `AudioChunkPipe` resize or pause-capture backpressure (`ServerClient.swift:132`): overflow behavior unchanged in this batch.
- Server throughput: per-generation locks, seal-time-only fsync, stage pipelining, queue position (P1-8/P1-9 = Batch 4). Slice 2/4 deliberately avoid touching `mutate:196` or `processingQueue:1019` structure.
- Structured logging / quarantine-and-serve startup / helper error taxonomy (P1-5/P1-6/P1-7 = Batch 3). Slice 6 adds no new startup throws but does not fix the existing brick-on-corrupt-record behavior (`generation-service.ts:252`).
- Outcome visibility beyond the badge: VoiceOver announcements, copied=attention HUD, stage text (P0-1/P1-12/P1-13 = Batch 2). Slice 5's forced-clipboard results use today's copied presentation until Batch 2.
- Cancel-deletes-audio (rejected by DECISIONS `:14`), retention caps, per-device tokens (Batch 5); event-tap hold cancellation P2 (`04 F4`); mid-delivery cancel receipt loss P2 (`04 F6`, `InlayController.swift:1123`); persistent-mute self-heal P2 — all untouched.
- Swift reference server parity or deletion (Batch 8); no deterministic parity checks are added by this batch (DECISIONS `:21` permits them only as Batch 8's vehicle).

## Risks

- **Sleep race remains best-effort (Slice 5):** `willSleep` may suspend the process before the salvage `finish` lands, and `AudioRecorder`'s own sleep observer (`AudioRecorder.swift:112`) plus `restForSystem` (`InlayController.swift:1219`) both fire — the implementation must ensure exactly one salvage runs (single `finishDictation` entry guarded by `isCapturing`/pending handoff, as the release path already is). Mitigated, not eliminated, by Slices 2+9.
- **Double-stream alignment (Slice 5):** when `keepOriginalAudio` is on, an upload death between the inference flush and the original flush (`ServerClient.swift:370`) leaves acknowledged intervals misaligned by up to ~0.5 s of inference / 768 KB of original; the server will 400 `audio_mismatch` and that take is still lost. Frequency is unmeasured (no logging until Batch 3). If Inlay Dev verification shows this dominates, the follow-up is aligning flush pairs before acknowledging — a Batch 1.1, not a server-contract change.
- **Retry vs. pipe backpressure (Slice 3):** a 60 s flush retry on a dead link will likely trip `.uploadBacklog` first and convert a recoverable stall into the existing explicit failure. Acceptable for this batch; the failure is at least honest and bounded.
- **Restart storms (Slice 6):** a take that deterministically crashes processing will now be requeued on every restart instead of failing once. `process`'s catch (`:1124`) still marks genuine inference errors `failed`, so only process-death loops repeat — visible in history, deletable, and strictly better than silent loss. No attempt counter in this batch (less is more; revisit with Batch 3 logging).
- **Reprocess rewrites text (Slice 7):** re-running with the recorded settings snapshot can produce different text than a failed attempt's partial artifacts; fields are cleared first so no stale `finalText` can be mistaken for output mid-reprocess. Completed records are excluded so delivered text is never rewritten.
- **Badge staleness (Slice 9):** without the 7-day drop and endpoint scoping, a months-old server's completed take could badge unexpectedly; both guards are in the slice. Copy is always an explicit user action, so clipboard is never clobbered unasked.
- **No field observability:** until Batch 3, none of these paths log (server `logger:false`, `Server/src/http-server.ts:60`). Verification below is therefore entirely owner-observed behavior in Inlay Dev + archive inspection; frequencies of every path remain unknown.

## Verification

Per `AGENTS.md:5`–`:9` and `docs/development.md:36`. No automated behavioral tests are added or run. Run from the worktree root unless noted.

Static/build gates, after every slice (all must pass; fix rather than suppress):

```sh
bun run check        # strict TS + generated API consistency (AGENTS.md:5)
bun run lint         # type-aware oxlint, warnings fail
bun run fmt          # required after any TS edit (AGENTS.md:3); then bun run fmt:check
```

Swift/API slices (1, 3, 5, 7, 8, 9) additionally:

```sh
bun run generate:api --check   # Slice 7: run `bun run generate:api` first, commit outputs
swift build --force-resolved-versions   # macOS, Swift 6.2+ (AGENTS.md:9)
./scripts/build-dev-app.sh     # produce build/Inlay Dev.app for manual steps
```

Runtime setup for **all** manual steps (isolated instance only; never touch an existing server, archive, or settings — `docs/development.md:40`):

```sh
bun install --frozen-lockfile
bun run dev    # 0.0.0.0:8392, data in .local/dev-server/data, private token file
mkdir -p .local/dev-client && chmod 700 .local/dev-client
open --env "INLAY_CLIENT_DATA_DIR=$PWD/.local/dev-client" "build/Inlay Dev.app"
```

- Point Inlay Dev at `http://localhost:8392` with the dev token; complete real model setup (`docs/development.md:18`, `./scripts/build-server.sh`, `INLAY_SPEECH_MODEL`, `INLAY_TEXT_MODEL`). `ready:false` verifies nothing about dictation. If models/devices are unavailable, record that explicitly and mark the affected steps unverified — do not substitute fake inference (`AGENTS.md:7`).

Inlay Dev manual steps for the owner, per slice (observe and record actual outcomes):

1. **Slice 1:** Start a dictation in a text field; mid-take, take the server briefly unreachable _for health only_ is impractical — instead pause the dev server process (`SIGSTOP`) for ~6 s (one poll misses), resume (`SIGCONT`), keep speaking, release. Expect: recording never cancels from the poll; insert succeeds or fails only via the upload path. Also confirm idle status still shows offline during a poll failure when _not_ recording.
2. **Slice 2:** Start a take, `SIGSTOP` the server 60 s mid-recording, `SIGCONT`, release. Expect: take seals and completes (no 45 s `receiving` expiry). Then start a take and kill the _client_ mid-upload; confirm via History that the record is cleaned up as cancelled within ~15 min, not left `receiving` forever.
3. **Slice 3:** Record 60–120 s while toggling Wi-Fi off for 10–20 s mid-take (or `SIGSTOP`/`SIGCONT` the server twice). Expect: recording survives, transcript contains speech from before _and_ after the gap. Repeat with the server fully stopped mid-take: expect an honest failure only after the retry budget, not instantly.
4. **Slice 4:** Fill is intrusive — instead point a scratch dev archive (`bun run dev --data-dir "$PWD/.local/tiny-archive"`) at a small `hdiutil` disk image with <2× a long take's audio free, dictate 120 s, release. Expect: a clear storage error, the record still `receiving` with `.raw` files present on disk (inspect the archive dir), and after freeing space the same take can finish (via Slice 9 relaunch once landed; before Slice 9, verify the raw survives and state the finish-retry gap).
5. **Slice 5:** Three interruption drills at ~60 s in: (a) close the lid / lock the screen for 30 s, unlock; (b) disconnect the active mic (unplug or power off AirPods); (c) stop the server mid-upload. Expect each take to appear as a salvaged result (clipboard-ready, **not** auto-inserted into whatever is focused after unlock) or in History, with the pre-interruption speech present. Dictate <0.25 s and interrupt: expect a clean too-short/cancelled outcome, no hang.
6. **Slice 6:** Queue two long takes, `Ctrl-C` the dev server while the second is queued/first is transcribing, restart `bun run dev`. Expect: both eventually `completed` in History with no user action; none shows "Server restarted before this generation completed." Repeat by `kill -9`-ing the server (crash path through `initialize`, not `shutdown`). Start a take and kill the server _while still holding_ (unsealed): expect that one to show the restarted `failed` message — the accepted remaining loss.
7. **Slice 7:** From the Slice 6 unsealed-failure control and a deliberately failed take (e.g. stop helpers so inference fails), open History, select a `Failed` record with audio. Expect: `Transcribe again` visible; clicking returns it to processing and then `completed` with fresh text; the button is absent for completed, imported, and audio-less records.
8. **Slice 8:** Hard to wedge naturally — approximate by `SIGSTOP`-ing the server _after_ sealing (release first, stop during "Transcribing…") and leaving it stopped: expect the client to stop waiting with the still-processing message and — critically — a subsequent dictation (after `SIGCONT`) to deliver normally instead of queuing forever behind the wedged one. Timed confirmation of the 15 min bound itself is code-reviewed, not stopwatch-verified; record it as such.
9. **Slice 9:** Seal a take, quit Inlay Dev while it processes, wait for completion, relaunch with the same `INLAY_CLIENT_DATA_DIR`. Expect: a ready-to-copy badge; clipboard unchanged until Copy is clicked; after copying, re-quit/relaunch shows no badge (delivery recorded). Repeat with an endpoint change in between (expect no badge). Force-quit (`kill -9`) mid-processing instead of clean quit: same badge expectation.
10. **Regression sweep (whole batch):** One normal hold-dictation inserts at the cursor; one Test microphone take pastes nothing; one explicit Esc-cancel while capturing leaves no transcript and (per DECISIONS keep-in-history) behaves exactly as before this batch; preferences save/reload and History paging still work.

Record per step: server archive used, Inlay Dev data dir, `ready` state, and observed vs expected. State explicitly which steps could not run (models, hardware, sleep/lock on the verification machine).

## Open questions

- None blocking. Chosen defaults, recorded above for owner override: retry budget 60 s/flush with 8 s cap (Slice 3); receiving bounds 120 s idle / 900 s total (Slice 2); salvaged takes force clipboard, never insert (Slice 5); reprocess keeps the recorded settings snapshot and includes post-seal `cancelled` records (Slice 7); no `apiVersion` bump for the additive route (Slice 7); pending entries drop after 7 days (Slice 9).
