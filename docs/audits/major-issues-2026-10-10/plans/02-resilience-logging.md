# Batch 3 — Server resilience + observability (`stack/02-resilience-logging`)

- REPORT Batch 3 = P1-5, P1-6, P1-7 + warmup/health P2s (`REPORT.md:42-44`, `:65`, `:81`; `DECISIONS.md:40` orders it second, after Batch 1). Branch `stack/02-resilience-logging`, stacked on the Batch 1 branch (base `stack/00-audit-plan` on `audit/major-issues`).
- All `file:line` cites are at audit base `b6d6815` and were re-opened for this plan. Re-anchor on the Batch 1 tip before editing: Batch 1 changes `initialize()` reconciliation (`Server/src/generation-service.ts:272-283`, sealed-record requeue) and `shutdown()` (`:315`), which slices 4–5 sit next to.
- Sources: `05-server-api-bugs.md` F2/F7, `07-audio-pipeline-perf.md` F2/F11, `10-reliability-errors.md` F4/F5/F7, `11-crosscheck.md:15-16,56,79` (all CONFIRMED).

## Goal

- A single corrupt/future/duplicate generation record no longer bricks startup: it is quarantined, named in the log, and the rest of the archive serves (P1-5).
- The server emits structured, metadata-only logs (never transcripts, audio, prompts, or tokens) covering startup, readiness, 5xx, tick/warmup failures, and helper spawn/exit with code+signal and a bounded stderr tail (P1-6).
- A routine per-request helper error (VAD failure, Qwen context overflow/deadline) fails only that request; the warm model process survives (P1-7).
- An unloadable helper stops respawning forever: exponential backoff + latch, and proof-load failures stop being masked (P2, `10 F7`).
- Unauthenticated callers get liveness only; detailed readiness (and its warmup nudge) requires the token (P2, `05 F7`).

## Decisions applied

- **Corrupt records: quarantine + serve, named** (`DECISIONS.md:20`). Overrides the REPORT Q13f open question; the Q13f rider "keep preferences strict" (`REPORT.md:150`, `10 F4`) is kept — `preferences.json` stays fail-fast but its error now names the path (slice 4). Serving on default preferences would silently reset the user's revision counter and let the next `updatePreferences` overwrite real preferences.
- **Logging: operational metadata only, never transcripts or audio; unauthenticated health = liveness only** (`DECISIONS.md:29`, REPORT Q11 recommended default). No new log-level config flag; logs are always on (shareable-product audience, `DECISIONS.md:8`).
- **Cancel keeps history** (`DECISIONS.md:14`): quarantine moves a record aside on disk, it never deletes audio. Quarantined dirs are operator-recoverable (rename back after repair).
- **Swift reference server is deleted** (Batch 8, `DECISIONS.md:18`): nothing in this batch is mirrored into `Sources/InlayServerKit/`. One server only.
- **No automated behavioral tests** (`AGENTS.md:4`); this batch adds no parity checks either (those are Batch 8 only, `DECISIONS.md:21`). Verification is static gates + owner-run Inlay Dev steps below.
- Errors-as-values where the codebase already does it (`InferenceError` codes, `ServiceError`); no `as any`; reuse `atomicPrivateWrite`/`rename` patterns from `Server/src/storage.ts`.

## IN scope — commit slices (ordered)

### Slice 1 — Structured metadata-only logger + HTTP/startup wiring (M)

- New `Server/src/log.ts` (~50 lines): `logInfo(event, fields?)`, `logWarn`, `logError(event, error?, fields?)` writing one JSON line per call to stderr (`process.stderr.write`), shape `{time, level, event, ...fields}`. Allowlisted field kinds only: generation/import IDs, helper name, route pattern (`request.routeOptions.url`, never raw URL with query secrets — there are none today, but pattern is safe by construction), status code, error `code`, exit code/signal, millisecond durations, counts. `logError` includes `error.message` truncated to 512 chars and `error.stack` only for non-`ServiceError`/non-`InferenceError` (unexpected) errors. No dependency added (Fastify's pino would serialize request headers incl. `authorization` by default — explicitly not used).
- `Server/src/http-server.ts:60` `createHTTPServer`: keep `logger: false`. In `setErrorHandler` (`:97-114`): log every fallthrough 500 with stack + route pattern + method before replying; log `ServiceError` only when `status >= 500` (4xx are client errors, already returned verbatim). Do not log request/response bodies anywhere.
- `Server/src/main.ts:48-49,59-61`: replace the bare `console.log` listening line with `logInfo("server_listening", {address})` (keep `--help`/`--print-default-proofreading-prompt` on stdout — scripts consume them). Catch at `:60` becomes `logError("server_start_failed", error)` so a startup failure prints name + message + stack (culprit naming lands in slice 4); exit code unchanged.
- Why first: slices 3–7 all emit through this module; stderr is already captured by `scripts/run-dev.sh:152` (`.local/server.log`), systemd/journal, and Docker.

### Slice 2 — Per-request vs fatal helper errors (P1-7) (S–M)

- `Server/src/inference/helper-process.ts:300-308` `HelperProcess.receive`, `case "error"`: classify instead of unconditional `reset()`:
  - `response.id === this.requestID` with a pending `this.result` → per-request failure: copy the `case "result"` teardown (`:290-298`: clear `result`/`requestID`/`progress`/timeout) and `result.reject(new InferenceError("unavailable", response.message ?? …))`. Child, `loaded`, and `engineVersion` are untouched; `request()`'s `finally` (`:164-167`) clears `operation` as today.
  - No pending result and `this.ready` pending (load phase, id absent) → fatal: keep today's `reset()` (rejects `ready`).
  - Id present but not matching → keep today's ignore (`:301`), add `logWarn("helper_error_unknown_id", {helper})`.
  - No pending result, already `loaded`, unsolicited error → protocol corruption: `reset()` + `logError`.
- `Server/src/inference/helper-process.ts:336` `reset` stays private; add a public `discard(error: InferenceError)` that calls `reset` without awaiting retirement, and use it from `Server/src/inference/native-inference.ts:320,327,387` replacing `await this.speech.shutdown()` / `await this.proof.shutdown()` on response-validation failure (protocol corruption must still evict the process, but the request path should not block up to the 2 s SIGTERM→SIGKILL escalation in `retire` `:357-375`; `shutdown()` keeps awaiting all `retiring` children at `:175-180`).
- Unchanged, deliberately: timeout (`setTimeout` `:323-328`) and `AbortSignal` cancel (`:144-146`) still `reset()` — mid-compute the helper's next stdout line would be misattributed to a future request. Invalid JSON / unknown event (`:264-268,309-315`) still `reset()`.
- Why safe: workers clear per-request state themselves — speech sets `parameters.no_context = true` (`Engine/worker.cpp`, `parakeet_full` params, ~`:278`) and resets VAD state per call; text worker calls `llama_memory_clear` before and after each request (`TextEngine/worker.cpp`, `correct` handler ~`:155`). No C++/Swift helper changes; classification is server-side by (id, phase) only. Behavior for the failing take is unchanged: speech error still fails the take via `process` (`generation-service.ts:1047`), Qwen error still degrades to deterministic text (`:1192-1196`) — only the _next_ take stops paying a reload.

### Slice 3 — Helper stderr ring + exit code/signal (P1-6, helper half) (M)

- `Server/src/inference/helper-process.ts:250-252` `start()`: replace `child.stderr.resume()` drain-and-drop with a bounded ring: `private stderrTail: Buffer[]` + byte count on `HelperProcess` (declared near `:80-91`), `child.stderr.on("data")` appends and trims to the last 64 KiB (drop whole chunks from the front; no per-byte work). Expose `stderrSnapshot()` returning the decoded tail (last ≤4 KiB, `TextDecoder` non-fatal) for logging.
- `Server/src/inference/helper-process.ts:256` `child.on("close", () => …)`: take `(code, signal)` args (currently ignored). On close of the _current_ generation: `logError("helper_exited", …, {helper: configuration.name, exitCode: code, signal, stderrTail: snapshot})`, then the existing `transportFailed`. Also `logInfo("helper_spawned", {helper})` after `spawn` (`:218`) and `logInfo("helper_ready", {helper, engineVersion})` in `case "ready"` (`:271-279`). Retired (superseded-generation) children close silently except at `debug`-equivalent: skip logging when `current !== this.generation`, mirroring `transportFailed` (`:319-321`).
- Record-facing strings stay short: `record.error` keeps `error.message` only (`generation-service.ts:1129-1134`) — the stderr tail goes to the server log, never into `metadata.json`. Privacy basis: both workers route only ggml WARN/ERROR to stderr and explicitly suppress debug output because it "can contain decoded tokens" (`Engine/worker.cpp:65-69`, `TextEngine/worker.cpp:48-51`); the ring is still treated as untrusted diagnostics — capped, in-memory only, logged only on exit/failure. Model file paths may appear; that is operational metadata per `DECISIONS.md:29`.

### Slice 4 — Quarantine-and-serve startup, culprit named (P1-5) (M)

- `Server/src/generation-service.ts:236-286` `GenerationService.initialize` per-generation loop: wrap the whole per-record block (`requireRegularDirectory` `:239`, lowercase-dir rename `:241`, metadata parse/validate `:243-255`, `normalizePreferences` `:256`, structural check `:257-270`, non-terminal reconciliation `:272-283` as left by Batch 1, `imports.indexRecord` `:285`) in one `try/catch`. Extract the block to a private `loadRecord(name): Promise<"loaded" | "missing">` so the catch is the only new control flow in the loop.
- Catch behavior = quarantine-and-continue (never rethrow from inside the loop):
  - `ENOENT` for `metadata.json` keeps today's skip (`:252-253`): an empty/partially-created dir is not corruption; `logWarn("generation_metadata_missing", {id})`, no rename.
  - Everything else (invalid JSON, Ajv validation failure, `schemaVersion !== 1`, id/dir mismatch, bad nested preferences, duplicate Wispr source ID from `Server/src/imports.ts:466-470` `indexRecord`): `rename` (already imported `:2`) the directory to `generations/<ID>.quarantined` (append `-2`, `-3`, … if that name exists). The loop's `isUUID(name)` guard (`:237`) then skips it on every future boot. After the rename, `atomicPrivateWrite` a `quarantine-reason.txt` into it (`<ISO time>\n<error message>\n`, message from the caught error; validation errors name only the schema, so this stays metadata-only), and `logError("generation_quarantined", error, {id, from, to})`. The record is not added to `this.records`, never reaches Batch 1 requeue, and its audio is preserved on disk per cancel-keeps-history.
  - If the `rename` itself throws: `logError("generation_quarantine_failed", …)` and still continue without the record (next boot retries; serving the rest beats bricking).
  - Duplicate Wispr ID: first record in `readdir` order wins, later duplicates quarantine. Which one survives is order-dependent — accepted, both IDs are in the log line, and import duplicates imply manual archive damage.
- End of `initialize`: `logInfo("archive_loaded", {records: this.records.size, quarantined})` with a local counter.
- Preferences block (`:216-235`): keep fail-fast, but wrap the non-`ENOENT` rethrow so the message names the file: replace `throw error` at `:233` with `throw new Error("Server preferences at <path> are invalid: <message>")`-equivalent preserving the original as `cause`. `path` is already in scope (`:215`). Combined with slice 1's `server_start_failed` stack, the culprit is named.
- `Server/src/imports.ts:415-461` `WisprFlowImports.initialize` (called at `generation-service.ts:214`, before generations load): keep journal recovery fail-fast (choosing live-vs-backup wrong can destroy a record), but name the journal file in the two throws (`:437` invalid journal, `:450-453` lost archive) by including `filename` in the message. Full import-journal quarantine is deferred (below).

### Slice 5 — Warmup backoff + latch, unmask proof-load errors (P2, `10 F7`) (S–M)

- `Server/src/generation-service.ts:169-173` fields: add `proofLoadError?: string`, `warmupFailures = 0`, `nextWarmupAt = 0` (epoch ms) next to `speechLoadError`.
- `beginWarmup` (`:1244-1271`): signature becomes `beginWarmup(force = false)`. After the existing `stopping || warming || processingControllers.size` guard (`:1245`), add `if (!force && Date.now() < this.nextWarmupAt) return;`.
  - Success handler (`:1251-1254`): also clear `proofLoadError`, `warmupFailures = 0`, `nextWarmupAt = 0`; `logInfo("warmup_succeeded")`.
  - Failure handler (`:1255-1263`): keep the cancelled early-return (`:1257`, and cancelled attempts do not count or arm backoff). Otherwise: classify by the readiness snapshot already fetched — speech loaded but proof not (and proofreading enabled) → `proofLoadError = message`, `speechLoadError = undefined`; speech not loaded → existing `speechLoadError` logic (`:1258-1262`) unchanged. Then `warmupFailures++`, `nextWarmupAt = Date.now() + min(300_000, 5_000 * 2 ** (failures - 1))` (5 s → 10 s → … → 5 min cap; no jitter needed for a single local server), `logWarn("warmup_failed", {attempt, retryAfterMs, speech: !speechLoaded})`. The 5 min cap is the latch for practical purposes: health polls every 5 s (`InlayController` via `:359`) become no-ops, so a broken install retries at most every 5 min instead of at poll cadence, and self-heals if the operator fixes the model files — no permanent-latch state to wedge.
- Call sites: `updatePreferences` (`:408`) passes `force: true` (an explicit config change is the "try again now" signal). All others stay non-force: `start` (`:290`), `health` (`:359`), `create` (`:436,444`), `finish` catch (`:699`), processing `finally` (`:1028`).
- `health` (`:370-379`): `proofreading.message` uses `this.proofLoadError` when set instead of the generic `"Unavailable; deterministic text is preserved."` (`:377`). No `ServerHealth` shape change (`ModelRuntimeInfo.message` already exists, `Server/api/openapi.yaml:663-673`).
- `NativeInference.warmUp` (`Server/src/inference/native-inference.ts:231-252`) and the `InferenceBackend` interface (`:79-98`) are unchanged; speech/proof classification uses the readiness snapshot GenerationService already takes.

### Slice 6 — Split unauthenticated liveness from authenticated readiness (P2, `05 F7`) (M)

- Chosen shape (default, not a question): keep `GET /v1/health` as the detailed readiness payload, authenticated whenever a token is configured; add `GET /v1/liveness` as the only unauthenticated route. This keeps the Swift client working unchanged — `ServerClient.swift:64` already sends the bearer token on every request including `health()` (`Sources/Inlay/ServerClient.swift:149`), and `InlayController` needs `apiVersion`/`ready`/`message` from the full payload.
- `Server/src/http-server.ts:72-93` `onRequest` hook: change the exemption at `:80-81` from `"/v1/health"` to `"/v1/liveness"`. Tokenless mode is untouched (`:73-79` Host check still applies to every route including liveness).
- `Server/src/http-server.ts:119`: add `app.get("/v1/liveness", () => ({ status: "ok" }))` — it must not call `service.health()` (no `beginWarmup` side effect, no disk/model fingerprint: no versions, no `isDev`, no messages).
- Contract: `Server/api/openapi.yaml` — remove `security: []` from `/v1/health` (`:30`, it then inherits global `bearerAuth` `:9-10`); add `/v1/liveness` path with `security: []` and a small `LivenessResponse` schema (`status: string`, required) next to `ServerHealth` (`:675`). Regenerate (`bun run generate:api`), commit `Server/src/generated/api.ts` and any `Sources/InlayAPIWire/*` diff the full generator produces (additive only; no hand-written Swift changes — the client never calls liveness).
- Consumers that currently rely on unauthenticated `/v1/health` move to liveness, same slice:
  - `Server/Dockerfile:55` `HEALTHCHECK` curl → `/v1/liveness` (containers commonly run with a token; this is the `05 F7` "Docker needs pre-token liveness" carve-out).
  - `scripts/run-dev.sh:99,161` wait-for-server curls → `/v1/liveness` (they only need "listening"; `health` already returns 200 with `ready: false`, so semantics are unchanged).
  - Docs one-liners: `docs/development.md:17`, `README.md:61`, `Server/README.md:129,187` — browser/curl reachability now points at `/v1/liveness`; readiness detail is "in the app, or `curl -H \"Authorization: Bearer …\" /v1/health`".
- `GenerationService.health` (`:337-383`) itself is unchanged except slice 5's message; its `beginWarmup` nudge (`:359`) now only runs for authenticated callers and is backoff-gated anyway.

### Slice 7 — Process/tick failure logging + stage-timing log lines (P1-6, server half) (S)

- Swallowed-error sites get `logError` (behavior otherwise identical): `start` tick catch (`:291-293`, `tick_failed`), `processingQueue` catch (`:1025`, `processing_chain_failed` — `process` catches its own errors, so this only fires on mutate/queue machinery failure), `process` catch (`:1124-1139`, `generation_failed {id}` before/after persisting `record.error` as today), `finish` seal catch (`:693-701`, `seal_failed {id}`; note the raw-audio deletion there is Batch 1's seal fix — do not change semantics here).
- Stage timings, log-only (no record/API change): in `enqueueProcessing` (`:1014-1030`) record `enqueuedAt = performance.now()` in the closure; in `process` (`:1031-1140`) measure wall durations around `inference.transcribe` (`:1047`), the deterministic text stage (`:1068-1072`, negligible — fold into proof), `proofread` (`:1084`, whose internal scoring via `evaluateCorrectionInWorker` at `:1180` is included in proof wall time), and emit one line on completion: `logInfo("generation_processed", {id, queueWaitMs, transcribeMs, proofMs, totalMs, status})`, and the same fields minus un-run stages on the failure path. Existing persisted timings (`speech.processingSeconds` `:1066`, `proofreading.processingSeconds` `:1107`, `textProcessing.wallSeconds` via `proofread` `:1149`) are untouched — the new value is queue-wait and load-inclusive wall time, which only the log can show today. Also `logInfo("readiness_changed", …)` is explicitly _not_ added here (health is polled; derive transitions from `warmup_*` + `helper_*` lines instead of diffing state in `health()`).

## API / data changes + migration

- API: `GET /v1/liveness` added (`{status: "ok"}`, unauthenticated); `GET /v1/health` requires the bearer token when one is configured (401 `unauthorized` otherwise, same body shape as every other route). Response bodies otherwise unchanged; `apiVersion` stays 2 (additive route + auth tightening the first-party client already satisfies). Generated TS (`Server/src/generated/api.ts`) and Swift wire bindings regenerated in slice 6.
- Data on disk: no schema change. New artifacts: `generations/<ID>.quarantined/` directories (moved, not copied) each containing the original files plus `quarantine-reason.txt`. No migration needed; older servers ignore non-UUID directory names exactly like the current one (`generation-service.ts:237`), so downgrade is safe. Recovery is manual: fix or remove files, rename the directory back to `<ID>`, restart.
- Log destination: stderr only. No log files are created or rotated by the server; `run-dev.sh`, launchd/systemd/Docker capture (documented supervision, `Server/README.md:129`) own retention.
- No configuration changes (`Server/src/configuration.ts` untouched), no new dependencies (`Server/package.json` untouched).

## OUT of scope / deferred

- Persistent stage timings on `GenerationRecord` (needs OpenAPI + Swift schema work; revisit with Batch 4 pipelining where queue position lands). Slice 7 is log-only.
- Client-side `Logger` coverage and an "Export diagnostics" action (`10 F5` client half) — Batch 2 owns client feedback; this batch is server-only.
- Cancel/timeout still evicting the warm helper (`helper-process.ts:144-146,323`): needs a cooperative abort in the helpers (`07 F8`, Batch 4/C++). The MLX text helper's self-exit 2 s after its own deadline (`TextEngine/Sources/InlayTextEngine/CorrectionEngine.swift:22`) is likewise untouched — after slice 2 its `error` line rejects the request and the subsequent process close resets an already-dead child harmlessly, now with an exit log line.
- Import-journal quarantine (`imports.ts:415-461` stays fail-fast + named) and all other Wispr work — Batch 8 isolates Wispr (`DECISIONS.md:17`).
- Tokenless-loopback exposure of detailed health to local processes — that's P1-15, fixed by per-device tokens in Batch 5. This batch only removes the _remote, token-configured_ unauthenticated leak/warmup trigger.
- `serverVersion: "0.1.0"` hardcoding (`generation-service.ts:362`) — Batch 7 version stamping.
- Log-level/`INLAY_LOG_LEVEL` plumbing, log shipping, and health readiness-transition diffing.
- Anything in `Sources/InlayServerKit/` (deleted in Batch 8), `Engine/`/`TextEngine/` C++/Swift helpers, or `vendor/`.

## Risks

- **Slice 2 misclassification**: a helper `error` that actually leaves the process corrupt would now poison the next request instead of being evicted. Mitigations already in code: single outstanding request (`helper-process.ts:140`), workers reset context per request (above), and any follow-on protocol garbage still hits the invalid-JSON/unknown-event `reset()` paths. Residual risk accepted; the exit/stderr logging in slice 3 makes a bad survivor visible.
- **stderr ring content**: upstream ggml libraries could in principle print unexpected text to stderr on new driver versions. The ring is capped (64 KiB retained, 4 KiB logged), memory-only, never persisted to records — worst case is noisy operator logs, not transcript archival. Both Inlay workers suppress token-bearing debug output by design.
- **Quarantine hides data silently**: a record vanishing from history after an upgrade could confuse. Mitigated by the startup log line + `quarantine-reason.txt`; a user-facing "N records need attention" surface is a Batch 2/5 UI question, noted here so it isn't lost. Quarantined audio also sits outside any future retention sweep (Batch 5 cap) — Batch 5 must count `*.quarantined` dirs in usage.
- **Health auth breakage for unknown consumers**: anything besides the Swift client, Dockerfile, and `run-dev.sh` that polls `/v1/health` without the token now gets 401. Grep covered the repo; external scripts are the owner's. Liveness is the documented replacement.
- **Backoff delays legitimate recovery**: after fixing a missing model, warmup may wait up to 5 min unless preferences are saved (force path) or the server restarts. Acceptable vs. today's infinite respawn; `warmup_failed` log lines state `retryAfterMs`.
- **Stack drift**: Batch 1 will have edited `initialize`/`shutdown`/`finish`; re-anchor all line numbers on the stack tip and expect small merge adjustments in slices 4 and 7, not redesign.

## Verification

Static gates (run from repo root after the final code edits of each slice that touches TS, per `AGENTS.md:3,5`):

```sh
bun install --frozen-lockfile
bun run fmt
bun run check
bun run lint
bun run fmt:check
```

- Slice 6 only (API contract change, per `AGENTS.md:9` and `docs/development.md:42`): `bun run generate:api --check` (full, not `--typescript-only`) and, if the generator changed Swift bindings, `swift build --force-resolved-versions`. No other slice touches Swift or the API, so no other slice needs the Swift build.
- No automated behavioral tests are added or run (banned, `AGENTS.md:4`); no parity checks in this batch.

Owner manual verification with Inlay Dev (isolated instance only — never the real archive; `bun run dev` uses `.local/dev-server/data` on `0.0.0.0:8392`, `docs/development.md:14`):

1. Start `bun run dev`, build/launch Inlay Dev per `docs/development.md:24-33`, dictate one normal take end-to-end (real helpers/models; `ready: false` does not count). Confirm stderr/`server.log` shows `server_listening`, `helper_spawned`/`helper_ready`, and one `generation_processed` line with plausible stage timings — and that no transcript text appears in any log line.
2. Quarantine: stop the dev server, copy one generation dir in the dev archive to a fresh UUID name, corrupt its `metadata.json` (truncate it), restart. Server must start, log `generation_quarantined` naming the ID, the dir must now be `<ID>.quarantined` with `quarantine-reason.txt`, and Inlay Dev history/dictation must still work. Rename it back with repaired metadata, restart, confirm it reappears. (Repeat mentally for `schemaVersion: 99`.)
3. Helper survival: dictate a take long enough to overflow the proofreading context (or otherwise trigger a Qwen per-request error); the take completes with deterministic text, and the _next_ take must not pay a model reload — log shows no second `helper_spawned` for Qwen between the two.
4. Warmup backoff: point a throwaway dev instance at a missing/wrong model path; confirm `warmup_failed` lines space out (5 s, 10 s, 20 s…) instead of every 5 s health poll, health in the app explains the failure, and saving Server preferences retries immediately.
5. Liveness/readiness: `curl http://localhost:8392/v1/liveness` returns `{"status":"ok"}` with no token; `curl /v1/health` without the dev token returns 401, with `Authorization: Bearer` returns the full readiness payload; Inlay Dev (token configured) shows normal server status throughout. Liveness curls must not trigger `helper_spawned` on a cold server.
6. Helper exit diagnostics: kill a helper process mid-idle (or use a bad helper path in the throwaway instance) and confirm `helper_exited` logs exit code/signal plus a stderr tail.

Report explicitly any step that could not be exercised because models, microphone, or permissions were unavailable (`AGENTS.md:8`); a `/v1/liveness` or `/v1/health` curl alone verifies reachability only.

## Open questions

None blocking. Chosen defaults, stated above and applied without asking: preferences stay strict-but-named while generations quarantine; two-route liveness/readiness split (not two shapes on one route); 5 min capped exponential warmup backoff with preferences-save as the force-retry (not a permanent latch); stage timings log-only this batch; stderr tail logged on helper exit but never persisted into `metadata.json`.
