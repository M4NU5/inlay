# Accessibility and macOS platform audit (2026-10-10)

- Scope: `Sources/Inlay/` (app, views, system layer), `Resources/Info.plist`, `Resources/Info-Dev.plist`, `Resources/Inlay.entitlements`, `Package.swift`, `scripts/build-app.sh`, read in the `major-issues-audit` worktree at HEAD `b6d6815`. Vendor, build, and generated directories were excluded. Every file cited below was opened and read; search was used only to locate candidates and to verify absences (noted where an absence claim rests on a full-tree search).
- Method limits: no automated tests were run or added and no fake inference was used, per `AGENTS.md:4` and `AGENTS.md:7`. Nothing was exercised in Inlay Dev, VoiceOver, or Full Keyboard Access in this pass, so runtime behavior claims are code-path reads unless marked otherwise.
- Credit where due: the baseline is better than most menu-bar utilities. Icon-only controls generally carry labels (for example `Sources/Inlay/Views/HistoryPage.swift:157`, `Sources/Inlay/Views/MicrophonePage.swift:224`), priority rows expose VoiceOver actions (`Sources/Inlay/Views/MicrophonePage.swift:337`), Reduce Motion is honored in the HUD, key cap, and waveform (`Sources/Inlay/Views/MenuHUDViews.swift:103`, `Sources/Inlay/Views/InlayTheme.swift:298`, `Sources/Inlay/Views/InlayTheme.swift:327`), Reduce Transparency and increased contrast get solid surfaces (`Sources/Inlay/Views/InlayTheme.swift:187`), and the palette defines high-contrast variants (`Sources/Inlay/Views/InlayTheme.swift:23`). The findings below are the remaining major gaps, roughly in severity order.

## Findings

### 1. VoiceOver users get no dictation outcome: the HUD is unreachable and nothing is ever announced

- Severity: P0 for VoiceOver users (core loop feedback missing), P1 overall
- Confidence: verified-in-code
- Evidence:
  - The dictation HUD lives in a non-activating panel that can never become key: `Sources/Inlay/InlayApp.swift:246` (`.nonactivatingPanel`), `Sources/Inlay/InlayApp.swift:251` (`.ignoresCycle`), `Sources/Inlay/InlayApp.swift:273` (`canBecomeKey` is `false`). This is deliberate so insertion focus stays in the target app, but it also means VoiceOver and keyboard focus can never reach the HUD content during normal use in another app.
  - The HUD does carry good labels and values (`Sources/Inlay/Views/MenuHUDViews.swift:182`, `Sources/Inlay/Views/MenuHUDViews.swift:163`, `Sources/Inlay/Views/MenuHUDViews.swift:209`), but they sit in that unfocusable window.
  - A full search of `Sources/Inlay/` finds zero `NSAccessibility.post(element:notification:)` calls and zero announcement APIs. State changes only mutate `@Published` strings, for example `Sources/Inlay/InlayController.swift:1195` (`showError`) and `Sources/Inlay/InlayController.swift:1147` (`lastDelivery`).
  - Results auto-dismiss quickly: success after 1.7 seconds, needs-attention after 4 seconds (`Sources/Inlay/InlayController.swift:1038`), errors after 4 seconds (`Sources/Inlay/InlayController.swift:1199`).
  - After a failure the menu bar icon returns to the idle logo, so there is no persistent visual trace either: `Sources/Inlay/Views/InlayBrand.swift:30` maps `.failed` to `restingStatusImage`, even though its tooltip/accessibility description says "dictation needs attention" at `Sources/Inlay/Views/InlayBrand.swift:50`.
  - There is also no sound feedback at all: no `NSSound`, `NSBeep`, or audio-player usage anywhere in `Sources/Inlay/` (verified by search), so start, stop, inserted, copied, and failed are visual-only events.
- What the user experiences: a blind user holds the key in another app, speaks, releases — and hears nothing. They cannot tell whether recording started, whether text was inserted at the cursor, silently copied to the clipboard, or failed outright. For a product whose entire value is hands-free text entry, this closes the core loop to the users who may need it most.
- Fix direction: post `NSAccessibility.post` announcements (`.announcementRequested`) for recording started, inserted, copied-to-clipboard, no-speech, and failed-with-reason, driven from the same delivery receipt path at `Sources/Inlay/InlayController.swift:1035`; add optional, distinct start/stop/result sounds via `NSSound` (respecting the system "mute while recording" setting, see finding 6); keep a failure visible in the menu bar until dismissed instead of reverting to the idle glyph at `Sources/Inlay/Views/InlayBrand.swift:30`.
- Effort: M
- Owner-decision question: should announcements be on by default whenever VoiceOver is running, or an explicit "Spoken confirmations" preference (also useful eyes-free without VoiceOver)?

### 2. The only real dictation triggers are a sustained hold or a 450 ms double tap of three right-side modifiers

- Severity: P1
- Confidence: verified-in-code
- Evidence:
  - The dictation key can only be Right Option, Right Control, or Fn/Globe: `Sources/Inlay/System/HotkeyMonitor.swift:4`. The hold must survive a fixed 180 ms acceptance delay (`Sources/Inlay/System/HotkeyMonitor.swift:165`) and be sustained for the whole take, up to the 180-second cap.
  - The alternative is a double tap inside one fixed 0.45-second window (`Sources/Inlay/System/HotkeyMonitor.swift:227`, modes defined at `Sources/Inlay/System/HotkeyMonitor.swift:83`).
  - Touching anything else mid-hold cancels it: another modifier (`Sources/Inlay/System/HotkeyMonitor.swift:341`), another key (`Sources/Inlay/System/HotkeyMonitor.swift:355`), or any mouse click (`Sources/Inlay/System/HotkeyMonitor.swift:360`). This is good accidental-activation protection, but it also punishes tremor and switch/pointer overlap.
  - The only button-driven recording path is the microphone test, which never inserts (`Sources/Inlay/InlayController.swift:697`, `Sources/Inlay/InlayController.swift:1102`). There is no menu item, window button, URL scheme, or AppleScript command that starts a real, inserting dictation, so Voice Control, Switch Control, and automation users have no alternative trigger.
  - Fn carries an extra platform caveat the app itself documents: "Some keyboards do not report Fn to apps" (`Sources/Inlay/System/HotkeyMonitor.swift:28`).
- What the user experiences: users with limited strength, one-handed (left-side) keyboard layouts, tremor, or switch devices must either hold a right-side modifier for the full utterance or land two taps inside 450 ms. Sticky Keys users get modifier-latching semantics the raw HID flag checks at `Sources/Inlay/System/HotkeyMonitor.swift:39` were not designed around (inferred — needs device verification).
- Fix direction: add a latch-style single-tap toggle mode (tap once to start, tap again or wait for silence to stop), allow a user-recorded arbitrary key or left-side modifiers, make the double-tap window adjustable (or read the system double-click interval), and expose "Start/Stop Dictation" as a menu-bar menu item so Voice Control and Switch Control can invoke it by name.
- Effort: L (trigger engine and settings), M for just the menu-item trigger
- Owner-decision question: is a hands-free latching mode acceptable given the accidental-recording and privacy risks the hold model was designed to avoid?

### 3. A failed hotkey press steals focus into the Inlay window and never gives it back

- Severity: P1
- Confidence: verified-in-code
- Evidence: when a take cannot start (server not ready, no microphone permission, no microphone device), the failure path calls `onShowWindow` (`Sources/Inlay/InlayController.swift:852`, `Sources/Inlay/InlayController.swift:853`, `Sources/Inlay/InlayController.swift:855`). Showing the window sets the activation policy to `.regular`, activates the app ignoring other apps, and makes the window key (`Sources/Inlay/InlayApp.swift:126`, `Sources/Inlay/InlayApp.swift:127`, `Sources/Inlay/InlayApp.swift:128`).
- What the user experiences: pressing the dictation key while composing in Mail (with the server briefly down) yanks the user out of Mail into Inlay. Keyboard and VoiceOver focus move with the activation and are not restored on dismissal; the caret context they were about to dictate into must be re-found by hand. This contradicts the menu-bar utility convention that the app stays out of the way and reports trouble in its HUD/menu.
- Fix direction: report start failures in the HUD and menu-bar popover only (the HUD already shows errors via `Sources/Inlay/InlayController.swift:1195`); open the window only when the user clicks through, as the needs-attention HUD path already does at `Sources/Inlay/Views/MenuHUDViews.swift:247`.
- Effort: S

### 4. Error and warning text fails contrast in light mode, and status is largely color-only

- Severity: P1
- Confidence: verified-in-code (ratios computed from the hex values in `Sources/Inlay/Views/InlayTheme.swift:4`; `systemOrange`/`systemGreen` use Apple's documented light-appearance values, so those two ratios are approximate)
- Evidence:
  - `InlayPalette.warning` is `systemOrange` (`Sources/Inlay/Views/InlayTheme.swift:21`) and is used for caption-size error text throughout, for example `Sources/Inlay/Views/InlayTheme.swift:463`, `Sources/Inlay/Views/PreferencesPage.swift:75`, `Sources/Inlay/Views/HistoryPage.swift:178`. Light-mode `systemOrange` on the app's near-white surfaces measures roughly 2.2:1 — well under the 4.5:1 expected for small text. (Dark mode passes at roughly 6.9:1.)
  - Secondary text in the sidebar (`muted` `0x766D78` on `sidebar` `0xEDE8E4`, `Sources/Inlay/Views/InlayTheme.swift:7`, `Sources/Inlay/Views/InlayTheme.swift:12`) measures 4.08:1, and `accentInk` on `tint` (Dev badge, pressed key) measures 4.29:1 at `Sources/Inlay/Views/InlayTheme.swift:17` — both under 4.5:1 for the small sizes used.
  - Server and model status is communicated by a 6-point green/orange dot (`Sources/Inlay/Views/InlayTheme.swift:225`, used at `Sources/Inlay/Views/InlayWindowView.swift:110` and `Sources/Inlay/Views/PreferencesPage.swift:315`). `accessibilityDifferentiateWithoutColor` is never read anywhere in `Sources/Inlay/` (verified by search), so users who cannot distinguish the dot colors — or who enabled Differentiate Without Color — get no shape or label alternative at dot-only sites.
- What the user experiences: in light mode, the exact messages that explain why dictation failed are the hardest text in the app to read, and ready/not-ready may be indistinguishable for color-blind users.
- Fix direction: give `warning` a custom light-appearance value that passes 4.5:1 at caption size (the palette's `adaptive` helper at `Sources/Inlay/Views/InlayTheme.swift:23` already supports this), darken `muted` on sidebar or use `ink` there, and pair each status dot with a redundant cue (check/exclamation glyph or text) when Differentiate Without Color is on.
- Effort: S

### 5. Fixed point sizes and fixed frames ignore larger text, display zoom, and small displays

- Severity: P2
- Confidence: verified-in-code
- Evidence:
  - 25 text sites use non-scaling `.system(size:)` fonts, including body-critical text: sidebar labels (`Sources/Inlay/Views/InlayWindowView.swift:42`), permission instructions — the whole help sheet (`Sources/Inlay/Views/PermissionHelpView.swift:27`, `Sources/Inlay/Views/PermissionHelpView.swift:31`, `Sources/Inlay/Views/PermissionHelpView.swift:54`), HUD text (`Sources/Inlay/Views/MenuHUDViews.swift:200`, `Sources/Inlay/Views/MenuHUDViews.swift:260`), and page headings (`Sources/Inlay/Views/InlayWindowView.swift:255`). These do not respond to the system text-size accessibility setting, unlike the semantic styles (`.body`, `.caption`) used elsewhere.
  - Hard frames compound it: main window minimum 820×620 (`Sources/Inlay/InlayApp.swift:119`, `Sources/Inlay/Views/InlayWindowView.swift:82`), import sheet fixed 520×380 (`Sources/Inlay/Views/HistoryPage.swift:362`), profile sheet fixed 350 wide (`Sources/Inlay/Views/MicrophonePage.swift:535`), help sheet fixed 580 wide (`Sources/Inlay/Views/PermissionHelpView.swift:75`), transcript area fixed 205 high (`Sources/Inlay/Views/InlayWindowView.swift:181`), HUD fixed 160/200×44 (`Sources/Inlay/Views/MenuHUDViews.swift:97`), menu popover fixed 310 wide (`Sources/Inlay/Views/MenuHUDViews.swift:5`) with its height frozen while open (`Sources/Inlay/InlayApp.swift:173` sets `sizingOptions = []`).
  - Single-line truncation is common on status text (`Sources/Inlay/Views/InlayWindowView.swift:114`, `Sources/Inlay/Views/MenuHUDViews.swift:28`), so scaled-up text clips instead of wrapping.
- What the user experiences: a low-vision user who raises the system text size or uses a scaled/zoomed display gets an app that mostly stays the same size, with some labels truncated and a minimum window that can exceed a zoomed 13-inch display's comfortable working area.
- Fix direction: replace `.system(size:)` with semantic styles (or `Font.system(size:relativeTo:)`) outside of pure glyphs/ornaments, replace fixed sheet/popover sizes with min/ideal sizing, and let status lines wrap instead of truncating.
- Effort: M

### 6. "Mute system audio while recording" mutes VoiceOver itself, with no warning

- Severity: P2
- Confidence: verified-in-code (that the default output device is muted); inferred (that VoiceOver speech, which normally follows the default output, is silenced with it — not device-verified)
- Evidence: the toggle is offered plainly with no explanatory text (`Sources/Inlay/Views/PreferencesPage.swift:71`). When on, recording start mutes the system default output device (`Sources/Inlay/InlayController.swift:870`, `Sources/Inlay/System/SystemOutputMuter.swift:23`), using "the same mute control as the menu bar and keyboard" (`Sources/Inlay/System/SystemOutputMuter.swift:47`). Restore happens on finish/cancel (`Sources/Inlay/InlayController.swift:944`), and failed restores are retried later (`Sources/Inlay/System/SystemOutputMuter.swift:33`), but a crash mid-take can leave output muted, as `docs/architecture.md` already concedes.
- What the user experiences: a VoiceOver or Spoken Content user who enables this option loses all speech output for the duration of every take — precisely while they most need the recording/processing feedback from finding 1 — and after a crash may be left with a silent Mac and no screen-reader output to debug it with.
- Fix direction: label the risk inline ("Also silences VoiceOver and other speech output while recording"), default it off when VoiceOver/Spoken Content output is detected if feasible, and prefer ducking non-speech audio over device mute if CoreAudio allows.
- Effort: S for labeling, M for smarter behavior
- Owner-decision question: is device mute still the right mechanism, or should this become "pause media playback" instead?

### 7. Keyboard operation is thin: no shortcuts for core actions, invisible focus on custom controls, drag-only reordering for sighted keyboard users

- Severity: P2
- Confidence: verified-in-code
- Evidence:
  - Only five `keyboardShortcut` modifiers exist in the client (`Sources/Inlay/Views/MenuHUDViews.swift:47`, `Sources/Inlay/Views/MenuHUDViews.swift:50`, `Sources/Inlay/Views/MicrophonePage.swift:528`, `Sources/Inlay/Views/MicrophonePage.swift:530`, `Sources/Inlay/Views/PermissionHelpView.swift:71`). Nothing covers page navigation (⌘1–5), copy last transcript, test microphone, history refresh/load-older, or saving shared preferences.
  - The sidebar is a list of `.plain`-style buttons whose selected state is only a tint plus font weight/color (`Sources/Inlay/Views/InlayWindowView.swift:40`, `Sources/Inlay/Views/InlayWindowView.swift:42`, `Sources/Inlay/Views/InlayWindowView.swift:51`). Plain buttons draw no clear focus ring, so Full Keyboard Access users tab through an invisible focus. The HUD buttons are likewise `.plain` (`Sources/Inlay/Views/MenuHUDViews.swift:159`).
  - Microphone priority reordering is a custom drag gesture on a 28×28 handle (`Sources/Inlay/Views/MicrophonePage.swift:365`, `Sources/Inlay/Views/MicrophonePage.swift:407`, handle at `Sources/Inlay/Views/MicrophonePage.swift:485`). VoiceOver users are well served by the custom actions at `Sources/Inlay/Views/MicrophonePage.swift:340`, but a sighted keyboard-only user has no focusable row or key command — the only fallback is a right-click context menu (`Sources/Inlay/Views/MicrophonePage.swift:325`).
  - The menu-bar UI is a custom `NSPopover` of SwiftUI buttons (`Sources/Inlay/InlayApp.swift:165`) rather than an `NSMenu`, so it loses native menu keyboard navigation, type-selection, and standard VoiceOver menu semantics. The main menu bar is similarly minimal: only App and Edit menus, no Window or Help menu, no About item, and an Edit menu with Undo but no Redo (`Sources/Inlay/InlayApp.swift:183`, items at `Sources/Inlay/InlayApp.swift:199`).
- What the user experiences: keyboard-first users (including switch and head-pointer users emulating keys) must pointer-hunt for everyday actions, cannot see where focus is in the sidebar, and cannot reorder microphones without a pointing device or VoiceOver.
- Fix direction: move the sidebar to a native `List(selection:)` (as History already does at `Sources/Inlay/Views/HistoryPage.swift:107`) or add visible focus styling; add ⌘-shortcuts for pages and primary actions; make priority rows focusable with ⌥↑/⌥↓ move commands; consider an `NSMenu`-based status menu (or `MenuBarExtra` with `.menu` style) for the menu-bar surface.
- Effort: M

### 8. Permission UX: the low-privilege Input Monitoring path exists in code but has no UI, and denied prompts look broken

- Severity: P2
- Confidence: verified-in-code
- Evidence:
  - The platform layer fully supports listening with Input Monitoring alone (`Sources/Inlay/System/Permissions.swift:14`, request at `Sources/Inlay/System/Permissions.swift:44`), the hotkey starts when either grant exists (`Sources/Inlay/InlayController.swift:1245`), and both plists declare `NSInputMonitoringUsageDescription` (`Resources/Info.plist:29`). The controller even exposes `requestInputMonitoring` (`Sources/Inlay/InlayController.swift:1270`) — but no view calls it, and no permission row exists for it (verified by search of `Sources/Inlay/Views/`). The help sheet instead tells users "You do not need a separate Input Monitoring grant" (`Sources/Inlay/Views/PermissionHelpView.swift:53`).
  - `allPermissionsGranted` is defined as microphone plus Accessibility (`Sources/Inlay/InlayController.swift:154`), so a privacy-conscious user who grants Input Monitoring only (hotkey works, delivery falls back to clipboard) is permanently shown as unpermitted, and first-launch logic keeps presenting the window (`Sources/Inlay/InlayApp.swift:67`).
  - The Accessibility "Allow" button calls `AXIsProcessTrustedWithOptions` with the prompt option (`Sources/Inlay/System/Permissions.swift:39`). macOS shows that prompt only until the user has interacted with it; on later clicks nothing visibly happens while the app silently polls for 60 seconds (`Sources/Inlay/InlayController.swift:1339`). The "Not listed in System Settings?" escape hatch exists (`Sources/Inlay/Views/PermissionHelpView.swift:9`) but its primary manipulation is drag-oriented (`Sources/Inlay/Views/PermissionHelpView.swift:41`), with the keyboard path (⌘⇧G) described only in running text.
  - Re-grant churn is structural: Release and Dev are separate TCC identities (`Resources/Info.plist:8`, `Resources/Info-Dev.plist:8`), and the build falls back to ad-hoc signing when no single identity exists (`scripts/build-app.sh:48`), which `README.md:81` notes can force re-granting after every rebuild.
- What the user experiences: users unwilling to grant full Accessibility control cannot choose the lesser permission the code already supports; users whose prompt was dismissed click Allow and get no feedback; developers and beta users re-grant permissions after ad-hoc rebuilds.
- Fix direction: add an Input Monitoring row (framed as "dictation key only, results copied instead of inserted"), treat microphone plus (Accessibility or Input Monitoring) as a fully-permitted state with degraded-delivery messaging, and make Allow always open the correct Settings pane after the one-shot system prompt has been consumed.
- Effort: M
- Owner-decision question: is clipboard-only delivery under Input Monitoring an acceptable supported mode, or is Accessibility intentionally mandatory? If mandatory, the unused request path and plist string should be removed so the product tells one story.

### 9. A global Escape tap cancels dictation from any app, and a global click cancels a hold

- Severity: P2
- Confidence: verified-in-code
- Evidence: the listen-only event tap observes key downs system-wide (`Sources/Inlay/System/HotkeyMonitor.swift:140`, tap created at `Sources/Inlay/System/HotkeyMonitor.swift:142`). Any Escape key-down in any application routes to cancel/dismiss (`Sources/Inlay/System/HotkeyMonitor.swift:322`, `Sources/Inlay/InlayController.swift:838`), except during an Fn hold where it is deliberately left to the app (`Sources/Inlay/System/HotkeyMonitor.swift:311`, `Sources/Inlay/InlayController.swift:150`). Any left/right mouse-down during a physical hold likewise blocks/cancels it (`Sources/Inlay/System/HotkeyMonitor.swift:360`).
- What the user experiences: while a take is transcribing, pressing Escape for an unrelated reason in the target app (closing a dialog, exiting a mode) also cancels the pending dictation. The event itself is not swallowed, so both things happen. Assistive-technology users who lean on Escape heavily, and pointer users who steady the pointer with a click while holding the key, lose takes for reasons unrelated to dictation intent.
- Fix direction: scope Escape-to-cancel to the capturing phase only (while the microphone is live, when the user is demonstrably "in" dictation), or require the dictation key to be involved (for example Escape only while the hold is physically down or latched); do the same scoping for mouse-down.
- Effort: S
- Owner-decision question: is Escape-during-processing an intentional feature users rely on? If so, consider keeping it but surfacing it in the HUD ("Esc to cancel") so the side effect is discoverable.

### 10. Insertion follows keyboard focus, not the VoiceOver cursor

- Severity: P2
- Confidence: inferred (code path verified; VoiceOver focus-divergence behavior not device-verified)
- Evidence: the destination is captured from the system-wide keyboard-focused element and application (`Sources/Inlay/System/TextInserter.swift:515`, field read at `Sources/Inlay/System/TextInserter.swift:520`), and eligibility requires an enabled `AXTextField`/`AXTextArea`/`AXComboBox` role (`Sources/Inlay/System/TextInserter.swift:109`). This is the correct safety model for sighted keyboard focus, but macOS VoiceOver maintains its own cursor that can sit on a different element than keyboard focus (for example after VO-cursor exploration without moving focus).
- What the user experiences: a VoiceOver user who navigates to a text field with the VO cursor but whose keyboard focus still rests elsewhere will dictate into the wrong field or get a clipboard fallback with no explanation of the mismatch — compounding finding 1, since the outcome is not announced either.
- Fix direction: when VoiceOver is running (detectable via `NSWorkspace.shared.isVoiceOverEnabled`), warn once in the permissions/dictation UI that dictation lands at keyboard focus and suggest enabling VO cursor tracking/focus syncing; include the destination app/field role in the announcement proposed in finding 1 ("Inserted in Mail").
- Effort: S

### 11. Platform and distribution posture: unsandboxed, ad-hoc-signable, and an accessory/regular identity switch

- Severity: P2
- Confidence: verified-in-code
- Evidence:
  - The client entitlements grant only audio input and do not enable the App Sandbox (`Resources/Inlay.entitlements:5`). The app nevertheless reads other apps' accessibility trees, installs a session event tap, manipulates the pasteboard, and reads Wispr Flow's local databases — all incompatible with the Mac App Store sandbox, so distribution is limited to direct download either way; but unsandboxed hardened-runtime signing still deserves an explicit rationale since it maximizes TCC blast radius.
  - The build signs with the hardened runtime (`scripts/build-app.sh:50`) but falls back to ad-hoc identity `-` when the machine does not have exactly one Apple Development identity (`scripts/build-app.sh:44`), and no notarization or stapling step exists in `scripts/build-app.sh`. Ad-hoc builds trigger Gatekeeper friction and the TCC re-grant churn in finding 8.
  - The app is an accessory (`Resources/Info.plist:23` `LSUIElement`, `Sources/Inlay/InlayApp.swift:20`) that becomes a regular app while its window is open (`Sources/Inlay/InlayApp.swift:126`) and reverts on close (`Sources/Inlay/InlayApp.swift:99`). Its Dock icon and ⌘Tab presence therefore appear and disappear, which disorients switcher/VoiceOver window cycling and violates the usual "menu bar app stays a menu bar app" expectation.
  - Dictating into an Electron app permanently (for that app's lifetime) enables its accessibility tree — requested once at capture time (`Sources/Inlay/System/InsertionPreparation.swift:55`, activation at `Sources/Inlay/System/InsertionPreparation.swift:70`) and disclosed in `README.md:85` as increasing that app's memory/CPU use. Assistive-technology users of that target app inherit the performance cost whether or not they use Inlay's output.
- What the user experiences: no App Store path, first-launch Gatekeeper warnings on ad-hoc builds, an app that flickers in and out of the Dock/app switcher, and a silent performance tax on any Electron app ever dictated into.
- Fix direction: require a real signing identity plus notarization for any build that leaves the developer's machine (fail the script instead of silently ad-hoc signing), stay `.accessory` permanently and present the settings window as a normal window of an accessory app (the common pattern for menu-bar utilities), and surface the Electron AX-mode side effect in-app the first time it is triggered rather than only in the README.
- Effort: M
- Owner-decision question: is Mac App Store distribution ever a goal? If not, record "direct distribution only, unsandboxed by design" as a decision; if yes, the event-tap/AX architecture needs a rethink before it hardens further.

## Questions for owner

- Should spoken confirmations and sounds be on by default when VoiceOver is running, or an explicit preference (finding 1)?
- Is a latching, hands-free trigger (tap to start/stop, or silence-based stop) acceptable given the accidental-recording risk the hold model avoids (finding 2)?
- Should "mute while recording" remain a whole-device mute, or become pause/duck of media only (finding 6)?
- Is clipboard-only delivery under Input Monitoring a supported mode, or is Accessibility mandatory by design (finding 8)?
- Is Escape-to-cancel during processing intentional and relied upon (finding 9)?
- Is Mac App Store distribution ever a goal, given the unsandboxed event-tap/AX design (finding 11)?
- The server proofreads 17 languages (`Sources/Inlay/Views/PreferencesPage.swift:136`) but the client UI is hardcoded English with no localization catalog anywhere in `Sources/` (verified by search). Is UI localization in scope, or is English-only an accepted decision? Not graded above, but it interacts with the fixed-width frames in finding 5 if it ever happens.

## What I could not verify

- No runtime behavior was observed: Inlay Dev was not launched, and VoiceOver, Full Keyboard Access, Sticky Keys, Switch Control, Display Zoom, Increase Contrast, and Differentiate Without Color were not exercised on a device, per the audit's static-read constraints and `AGENTS.md:6` (behavioral verification would be a follow-up computer-use pass against Inlay Dev with real helpers/models, per `AGENTS.md:8`).
- Whether Sticky Keys modifier latching confuses the raw HID flag checks at `Sources/Inlay/System/HotkeyMonitor.swift:39` (finding 2) is inferred, not observed.
- Whether the device mute in finding 6 actually silences VoiceOver on current macOS depends on the user's Speech output routing; the code-verified fact is that the default output device is muted.
- Contrast ratios for `systemOrange`/`systemGreen` use Apple's documented light-appearance color values rather than pixels sampled from a running build; custom-palette ratios are computed exactly from the hex constants in `Sources/Inlay/Views/InlayTheme.swift:4`.
- Whether the VoiceOver cursor and keyboard focus actually diverge in the apps users dictate into (finding 10) needs per-app VoiceOver testing; the insertion target being keyboard focus is code-verified.
- Secure Event Input interaction was not assessed beyond noting no `IsSecureEventInputEnabled` check exists in `Sources/Inlay/` (verified by search); whether the hotkey silently dies under secure input in Terminal/password fields should be confirmed on a device before treating it as a finding.
- The server's Linux/container accessibility and the Swift reference server were out of scope for this file; nothing here should be read as covering them.
