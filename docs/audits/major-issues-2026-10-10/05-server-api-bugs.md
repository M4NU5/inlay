# Server / API correctness bugs — major-issues audit (2026-10-10)

- Scope: Bun/TypeScript server (`Server/src/`) HTTP boundary, validation, error shapes, token handling, NDJSON streaming, and contract consistency with `Server/api/openapi.yaml`, the Swift client (`Sources/Inlay/ServerClient.swift`, `Sources/InlayAPI/`), and the generation/import services. Swift reference server (`Sources/InlayServerKit/`) was out of scope for this file except as drift surface.
- Method: every cited file body was opened and read in the audit worktree at HEAD `b6d6815`. No code was edited and no tests were run, per `AGENTS.md:4`. "Verified-in-code" means the mechanism is directly visible in the cited lines; user-visible timing effects that depend on load are marked inferred.

## Findings

### 1. P1 — Global mutation queue serializes heavy file I/O, so one take's finish/seal stalls every other take's uploads

- Evidence: all durable operations share one promise chain (`Server/src/generation-service.ts:196`). Inside that queue, `appendAudio` writes and `fsync`s every chunk (`Server/src/generation-service.ts:614`), and `finish` seals both WAV files (`Server/src/generation-service.ts:687`) — `seal` copies the entire raw stream in 1 MiB chunks before returning (`Server/src/generation-service.ts:964`). `history` also sorts and deep-clones records inside the queue (`Server/src/generation-service.ts:720`), and Wispr import completion does multi-file reads/writes and directory swaps under the same queue (`Server/src/generation-service.ts:919`, `Server/src/imports.ts:643`).
- User impact: the product promise is "start another take immediately: finished recordings queue on the server" (`README.md:3`). While take A is sealing (up to ~11 MiB inference + much larger original audio) or an import completes, take B's chunk acknowledgements wait behind that I/O. The client pipe drops the whole take when its 512-chunk buffer overflows (`Sources/Inlay/ServerClient.swift:132`), surfacing "The connection cannot keep up with the microphone." Health and preferences calls also queue behind the same lock, so the app can look disconnected during a seal.
- Confidence: verified-in-code for the serialization structure; inferred for how often the stall exceeds client buffering (depends on disk speed and original-audio format).
- Fix direction: keep the mutation queue for in-memory/metadata state transitions only; move chunk writes, sealing, and import file copying outside it, keyed by per-generation locks, and re-enter the queue briefly to publish results.
- Effort: L
- Owner decision: none — internal restructure, no contract change.

### 2. P1 — One corrupt, duplicate, or future-version record in the archive prevents the server from starting at all

- Evidence: startup loads every generation directory and rethrows anything other than a missing file, including schema-validation failures (`Server/src/generation-service.ts:252`) and `schemaVersion !== 1` (`Server/src/generation-service.ts:259`). A duplicate Wispr Flow source ID throws the same way during indexing (`Server/src/imports.ts:465`, called at `Server/src/generation-service.ts:285`). `startServer` propagates the failure before listening (`Server/src/main.ts:11`).
- User impact: a single damaged `metadata.json` (disk error, interrupted future migration, hand-edited archive, downgrade after an upgrade) takes down the entire API — no dictation, no history, no health — until an operator finds and removes the offending directory by hand. There is no quarantine or per-record skip.
- Confidence: verified-in-code.
- Fix direction: load records defensively at startup: quarantine unreadable/invalid directories to a named side folder (or mark them failed in memory), log the directory name, and serve the remaining archive.
- Effort: M
- Owner decision: should an unrecognized future `schemaVersion` quarantine-and-serve, or is fail-closed an intentional anti-downgrade guard? See Questions.

### 3. P1 — History returns full records; the client hard-fails the whole history page above 16 MiB

- Evidence: `GET /v1/generations` returns deep copies of complete `GenerationRecord`s — including each record's full settings snapshot (dictionary + proofreading prompt) and text-processing provenance with duplicate transcript copies (`Server/src/generation-service.ts:740`). Page size defaults to 50 and allows 100 (`Server/src/http-server.ts:131`, `Server/src/generation-service.ts:712`), and each record's stored metadata may be up to 1 MiB (`Server/src/generation-service.ts:1201`). The client rejects any history response over 16 MiB as an invalid response (`Sources/Inlay/ServerClient.swift:160`).
- User impact: as the shared dictionary and prompt grow (both are copied into every record), the first history page eventually crosses 16 MiB and history stops loading entirely on every Mac — data is intact on disk but unreachable through the app. The same cost is paid on every page fetch: sorting all records and cloning full snapshots inside the global mutation queue (finding 1).
- Related minor defects in the same endpoint: a cursor whose record was deleted between pages returns `400 invalid_cursor` instead of resuming (`Server/src/generation-service.ts:731`), and ordering is whole-second `createdAt` with a random-UUID tiebreak (`Server/src/generation-service.ts:56`, `Server/src/generation-service.ts:726`), so takes started within the same second can display out of order.
- Confidence: verified-in-code for the shapes and the client cap; inferred for the archive size at which a given user's page crosses the cap.
- Fix direction: add a summary projection for list responses (omit `settings`, `textProcessing`, and hint details; fetch full records per-item on demand). That is an API-shape change and needs a version decision.
- Effort: L
- Owner decision: is a breaking history-payload change acceptable now, or should the server first strip fields the history UI provably never reads under the current schema? See Questions.

### 4. P1 — Validation failures are undiagnosable, and several rejection rules exist only in server code, not in the contract

- Evidence: Ajv is configured with `allErrors: true`, but `validateBody` discards them and throws one generic message naming only the schema (`Server/src/validation.ts:18`, `Server/src/validation.ts:27`). The client displays the server `message` verbatim (`Sources/Inlay/ServerClient.swift:95`). Separately, enforceable rules live outside OpenAPI: delivery `status` is a free string in the schema (`Server/api/openapi.yaml:816`) but the server accepts only a hardcoded list (`Server/src/generation-service.ts:821`); device `id`/`name` are unconstrained strings in the schema (`Server/api/openapi.yaml:590`) but the server imposes grapheme/whitespace rules (`Server/src/generation-service.ts:417`); preference byte limits appear only as prose descriptions (`Server/api/openapi.yaml:626`) while enforcement is server-only (`Server/src/generation-service.ts:114`). The schema also requires `DeliveryReceipt.reportedAt` (`Server/api/openapi.yaml:820`), yet the server ignores the client's value and stamps its own time (`Server/src/generation-service.ts:850`).
- User impact: when a schema-valid request is rejected — most likely after a client/server version skew or a dictionary edge the UI allowed — the user sees "The request did not contain valid PreferencesSnapshot JSON." with no field, rule, or recovery hint, for what may be a one-character problem. Any third-party or future generated client built from the contract alone will produce requests the server rejects for reasons the contract never states.
- Confidence: verified-in-code.
- Fix direction: include the first failing JSON pointer/reason in the `invalid_json` message (the error shape already has `code`/`message`); move the delivery-status enum and device/preferences constraints into `openapi.yaml` so generated bindings carry them; either honor or drop the required `reportedAt` field.
- Effort: M
- Owner decision: is `reportedAt` meant to be client-reported evidence or server receipt time? See Questions.

### 5. P2 — Event streaming re-sends the full record every two seconds and on every progress tick, with silent drops for slow readers

- Evidence: a 2-second timer pushes the current full record to every watcher as a heartbeat (`Server/src/generation-service.ts:291`, `Server/src/generation-service.ts:298`), and transcription progress callbacks push another full copy per helper update with no coalescing (`Server/src/generation-service.ts:1052`, `Server/src/generation-service.ts:1228`). Each push deep-clones the record per watcher (`Server/src/generation-service.ts:1224`), and a watcher queue longer than 8 silently discards the oldest records (`Server/src/generation-service.ts:1225`). The per-generation stream cap is 8 (`Server/src/generation-service.ts:752`) but there is no global cap across generations.
- User impact: a take queued behind other work streams its entire record — settings snapshot, dictionary, and all — roughly 30 times a minute per watcher for as long as it waits, over what may be a remote/Tailscale link (supported topology, `README.md:41`). Heartbeat cloning runs inside the mutation queue, competing with audio uploads (finding 1). Slow readers lose intermediate states without any signal; today the client only needs the terminal record, but the drop policy is invisible in the contract (`Server/api/openapi.yaml:279` promises "complete GenerationRecord values" without mentioning drops).
- Confidence: verified-in-code.
- Fix direction: send a lightweight heartbeat (blank line or tiny keep-alive object the client already tolerates — it skips empty lines, `Sources/Inlay/ServerClient.swift:340`), dedupe unchanged records, and throttle progress to a fixed cadence; document the drop policy in the contract.
- Effort: M
- Owner decision: none if the heartbeat stays wire-compatible.

### 6. P2 — A 45-second upload stall permanently kills an in-progress remote take, and uploads cannot resume

- Evidence: receiving generations are cancelled when `updatedAt` is 45 seconds old (or the take is 5 minutes old) (`Server/src/generation-service.ts:307`); `updatedAt` only advances on a successful chunk append (`Server/src/generation-service.ts:622`). Upload state (sequence/format/byte counts) lives only in memory (`Server/src/generation-service.ts:166`), and restart deletes partial audio and marks receiving records failed (`Server/src/generation-service.ts:272`, `Server/src/generation-service.ts:1234`). The client has no chunk-upload retry — a failed flush throws (`Sources/Inlay/ServerClient.swift:389`) — even though the server implements exact-byte replay detection for repeated sequences (`Server/src/generation-service.ts:551`), which only helps within the 45-second window.
- User impact: on a remote server (Wi-Fi roam, VPN/Tailscale hiccup, laptop sleep mid-hold, or a server stall from finding 1), a pause longer than 45 seconds loses the entire dictation; the next chunk gets `409 upload_closed` and the user must re-speak everything. The user gets no "resume/retry" affordance even though most audio already reached the server.
- Confidence: verified-in-code for the mechanism; inferred for real-world stall frequency.
- Fix direction: lengthen/configure the receiving expiry, persist upload manifests next to the `.raw` files so appends survive restart, and let the client retry a flush (the server's replay check makes this safe).
- Effort: L
- Owner decision: how long should the server hold partial audio for a possibly-abandoned take? See Questions.

### 7. P2 — Health is unauthenticated on token-protected servers, leaks internal state, and triggers model loading

- Evidence: the auth hook exempts `/v1/health` even when a token is configured (`Server/src/http-server.ts:80`), matching `security: []` in the contract (`Server/api/openapi.yaml:30`). The response discloses API/server version, Dev mode, per-model readiness, and operational messages such as storage-full and shutting-down (`Server/src/generation-service.ts:349`). Worse, health is side-effectful: when speech is not loaded it kicks off warmup (`Server/src/generation-service.ts:359`), including after a load failure — failed warmups record an error but nothing stops the next anonymous health call from retrying the load (`Server/src/generation-service.ts:1255`).
- User impact: anyone who can reach a remote server's port — without the token — can fingerprint it, learn when its disk is full or its models are down, and repeatedly force multi-gigabyte model-load attempts (GPU/RAM churn) on the owner's machine.
- Confidence: verified-in-code.
- Fix direction: require the token for health when one is configured (the client already sends it on every request, `Sources/Inlay/ServerClient.swift:64`), or split a minimal unauthenticated liveness response from the detailed readiness payload; make warmup timer/admission-driven rather than health-driven, with backoff after load failure.
- Effort: S–M
- Owner decision: does any supported flow need pre-token health (e.g. first-run server discovery before the token is entered)? See Questions.

### 8. P2 — Tokenless mode trusts the Host header, and the single shared token plus self-asserted device IDs give every caller full power

- Evidence: with no token, access is granted solely on a loopback-looking `Host` header; the socket peer is never checked (`Server/src/http-server.ts:73`, `Server/src/http-server.ts:24`). Configuration only guarantees a token when the bind host itself is non-loopback (`Server/src/configuration.ts:103`). With a token, one shared bearer (compared case-sensitively including the `Bearer ` prefix, `Server/src/http-server.ts:83`) authorizes every route, including history deletion, artifact download, and preference rewrites. Device identity is a caller-asserted plain string (`Server/api/openapi.yaml:590`), and security-relevant decisions trust it: create idempotency is keyed by it (`Server/src/generation-service.ts:428`) and continuation requires only a matching asserted device ID (`Server/src/generation-service.ts:1003`).
- User impact: if a tokenless same-Mac server is ever exposed through a TCP forwarder, container port publish, or SSH tunnel — a common way to "just try it from the other machine" — any remote caller who sends `Host: localhost` gets the full unauthenticated API: all recordings, transcripts, and delete/preference control. On a shared-token multi-Mac setup, any compromised or malicious client can impersonate another Mac's device ID, hijack its in-flight idempotency keys and list continuations, and erase shared history; there is no per-device credential to revoke.
- Confidence: verified-in-code; exposure scenario inferred (requires operator forwarding, since direct non-loopback binds demand a token).
- Fix direction: in tokenless mode also require a loopback peer address (`request.socket.remoteAddress`); document the shared-token trust model explicitly, or introduce per-device tokens if multi-user servers are ever in scope.
- Effort: S for the peer check; L for per-device credentials.
- Owner decision: is the server strictly single-user (all token holders fully trusted)? See Questions.

### 9. P2 — Generation checks cover only TypeScript locally, and wire constants are hand-duplicated in three places

- Evidence: the root `check` gate runs API generation with `--typescript-only` (`package.json:16`), although `AGENTS.md:5` describes it as the "generated API consistency" gate; Swift wire bindings are only checked by the separate full generation, which clones the generator from GitHub when its checkout is missing (`Server/scripts/generate-api.ts:59`). The values that actually decide compatibility are literals maintained by hand in three spots: `x-inlay-*` extensions (`Server/api/openapi.yaml:11`) vs `Server/src/api.ts:71` vs `Sources/InlayAPI/API.swift:5`. The supported-language list exists three times (`Server/api/openapi.yaml:605`, `Server/src/generation-service.ts:90`, `Sources/InlayAPI/API.swift:55`), and the default proofreading prompt is duplicated verbatim in both languages (`Server/src/generation-service.ts:64`, `Sources/InlayAPI/API.swift:37`).
- User impact: a contract edit can pass local checks with stale Swift bindings and only fail in CI — or worse, ship with one constant changed in only one place. Because the client hard-rejects any server whose `apiVersion` differs (`Sources/Inlay/InlayController.swift:151`), a one-sided version/limit edit becomes a total dictation outage after upgrade, diagnosed only as "Server API version is incompatible."
- Confidence: verified-in-code (current values agree today; the risk is the missing guardrail).
- Fix direction: generate the constants (version, port, limits, language list) into both facades from `openapi.yaml`, and make the default prompt single-sourced (server-served, as `--print-default-proofreading-prompt` already half-implies, `Server/src/main.ts:45`); document that Swift binding freshness is a CI-only gate, or vendor the generator so the local check can include it offline.
- Effort: M
- Owner decision: none — build hygiene.

### 10. P2 — Create idempotency ignores the request payload, and replays return 201 even for terminal records

- Evidence: `create` returns any existing record with the same `(requestID, device.id)` without comparing `mode` or device name (`Server/src/generation-service.ts:428`), and the HTTP layer always answers `201` (`Server/src/http-server.ts:124`). This check also runs before the readiness/disk gates (`Server/src/generation-service.ts:434`) and scans the entire in-memory archive linearly on every create.
- User impact: if a client ever retries create with a reused request ID but a different mode — e.g. Test Microphone after a dictation attempt reused the ID — the server silently returns the original record, so a take the user believes is a non-inserting test can be processed and delivered under dictation rules. A replay against a failed/cancelled record returns that terminal record as if newly created; the client then fails on its first audio chunk with `upload_closed` instead of a clear "this request already finished." The linear scan also gets slower with every generation ever recorded.
- Confidence: verified-in-code for the semantics; inferred for reachability (the current Swift client mints a fresh UUID per take and does not retry create, `Sources/Inlay/ServerClient.swift:226` — so today this needs a future client change or a second client implementation to bite).
- Fix direction: bind the idempotency key to the payload (mode at minimum): same key + same payload → replay (200); same key + different payload → `409`; terminal replay → return the record with a status the client can branch on. Index by `(requestID, device.id)` in a map.
- Effort: S
- Owner decision: none.

### 11. P2 — Size limits disagree across layers, so contract-valid requests fail mid-flight with wrong errors

- Evidence: (a) `WisprFlowKnownIDsRequest` allows 10,000 UUIDs (`Server/api/openapi.yaml:1052`) and the service repeats that limit (`Server/src/imports.ts:478`), but 10,000 UUIDs serialize to roughly 390 KiB of JSON — above the server's global 256 KiB body limit (`Server/src/http-server.ts:60`) — so schema-valid requests die as `body_too_large` at roughly 6,700 IDs. The first-party client already works around this with a hardcoded 5,000 batch and an apologetic comment (`Sources/Inlay/ServerClient.swift:164`). (b) Appends accept audio up to 180.1 seconds per stream (`Server/src/generation-service.ts:584`) but `finish` rejects anything over 180.0 (`Server/src/generation-service.ts:655`), so a boundary take uploads completely and only then fails with `400 invalid_duration`, leaving the record stuck in `receiving` until expiry. (c) The same append cap of 256 MiB per stream makes the contract's advertised formats unusable at full length: 192 kHz/8-channel original audio (allowed at `Server/api/openapi.yaml:174`) hits the cap near 44 seconds, mid-take, with a generic `recording_limit` error. (d) A negative audio `sequence` is reported as `413 chunk_limit` ("exceeded its audio chunk limit") rather than a 400 parameter error (`Server/src/generation-service.ts:505`).
- User impact: bulk Wispr imports from any contract-following client fail inexplicably; boundary-length dictations and high-end audio interfaces (192 kHz interfaces are exactly what pro-audio users own) lose takes at the last moment or mid-recording with misleading messages.
- Confidence: verified-in-code, including the byte arithmetic for (a) and (c).
- Fix direction: align the four layers — contract `x-maximumBytes`/`maxItems`, Fastify `bodyLimit`, service checks, and client batching — to one derived set of numbers; enforce the 180-second rule identically in append and finish; express the original-audio cap as duration-per-format or state the byte cap in the contract.
- Effort: S–M
- Owner decision: none.

## Questions for owner

- Startup resilience (finding 2): should an unrecognized future `schemaVersion` or corrupt record quarantine-and-serve, or is fail-closed an intentional guard against downgrades and silent data loss?
- History payload (finding 3): is a breaking list-response change (summary records) acceptable as an API version bump, given clients already hard-reject mismatched versions and must upgrade in lockstep?
- Delivery receipt (finding 4): is `reportedAt` supposed to record when the Mac actually inserted the text (client clock) or when the server heard about it (server clock)? The schema requires the client to send it; the server currently discards it.
- Partial uploads (finding 6): how long should the server retain partial audio for a take that may resume — minutes, hours, or is the 45-second expiry a deliberate privacy/storage tradeoff?
- Health access (finding 7): does any first-run or diagnostic flow require reading health before a token is configured on the client?
- Trust model (finding 8): is the server permanently single-user, with every token holder fully trusted (delete history, rewrite preferences, assert any device identity)? If yes, this should be stated in `Server/README.md` so operators do not share tokens across trust boundaries.
- Test-mode safety (finding 10): should `mode` be part of the create idempotency key even though the current client never reuses request IDs, as a guard for future clients?

## What I could not verify

- Runtime behavior of any endpoint: per `AGENTS.md:4` no tests were run and no live server was started; all findings are code-path reads at HEAD `b6d6815`, not observed responses. Fastify behaviours I rely on (encapsulated content-parser inheritance into the import routes at `Server/src/http-server.ts:240`, stream destruction closing artifact file handles from `Server/src/generation-service.ts:890`) are inferred from framework documentation, not exercised.
- Generated binding freshness: I read the generator (`Server/scripts/generate-api.ts`) and facades but did not run `generate:api --check` (the Swift half clones a generator from the network), so I cannot confirm the committed `Sources/InlayAPIWire/*` and `Server/src/generated/api.ts` are byte-current — only that the hand-written constants agree with the contract today.
- Helper progress cadence behind finding 5: the server forwards `inlay-engine` progress unthrottled, but how often the helper actually emits progress is engine-side (`Engine/worker.cpp`) and outside this file's scope.
- Swift reference server parity: whether `Sources/InlayServerKit/` shares these defects was not audited here; each finding above is verified only against the shipped TypeScript server.
- Client retry behaviour beyond `ServerClient`: which HTTP errors `InlayController` surfaces verbatim versus replaces was only spot-checked at the delivery/history call sites, not exhaustively traced.
