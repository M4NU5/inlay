# Batch 2 plan — Make outcomes visible (branch `stack/03-visible-outcomes`)

- Batch: REPORT.md Batch 2. Covers P0-1, P1-11, P1-12, P1-13, P1-20, P1-14 (S slice only), plus P2s: pre-record Accessibility check (01 F6), silent settings failures (01 F7), menu test-button label (01 F9), silent short takes (01 F10), mute-silences-VoiceOver label (03 F6, label only).
- Branch stack: implement on `stack/03-visible-outcomes`, stacked on the previous batch branch (base `stack/00-audit-plan` on `audit/major-issues`). All `file:line` citations below are at `audit/major-issues` @ `b6d6815` and were verified by opening each body. Batch 1 (stop losing speech) edits `InlayController.swift` heavily, so after restacking, re-anchor every change by symbol name, not line number.
- Client-only batch: no server, API, or generated-bindings changes. Swift 5 mode (`Package.swift:38`), `@MainActor` controller, errors-as-values where the codebase already does (`DictationClipboard.copy` returns `Result`, `Sources/Inlay/System/TextInserter.swift:632`). No `as any` (n/a in Swift; no force casts added).
- Visual constraints that bind every slice: no all-caps headings, no new explanatory subheadings, no layout shift. HUD and form slots keep their current frames; new text goes into already-reserved slots (`InlayActionMessage` fixed 32pt, `Sources/Inlay/Views/InlayTheme.swift:466`) or the HUD's existing expanded width (`DictationHUD.width`, `Sources/Inlay/Views/MenuHUDViews.swift:97`).

## Goal

- Every dictation outcome — refused at key-press, recording, queued/processing, inserted, copied-not-inserted, unconfirmed, failed, too short — is perceivable at the moment it happens by sighted and VoiceOver users, without stealing focus and without the one visible affordance being destructive.
- "Copied instead of inserted" stops looking like success: it persists in the HUD with a copy-again button until the user dismisses it, and the menu bar keeps an attention trace while it is unresolved.
- Light-mode failure text is readable (≥4.5:1), and ready/not-ready status is not color-only.
- First launch lands on the connection form with a token-location hint; settings failures (bad `config.json`, unreachable server preferences, distinct connection errors) are surfaced instead of silent or collapsed.

## Decisions applied

- `DECISIONS.md:16` — copied = attention state with copy-again affordance, not a success dismissal. Applied in slice 4 (supersedes the 1.7s dismissal at `InlayController.swift:1038`).
- `DECISIONS.md:30` — missing Accessibility blocks at key-press with a permission HUD instead of recording an unusable take. Applied in slice 1; the clipboard-only-under-Input-Monitoring alternative stays in Batch 6.
- `DECISIONS.md:8` — audience is a shareable product, so the P1-14 S slice (route + token hint) is in this batch; guided server install/model download is Batch 7 (`DECISIONS.md:10`) and out of scope here.
- `DECISIONS.md:31` — mic-test stays a separate, clearly labeled action. Applied in slice 8 (label only); the menu-button real-dictation trigger (`DECISIONS.md:15`) is Batch 6.
- `DECISIONS.md:41` — Batch 2 contents confirmed: HUD-only refusals, VoiceOver announcements + sounds, copied=attention, stage/queue HUD text, contrast fix.
- `DECISIONS.md:18` — Swift reference server is deleted in Batch 8; this batch does not touch `InlayServerKit` and does not mirror any behavior there.
- `AGENTS.md:4` — no automated behavioral tests anywhere in this batch; DECISIONS' parity-check allowance belongs to Batch 8 only. Verification is build/static checks plus Inlay Dev manual steps.
- Retained from REPORT (not overridden by DECISIONS): HUD-only on hotkey refusals (P1-11 fix), stage+queue HUD text with click-shows-progress (P1-13), passing light warning + redundant cues (P1-20), surface `ConfigurationStore.errorMessage` (01 F7).

## IN scope commit slices

Ordered so each commit builds and is independently reviewable. Sizes: S ≤ ~1 file-hour, M = multi-file state change.

### Slice 1 (S) — HUD-only hotkey refusals + pre-record Accessibility block

- Why: P1-11 (focus theft destroys the insertion caret) and 01 F6 / `DECISIONS.md:30` (a take recorded without Accessibility can never insert; `TextInserter.swift:394` only learns this after speaking).
- Files and symbols:
  - `Sources/Inlay/InlayController.swift:849` — `InlayController.beginDictation(isTest:)`:
    - Remove `onShowWindow?()` from all three refusal guards (`:852` server not ready, `:853` microphone permission, `:854`–`:856` no microphone). Keep `showError(...)` and `refreshServer()` exactly as-is. `onShowWindow` (`:156`, wired at `InlayApp.swift:59`) remains for launch (`InlayApp.swift:67`), menu Open, and HUD attention clicks (`MenuHUDViews.swift:247`).
    - At the top of `beginDictation` (after the `!isCapturing` guard, before `stopShortcutCheck()`), add `permissions = PermissionSnapshot.capture()` so the checks use a fresh AX/mic grant. Deliberately not `refreshPermissions()` (`:1241`), which restarts the hotkey monitor (`:1246`) from inside its own `onPress` callback (`:824`–`:832`). `PermissionSnapshot.capture()` is `Sources/Inlay/System/Permissions.swift:16`.
    - Insert a new guard after the microphone guard, before device resolution: `guard isTest || permissions.accessibility else { showError("Allow Accessibility access so Inlay can insert text, then try again."); return false }`. Test takes are exempt because `deliver` never inserts for them (`InlayController.swift:1100`). `return false` preserves the double-tap no-latch contract documented at `:846`–`:848`.
  - No view changes: `showError` already presents the HUD (`InlayController.swift:1195`).
- Do not change: window presentation from Test-button clicks in the menu/window (user is already in Inlay), destination capture after a successful start (`TextInserter.swift:393`, `:445` still protect against mid-take revocation).

### Slice 2 (S) — Light-mode contrast tokens + differentiate-without-color status cue

- Why: P1-20 (03 F4, arithmetic re-confirmed in `11-crosscheck.md:35`): `warning` is `systemOrange` ≈2.1:1 in light mode and it is the error text color; status dots are color-only.
- Files and symbols:
  - `Sources/Inlay/Views/InlayTheme.swift:21` — `InlayPalette.warning`: replace `Color(nsColor: .systemOrange)` with `adaptive(light: 0xA65A00, dark: 0xFF9F0A, contrastLight: 0x8A4A00, contrastDark: 0xFFB340)` using the existing `adaptive` helper (`:23`–`:39`). Starting candidates only: before committing, compute WCAG ratios for the final light value against `canvas` `0xF8F6F2` (`:9`) and `surface` `0xFFFDF9` (`:11`) and require ≥4.5:1; darken until it passes and record the final ratios in the commit message. Sidebar `0xEDE8E4` (`:12`) only carries the dot (non-text), so it is covered by the symbol cue below, not the 4.5:1 text rule.
  - `Sources/Inlay/Views/InlayTheme.swift:7` — `InlayPalette.muted`: darken the light value from `0x766D78` (4.08:1 on sidebar) starting at `0x6B6270`, same verification loop against `sidebar` and `canvas`, require ≥4.5:1. Dark/high-contrast values unchanged (`contrastLight: 0x514955` already exists).
  - `Sources/Inlay/Views/InlayTheme.swift:225` — `StatusDot`: change the API from `StatusDot(color:)` to `StatusDot(isReady: Bool)`; color is derived inside (`success`/`warning`). Add `@Environment(\.accessibilityDifferentiateWithoutColor)`: when on, render `Image(systemName: isReady ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")` at `.caption` size with the same foreground token instead of the 6pt `Circle`; when off, render today's circle. Keep `.accessibilityHidden(true)` — every call site has adjacent status text. Because a symbol renders in both states, ready↔not-ready changes do not resize the row (only toggling the system setting does, which is a user-initiated relayout).
  - Update the three call sites: `Sources/Inlay/Views/InlayWindowView.swift:110` (`ServerConnectionStatus`), `Sources/Inlay/Views/MenuHUDViews.swift:24` (`InlayMenuView`), `Sources/Inlay/Views/PreferencesPage.swift:315` (`runtimeRow`).
  - `Sources/Inlay/Views/MicrophonePage.swift:262` and `:524` — replace raw `Color.orange` (the only palette bypasses for error text, 02 F6) with `InlayPalette.warning` in the two `.foregroundStyle(error == nil ? .secondary : ...)` expressions (priority-list footer `footer` `:251` and profile-name sheet body `:512`).
- Automatic improvement, no edits needed: every existing `InlayPalette.warning` text site — `InlayTheme.swift:443`, `:463`, `InlayWindowView.swift:186`, `PreferencesPage.swift:75`, `HistoryPage.swift:116`, `:178`, `:201`, `:205`, `:344`, `:394`, `:403`, `:439`, `:442`, `:445`.

### Slice 3 (M) — Processing HUD: stage + queue text; HUD click no longer cancels

- Why: P1-13 (01 F5 + F9). Today non-capturing states collapse to a bare spinner (`MenuHUDViews.swift:219`), the stage string from `applyProgress` renders only in the window (`InlayWindowView.swift:220`), `pendingDictations` reaches no view, and `activateCircle` server-cancels the take being checked (`MenuHUDViews.swift:244`).
- Files and symbols:
  - `Sources/Inlay/InlayController.swift`:
    - Add `@Published private(set) var hudProgressText: String?` next to `statusMessage` (`:121`), and a computed `var pendingDictationCount: Int { pendingDictations.count }` (`pendingDictations` is already `@Published` at `:172`, so count changes re-render observing views).
    - `applyProgress(_:session:)` (`:1081`): set the short HUD string next to each existing `statusMessage`: `.receiving` → `"Uploading"`, `.queued` → `"Queued"`, `.transcribing` → `"Transcribing"`, `.proofreading` → `"Proofreading"`, `.completed` → `"Preparing"`, `.failed`/`.cancelled` → `nil`. Long window strings unchanged.
    - `finishDictation` (`:954`–`:955`): set `hudProgressText = "Uploading"` where `statusMessage = "Finishing upload…"` is set.
    - `deliver(...)` (`:1117`): where the HUD-owned session sets `statusMessage = "Inserting at your cursor…"`, also set `hudProgressText = "Inserting"`.
    - Clear `hudProgressText = nil` in the delivery receipt branch (`:1036`), in `showError` (`:1195`), and in `cancelDictation`'s settle tail (`:721`–`:727`), so a settled HUD never shows a stale stage.
  - `Sources/Inlay/Views/MenuHUDViews.swift` — `DictationHUD`:
    - Expansion rule: `.onChange(of: controller.activity)` (`:115`–`:118`) currently collapses whenever capture ends. Change the predicate so the capsule stays expanded while `activity.isBusy` (AppState `isBusy`, `Sources/Inlay/AppState.swift:7`) and collapses only on `.success`/`.failed`/`.idle`; `enter()` (`:125`, `:135`, `:144`) uses the same predicate instead of `isCapturing`.
    - Add a `processingContent` row used when expanded but not capturing, mirroring `expandedContent` (`:188`): `ProgressView().controlSize(.small)` + `Text(stageText)` (`.system(size: 11, weight: .medium)`, `InlayPalette.muted`, `lineLimit(1)`, `minimumScaleFactor(0.8)` as in `RecordingLimitNote` `:262`–`:263`) + the existing ✕ cancel button pattern (`:202`–`:211`, identifier `hud.cancel`). `stageText` = `controller.hudProgressText ?? hudLabel`, suffixed when `controller.pendingDictationCount > 1` with `" · \(count - 1) more"`.
    - `activateCircle()` (`:244`–`:250`): delete the `if controller.isBusy { controller.cancelDictation() }` branch. Busy clicks are a no-op (processing is expanded and already shows progress; the only compact-busy moment left is the 0.18s morph). Non-busy behavior unchanged in this slice.
    - Circle accessibility (`:163`–`:165`): label becomes `hudLabel` plus stage text when busy; remove the `"Cancel dictation"`/`"Cancel this dictation"` busy label and hint. The ✕ buttons keep the cancel label/hint.
    - `compactIcon` spinner case (`:219`–`:220`) stays as the transient during morphs.
  - `Sources/Inlay/InlayApp.swift` — `DictationPanel.updateCompactState()` (`:289`–`:307`): compact-trim the native window only when `!activity.isBusy` (today: whenever not capturing). The expanded width is the pre-existing `DictationHUD.width` via `DictationPanelLayout.windowFrame` (`:216`–`:220`), so no new panel geometry and no layout shift.
- Explicitly unchanged: cancel stays available via ✕ while capturing and processing, and via Escape per current behavior (`MenuHUDViews.swift:177`, `InlayController.canCancelWithEscape` `:150`). Escape scoping is Batch 6.
- Queue position means client pending count only; the server exposes no position, and adding one is Batch 4 (see API section).

### Slice 4 (M) — Copied = persistent attention state + copy-again + menu-bar trace

- Why: P1-12 + `DECISIONS.md:16`, and the persistent-failure half of P0-1. Today `.copied` is not `needsAttention` (`MenuHUDViews.swift:93`), so it dismisses in 1.7s like success (`InlayController.swift:1038`); the reason is tooltip-only (`MenuHUDViews.swift:181`, `:184`); after failure the menu-bar glyph reverts to idle (`InlayBrand.swift:30`).
- Files and symbols:
  - `Sources/Inlay/Views/MenuHUDViews.swift:93` — `DictationDeliveryStatus.needsAttention`: add `.copied` → `self == .copied || self == .failed || self == .unconfirmed`. Existing ripples are desired: warning-colored icon (`:224`), attention hint (`:164`).
  - `Sources/Inlay/InlayController.swift`:
    - Add `@Published private(set) var attentionPresented = false` near `lastDeliveryStatus` (`:88`). This is the single source of truth for "unresolved delivery attention is on screen"; do not derive it from `activity`/`lastDeliveryStatus` (a dismissed `.copied` must stop counting while `lastDeliveryStatus` keeps its history value).
    - Delivery receipt branch (`:1035`–`:1042`), HUD-owned session only: if `delivered.needsAttention`, set `attentionPresented = true` and do not call `dismissHUDAfter`; otherwise keep `dismissHUDAfter(seconds: 1.7)` for plain success. The earlier-take branch (`:1039`–`:1041`) is unchanged in this slice (a newer take owns the HUD; promotion of older attention is out of scope below).
    - Clear `attentionPresented = false` on every settle path: `dismissFeedback()` (`:782`), `beginDictation` next to `hudTask?.cancel(); errorMessage = nil` (`:857`), `clearLastTranscript()` (`:774`), and `cancelDictation`'s settle tail (`:721`–`:727`).
    - `dismissFeedback()` (`:782`–`:788`): in addition to hiding the HUD, settle `activity` `.failed`/`.success` → `.idle` and refresh `statusMessage` exactly as `dismissHUDAfter` does at `:1211`, so dismissal also clears the menu-bar trace. (Today `.failed` is never settled by dismissal.)
    - `dismissHUDAfter` (`:1202`–`:1213`): extend the settle at `:1211` to `.failed` as well when `!attentionPresented`, so transient errors (slice 1 refusals, slice 9) do not leave a permanent failed activity behind their 4s HUD. `showError` (`:1195`) itself is unchanged and stays a 4s transient — persistence is only for delivery outcomes, which have a transcript to recover.
  - `Sources/Inlay/Views/MenuHUDViews.swift` — `DictationHUD` attention presentation:
    - When `controller.attentionPresented` is true, keep the capsule expanded (extend slice 3's expansion predicate) and show an `attentionContent` row: `Image(systemName: result.hudSymbol)` in `InlayPalette.warning` + visible `Text(result.hudLabel)` (same 11pt treatment as processing; this replaces tooltip-only comprehension) + a copy-again button (`doc.on.doc`, identifier `hud.copy-again`, label `"Copy again"`, shown only when `!controller.lastTranscript.isEmpty`) + a dismiss button (`xmark`, identifier `hud.dismiss`, label `"Dismiss"` → `controller.dismissFeedback()`).
    - Copy-again action in the view: `controller.copyLastTranscript()` then, if `controller.lastDeliveryStatus == .copied`, `controller.dismissFeedback()`. An explicit click is consent to overwrite the clipboard, which the guarded delivery path lacks: `copyLastTranscript` (`InlayController.swift:766`) already calls `DictationClipboard.copy` without `onlyIfUnchangedSince` (`TextInserter.swift:632`–`:646`), i.e. the unguarded intentional copy; reuse it, do not add a second copy path. Its `!isBusy` guard (`:767`) is satisfied because attention persists only when not busy.
    - Capsule-body click while attention is presented keeps today's deliberate behavior (`activateCircle` `:247`–`:248` → `onShowWindow?()` + dismiss): user-initiated clicks may take focus; hotkey presses may not (slice 1).
  - `Sources/Inlay/InlayApp.swift` — `DictationPanel` (`:228`): add a third subscription on `controller.$attentionPresented` next to `:263` and `:267`; while true, cancel `compactTask`, set `compact = false`, and `updateFrame()`, so the native window never trims under the attention row.
  - `Sources/Inlay/Views/InlayBrand.swift` — `InlayBrand`: add `attentionStatusImage = makeStatusSymbol("exclamationmark.circle", description: "… — dictation needs attention")` next to `:23`–`:24`. Change `statusImage(for:)` (`:26`) to `statusImage(for activity:, showsAttention: Bool)` returning the attention image when `showsAttention` and the activity is settled (not capturing/processing); thread the parameter through `updateStatusButton` (`:36`).
  - `Sources/Inlay/InlayApp.swift` — `configureStatusItem()` (`:155`–`:181`): pass `controller.attentionPresented` at the initial call (`:160`) and add `controller.$attentionPresented` to the `combineLatest` subscription (`:175`–`:180`), including it in `removeDuplicates`.
- Boundary: processing failures with no delivery (server error, upload failure) keep the 4s `showError` + window `errorMessage`; they have no local transcript to re-copy, so a persistent copy-again row would be a dead affordance. They are still announced in slice 5.

### Slice 5 (M) — VoiceOver announcements + outcome sounds (with preference)

- Why: P0-1 (03 F1). The HUD panel can never become key (`InlayApp.swift:273`), and a full search of `Sources/Inlay/` finds zero `NSAccessibility.post`, `NSSound`, or `NSBeep` — start/inserted/copied/failed are visual-only events.
- Files and symbols:
  - New file `Sources/Inlay/System/OutcomeFeedback.swift`:
    - `@MainActor enum OutcomeFeedback` with `enum Sound { case start, success, attention, failure }` and one entry point: `static func announce(_ message: String, sound: Sound?, soundsEnabled: Bool)`.
    - Announcement: `NSAccessibility.post(element: NSApp as Any, notification: .announcementRequested, userInfo: [.announcement: message])` (user-info key `NSAccessibility.NotificationUserInfoKey.announcement`). Always post: macOS speaks it only when VoiceOver is running, so no preference or VO check is needed. If Inlay Dev verification shows `NSApp` as element is not spoken while another app is frontmost, switch the element to the status-item button (the panel itself is unreachable by design) — verification step 4 decides; do not make the panel key-capable.
    - Sounds: `NSSound(named:)` against standard system sounds — `start: "Tink"`, `success: "Pop"`, `attention: "Purr"`, `failure: "Basso"` — held in private statics; before `play()`, `stop()` if already playing so rapid takes re-trigger cleanly. A missing name is a silent no-op. No new bundled audio resources (nothing to add under `Resources/`).
  - `Sources/InlayCore/InlayConfiguration.swift` — add `dictationSoundsEnabled: Bool` following the `muteOutputWhileRecording` pattern exactly: stored property (`:10`), init parameter default `true` (`:15`–`:16`), `CodingKeys` case (`:26`), decoder `try values.value(Bool.self, for: .dictationSoundsEnabled, default: true)` (`:43`–`:49`). `schemaVersion` stays `1`: a missing key defaults per `KeyedDecodingContainer.value` (`:137`–`:140`), and older builds ignore the unknown key on decode.
  - `Sources/Inlay/InlayController.swift`:
    - Add `@Published var dictationSoundsEnabled = true` with the same `didSet` persistence as `muteOutputWhileRecording` (`:116`–`:120`), and apply it in `applyConfiguration` (`:270` pattern). Private helper `announceOutcome(_ message: String, _ sound: OutcomeFeedback.Sound?)` forwarding `soundsEnabled: dictationSoundsEnabled`.
    - Hook points (announce existing user-facing strings, invent no new copy except the start cue):
      - Recording actually started, at `activity = .recording` (`:922`): announce `"Listening"` (already the `statusMessage` at `:923`), sound `.start`.
      - Delivery receipt branch, HUD-owned session (`:1036`–`:1038`): announce `receipt.message ?? delivered.hudLabel`; sound map — `.inserted`, `.tested`, `.listUpdated` → `.success`; `.copied`, `.unconfirmed` → `.attention`; `.failed`, `.none` (no speech, `deliver` `:1107`) → `.failure`.
      - Earlier-take receipt (`:1039`–`:1041`): announce the same `"Earlier dictation: …"` string that is written to `errorMessage`, sound `.attention` (only fires when that take needs attention, as today).
      - `showError` (`:1195`): announce `message`, sound `.failure`. This single hook covers slice 1 refusals (`:852`–`:855`), `failSession` (`:761`), HUD-owned processing failure (`:1050`), interruption (`:1237`), and slice 9. No double announcements: the receipt branch never calls `showError`.
      - Earlier-take processing error while busy (`:1051`–`:1059`): in the `else` path where `showError` is skipped, announce `message` directly with `.failure`; when `showError` does run, rely on its hook (announce exactly once).
  - `Sources/Inlay/Views/PreferencesPage.swift` — `DevicePreferencesForm`, "This Mac" section (`:71`–`:77`): add `Toggle("Play dictation sounds", isOn: $controller.dictationSoundsEnabled)` (identifier `preferences.dictation-sounds`) directly after the mute toggle. Under the mute toggle add one caption (`Text(...).font(.caption).foregroundStyle(InlayPalette.muted)`, the established pattern at `:61`): `"Muting also silences VoiceOver and dictation sounds while recording."` (03 F6 label fix).
- Mute interplay, stated so nobody "fixes" it blindly: `outputMuter.mute()` runs at `:870`, before the start cue at `:922`, so with mute-output enabled the start cue and any VoiceOver speech during capture are inaudible — that is what the new caption warns about. All outcome hooks run after `outputMuter.restore()` (`finishDictation` `:944`; `resetSession` `:747` precedes `showError` in `failSession` `:760`–`:761`), so result announcements/sounds are always audible. Default `muteOutputWhileRecording` is `false` (`InlayConfiguration.swift:16`).
- No spoken stage announcements (Queued/Transcribing chatter); start + outcome only.

### Slice 6 (S) — First-run routing to the connection form + token hint (P1-14 S slice)

- Why: 01 F2. First launch shows the window (`InlayApp.swift:67`) but lands on Dictation (`InlayWindowView.swift:25`), endpoint is a silent default (`ClientPreferences.swift:30`), and the token field has no location hint (`PreferencesPage.swift:27`).
- Files and symbols:
  - `Sources/Inlay/Views/InlayWindowView.swift`: drop `private` from `InlayPage` (`:4`) and give `InlayWindowView` (`:23`–`:25`) an `initialPage: InlayPage = .dictation` used to initialize `@State page`. Nothing else in the view changes.
  - `Sources/Inlay/InlayApp.swift` — `InlayAppDelegate`: add `private var initialPage: InlayPage = .dictation`. In `finishLaunching` (`:56`–`:71`), read `let isFirstLaunch = !defaults.bool(forKey: "hasLaunched")` before `:70` sets it, and set `initialPage = isFirstLaunch ? .device : .dictation` before any `showWindow()` call. In `showWindow()` (`:120`), construct `InlayWindowView(controller: controller, initialPage: initialPage)`. The window/hosting controller is created once, so later opens keep the user's current page.
  - `Sources/Inlay/Views/PreferencesPage.swift` — `DevicePreferencesForm` Connection section (`:23`–`:41`): directly under `SecureField("Access token", ...)` (`:27`) add one caption line (`.font(.caption).foregroundStyle(InlayPalette.muted)`), chosen by `InlayBuild.current.isDevelopment` (`Sources/InlayCore/InlayBuild.swift:12`):
    - Release: `"Use the token from your server's token file (its --token-file path, or /etc/inlay/token on Linux)."` (paths per `Server/README.md:24`, `:146`).
    - Dev: `"Dev server token: .local/dev-server/token (created by bun run dev)."` (per `docs/development.md:16`).
  - No change to `ClientPreferences.swift:30` (the `http://127.0.0.1:8391` packaged default is correct for the shareable product) and no deletion of the dead `ModelStatus`/`EngineStatus` (`AppState.swift:14`–`:20`) — Batch 7's guided install owns those.
- Returning users with an unreachable server are not re-routed (their page is preserved); slice 7 makes the failure legible wherever they land.

### Slice 7 (S–M) — Silent settings failures surfaced

- Why: 01 F7. Three independent silences: a bad `config.json` sets an error no view reads (`ConfigurationStore.swift:146`–`:147`; `revealFile()` `:115` has no caller), Server preferences renders default draft values when offline (`PreferencesPage.swift:124`, `:320`), and every `URLError` collapses to "Server offline" (`InlayController.swift:692`–`:695`).
- Files and symbols:
  - `Sources/Inlay/Views/PreferencesPage.swift` — config error row: `DevicePreferencesPage` (`:5`–`:11`) passes `configuration: controller.configuration` into `DevicePreferencesForm`, which gains `@ObservedObject var configuration: ConfigurationStore` next to `preferences` (`:15`). In the "This Mac" section after the `loginItemError` pattern (`:74`–`:76`), add `if let error = configuration.errorMessage { HStack { Text(error).font(.caption).foregroundStyle(InlayPalette.warning); Button("Reveal config file") { configuration.revealFile() } } }`. Conditional appearance mirrors `loginItemError`; launch already opens the window on this error (`InlayApp.swift:67`).
  - `Sources/Inlay/Views/PreferencesPage.swift` — unreachable server preferences: in the existing message row (`:165`–`:173`), extend the `InlayActionMessage` precedence to `!available ? "Cannot load shared preferences while the server is unavailable." : (changedRemotely ? ... : ...)`. The form stays mounted and `.disabled(!available)` (`:210`, `:218`, `:224`), so nothing moves when health arrives; the user is just no longer shown believable-looking defaults without a label. `available` is `:133`.
  - `Sources/Inlay/InlayController.swift:692` — `connectionMessage(_:)`: replace the single `URLError` branch with a switch on `(error as? URLError)?.code`:
    - `.cannotFindHost`, `.dnsLookupFailed` → `"Server not found at this address · Recording unavailable"`
    - `.timedOut` → `"Server connection timed out · Recording unavailable"`
    - `.secureConnectionFailed`, `.serverCertificateHasBadDate`, `.serverCertificateUntrusted`, `.serverCertificateHasUnknownRoot`, `.serverCertificateNotYetValid`, `.clientCertificateRejected` → `"Secure connection to the server failed · Recording unavailable"`
    - any other `URLError` (including `.cannotConnectToHost`, `.notConnectedToInternet`, `.networkConnectionLost`) → today's `"Server offline · Recording unavailable"`
    - Non-`URLError` → `error.localizedDescription` as today. This already covers token rejection: `ServerClient.validate` throws `.rejected(401, ...)` with the credential message (`Sources/Inlay/ServerClient.swift:100`), which `checkServer` surfaces on the preferences/history fetch (`InlayController.swift:296`–`:318`; health itself is auth-exempt). All existing callers (`:318`, `:909`, `:930`, `:1049`, `:1052`) improve with no call-site edits.

### Slice 8 (S) — Menu primary button is honestly a mic test

- Why: 01 F9 + `DECISIONS.md:31`. The menu's primary button reads "Press Right Option to dictate" but runs `toggleTestRecording`, which never inserts (`MenuHUDViews.swift:36`–`:37`, `InlayWindowView.swift:270`, `InlayController.swift:1102`).
- Files and symbols: `Sources/Inlay/Views/MenuHUDViews.swift` — remove the `idleTitle: idleDictationHint` argument (`:36`–`:37`) so `InlayMicrophoneTestButton` uses its default `"Test microphone"` (`InlayWindowView.swift:266`), and delete the now-unused `idleDictationHint` (`:10`–`:14`). No behavior change; the real menu-trigger dictation is Batch 6.

### Slice 9 (S) — Too-short take says so instead of vanishing

- Why: 01 F10. Releases under 0.25s call `cancelDictation()` silently (`InlayController.swift:948`).
- Files and symbols: `Sources/Inlay/InlayController.swift` — `finishDictation`, the duration guard (`:948`): `guard releasedAt - recordingStart >= 0.25 else { cancelDictation(); showError("Too short — hold the key, speak, then release."); return }`. Order matters: `cancelDictation` (`:703`–`:727`) resets the session, sends the server cancel, and hides the HUD; `showError` (`:1195`) then re-presents the note for 4s (and announces it after slice 5). If an earlier take is still pending, `dismissHUDAfter` returns the HUD to it (`:1207`–`:1208`). The release-during-`.starting` cancel at `:945` stays silent (microphone never opened; no speech to explain), and the server's own 0.25s floor is untouched.

## API/data changes + migration

- Server API (OpenAPI/`Server/`): none. No route, wire type, or generation-record field changes; `DeliveryReceipt` statuses already include `copied`/`unconfirmed` (`DictationDeliveryStatus`, `Sources/Inlay/AppState.swift:10`–`:12`, sent at `InlayController.swift:1150`). The HUD queue text uses the client-side `pendingDictations` count only; a server queue position would need an API addition and is deferred to Batch 4. Do not regenerate Swift/TypeScript bindings; `bun run generate:api --check` is run below only as a no-drift gate.
- Client data, additive only: `config.json` (`InlayConfiguration`) gains one optional key, `dictationSoundsEnabled` (Bool, default `true` when absent). `schemaVersion` stays `1` — the decoder's missing-key default (`InlayConfiguration.swift:137`–`:140`) makes existing files valid untouched, and an older build reading a newer file ignores the unknown key, so rollback is safe with no migration step. No `client.json`/Keychain changes (`ClientPreferences.swift` untouched); `UserDefaults` `hasLaunched` is read, not changed (`InlayApp.swift:67`, `:70`).
- Persistence: nothing new is persisted for attention/announcements. `attentionPresented` and `hudProgressText` are in-memory controller state; a copied result that is unresolved at quit remains recoverable via History/Copy, and relaunch reconciliation of undelivered results is Batch 1's badge+copy path (coordinate, see Risks).
- Swift reference server (`InlayServerKit`): untouched per `DECISIONS.md:18` (deleted in Batch 8). `InlayCore` is consumed only by the `Inlay` executable here (`Package.swift:22`–`:24`); `InlayServerKit` does not depend on it (`Package.swift:12`), so the configuration change cannot break the server targets — `swift build` below proves it.

## OUT of scope/deferred

- Guided server install + model download (Batch 7, `DECISIONS.md:10`) — slice 6 is routing + one hint line only; `ModelStatus`/`EngineStatus` stay as they are.
- Latch tap-to-start/stop and menu-button real dictation trigger (`DECISIONS.md:15`), arbitrary/left-side keys, Input Monitoring clipboard-only mode and its permission row (03 F8), Escape-cancels-only-while-capturing (`DECISIONS.md:31`, Batch 6), keyboard shortcuts/focus, flat page scaffold, typography/text-scaling, HUD Dev/Release width unification (02 F9 — new HUD strings must fit the 160pt Release width as-is). All Batch 6.
- Server queue position / admission cap (Batch 4); client shows stage + client pending count only.
- Relaunch badge+copy for completed-undelivered results and persisted pending IDs (Batch 1). If Batch 1 lands first, its badge surface and slice 4's `attentionPresented` must present one shared "ready to copy" state, not two.
- Earlier-take attention promotion: when an older take resolves to copied/failed while a newer take owns the HUD (`InlayController.swift:1039`–`:1041`), it announces (slice 5) and sets `errorMessage`, but does not seize the HUD or set `attentionPresented`. Promoting it after the newer take settles is deferred.
- Persistent-mute crash self-heal and any change to the mute mechanism itself (10 F6 / 03 F6 mechanism half) — slice 5 adds the warning label only.
- History search/empty-state copy (01 F8), color-identity unification and `error`-vs-`warning` token split (02 F6 beyond the two `Color.orange` stragglers), serif wordmark, duplicate server-status rows (02 F7), persisted failure state across relaunch, spoken per-stage announcements, custom sound assets, and a general announcement-preference UI beyond the single sounds toggle.

## Risks

- Stack drift (highest): Batch 1 rewrites much of `InlayController.swift` (health-poll kill at `:319`, upload retry, pending-ID persistence) and Batch 3 touches health semantics. Every citation here must be re-anchored by symbol after restacking; specifically re-check the receipt branch (`:1035`–`:1042`), `dismissHUDAfter`/`showError`, and whether Batch 1 introduced its own undelivered-result state that `attentionPresented` should reuse instead of paralleling.
- Attention state leaks: if any settle path misses `attentionPresented = false`, the HUD/panel/menu-bar attention sticks forever. The clear list in slice 4 is exhaustive against today's code; re-audit against post-Batch-1 settle paths (new retry/requeue entries into a take must also clear it, like `beginDictation` does).
- Persistent HUD friction: an unresolved copied HUD now sits over other apps indefinitely until dismissed/copied/next take. That is the decided attention semantics (`DECISIONS.md:16`), but verify in Inlay Dev that starting the next take cleanly replaces it (slice 4 clears at `beginDictation`) and that `DictationPanel` never trims its frame mid-attention (compact race with `compactTask`, `InlayApp.swift:295`–`:306`).
- Announcement delivery is the P0 mechanism and cannot be verified statically: `NSAccessibility.post` with `NSApp` as element while Inlay is a background accessory app may or may not be spoken by VoiceOver in the target app. The fallback (post against the status-item button) is named in slice 5; making `DictationPanel.canBecomeKey` (`InlayApp.swift:273`) true is explicitly not an option — it would break insertion focus.
- Sounds surprise: `dictationSoundsEnabled` defaults to `true` for existing users on upgrade (chosen default below). Sounds are short system cues with an off toggle, but this is a behavior change on update.
- Release-width blind spot: Inlay Dev renders the HUD at 200pt (`MenuHUDViews.swift:97`) while Release uses 160pt, so Dev verification cannot prove the stage/attention rows fit Release. Keep stage strings to the short forms in slice 3 with `minimumScaleFactor(0.8)`; flag any truncation as a follow-up rather than widening the HUD here (02 F9 owns width unification).
- Accessibility block behavior change: slice 1 makes Accessibility an absolute precondition for non-test dictation. Users on the Input-Monitoring-only path (`Permissions.swift:14`) previously recorded and failed at delivery; now they are refused at key-press. That is strictly less speech wasted and matches `DECISIONS.md:30`, but the refusal copy must not promise a clipboard mode that Batch 6 hasn't built yet.
- Contrast candidates are starting points, not verified values: slice 2 must not merge until the computed ratios (recorded in the commit message) pass ≥4.5:1 on `canvas`/`surface` (warning) and `sidebar`/`canvas` (muted). Dark-mode values must be eyeballed for no regression, since the audit's failures are light-mode only (dark ≈6.9:1 today).

## Verification

No automated behavioral tests (prohibited by `AGENTS.md:4`); no parity checks in this batch (Batch 8 only, `DECISIONS.md:21`). Static/build gates plus Inlay Dev computer-use, per `AGENTS.md:5`–`:9` and `docs/development.md:36`–`:42`. This batch edits no TypeScript, so the `bun run fmt` on-TS-edit rule (`AGENTS.md:3`) does not trigger; still run `fmt:check`.

Commands (repo root, in order; run after the final slice, and the Swift build after every slice):

```sh
bun install --frozen-lockfile
bun run check
bun run lint
bun run fmt:check
bun run generate:api --check
swift build --force-resolved-versions
./scripts/build-dev-app.sh
```

Dev instance for manual steps — isolated client data, real helpers/models (never fake inference, `AGENTS.md:7`). One-time model/helper setup per `docs/development.md:18` (`./scripts/build-server.sh`, `INLAY_SPEECH_MODEL`, `INLAY_TEXT_MODEL`), then:

```sh
bun run dev   # 0.0.0.0:8392, token at .local/dev-server/token
mkdir -p .local/dev-client-batch2 && chmod 700 .local/dev-client-batch2
open --env "INLAY_CLIENT_DATA_DIR=$PWD/.local/dev-client-batch2" "build/Inlay Dev.app"
```

In Inlay Dev, set This Mac → `http://localhost:8392` + the dev token, grant Microphone and Accessibility to Inlay Dev, and confirm health is ready before judging any dictation outcome. Do not stop or reuse the owner's packaged server (8391) or settings; if models/helpers are unavailable, report the dictation steps as unverified rather than substituting anything fake.

Manual steps for the owner (each names the expected observable):

1. Focus-safe refusal: stop the dev server; in TextEdit place a caret and hold the dictation key. Expect: HUD error only, TextEdit stays frontmost with caret intact; repeat press repeats HUD only (slice 1).
2. Accessibility block: revoke Inlay Dev's Accessibility grant in System Settings; restart dev server; hold the key in TextEdit. Expect: refusal HUD before any recording (no waveform, no take); Test microphone still records (slice 1 exempt path). Re-grant after.
3. Stage + queue + click safety: dictate two long takes back-to-back. Expect: HUD stays expanded through processing showing stage text (Queued/Transcribing/Proofreading) and a "· N more" suffix while the second waits; clicking the HUD body during processing changes nothing; the ✕ cancels only the shown take (slice 3).
4. VoiceOver + sounds: enable VoiceOver (⌘F5), dictate into TextEdit. Expect spoken "Listening" at start and the delivery message on completion, with distinct start/success cues; a refused press (server stopped) is spoken too. If nothing is spoken while TextEdit is frontmost, apply slice 5's element fallback before merge. Turn "Play dictation sounds" off: announcements remain, cues stop (slice 5).
5. Copied persistence: dictate, then click a different app/field while processing so delivery falls back to clipboard. Expect: HUD stays up expanded with the copied label and Copy again; copy unrelated text elsewhere, press Copy again in the HUD, paste to confirm the transcript is back; menu-bar icon shows the attention glyph until dismissed/copied (slice 4).
6. Contrast + color independence: switch macOS to Light appearance; read Dictation/History error text (visibly darker orange) and sidebar secondary text. Enable Settings → Accessibility → Display → Differentiate Without Color: status dots become check/exclamation symbols in sidebar, menu, and Server models rows, with no row resizing when server readiness flips (slice 2).
7. First run: quit Inlay Dev, run `defaults delete dev.davis.inlay.dev hasLaunched`, launch with a fresh `INLAY_CLIENT_DATA_DIR`. Expect: window opens directly on This Mac with the Dev token hint under the token field; connect with `.local/dev-server/token` succeeds (slice 6).
8. Silent settings: (a) stop Inlay Dev, corrupt `config.json` inside the batch2 dev-client dir, relaunch — This Mac shows the error and Reveal config file selects it in Finder; (b) stop the dev server and open Server preferences — the "Cannot load shared preferences…" line shows instead of plausible defaults; (c) point the endpoint at a nonexistent host — status reads "Server not found at this address…", distinct from plain offline (slice 7).
9. Labels: open the menu — the primary button reads "Test microphone" (slice 8). Tap the dictation key for <0.25s — the "Too short" HUD note appears instead of silence (slice 9).
10. Regression sweep: one normal inserted dictation, one mic test (nothing pasted), one cancel while capturing (✕), and quit/relaunch with an unresolved copied HUD — after relaunch it is gone by design (Batch 1 owns relaunch reconciliation); confirm the transcript is still in History.

## Open questions

None blocking. Chosen defaults (change on owner request):

- Sounds default on for all users with a This Mac off toggle; VoiceOver announcements always posted (spoken only when VoiceOver runs). Rationale: `DECISIONS.md:41` scopes "announcements + sounds" into this batch and REPORT Q15 recommends on-with-VoiceOver plus a preference — the toggle is that preference.
- Test takes are exempt from the pre-record Accessibility block (they never insert).
- First-launch-only routing to This Mac; returning users keep their current page even when the server is unreachable.
- Warning/muted hex values are verified-then-committed per slice 2's ratio requirement, not owner-approved brand values; the hue family (orange warning, warm muted) is preserved.
- Earlier-take attention is announced but not promoted into the persistent HUD (see Out of scope).
