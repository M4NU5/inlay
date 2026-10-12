# Audit decisions — 2026-10-10

Source: `REPORT.md` in this directory. Decided by Ben, one/two questions at a time.
Worktree: `audit/major-issues` @ `b6d6815`.

## Decided

- **Audience: shareable product.** Non-devs should be able to download/install/update. P1-14, P1-21, P1-22 are real P1s.
- **Interruption: save the speech.** Retry chunk uploads, partial-finish on interruption, requeue sealed takes on restart. No total loss for recorded speech.
- **First run: guided install.** The app installs/starts the server and downloads models itself.
- **Undelivered results: badge + copy.** Finished-but-undelivered text shows as ready to copy on relaunch. Never auto-paste.
- **Auth: per-device token.** Each Mac gets its own revocable credential. Same-Mac unauthenticated access (P1-15) is fixed as part of this.
- **Retention: cap audio.** Text kept forever; audio auto-deletes after a cap with a usage/purge screen.
- **Cancel/failure: keep in history.** Cancelled/failed audio stays in history (explicitly overrides the report's P1-18 recommendation). Consequence: "cancel ≠ discard" becomes intended product semantics, so kept cancelled audio must be clearly labeled and deletable in the UI.
- **Trigger: latch + menu.** Tap to start, tap to stop, plus a menu-button trigger. Hold still works.
- **Copied: attention state.** "Copied, not inserted" stays visible with a copy-again affordance. Not a success dismissal.
- **Wispr import: migration bridge.** Isolate now, delete after a set window. Imported history stays readable.
- **Swift reference server: delete it.** One server (Bun/TypeScript).
- **Remote transport: Tailscale only.** No built-in TLS. Warn loudly on plain HTTP outside trusted networks.
- **Corrupt records: quarantine + serve.** Server starts, sets the bad record aside, names it.
- **Checks: parity checks allowed.** Deterministic constant/prompt/API agreement checks may live in `bun run check`. Behavioral test suites remain banned per `AGENTS.md`.
- **Layout: flat + dividers.** One shared flat page scaffold across all five pages.
- **Start with: stop losing speech** (Batch 1).

## Defaults applied (not asked — from REPORT.md recommendations, change on request)

- Failed-with-audio records get "transcribe again" (consistent with keep-in-history).
- Client deadline for a wedged generation: 15 minutes, then badge + copy path.
- Logging: operational metadata only, never transcripts or audio. Unauthenticated health = liveness only.
- Missing Accessibility blocks at key-press with a permission HUD, instead of recording an unusable take.
- Escape cancels only while capturing, not while processing. Mic-test stays a separate, clearly labeled action.
- Audio cap starting point: 90 days for both audio streams, visible usage + manual purge.
- API: retire the hand-written Swift wire facade type-by-type; `reportedAt` = server receipt time; `mode` joins the create idempotency key; history-summary payload may break under an API version bump.
- Versions independent per component, API integer is the contract; real GitHub Releases instead of expiring artifacts.
- Test-microphone takes excluded from shared history.

## Fix order under these decisions

1. **Batch 1 — Stop losing speech (L, start here):** health-poll no-kill (S), chunk-flush retry with backoff, partial-finish on interruption, receiving-expiry fix, startup requeue of sealed records, events deadline + persisted pending IDs with badge+copy relaunch path, seal space pre-check.
2. **Batch 3 — Server resilience + observability (M):** quarantine-and-serve startup, structured metadata-only logging + helper stderr ring, per-request vs fatal helper errors, health liveness/readiness split.
3. **Batch 2 — Make outcomes visible (M):** HUD-only refusals, VoiceOver announcements + sounds (P0-1), copied=attention, stage/queue HUD text, contrast fix.
4. **Batch 4 — Server throughput (L):** per-generation locks, stage pipelining or cap+position, original-audio admission policy.
5. **Batch 5 — Trust & data (L with per-device creds):** per-device tokens incl. same-Mac, retention cap + usage/purge UI, label kept cancelled/failed audio.
6. **Batch 6 — Access & platform (L):** latch + menu trigger, keyboard/focus, flat single page scaffold.
7. **Batch 7 — Ship shape (L):** guided install + model download, Developer ID + notarized DMG, real Releases, CI packaging job (the CI job itself is S and can land any time).
8. **Batch 8 — De-duplicate (L, opportunistic):** delete Swift server, isolate Wispr, generate shared constants/prompt/pins with parity check, retire wire facade type-by-type.
