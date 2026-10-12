# Visual design / UI consistency — major issues (2026-10-10)

- Scope: all 8 SwiftUI view files (`Sources/Inlay/Views/*`) plus window/popover/HUD chrome in `Sources/Inlay/InlayApp.swift`, read in full at HEAD `b6d6815`. There is no web UI (no `.html`/`.css` under `Server/`); every user-visible surface is the menu-bar popover, the dictation HUD, and the 5-page window. Nothing was run — per `AGENTS.md:4` no tests, and no Inlay Dev session was performed; every claim is verified-in-code unless marked inferred.
- Credit where due (do not regress): no all-caps headings anywhere — grouped `Form` headers explicitly opt out (`Sources/Inlay/Views/PreferencesPage.swift:41`, `:77`, `:90`, `:180`, `:209`, `:218`, `:223` all `.textCase(nil)`); error slots reserve height so messages do not push controls (`Sources/Inlay/Views/InlayTheme.swift:456` comment, `:466` fixed 32pt; `Sources/Inlay/Views/MicrophonePage.swift:124` fixed 30pt); high-contrast/reduce-transparency/reduce-motion variants exist (`Sources/Inlay/Views/InlayTheme.swift:25`, `:50`, `:196`). The findings below are what still makes the app feel unfinished despite that care.

## Findings

### 1. Each page is a different app: four containers, four content widths, four paddings

- Severity: P1
- Evidence: Dictation is a flat `ScrollView` + `Divider`s, `.padding(30)`, `maxWidth: 780` (`Sources/Inlay/Views/InlayWindowView.swift:140`, `:179`, `:205`); Microphone is custom cards (`InlaySettingsGroup`, `Sources/Inlay/Views/InlayTheme.swift:153`) at `padding(.horizontal, 28)` / top 30, `maxWidth: 700` (`Sources/Inlay/Views/MicrophonePage.swift:42`, `:58`); This Mac and Server preferences are grouped `Form`s at `maxWidth: 760` / `800` (`Sources/Inlay/Views/PreferencesPage.swift:108`, `:111`, `:226`, `:230`); History is an inset `List` + flat detail at `.padding(26)` with no max width (`Sources/Inlay/Views/HistoryPage.swift:83`, `:134`). Corner radii also wander: cards 10 (`InlayTheme.swift:160`), sidebar selection 7 (`InlayWindowView.swift:48`), prompt editor 6 (`PreferencesPage.swift:204`), notice 9 (`InlayTheme.swift:447`), permission card 10 (`Sources/Inlay/Views/PermissionHelpView.swift:51`).
- What the user experiences: clicking sidebar items makes the content column jump sideways (700 vs 760 vs 780 vs 800 vs full-bleed) and the surface language changes (flat → cards → grouped form → split list). It reads as five screens built at five different times, which is the single biggest "unfinished" signal in the window.
- Confidence: verified-in-code. Corroborating drift: the theme file still carries unused components from abandoned directions — `SettingsIcon` gradient tiles (`InlayTheme.swift:172`), `MicrophoneTestLabel` (`:237`), the old `KeyCap` (`:264`, superseded by `InlayHoldKeyCap` `:295`), `InlineNotice` (`:429`), `InlayRule` (`:378`) — none referenced anywhere else in `Sources/Inlay/`.
- Fix direction: pick one page scaffold (one max width, one padding, one grouping treatment — grouped form _or_ flat-with-dividers, not both) and migrate all five pages onto it; delete the dead theme components so the next page cannot pick a sixth style.
- Effort: M
- Owner-decision question: grouped-form cards everywhere, or flat everywhere? (Owner's stated preference is cards sparingly, which points flat — but that means rebuilding Microphone's card groups.)

### 2. Fixed-height boxes inside scrolling pages: nested scroll traps that are cramped on tall windows and wasted on short ones

- Severity: P1
- Evidence: Dictation transcript is a `ScrollView` inside the page `ScrollView`, hard-capped at 205pt (`Sources/Inlay/Views/InlayWindowView.swift:140`, `:181`, `:238`); Microphone priority list is a `ScrollView` inside the page `ScrollView`, hard-capped at 270pt with 64pt rows, so ~4 devices visible (`Sources/Inlay/Views/MicrophonePage.swift:33`, `:180`, `:247`, `:369`); cleanup-prompt editor fixed at 352pt (`Sources/Inlay/Views/PreferencesPage.swift:203`); shortcut diagnostics fixed at 90pt (`PreferencesPage.swift:69`); Wispr sheet fixed 520×380 (`Sources/Inlay/Views/HistoryPage.swift:362`), profile sheet fixed width 350 (`MicrophonePage.swift:535`), permission sheet fixed width 580 (`PermissionHelpView.swift:75`).
- What the user experiences: wheel/trackpad scrolling gets captured by the inner box mid-page; a long transcript — the product's core output — is read through a 205pt letterbox even in a maximized window, while empty canvas sits below it; on the 580pt-minimum window (`InlayWindowView.swift:82`) the same boxes squeeze everything else. Fixed sheet widths also cannot grow for long device names, prompts, or localized strings.
- Confidence: verified-in-code (heights and nesting); the scroll-capture feel is inferred from the nesting, not observed.
- Fix direction: let Dictation and Microphone pages be single-scroll (transcript and priority list flow with the page, sized by window), or make those pages non-scrolling with the inner region taking `maxHeight: .infinity`. Sheets: min/ideal sizes instead of exact frames.
- Effort: M

### 3. Layout shifts the reserved-slot discipline missed: permission block, History selection, conditional detail footer

- Severity: P1
- Evidence: (a) Dictation's whole permission section appears/disappears, pushing the transcript block up/down when the last grant lands (`Sources/Inlay/Views/InlayWindowView.swift:159`); (b) changing History's source filter clears the selection, blanking the detail pane to "Select a dictation" on every filter change (`Sources/Inlay/Views/HistoryPage.swift:85`, `:269`); (c) History detail's bottom area changes shape per record — "Open audio"/"Open original"/"Open Wispr Flow audio"/"Source files" buttons exist only when those artifacts exist, and the `modelID · backend` line exists only when speech metadata exists — so clicking down the list makes the footer and divider jump (`HistoryPage.swift:226`, `:258`); (d) the device filter's items are derived only from _loaded_ records, so "Load older" can silently grow the picker (`HistoryPage.swift:13`); (e) Server preferences' "Server models" section pops in when health arrives, pushing the form down (`Sources/Inlay/Views/PreferencesPage.swift:176`).
- What the user experiences: content moves under the cursor while doing the two most common window tasks (granting permissions on first run, browsing history). First-run is exactly where (a) hits.
- Confidence: verified-in-code for the conditional structure; pixel-level jump magnitude inferred.
- Fix direction: reserve/collapse gracefully — keep the permission block mounted at reduced opacity or animate only that block; keep selection if it survives the filter (or auto-select first row); render disabled placeholders for missing audio buttons and always reserve the model line; reserve the models section height while health loads.
- Effort: M

### 4. History at minimum window size is over-constrained: header, split view, and fixed pickers fight for ~590pt of detail width

- Severity: P2
- Evidence: window minimum 820×580 (`Sources/Inlay/Views/InlayWindowView.swift:82`, AppKit floor 820×620 at `Sources/Inlay/InlayApp.swift:119`) minus sidebar min 210 (`InlayWindowView.swift:65`) leaves ~590pt; History then demands heading + 180pt device picker + 130pt source picker + refresh in one row (`Sources/Inlay/Views/HistoryPage.swift:26`, `:33`, `:41`), and an `HSplitView` of list min 220 + detail min 280 (`HistoryPage.swift:56`). There is no compact/narrow adaptation anywhere in the window.
- What the user experiences: at or near minimum size (plausible for a utility kept beside a document), the "History" heading truncates or the pickers compress, long device names truncate in the fixed 180pt picker, and the detail transcript column is a narrow strip while the list keeps 220pt.
- Confidence: verified-in-code (constraints arithmetic); actual truncation behavior inferred, not observed.
- Fix direction: stack the two pickers under the heading below a width threshold (or move filters into the list header), let picker widths be flexible with max widths, and consider allowing the history list to collapse.
- Effort: S–M

### 5. Typography: fixed point sizes ignore system text sizing, and there are four different "title" treatments

- Severity: P2
- Evidence: 24 uses of `.font(.system(size:…))` across `Sources/Inlay/Views/` (e.g. `InlayTheme.swift:146`, `:439`; `Sources/Inlay/Views/MenuHUDViews.swift:200`; `PermissionHelpView.swift:27`), interleaved with semantic styles elsewhere (`.body`, `.headline`, `.callout`) — so with a larger system text size, some text grows and some does not, and fixed-height slots (28–48pt rows throughout) clip or crowd. Titles: sidebar wordmark is 27pt **serif** (`InlayWindowView.swift:32`) — the only serif in the app — page headings 26pt semibold sans (`InlayWindowView.swift:255`), Dictation hero 28pt medium (`:145`), permission sheet 23pt semibold (`PermissionHelpView.swift:27`), Wispr sheet `.title2` (`HistoryPage.swift:326`), profile sheet `.headline` (`MicrophonePage.swift:515`).
- What the user experiences: the serif wordmark promises an editorial voice nothing else keeps; sheet titles all sit at different weights/sizes; users who raise text size get a patchwork where headings stay small while body text grows into fixed-height rows.
- Confidence: verified-in-code.
- Fix direction: define 2–3 named text roles in the theme (page title, sheet title, small label) on semantic styles with `@ScaledMetric` where a custom size is truly needed; decide whether serif is the brand (then use it for page titles too) or drop it.
- Effort: M
- Owner-decision question: is the serif wordmark intentional brand, or leftover?

### 6. Two brands and muddled semantics: vermilion light mode vs glacier-blue dark mode; orange means error; the primary button is red

- Severity: P2
- Evidence: light accent is vermilion `0xC5513E`, dark accent is pale ice-blue `0xA7D6F5`, and every surface hue similarly flips warm→cool (`Sources/Inlay/Views/InlayTheme.swift:5` comment, `:9`–`:19`); the app-icon tile stays warm in both appearances (`Sources/Inlay/Views/InlayBrand.swift:91`–`:108`). `warning` (system orange, `InlayTheme.swift:21`) is used for outright errors: action messages (`:463`), `InlineNotice` errors (`:443`), server-not-ready dots (`Sources/Inlay/Views/InlayWindowView.swift:110`), failed generations (`Sources/Inlay/Views/HistoryPage.swift:116`) — while primary actions take the vermilion accent via window tint (`InlayWindowView.swift:83`), so in light mode the main button is error-red-adjacent and actual errors are orange. Palette bypasses: `.secondary` (system grey, not the warm `muted`) in `InlayTheme.swift:408`, `MicrophonePage.swift:111`, `:122`, `:192`, and raw `Color.orange` at `MicrophonePage.swift:262`, `:524`.
- What the user experiences: switching to dark mode changes the product's identity color completely (and clashes with the warm Dock icon they just clicked); in light mode "Save"/"Test microphone" read destructive at a glance, while failures read as mild warnings; muted text is two different greys depending on which page you're on.
- Confidence: verified-in-code (values/resolution logic read; no contrast ratios measured — see unverified section).
- Fix direction: choose one accent hue family and vary only lightness per appearance; add a real `error` color distinct from warning; ban `.secondary`/`Color.orange` in views in favor of palette tokens.
- Effort: S–M
- Owner-decision question: is "warm light / glacier dark" a deliberate dual identity? If yes, the icon tile and accent still need to agree within each appearance.

### 7. The same server status is rendered twice on screen at once, on most pages

- Severity: P2
- Evidence: sidebar always shows `ServerConnectionStatus(compact: true)` (`Sources/Inlay/Views/InlayWindowView.swift:61`); the detail area then shows it again on Dictation (`:152`), History (`Sources/Inlay/Views/HistoryPage.swift:51`), Server preferences (`Sources/Inlay/Views/PreferencesPage.swift:148`), and inside the This Mac connection section (`PreferencesPage.swift:32`) — with different truncation (compact `lineLimit(2)` vs full `lineLimit(1)` + endpoint, `InlayWindowView.swift:114`, `:117`), so the two copies can even _read_ differently.
- What the user experiences: duplicate dot + message consumes the top of nearly every page (violates "less is more"), and when the message wraps in the sidebar but truncates in the page it looks like two different systems disagreeing.
- Confidence: verified-in-code.
- Fix direction: one canonical home (sidebar) — remove from page bodies; keep only contextual actions ("Check connection") where they earn their place. This Mac's connection section is the one defensible exception.
- Effort: S

### 8. Diagnostics and pipeline internals are presented as product UI in History and This Mac

- Severity: P2
- Evidence: History detail mixes the transcript with "Flow status: …" (`Sources/Inlay/Views/HistoryPage.swift:168`), "Stored text versions: …" (`:196`), formatting-rejection reasons (`:201`), "Rejected cleanup" (`:208`), vocabulary-hint accounting ("Used: / Did not fit:", `:308`), and a caption-2 `modelID · backend` line (`:259`); the Wispr import sheet leads with "Metadata only", "WAV found", "Opus packet sets", "Screenshots" count rows (`HistoryPage.swift:383`); This Mac embeds a monospaced raw event log ("Shortcut diagnostics", `Sources/Inlay/Views/PreferencesPage.swift:62`); language picker labels leak implementation ("Japanese (proofreading only)", `PreferencesPage.swift:140`) with a standing caption explaining Parakeet's language behavior (`:186`).
- What the user experiences: after their first successful dictations, opening History shows what looks like a debug console around their words. It is genuinely useful for the owner debugging cleanup quality — but as shipped UI it is the clearest "developer build" tell, and it competes visually with the transcript (three different caption/warning text styles stacked under it).
- Confidence: verified-in-code.
- Fix direction: keep the transcript clean; move provenance behind one "Details" disclosure (or a Dev-only surface), rename import counts to user words ("Sessions with audio", "Sessions with text"), and put shortcut diagnostics behind the existing Check flow without a permanent disclosure in the form.
- Effort: S–M
- Owner-decision question: is this provenance a permanent power-user feature or migration-era debugging aid? That decides hide-vs-polish.

### 9. HUD and menu popover: small targets/type, and Dev builds literally look different from Release

- Severity: P2
- Evidence: HUD text is 11pt and the cancel target 22×28 (`Sources/Inlay/Views/MenuHUDViews.swift:200`, `:204`); HUD width is 200 in Dev vs 160 in Release (`MenuHUDViews.swift:97`) and menu-bar item length 62 vs 30 (`Sources/Inlay/InlayApp.swift:158`), with the Dev badge overlaid outside the capsule at `offset(x: 10, y: -6)` (`MenuHUDViews.swift:172`) relying on the panel's 18pt padding not to clip (`InlayApp.swift:239`); the popover (310 wide, `MenuHUDViews.swift:5`) mixes a prominent primary button, borderless rows, and bare `Button`s in one 12pt-spaced stack with a single divider before Quit only (`MenuHUDViews.swift:36`–`:50`), and its status text is squeezed into a fixed 112pt trailing slot (`:30`).
- What the user experiences: the surface seen on _every_ dictation (the HUD) has the smallest type and target in the app, mid-speech, at screen bottom; the popover reads as a settings form fragment rather than a menu; and since Dev is wider, the owner daily-drives a HUD layout Release users never see (and vice versa), so visual bugs in one width hide from the other.
- Confidence: verified-in-code for sizes/widths; badge clipping inferred (padding arithmetic suggests it survives, unobserved).
- Fix direction: one HUD width for both builds (badge inside the capsule only, as already done in expanded state at `MenuHUDViews.swift:190`); enlarge cancel hit area to ≥28×44-height of capsule; give popover status its own full-width row.
- Effort: S

## Questions for owner

- Page scaffold: flat-with-dividers everywhere or grouped cards everywhere? (Finding 1 — everything else inherits this choice.)
- Color identity: is warm-light/glacier-dark deliberate? If it stays, should the app icon get a dark-appearance tile so Dock and window agree? (Finding 6)
- Serif wordmark: brand or leftover? (Finding 5)
- History provenance/diagnostics: permanent power-user feature to polish, or debugging aid to hide behind a disclosure/Dev? (Finding 8)
- Is the fixed 205pt transcript letterbox on Dictation intentional (e.g. to keep the test button always visible), or should the page be single-scroll? (Finding 2)

## What I could not verify

- No runtime/visual verification: per `AGENTS.md:4` and `AGENTS.md:6` this audit did not launch Inlay Dev, so actual rendered truncation (Finding 4), scroll-capture feel (Finding 2), jump magnitudes (Finding 3), Dev-badge clipping (Finding 9), glass/material rendering on macOS 26 vs earlier (`Sources/Inlay/Views/InlayTheme.swift:200`), and light/dark contrast ratios of the palette values (Finding 6) are inferred from code, not observed or measured.
- The generated app icon (`scripts/make-icon.swift`, `Resources/Inlay.svg`) was not rendered or compared against `InlayAppIcon`'s in-app tile (`Sources/Inlay/Views/InlayBrand.swift:92`); only the code-level color disagreement in Finding 6 is claimed.
- Dynamic Type / larger-text behavior (Finding 5) is inferred from fixed point sizes and fixed row heights; no accessibility-size session was run.
- `Sources/Inlay/InlayController.swift` state timing (e.g. how long the permission block or models section takes to pop in) was out of scope; findings claim only that the view structure makes the shift possible.
