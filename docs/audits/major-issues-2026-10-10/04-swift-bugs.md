# Swift app correctness bugs — major-issues audit (2026-10-10)

Scope: `Sources/Inlay`, `Sources/InlayCore`, `Sources/InlayDomain` (spot-checked),
`Sources/InlayServerKit` (Swift reference server), `TextEngine/Sources/InlayTextEngine`
(MLX helper) at HEAD `b6d6815`. Static code-path audit only — no tests run and no
dictation performed, per `AGENTS.md:4`. Confidence is **verified-in-code** when the
mechanism is fully traced in the cited bodies, **inferred** when the trigger depends
on OS/framework behavior not exercised here.

General note: the codebase is unusually defensive (session IDs, generation IDs,
cancellation checks at most await points, no `try!`/`fatalError` in app sources, all
`as!` casts are `CFGetTypeID`-guarded). The findings below are the places where that
discipline has gaps with real user impact.

---

## 1. A failed background health poll cancels the live recording

- **Severity:** P1
- **Evidence:** `Sources/Inlay/InlayController.swift:319` (`checkServer` catch calls
  `failSession(..., cancelServer: true)` when `isCapturing`);
  polling loop `Sources/Inlay/InlayController.swift:253` (every 5 s);
  also triggered by app activation and window show
  (`Sources/Inlay/InlayController.swift:1217`, `Sources/Inlay/InlayApp.swift:129`);
  per-request timeout 12 s (`Sources/Inlay/ServerClient.swift:44`).
- **What the user experiences:** Mid-dictation, one transient health-check failure
  (server busy loading a model, a slow Tailscale path, a single dropped request)
  stops the microphone, restores output mute, and cancels the server generation —
  even though the take's own sequenced audio upload on a separate request may be
  perfectly healthy. The take is lost with a generic "Server offline" message.
- **Confidence:** verified-in-code (mechanism). The health request and the upload
  are independent `URLSession` tasks; nothing ties health failure to upload health.
- **Fix direction:** Let the upload task be the authority for a capturing session:
  on health failure while capturing, mark `serverHealth = nil`/update status text
  but do not `failSession`. The upload failure path
  (`Sources/Inlay/InlayController.swift:905`) already fails the session if the
  connection actually dies.
- **Effort:** S

## 2. Event-stream recovery has no overall deadline; a wedged generation blocks all later deliveries

- **Severity:** P1
- **Evidence:** `Sources/Inlay/ServerClient.swift:286` `events()` loops `while true`;
  `Sources/Inlay/ServerClient.swift:304` resets `recoveryFailures = 0` on every
  successful non-terminal generation fetch, so the 5-strike exit at
  `Sources/Inlay/ServerClient.swift:309` can never accumulate while the server is
  reachable. Call site `Sources/Inlay/InlayController.swift:1014`; deliveries are
  serialized behind it at `Sources/Inlay/InlayController.swift:1024` and chained at
  `Sources/Inlay/InlayController.swift:1066`. Pending removal only happens when the
  task ends (`Sources/Inlay/InlayController.swift:977`), and `isBusy` includes
  non-empty pending (`Sources/Inlay/InlayController.swift:149`).
- **What the user experiences:** If a server generation never reaches a terminal
  state (wedged helper, stuck FIFO job), the HUD shows "Transcribing…" forever.
  Because delivery is strictly ordered, every later finished take also never
  inserts. While wedged, `saveConnection` silently refuses
  (`Sources/Inlay/InlayController.swift:325`) and file-config changes are silently
  dropped (`Sources/Inlay/InlayController.swift:265`). Recovery requires the user
  to notice and press Escape (cancel path at
  `Sources/Inlay/InlayController.swift:710`) or quit the app.
- **Confidence:** verified-in-code.
- **Fix direction:** Add an overall wall-clock budget for one generation's
  processing wait (generously above the server's own queue/service bounds), after
  which the client fails that pending take locally (server copy still lands in
  history) and unblocks the delivery chain. Owner input needed on the budget — see
  Questions.
- **Effort:** S–M

## 3. Quit blocks the main thread synchronously on the Wispr Flow snapshot worker

- **Severity:** P1
- **Evidence:** `Sources/Inlay/InlayController.swift:795` `shutdown()` calls
  `wisprFlowPrepareGate?.cancel(waitForWorker: true)`, which does
  `value?.close(); work.wait()` synchronously
  (`Sources/Inlay/InlayController.swift:67`) on the main actor. The waited-on
  worker is `WisprFlowSourceReader()` init, which snapshots every discovered
  Wispr database via backup-API copy and full scan with no cancellation checks
  (`Sources/Inlay/WisprFlowSourceReader.swift:160`). `close()` itself takes the
  reader lock (`Sources/Inlay/WisprFlowSourceReader.swift:244`) that
  `session(for:)` holds for an entire artifact extraction
  (`Sources/Inlay/WisprFlowSourceReader.swift:254`), and shutdown also closes
  active readers directly (`Sources/Inlay/InlayController.swift:797`).
  `applicationShouldTerminate` calls `shutdown()` synchronously before it can
  reply `.terminateLater` (`Sources/Inlay/InlayApp.swift:83`).
- **What the user experiences:** Quitting during import preparation (or while a
  large artifact is being extracted) beach-balls the app until the snapshot
  finishes — potentially long for a large Wispr Flow database — with no way to
  force-quit cleanly through the normal terminate path; the deferred config flush
  in `applicationShouldTerminate` is never reached while blocked.
- **Confidence:** verified-in-code.
- **Fix direction:** Move gate wait/reader close off the main actor inside the
  existing `.terminateLater` task, and add cooperative cancellation checks inside
  the reader init/snapshot loop so the worker can actually stop.
- **Effort:** M

## 4. Event-tap recovery (or a 2-second health tick) cancels an in-progress take

- **Severity:** P2
- **Evidence:** tap-disabled events route to `recoverDisabledTap()`
  (`Sources/Inlay/System/HotkeyMonitor.swift:303`), which calls
  `refreshTap(resetHold: true)` (`Sources/Inlay/System/HotkeyMonitor.swift:551`);
  the periodic health timer calls `refreshTap()` every 2 s
  (`Sources/Inlay/System/HotkeyMonitor.swift:262`). Both paths fall through to
  `reset(cancelActive: true, keepLatch: true)`
  (`Sources/Inlay/System/HotkeyMonitor.swift:574`), which in hold mode sets
  `latched = false` and fires `onCancel`
  (`Sources/Inlay/System/HotkeyMonitor.swift:620`) →
  `InlayController.cancelDictation` (`Sources/Inlay/InlayController.swift:838`).
- **What the user experiences:** Anything that makes the tap report disabled —
  `tapDisabledByTimeout` after a main-runloop stall, or system Secure Event Input
  from another app — cancels the current dictation mid-hold, shown only as
  "Cancelled". The watchdog's hardware-state polling
  (`Sources/Inlay/System/HotkeyMonitor.swift:509`) could in principle carry the
  hold, but recovery discards it first.
- **Confidence:** mechanism verified-in-code; frequency of tap timeouts/Secure
  Input interactions on real machines **inferred**.
- **Fix direction:** Distinguish "tap unhealthy, key still physically held +
  watchdog alive" from "permission lost": only cancel the active hold for the
  latter; for the former, rebuild the tap and keep the hold under watchdog
  supervision. This is a safety/behavior tradeoff — see Questions.
- **Effort:** M

## 5. Configuration watcher mutates its source array off-queue in `deinit`

- **Severity:** P2
- **Evidence:** `ConfigurationWatcher` is `@unchecked Sendable`
  (`Sources/Inlay/ConfigurationStore.swift:171`); `sources` is mutated on its
  serial queue in `refresh()` (`Sources/Inlay/ConfigurationStore.swift:190`) and
  `stop()` (`Sources/Inlay/ConfigurationStore.swift:201`), but `deinit` iterates
  `sources` on whatever thread drops the last reference
  (`Sources/Inlay/ConfigurationStore.swift:183`). `stopWatching()` enqueues an
  async stop and immediately releases the watcher
  (`Sources/Inlay/ConfigurationStore.swift:107`), so `deinit` can race the queued
  `stop`/`refresh` blocks — a genuine data race on `Array` storage (crash or
  double-cancel of dispatch sources), most likely at quit.
- **What the user experiences:** Rare, timing-dependent crash during app quit or
  config-watcher teardown.
- **Confidence:** verified-in-code (race exists); crash frequency **inferred**.
- **Fix direction:** Make `deinit` capture through the queue (`queue.sync` is
  unsafe from within queue blocks — use a small box whose deinit enqueues
  cancellation, or guard `sources` with a lock).
- **Effort:** S

## 6. Cancelling between paste dispatch and bookkeeping loses the delivery receipt and continuation anchor

- **Severity:** P2
- **Evidence:** after `inserter.deliver` returns, `deliver` bails with a synthetic
  "Delivery cancelled" failed receipt if the task was cancelled
  (`Sources/Inlay/InlayController.swift:1123`), skipping `rememberContinuation`
  even when the outcome was `.inserted` and `confirmedAnchor` was set
  (`Sources/Inlay/System/TextInserter.swift:272`,
  `Sources/Inlay/InlayController.swift:1126`). The caller then throws on
  `checkCancellation` (`Sources/Inlay/InlayController.swift:1030`) and the catch
  returns without ever POSTing the delivery receipt
  (`Sources/Inlay/InlayController.swift:1047`). Realistic trigger: sleep/lock
  calls `stopPendingDictations()` (`Sources/Inlay/InlayController.swift:1235`),
  cancelling the pending task mid-delivery.
- **What the user experiences:** Text was actually inserted at the cursor, but
  history shows no delivery for that take and a following list continuation loses
  its anchor (next take can't continue the list) — with no error shown.
- **Confidence:** verified-in-code (window is narrow: between
  `TextDeliveryTransaction` returning and the receipt POST).
- **Fix direction:** Once an outcome of `.inserted` is known, finish the local
  anchor bookkeeping unconditionally; treat the receipt POST as best-effort with
  the already-known outcome instead of discarding it on cancellation.
- **Effort:** S

## 7. Realtime audio callback allocates and enqueues without bound

- **Severity:** P2
- **Evidence:** render-context callback forwards the driver's reused buffer
  synchronously (`Sources/Inlay/System/InputOnlyAudioUnit.swift:253`);
  `RecordingWriter.append` allocates a copy buffer and `memcpy`s on that same
  realtime thread (`Sources/Inlay/System/AudioRecorder.swift:409`), then
  `queue.async` per buffer with no cap
  (`Sources/Inlay/System/AudioRecorder.swift:419`). Conversion, file writes and
  chunk emission happen on the serial writer queue
  (`Sources/Inlay/System/AudioRecorder.swift:473`).
- **What the user experiences:** If the writer queue falls behind (slow disk,
  expensive 8-channel downmix at
  `Sources/Inlay/System/AudioRecorder.swift:509`), copied buffers accumulate in
  the writer queue for the whole take (bounded only by the 180 s cap — worst case
  is hundreds of MB at high channel counts/rates), and heap allocation on the
  realtime thread risks dropouts/glitches in the very audio being captured.
  `finish()` then has to drain the entire backlog before the take can seal.
- **Confidence:** verified-in-code (no bound exists); real-world lag **inferred**.
- **Fix direction:** Fixed-size ring of preallocated copy buffers between the
  render callback and the writer queue; on overflow, fail the take explicitly
  (like `AudioChunkPipe` does) instead of growing memory silently.
- **Effort:** M

## 8. Launching the other build (Dev vs Release) deletes this build's in-progress capture files

- **Severity:** P2
- **Evidence:** every take's temp directory is named `Inlay-Dev-recording-<UUID>`
  regardless of build (`Sources/Inlay/System/AudioRecorder.swift:377`);
  `CapturedAudio.cleanupOrphans()` deletes **all** such directories in the shared
  temporary directory with no age or owner check
  (`Sources/Inlay/System/AudioRecorder.swift:35`), and runs at every controller
  startup (`Sources/Inlay/InlayController.swift:248`). The single-instance guard
  is per bundle ID (`Sources/Inlay/InlayApp.swift:14`), so Inlay and Inlay Dev
  legitimately run side by side with separate data dirs.
- **What the user experiences:** Launching Inlay while Inlay Dev is recording
  (or vice versa) unlinks the live take's capture directory mid-recording. The
  streamed upload likely survives (open file descriptors keep the inode alive),
  but the handed-off `CapturedAudio` URLs dangle — the local WAV and original.wav
  paths no longer exist for anything that reads them by path.
- **Confidence:** verified-in-code for the deletion; extent of downstream damage
  **inferred** (depends on AVFoundation's reopen behavior after unlink).
- **Fix direction:** Namespace the directory by bundle/build identity and/or only
  delete entries older than the process start (or with a dead owner's PID file).
- **Effort:** S

## 9. Swift reference server: one cancelled waiter kills model load for all waiters

- **Severity:** P2 (Swift reference server only — not the packaged TS server)
- **Evidence:** `HelperProcess.ensureLoaded` shares one `loadingTask` among
  concurrent callers (`Sources/InlayServerKit/HelperProcess.swift:73`), but any
  single caller's cancellation invokes `cancelLoading` → `reset`
  (`Sources/InlayServerKit/HelperProcess.swift:91`,
  `Sources/InlayServerKit/HelperProcess.swift:145`), which terminates the helper
  and resumes the shared ready continuation with an error for **every** waiter
  (`Sources/InlayServerKit/HelperProcess.swift:303`). The output reader also
  blocks a dispatch-queue thread on a semaphore per line while the actor
  processes it (`Sources/InlayServerKit/HelperProcess.swift:214`).
- **What the user experiences:** (If the Swift server is ever run for real) a
  client disconnecting during the ~seconds-long model load can abort warmup for
  everyone and force a full helper restart on the next request.
- **Confidence:** verified-in-code; whether the Swift server serves real traffic
  is unresolved (see Questions and inventory caveats).
- **Fix direction:** Only reset on cancellation when the cancelling caller is the
  load's owner (or no other waiters remain); keep per-waiter cancellation from
  touching shared load state.
- **Effort:** S

---

## Questions for owner

1. **Stuck-generation budget (finding 2):** What client-side deadline for a
   non-terminal generation is acceptable (e.g. 10–15 min to cover deep FIFO
   queues), given the result remains available in server history afterwards?
2. **Tap-timeout behavior (finding 4):** Should a recovered event tap cancel an
   in-progress hold (current, safest against stuck-open microphones), or should
   the hardware-state watchdog be trusted to carry the hold through a tap rebuild
   (fewer lost takes, more risk if polling lies)?
3. **Swift reference server (finding 9):** Is `Sources/InlayServerKit` ever run
   against real clients, or is it comparison-only? If comparison-only, finding 9
   can be downgraded to drift risk; if it must stay runnable, the load-sharing
   cancellation should be fixed.
4. **Quit during Wispr snapshot (finding 3):** Is it acceptable for quit to
   abandon a half-built import snapshot (temp files removed next launch) rather
   than waiting for the worker — i.e. can the gate wait become a bounded/async
   wait inside `.terminateLater`?

## What I could not verify

- All runtime behavior: per `AGENTS.md:4` no tests were run and no dictation,
  insertion, sleep/lock, or quit-during-import was exercised in Inlay Dev. Every
  finding above is a traced code path, not an observed failure.
- How often `tapDisabledByTimeout`/Secure Input actually disables the tap on
  current macOS versions during a hold (finding 4 trigger frequency).
- Whether `AVAudioFile` finalization or any path-based reader misbehaves after
  its containing directory is unlinked mid-recording (finding 8 blast radius).
- The MLX text helper's runtime behavior
  (`TextEngine/Sources/InlayTextEngine/`): statically, its deadline/backstop and
  bounded I/O look sound, but Metal stall behavior and `ModelContainer`
  cancellation semantics were not exercised.
- Generated wire code (`Sources/InlayAPIWire/*`) was excluded from line audit, per
  the inventory; the hand-written facade `Sources/InlayAPI/API.swift` was only
  spot-checked where the client depends on it.
- Long-running leak behavior (e.g. the never-pruned clipboard revision set at
  `Sources/Inlay/System/TextInserter.swift:653`) was judged below the "major"
  threshold and not quantified.
