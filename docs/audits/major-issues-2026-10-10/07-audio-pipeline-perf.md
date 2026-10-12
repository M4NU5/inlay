# Audio + transcription pipeline + performance — major issues (2026-10-10)

- Scope: capture (`Sources/Inlay/System/AudioRecorder.swift`, `AudioCaptureSession.swift`, `InputOnlyAudioUnit.swift`), upload (`Sources/Inlay/ServerClient.swift`, `Sources/Inlay/InlayController.swift`), server pipeline (`Server/src/generation-service.ts`, `Server/src/http-server.ts`, `Server/src/storage.ts`), helpers (`Engine/worker.cpp`, `TextEngine/worker.cpp`, `TextEngine/Sources/InlayTextEngine/CorrectionEngine.swift`, `Server/src/inference/*`). Worktree HEAD `b6d6815`.
- Method: code-body reads only. No tests run and no inference performed, per `AGENTS.md:4` and `AGENTS.md:7`. "Verified-in-code" = the cited body was opened; runtime magnitudes are marked inferred.
- Headline: the happy path is carefully bounded (180 s cap, sequenced chunks, warm helpers, per-request state reset), but failure handling is all-or-nothing at every stage — one transient network error, one routine helper error, or one server restart destroys work that the system already holds durably. Performance risk concentrates in three serial bottlenecks: the client's single upload stream, the server's global mutation lock with per-chunk fsync, and the single FIFO inference queue.

## Findings

### 1. Any transient upload failure destroys the whole take, despite a complete local recording and idempotent server replay

- Severity: P1
- Evidence:
  - Upload has no per-chunk retry: `flush` does one `session.data` and throws (`Sources/Inlay/ServerClient.swift:382`), and the session request timeout is 12 s (`Sources/Inlay/ServerClient.swift:44`).
  - The in-memory pipe holds at most 512 chunks; on overflow it finishes with `uploadBacklog` and fires the failure callback (`Sources/Inlay/ServerClient.swift:132`).
  - Either failure calls `failSession(..., cancelServer: true)` (`Sources/Inlay/InlayController.swift:895`, `Sources/Inlay/InlayController.swift:905`), which cancels the server generation (`Sources/Inlay/InlayController.swift:757`) and calls `recorder.cancel()` (`Sources/Inlay/InlayController.swift:746`), discarding the local WAV (`Sources/Inlay/System/AudioRecorder.swift:616`).
  - The local WAV is never a resend source: on the success path it is awaited only to gate pipe finish, then deleted (`Sources/Inlay/InlayController.swift:984`).
  - The server already supports safe replay: an exact-repeat chunk is verified against the stored bytes and re-acknowledged (`Server/src/generation-service.ts:551`). Only `finish` uses that idempotency (`Sources/Inlay/ServerClient.swift:236`); audio chunks do not.
  - Same all-or-nothing shape for device/sleep interruptions: route loss, sleep, and lock all funnel into `failSession` + server cancel (`Sources/Inlay/InlayController.swift:822`, `Sources/Inlay/InlayController.swift:1232`), so the already-recorded portion is never offered as a shorter take.
- User experience: a Wi-Fi stall, a sleeping remote server, a 12 s hiccup on one ≤1 MiB POST, or unplugging the mic at second 170 of a 180 s dictation loses the entire dictation. The user must re-speak everything; nothing appears in history.
- Confidence: verified-in-code (loss mechanics); frequency on real networks inferred.
- Fix direction: retry `flush` on transient errors with the same sequence number (server replay already makes this safe), and treat the local `CapturedAudio` WAV as the resend source of truth on failure instead of deleting/discarding it. Salvaging partial takes on interruption is an owner decision (see Questions).
- Effort: M

### 2. Every helper "error" response kills the warm model process, including routine per-request errors

- Severity: P1
- Evidence:
  - `HelperProcess.receive` maps any `error` event to `reset()` (`Server/src/inference/helper-process.ts:300`), and `reset` destroys stdin and retires the child with SIGTERM→SIGKILL (`Server/src/inference/helper-process.ts:336`, `Server/src/inference/helper-process.ts:357`).
  - Routine, non-fatal conditions are reported exactly this way: speech VAD failure / bad WAV / transcription failure (`Engine/worker.cpp:246`, `Engine/worker.cpp:258`, `Engine/worker.cpp:284`); Qwen context overflow, 15 s deadline, output-limit, empty output (`TextEngine/worker.cpp:151`, `TextEngine/worker.cpp:176`, `TextEngine/worker.cpp:202`). The MLX helper additionally hard-exits its own process 2 s after its 15 s deadline (`TextEngine/Sources/InlayTextEngine/CorrectionEngine.swift:22`).
  - Response-validation failures go further and await full helper shutdown inside the request path (`Server/src/inference/native-inference.ts:320`, `Server/src/inference/native-inference.ts:387`).
  - The next request then pays a full model reload inline before its inference timeout even starts (`Server/src/inference/helper-process.ts:149`), with load budgets of 120 s (speech) / 30 s (proof) (`Server/src/inference/native-inference.ts:48`).
- User experience: one long transcript that overflows Qwen's context, or one take where Silero flakes, makes the _next_ dictation (and every queued one behind it) wait for a multi-second-to-minute model reload — with the UI just saying "Transcribing…". A user who habitually dictates near the proofreading limits gets a reload after nearly every take.
- Confidence: verified-in-code. (Reload wall-times not measured — see "What I could not verify".)
- Fix direction: distinguish per-request errors (reject the deferred, keep the process) from transport/protocol corruption (reset). Helpers already clear request state per request (`TextEngine/worker.cpp:155`, `Engine/worker.cpp:278`), so survival is safe by design.
- Effort: S–M

### 3. One unbounded serial FIFO couples transcription + proofreading + correction scoring; no queue depth, position, or per-device fairness

- Severity: P1
- Evidence:
  - All processing chains onto a single promise queue (`Server/src/generation-service.ts:1014`); each job runs transcribe → cleanup → proofread → worker scoring before the next job starts (`Server/src/generation-service.ts:1047`, `Server/src/generation-service.ts:1084`, `Server/src/generation-service.ts:1180`), even though speech and proof are different helper processes that could overlap across jobs.
  - Each helper also allows only one outstanding request (`Server/src/inference/helper-process.ts:140`), and correction scoring spawns a fresh Worker per job with a 10 s budget (`Server/src/domain/correction-runtime.ts:34`, `Server/src/domain/correction-runtime.ts:57`).
  - Worst-case budget per job is therefore ~180 s speech + ~18 s proof + ~10 s scoring (`Server/src/inference/native-inference.ts:49`). Nothing caps how many generations may be `queued`, and admission (`Server/src/generation-service.ts:412`) has no queue-depth or per-device limit. The client likewise appends pending dictations without bound (`Sources/Inlay/InlayController.swift:965`) and shows only "Queued on server…" with no position (`Sources/Inlay/InlayController.swift:1085`).
- User experience: dictate five takes in a row (or share a server between two Macs) and the last result arrives minutes after release, serialized behind proofreading it didn't need to wait for. There is no signal distinguishing "queued behind 4 takes" from "stuck".
- Confidence: verified-in-code (serialization and absence of limits); real per-stage durations inferred.
- Fix direction: pipeline stages across jobs (speech for job N+1 while proof runs for job N) or at minimum surface queue position; add an admission cap on queued+receiving generations so overload fails fast at key-press instead of after speaking.
- Effort: M (pipelining), S (cap + position)

### 4. Global mutation lock + fsync and statfs on every audio chunk serializes the entire server behind disk latency

- Severity: P1
- Evidence:
  - Every durable operation — chunk append, finish/seal, history, preferences, health's state read, event subscribe — runs through one promise chain (`Server/src/generation-service.ts:196`).
  - Each `appendAudio` inside that lock: scans every 4 bytes for non-finite floats (`Server/src/generation-service.ts:531`), calls `requireDiskSpace` (a `statfs`, `Server/src/storage.ts:95`) at `Server/src/generation-service.ts:590`, writes, and `file.sync()`s before acknowledging (`Server/src/generation-service.ts:614`).
  - `finish` then copies the whole raw file into the WAV (up to 256 MiB per stream) with another sync, still inside the lock (`Server/src/generation-service.ts:631`, `Server/src/generation-service.ts:928`, `Server/src/generation-service.ts:964`).
  - The client flushes roughly every 0.5 s of inference audio and every 768 KB of original audio (`Sources/Inlay/ServerClient.swift:370`), so a single take produces hundreds of lock-held fsyncs; two Macs recording simultaneously take turns chunk-by-chunk, and health/history/UI requests queue behind them.
- User experience: on a fast local SSD this is invisible (inferred); on the documented remote/Linux/container deployments with slower or network-backed storage, multi-device dictation and even opening History stutter in lockstep with recording, and one 180 s take's seal stalls everyone. The fsync-per-chunk durability is also wasted: a crash mid-upload discards the take anyway (Finding 6), so the cost buys nothing the seal-time sync doesn't.
- Confidence: verified-in-code (lock/fsync structure); magnitude on slow disks inferred.
- Fix direction: move chunk file I/O out of the global lock (per-generation serialization only), drop per-chunk fsync/statfs to seal-time only, and stream the seal copy outside the lock.
- Effort: M

### 5. Original-audio format/size mismatch: pro interfaces kill takes mid-recording; local temp WAV is duration-capped only (multi-GB)

- Severity: P1
- Evidence:
  - Capture accepts up to 768 kHz and 8 channels (`Sources/Inlay/System/InputOnlyAudioUnit.swift:85`).
  - But the upload path rejects sample rates above 192 kHz by throwing `invalidResponse` on the first original chunk (`Sources/Inlay/ServerClient.swift:407`), which fails the whole session (Finding 1) even though the normalized 16 kHz stream is fine.
  - The server separately caps each stream at 180.1 s **and** 256 MiB (`Server/src/generation-service.ts:584`). At 192 kHz × 8 ch × float32 (≈6.1 MB/s, arithmetic from the format), the byte cap is hit at ~43 s → HTTP 413 mid-take; at 96 kHz × 8 ch at ~85 s.
  - The client writer caps only by duration, never bytes (`Sources/Inlay/System/AudioRecorder.swift:479`): a 768 kHz × 8 ch interface held for 180 s writes ≈4.4 GB to the temp directory before anyone objects.
  - Nothing checks projected original-audio size/rate at admission; `keepOriginalAudio` (default on, `Server/src/generation-service.ts:87`) silently couples the take's fate to the archival stream (original chunk required at finish, `Server/src/generation-service.ts:662`).
- User experience: users with multichannel USB interfaces (a natural dictation-with-good-mic setup) get recordings that die partway with a generic error, or a multi-GB temp file, because of the _archival_ copy — the transcript itself needed 64 KB/s.
- Confidence: verified-in-code (limits and throw sites; byte rates are exact arithmetic from the negotiated format).
- Fix direction: decide original-audio policy at `create`/mic-start: if rate×channels×180 s exceeds the server cap, downmix/decimate the archival stream or disable it for that take with a visible note — never let it fail the inference take.
- Effort: M. Owner-decision involved (see Questions).

### 6. Server restart or shutdown discards sealed, queued dictations whose audio is already durable

- Severity: P1
- Evidence:
  - `shutdown()` cancels every non-terminal record, including `queued` ones with complete WAVs (`Server/src/generation-service.ts:315`).
  - Startup reconciliation marks **all** non-terminal records `failed` ("Server restarted before this generation completed") and cleans partials (`Server/src/generation-service.ts:272`) — it never re-enqueues a `queued`/`transcribing` record for processing, even though `finish` had already sealed and fsynced its WAV (`Server/src/generation-service.ts:686`).
- User experience: update/reboot/crash the server (or the container) while takes are queued behind a long transcription and those dictations are gone, despite the user having finished speaking and the audio sitting complete on disk. This contradicts the "finished recordings queue on the server" expectation set in `README.md:3`.
- Confidence: verified-in-code.
- Fix direction: on startup, re-enqueue records that reached `queued` (or have both sealed WAVs + frame counts in metadata); only `receiving` records are truly unrecoverable.
- Effort: S–M

### 7. Receiving-session expiry (45 s idle / 300 s total) can kill slow-uplink takes during post-release drain

- Severity: P2
- Evidence:
  - A 2 s tick cancels any `receiving` record idle >45 s or older than 300 s since creation (`Server/src/generation-service.ts:296`). `updatedAt` advances only on a successful chunk append (`Server/src/generation-service.ts:622`).
  - The generation is created _before_ the mic starts (`Sources/Inlay/InlayController.swift:886`), the take can run 180 s (`Sources/InlayCore/LifecyclePolicy.swift:4`), and only after release does the client finish the writer and drain the pipe sequentially (`Sources/Inlay/InlayController.swift:984`). Inference audio alone is ~11.5 MB for 180 s; original audio at 48 kHz stereo is ~69 MB (arithmetic from format).
- User experience: on a slow remote uplink (the documented other-machine/Tailscale setup, `README.md:32`), creation-to-seal can exceed 300 s, or a >45 s stall between chunks expires the session — the server cancels while the client is still faithfully uploading, and the take is lost (compounding Finding 1's no-retry).
- Confidence: verified-in-code (expiry and ordering); occurrence depends on link speed (inferred).
- Fix direction: base expiry on "no bytes for X" with X sized for remote links, exclude post-release drain from the total-age cap, or reset the age clock at first audio.
- Effort: S

### 8. Whole-utterance transcription with a fixed 180 s wall-clock timeout; cancellation only by process kill

- Severity: P2 (effectively P1 on CPU-only Linux servers)
- Evidence:
  - After a full-length CPU Silero pass (`Engine/worker.cpp:258`), the entire recording goes through one synchronous `parakeet_full` call (`Engine/worker.cpp:275`); `Engine/README.md:30` notes memory grows with recording length, and `Engine/README.md:22` warns progress can stall during large encodes. There is no abort callback or internal deadline in the speech helper.
  - The server's speech timeout is a fixed 180 s regardless of audio duration or hardware (`Server/src/inference/native-inference.ts:49`), enforced by resetting (killing) the helper (`Server/src/inference/helper-process.ts:323`). User cancellation of a transcribing take lands in the same kill path via the AbortSignal (`Server/src/inference/helper-process.ts:144`), so cancelling also evicts the warm model (Finding 2).
- User experience: latency after key-release grows with take length (no streaming/partial transcript, by design `README.md:3`), and on a CPU-only Linux box a near-180 s take must transcribe faster than real time or it is killed at the deadline and reported as a generic failure — repeatedly, for every long take, each time also forcing a reload.
- Confidence: verified-in-code (structure/timeout); CPU real-time factor not measured (inferred — see unverified list).
- Fix direction: scale the speech deadline with audio duration and a per-backend factor; wire a cooperative abort if parakeet.cpp exposes one; chunked/streaming decode is an owner-level product decision (see Questions).
- Effort: S (deadline scaling), L (chunking)

### 9. Cold start blocks dictation behind sequential SHA-256 of ~1.3 GB + sequential model loads; "ready" doesn't mean warm

- Severity: P2
- Evidence:
  - Warmup verifies then loads strictly sequentially: hash speech model → load speech → hash proof model → load proof (`Server/src/inference/native-inference.ts:231`). Pinned sizes: speech 1,255,897,319 bytes, Linux proof 2,497,281,120 bytes (`Server/src/inference/model-pins.ts:3`, `Server/src/inference/model-pins.ts:8`); hashing streams the whole file (`Server/src/inference/model-verification.ts:152`). (On macOS the proof pin is a manifest digest, so the second full hash is Linux-only, `Server/src/inference/native-inference.ts:160`.)
  - Until the speech hash is cached, `readiness` reports unavailable (`Server/src/inference/native-inference.ts:213`) and `create` rejects takes with 503 (`Server/src/generation-service.ts:434`); the client in turn refuses to record at all unless the server is ready (`Sources/Inlay/InlayController.swift:852`).
  - Once verified-but-not-loaded, health reports `ready: true` with "recordings will queue" (`Server/src/generation-service.ts:346`), so the first take after every restart silently absorbs the remaining load time inside "Transcribing…".
- User experience: after every server restart/update there is a window where holding the key just errors, followed by a first dictation that takes far longer than usual with no explanation.
- Confidence: verified-in-code; window duration depends on disk/GPU (inferred).
- Fix direction: verify+load speech and proof in parallel; let `create` accept (and the client record) while models warm, since audio upload and queueing don't need the models — only `process` does.
- Effort: S–M

### 10. Capture-side buffering is unbounded between the realtime callback and the network; per-sample Swift loops scale with channels × rate

- Severity: P2
- Evidence:
  - The realtime callback allocates a fresh `AVAudioPCMBuffer` and memcpys every buffer (`Sources/Inlay/System/AudioRecorder.swift:406`), then `queue.async`s onto the writer queue with no depth bound (`Sources/Inlay/System/AudioRecorder.swift:419`). The often-cited 512-chunk bound sits _downstream_ of the writer (`Sources/Inlay/ServerClient.swift:125`), so a writer that falls behind accumulates copied buffers in memory instead.
  - Per buffer, the writer runs: high-quality sample-rate conversion (`Sources/Inlay/System/AudioRecorder.swift:375`), a per-sample metering/peak loop (`Sources/Inlay/System/AudioRecorder.swift:571`), `Data` copies for both streams (`Sources/Inlay/System/AudioRecorder.swift:566`, `Sources/Inlay/System/AudioRecorder.swift:589`), and for >2-channel inputs two nested per-sample Swift loops (downmix `Sources/Inlay/System/AudioRecorder.swift:509`, interleave `Sources/Inlay/System/AudioRecorder.swift:600`) — at 192 kHz × 8 ch that's ~1.5 M samples/s through several scalar passes, plus two `AVAudioFile` writes.
- User experience: with a high-rate multichannel interface or a slow disk, the writer can fall behind realtime; memory grows for the rest of the take and the eventual failure mode is the backlog abort of Finding 1 or an OOM, not graceful degradation.
- Confidence: verified-in-code (allocation/bound structure); falling behind on real hardware inferred.
- Fix direction: bound the writer queue in _bytes/frames_ with an early, explicit "input too heavy" error at start; vectorize or skip per-sample passes for the archival stream; reuse callback buffers from a small pool.
- Effort: M

### 11. Pipeline failures are nearly undiagnosable: helper stderr is drained and discarded, server logs nothing

- Severity: P2
- Evidence:
  - Helper stderr is resumed and thrown away ("drain GPU diagnostics without retaining them", `Server/src/inference/helper-process.ts:250`); Fastify runs with `logger: false` (`Server/src/http-server.ts:60`). The only retained signals are per-record `processingSeconds` and error strings surfaced to the client.
  - Client-side, capture failures do log (`Sources/Inlay/System/AudioRecorder.swift:141`), but upload/queue waits have no timing breadcrumbs.
- User experience: when dictation gets slow or a helper OOMs on Metal/CUDA, neither the user nor a supporter can tell whether time went to upload drain, queue wait, model reload (Finding 2), VAD, decode, or proofreading — every performance complaint starts from zero.
- Confidence: verified-in-code.
- Fix direction: retain a bounded ring of helper stderr (already transcript-free by helper design, `Engine/worker.cpp:65`), and stamp stage timings (queue wait, load, VAD/decode, proof, scoring) onto the record — the metadata pipeline for this already exists (`Server/src/generation-service.ts:1061`).
- Effort: S

## Questions for owner

- Partial-take salvage: when capture is interrupted (sleep, device loss, network death) after ≥0.25 s of good audio, should Inlay offer/deliver the partial transcript instead of discarding it? (Findings 1, 7)
- Original audio vs take survival: is keeping original-rate audio ever worth failing a dictation? Should oversize originals be downmixed/decimated, dropped for that take with a note, or should such interfaces be rejected at mic selection? (Finding 5)
- Restart semantics: should sealed queued takes resume after a server restart (my recommendation), or is "restart loses the queue" an accepted product contract that should at least be documented? (Finding 6)
- Queueing policy: what is the intended multi-device story — strict global FIFO, per-device fairness, or "single user, don't care"? And is there a maximum acceptable queue depth before key-presses are refused? (Finding 3)
- Long-take strategy: is 180 s whole-utterance decode a permanent product choice, or is chunked/streaming transcription (partial results, lower release latency, bounded memory) on the roadmap? (Finding 8)
- CPU-only Linux: is it a supported dictation target or a "runs, best effort" target? This decides whether the fixed 180 s speech deadline is a bug or a non-issue. (Finding 8)
- Proofreading long takes: transcripts over ~6,000 graphemes skip Qwen entirely (`Server/src/generation-service.ts:1168`). Acceptable silent degradation, or should long takes be proofread in chunks (with the reload-on-error behaviour of Finding 2 fixed first)?

## What I could not verify

- Any runtime magnitude: Parakeet real-time factor and peak memory at 180 s on Metal/CUDA/CPU, model reload wall-times, fsync cost on target disks, writer-queue behaviour under a real 192 kHz interface. `AGENTS.md:4`/`AGENTS.md:7` prohibit test harnesses and fake inference, and no Inlay Dev dictation was performed for this audit; all such claims above are marked inferred.
- `parakeet_full` internals (memory growth, abort hooks, progress cadence) — vendored whisper.cpp/parakeet sources are excluded from audit scope; only `Engine/README.md:22`/`Engine/README.md:30` and the call site were relied on.
- MLX vs llama.cpp proof-helper parity in timing/timeout behaviour beyond the constants read in `TextEngine/worker.cpp:31` and `TextEngine/Sources/InlayTextEngine/CorrectionEngine.swift:8`.
- Whether the Swift reference server (`Sources/InlayServerKit/`) shares Findings 2–4/6–7: it has mirror files (`GenerationService.swift`, `HelperProcess.swift`, `NativeInference.swift`) but they were not line-audited here; per `docs/architecture.md:14` it is a comparison reference, so findings are stated against the packaged TypeScript server only.
- Real-world frequency of >192 kHz or 8-channel microphones among users, and typical remote-uplink speeds — Findings 5 and 7 are graded on consequence when they occur, not prevalence.
