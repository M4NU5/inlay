# Product UX flows — major issues (2026-10-10)

- Scope: onboarding, permissions, first-run, dictation start/stop, insertion, history, preferences/settings, model setup/download, errors/empty states, discoverability, friction, dead ends, evaluated as first-time and returning user. Static audit only in worktree `audit/major-issues` at HEAD `b6d6815`; no product code edited, no tests run, no runtime dictation performed (per `AGENTS.md:4`, `AGENTS.md:6`). Every claim below was verified by opening the cited body unless marked _inferred_. Severity: P0 = core flow broken / data-loss default, P1 = loses user work or trust / routinely blocks the core loop, P2 = significant friction/confusion with a workaround.
- Context worth keeping: much of the safety UX is genuinely good and should not be undone — destination re-validation with human reasons (`Sources/Inlay/System/TextInserter.swift:198`, blocked reasons at e.g. `:394`/`:461`), clipboard preservation (`TextInserter.swift:622`), protected-field blocking (`TextInserter.swift:368`), 30-second limit warning (`Sources/Inlay/RecordingFeedback.swift:25`), and permission help with drag-into-Accessibility (`Sources/Inlay/Views/PermissionHelpView.swift:41`). The findings below are the gaps those strengths do not cover.

## Findings

### 1. Failed dictation start steals focus from the target app (P1)

- Evidence: `Sources/Inlay/InlayController.swift:852` — when the server is not ready, microphone permission is missing, or no microphone resolves, `beginDictation` calls `onShowWindow?()` (`:852`–`:855`). `showWindow()` switches to `.regular` policy and activates Inlay ignoring other apps (`Sources/Inlay/InlayApp.swift:126`).
- User experience: a returning user in Slack/Mail holds Right Option while the server happens to be offline/restarting (common: server is a separate process, quitting the app leaves it running and vice versa — `README.md:83`, `Sources/Inlay/Views/PreferencesPage.swift:103`). Instead of a quiet "not ready" in place, Inlay's 940×700 window takes over the screen and the text field loses focus. Every retry while offline repeats the theft. The user must switch back, re-place the caret, and only then discover whether the server recovered. This punishes exactly the muscle-memory gesture the product is built on.
- Confidence: verified-in-code.
- Fix direction: on hotkey-initiated failure, show only the HUD error (already built: `showError` at `InlayController.swift:1195`) and let a HUD/menu click open the window. Reserve forced window presentation for Test-button clicks (user already in Inlay) and first-launch/permission-missing launch (`InlayApp.swift:67`).
- Effort: S.

### 2. First run is a dead end: no in-app server or model setup path (P1)

- Evidence: first launch opens the main window (`Sources/Inlay/InlayApp.swift:67`) on the Dictation page (default `page = .dictation`, `Sources/Inlay/Views/InlayWindowView.swift:25`), not the connection page. The endpoint defaults to `http://127.0.0.1:8391` (`Sources/Inlay/ClientPreferences.swift:30`); the client never starts a server (inventory `00-inventory.md:4`). Getting a server is terminal-only: build scripts, ~4 GB of pinned model downloads by shell loop (`Server/README.md:42`, `:59`), env vars, then token entry (`README.md:16`, `README.md:26`). `ModelStatus`/`EngineStatus` in `Sources/Inlay/AppState.swift:14` are unused — no download UI exists anywhere in `Sources/Inlay/` (grep confirms zero call sites).
- User experience: a first-time user who installs only the app gets "Server offline · Recording unavailable" (`InlayController.swift:693`) with permission rows that, once granted, change nothing. Nothing in the Dictation page says a separate server + models must exist, where to get them, where the token comes from, or that "This Mac" is the page that fixes it. The gear button does route to This Mac (`InlayWindowView.swift:200`), but the form there (`PreferencesPage.swift:24`) gives no token-location hint either. Owner-as-only-user may accept this, but as written the product is unusable without reading three README files and running shell commands.
- Confidence: verified-in-code (absence verified by grep over `Sources/Inlay/` plus the README flow).
- Fix direction (smallest viable): route first-launch/no-server-reachable users to the connection UI with one short explainer — where the token file lives, the two URLs to try (8391 packaged / 8392 dev), and a link/button to the server guide. Full guided install/model download is an owner-scope decision (see Questions).
- Effort: S for routing+copy; L for guided install.

### 3. An interrupted in-progress take loses the user's speech — there is no salvage or retry (P1)

- Evidence: sleep/lock/power-off → `restForSystem()` fails the capture and stops all pending dictations (`InlayController.swift:1229`); recorder interruption → `failSession(..., cancelServer: true)` (`InlayController.swift:822`, `:757`); periodic health check failing while capturing also fails the session (`InlayController.swift:319`); upload failure mid-take fails and cancels the server generation (`InlayController.swift:905`). Quitting likewise cancels the incomplete recording (`InlayController.swift:802`). Client capture files are explicitly temporary and orphan-cleaned at launch (`InlayController.swift:248`); there is no offline queue or upload retry (inventory `00-inventory.md:70`).
- User experience: dictate 90 seconds, the lid closes / screen locks / Wi-Fi drops / the Bluetooth mic dies — everything said so far is gone. The message even says "Check shared history for completed results" (`InlayController.swift:1233`), but an unsealed take was actively cancelled server-side, so history will not contain it. Only takes fully uploaded and sealed before the interruption survive. For a dictation product, silent loss of already-spoken words is the most trust-damaging failure class.
- Confidence: verified-in-code.
- Fix direction: keep the local captured audio on failure and offer "Retry upload" (the audio already exists locally until `cleanup()` — see `finishDictation` defer at `:975`); at minimum, distinguish "sealed — find it in History" from "discarded" in the message. A durable local outbox is the L version — owner decision.
- Effort: M (retry sealed-style re-upload) / L (outbox). Copy fix: S.

### 4. "Copied instead of inserted" and outright insertion failure are near-invisible at the moment they matter (P1)

- Evidence: delivery outcomes `.copied` / `.unconfirmed` / `.failed` are set at `InlayController.swift:1134`–`:1141`, but HUD dwell is 1.7 s for anything except `needsAttention` (failed/unconfirmed only — `Sources/Inlay/Views/MenuHUDViews.swift:93`), so clipboard fallback dismisses like success at `InlayController.swift:1038`; errors dismiss after 4 s (`:1199`). The HUD itself shows only an icon; the reason text is a tooltip/accessibility value (`MenuHUDViews.swift:181`, `:184`). There is no "insert/copy again" recovery action anywhere (grep: no retry/reinsert in controller or views); recovery means opening Inlay, finding `lastTranscript`, and using Copy (`InlayWindowView.swift:173`, disabled while busy) or digging through History.
- User experience: long queue times invite switching apps/fields, which is exactly what makes re-validation fail (`TextInserter.swift:252`, `:372`) or degrade to clipboard (`TextInserter.swift:226`). The user, eyes on the target app, sees a small icon flash, assumes insertion, keeps working — then copies something else and the transcript leaves the clipboard. The safety design correctly refuses blind typing; the feedback design does not carry that decision to the user.
- Confidence: verified-in-code (dwell times and icon-only HUD read directly; absence of reinsert via grep).
- Fix direction: treat `.copied` as needs-attention (long dwell + distinct label), keep failed/unconfirmed HUD up until dismissed or clicked, and add a one-click "Copy again" from the HUD/menu for the last result (menu already has Copy Last — surface the same for failed deliveries with status text, not just in the window).
- Effort: S–M.

### 5. While processing, the user sees an opaque spinner — no stage, queue position, or "stuck" signal (P1)

- Evidence: `applyProgress` maps server states to status text (`InlayController.swift:1081`), but that text renders only in the Dictation window (`InlayWindowView.swift:220`). In the HUD, non-capturing states collapse to a single `ProgressView` circle (`MenuHUDViews.swift:219`; expansion turns off when capture ends at `:115`). Inference is serial FIFO, so queued takes are normal (inventory `00-inventory.md:69`), yet no view exposes pending-take count (grep: `pendingDictations` never reaches a view). Health can also report ready while speech is still loading — "Loading server models; recordings will queue" (`Server/src/generation-service.ts:354`) — and the client admits takes on `ready` alone (`InlayController.swift:151`).
- User experience: release the key in another app, then watch an undifferentiated spinner. Queued behind two earlier takes? Models still loading? Server wedged? All identical. There is no elapsed time, no "queued (2 ahead)", no way to tell a healthy 20 s proofread from a dead connection, and clicking the spinner cancels the take (finding 9) — the one interactive affordance on the only visible surface is destructive. Users will cancel-and-redo, or double-dictate, creating duplicates and wrong-field risk.
- Confidence: verified-in-code.
- Fix direction: show the existing stage string (and queue count when >1 pending) as HUD text next to the spinner; add a soft "taking longer than usual" state after a threshold. No protocol change needed — states already stream.
- Effort: S–M.

### 6. A take can be recorded end-to-end that can never be inserted — Accessibility is checked only after speaking (P2)

- Evidence: `beginDictation` gates on server readiness, microphone permission, and device resolution only (`InlayController.swift:852`) — never Accessibility. The hotkey can legitimately work without Accessibility via Input Monitoring (`Sources/Inlay/System/Permissions.swift:14`, `canListenForHotkey`), so this state is reachable: record fully, then destination capture/delivery returns blocked — "Allow Accessibility access to check the destination safely. Nothing was pasted or copied." (`TextInserter.swift:394`, `:445`). Meanwhile `requestInputMonitoring()` exists (`InlayController.swift:1270`) but no view calls it (grep over `Views/`), and both permission surfaces offer only Microphone + Accessibility rows (`InlayWindowView.swift:159`, `PreferencesPage.swift:80`).
- User experience: the user speaks a full take, waits for processing, and only then learns insertion was impossible from the start — and because the outcome is `.failed` (not copied), even the clipboard doesn't save them. The transcript survives in the window/history, but the flow guaranteed the worst outcome at press time. Related: `requestAccessibility()` only fires the system prompt (`Permissions.swift:39`); if Inlay isn't listed yet, the user must discover the "Not listed in System Settings?" sheet unprompted, while `retryPermissions()` polls silently for 60 s (`InlayController.swift:1339`).
- Confidence: verified-in-code.
- Fix direction: at press time, if Accessibility is missing for a non-test take, either block with the permission HUD error before recording or explicitly enter "will copy to clipboard" mode if that is deemed safe (owner call — the blocked reasons deliberately refuse copying in some states). At minimum, warn before the microphone opens.
- Effort: S.

### 7. Settings are split across three mental buckets, and two failure modes are silent (P2)

- Evidence: pages are Dictation / History / Microphone / "Server preferences" / "This Mac" (`InlayWindowView.swift:4`); shared-vs-local scope is invisible until save conflicts happen. Two silent paths: (a) a bad `config.json` sets `ConfigurationStore.errorMessage` and reverts to last-good (`Sources/Inlay/ConfigurationStore.swift:146`), but no view ever displays that error (grep: `configuration.errorMessage` is read only to decide window-opening at `InlayApp.swift:67`) — hand editors' changes just "don't take"; (b) when the server is unreachable, Server preferences renders draft defaults (`ServerPreferences()`, `PreferencesPage.swift:124`; `loadLatest` no-ops without a snapshot at `:320`) greyed out via `.disabled(!available)` (`:210`), so the page confidently shows e.g. English/Qwen-on values that may not be the server's at all. Connection errors also collapse every `URLError` (wrong port, TLS failure, timeout) into "Server offline · Recording unavailable" (`InlayController.swift:693`), and the 401/token message only surfaces on refreshes that also fetch data (`InlayController.swift:296`; health itself is auth-exempt, `Server/src/http-server.ts:80`).
- User experience: "I changed the prompt/key and nothing happened", "my language setting looks wrong when the server is down", "it says offline but the server is up" (wrong port/token/TLS all look identical). The 409 remote-change handling is good (`PreferencesPage.swift:165`), which makes the silent cases feel more broken by contrast.
- Confidence: verified-in-code.
- Fix direction: surface `configuration.errorMessage` on This Mac (with Reveal File — `revealFile()` already exists unused, `ConfigurationStore.swift:115`); show an explicit "unavailable — showing nothing" placeholder instead of default draft values; split URLError copy for unreachable vs refused/TLS and say "token rejected" for 401.
- Effort: S–M.

### 8. History is the recovery safety net, but it cannot be searched and its empty states mislead (P2)

- Evidence: History offers device + source pickers only (`Sources/Inlay/Views/HistoryPage.swift:28`) — no text search (grep: no `searchable`). Retrieval of older items is a manual "Load older" pagination loop (`HistoryPage.swift:74`); nothing auto-expires (inventory `00-inventory.md:86`), so the list grows unbounded across all connected Macs. The empty overlay always says "No dictations — Record while connected to add a dictation." (`HistoryPage.swift:137`) even when the list is empty due to an active filter or a failed load, and load failures land in the one shared `errorMessage` (`InlayController.swift:351`) which also renders on unrelated pages. Detail recovery actions are icon-only copy/delete (`HistoryPage.swift:151`); delete is server-wide and permanent with no undo (confirm dialog at `:93` is good, but final).
- User experience: after any loss from findings 3/4, the recovery story is "scroll and eyeball timestamps across every device's dictations". A week in, that's tens of load-older round-trips. A filtered-empty view tells the user to go record something, implying their data is gone. Given the product explicitly tells users to "check shared history" on failure, history _is_ the error-recovery UX — it needs to be findable.
- Confidence: verified-in-code.
- Fix direction: client-side search over loaded pages (server search is better — owner call on API change), filter-aware empty copy ("No Wispr Flow imports" / "Couldn't load — retry"), and a labelled Copy button. Undo-delete or trash retention is an owner decision.
- Effort: M (client search) / L (server search).

### 9. Two discoverability traps: the menu's main button is mislabeled, and the HUD spinner is a cancel button (P2)

- Evidence: the menu's primary button passes `idleTitle: idleDictationHint` ("Press Right Option to dictate") into `InlayMicrophoneTestButton` (`MenuHUDViews.swift:36`), whose action is `toggleTestRecording` (`InlayWindowView.swift:270`) — clicking the "press the key" button actually starts a _test_ recording that by design pastes nothing (`InlayController.swift:1102`). Separately, clicking the HUD circle while busy cancels the dictation with no confirmation (`MenuHUDViews.swift:244`), including an already-sealed pending take, which sends a server cancel (`InlayController.swift:713`).
- User experience: a new user clicks the big menu button, speaks, and is confused nothing was inserted (it was a "test" — recoverable in the window, but trust is dented). A returning user who clicks the spinner to "see progress" destroys the take they were checking on (compounds finding 5).
- Confidence: verified-in-code.
- Fix direction: label the button "Test microphone" everywhere or make it a genuine insert-able dictation trigger (owner decision); make HUD click-while-busy show stage/open window, keeping the explicit ✕ (already present while capturing, `MenuHUDViews.swift:202`) and Escape as the cancel paths.
- Effort: S.

### 10. Very short takes vanish without explanation (P2)

- Evidence: releases under 0.25 s call `cancelDictation()` (`InlayController.swift:948`), which resets to idle and hides the HUD (`:721`); the server enforces the same floor (`Server/src/generation-service.ts:655`). "No speech detected" exists for the sealed-empty path (`InlayController.swift:1107`) but not for this client-side discard.
- User experience: a quick "yes" / "thanks" tap produces literally nothing — no error, no hint to hold longer. New users testing the hotkey with short words will conclude the app is broken on first contact, right after fighting through finding 2's setup.
- Confidence: verified-in-code.
- Fix direction: show a one-line HUD note ("Too short — hold and speak, then release") instead of silent cancel. Whether to lower the floor is an engine question, not UX — flag only.
- Effort: S.

## Questions for owner

- First-run ambition (finding 2): is Inlay permanently "bring your own server via shell" for a technical audience of one, or should the app eventually guide server install and model download? Everything above the S-effort routing/copy fix depends on this.
- Offline durability (finding 3): is a local outbox/retry for unsealed takes acceptable given the privacy posture (audio currently exists only transiently on the client), or is loss-on-interruption an accepted trade-off? If accepted, the "check history" copy still needs to stop promising what was discarded.
- Clipboard-as-degradation (findings 4, 6): should "copied" be a success state or an attention state? And may a take recorded without Accessibility fall back to clipboard, or is the current refuse-to-copy in unverifiable states a hard safety line (in which case blocking at press time is the right fix)?
- Menu button intent (finding 9): is the primary menu action meant to be a mic test or a mouse-driven dictation? The label and the behavior currently disagree; either answer is cheap, but one must win.
- History retention (finding 8): any appetite for search, retention/auto-expiry controls, or delete-undo? Unbounded shared audio retention is also a privacy/disk posture only you can set.

## What I could not verify

- No runtime behavior was observed: per `AGENTS.md:4`/`AGENTS.md:6` no tests were run and no Inlay Dev dictation, insertion, sleep/lock, or permission flow was exercised. Timing claims (1.7 s / 4 s dwells, 60 s permission polling) are code constants, not measured UX.
- Electron/Chrome insertion-preparation latency and its user feel (up-to-~3 s verifiability window, `README.md:85`) — code paths located in inventory but the AX timing behavior was not audited here; that belongs to the insertion-safety audit.
- Server-generated error strings for sealed-take failures (`GenerationRecord.error`) were spot-checked at the client surface only; full server message quality belongs to the server/API audits.
- VoiceOver behavior beyond the accessibility labels present in code (`MenuHUDViews.swift:163`, `MicrophonePage.swift:337`) was not exercised; label _presence_ was verified, usable flow was not.
- Wispr Flow import UX was read only at the sheet surface (`HistoryPage.swift:319`); the multi-hour import experience (progress, partial failures) is routed to the dedicated import audit in `00-inventory.md:160`.
