# Windows client instructions

`Windows/` is a standalone Rust crate for the Windows tray client. The root instructions about Bun, Swift and **Inlay Dev** do not apply here; these do. The repository rules still hold: no automated tests, and no changes to the server, the Swift client or the API for the sake of this client.

## Rules

- No automated tests of any kind: unit, integration, end-to-end, snapshot, smoke or regression suites, fixtures, mocks, runners, or scripted assertion harnesses. Verify by hand.
- The client must work with the unchanged server. Follow [`docs/client-server-contract.md`](../docs/client-server-contract.md) and the Mac flow in `Sources/Inlay/InlayController.swift` and `ServerClient.swift`.
- Insert only `insertionText`. Never send `continuationID`. Delivery receipts use the Swift client's statuses (`unconfirmed` after a sent paste, `none`, `failed`). The server rejects any other status.
- Never log transcripts, tokens or audio.

## Build checks

From the `Windows` folder:

```sh
cargo fmt --check
cargo clippy --release -- -D warnings
cargo xwin build --release   # from WSL/Linux; on Windows use `cargo build --release`
```

`.cargo/config.toml` targets `x86_64-pc-windows-msvc`, so `cargo check` and `cargo clippy` run from WSL without linking. Run `cargo fmt` after your final edits.

## Verify on Windows

1. Run a real server. Use the `inlay-server-linux-x64-cpu` package from the upstream "TypeScript server" Actions run that matches your commit, or build one per [`Server/README.md`](../Server/README.md). Put models under the repository's ignored `.local/`. Run it in WSL on `127.0.0.1:8391` with `--token-file` so token handling is exercised. To save CPU, turn proofreading off through `PUT /v1/preferences`. Never substitute fake inference.
2. Copy `inlay.exe` to a Windows path and start it. To keep test settings apart from a real install, set `INLAY_CLIENT_DATA_DIR`. When launching from WSL, pass it through `WSLENV`.
3. Check by hand, and record how:
   - The tray icon and tooltip for offline, ready, recording, processing and error.
   - The window: each page, and scrolling when it is smaller than its page. On This PC, **Connect** with a wrong token, with a LAN `http://` address (refused), and with the right token. Confirm `config.json`, the Credential Manager entry and the Run key.
   - **Test microphone**: `GET /v1/generations` should show a `test` record with delivery `tested`, and nothing pasted.
   - History: filters, selecting a row, scrolling the list, and deleting one of your own test records.
   - A hold-to-talk take. The log should show admission, the microphone, uploaded frames and chunks, and the delivery status. `GET /v1/generations` should show the record with `device`, audio artifacts and `delivery`.
   - Paste into Notepad (Ctrl+V) and Windows Terminal (Ctrl+Shift+V), then confirm the clipboard comes back. `inlay.exe --paste TEXT` exercises the same path with known text.
   - **Copy last message**, and **Quit**.
4. Report anything you could not verify, such as real speech or an elevated window, as unverified.

## Driving a shared desktop

The machine running these checks usually belongs to someone who is using it.

- Before injecting any key or click, check the foreground window and the idle time (`GetLastInputInfo`). Never inject input while the person is active, for example in a game.
- Drive Inlay's own windows with window messages (`WM_SETTEXT`, `WM_COMMAND`) and capture them with `PrintWindow`. Coordinate clicks and `SendKeys` land in whatever window happens to be on top.
- Open your own Notepad file and terminal window for paste tests. Never paste into the person's existing windows.
- Snapshot the clipboard before changing it, and put it back afterwards.

## Code map

| File                                        | Responsibility                                                                                          |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| `main.rs`                                   | Startup, single instance, threads, the `--paste` check.                                                 |
| `controller.rs`                             | Hold state machine: acceptance delay, chord cancel, missed key-up watchdog.                             |
| `take.rs`                                   | One take: admission, capture, acknowledged uploads, finish, events, ordered paste.                      |
| `api.rs`                                    | HTTP client for the v1 API, including NDJSON events and recovery.                                       |
| `audio.rs`                                  | WASAPI capture through cpal, downmix and windowed-sinc resampling to 16 kHz.                            |
| `hotkey.rs`                                 | `WH_KEYBOARD_LL` hook and the hold key list.                                                            |
| `paste.rs`                                  | Clipboard snapshot and restore, modifier release, Ctrl+V or Ctrl+Shift+V.                               |
| `app.rs`                                    | Shared state, server health monitor, paste ordering.                                                    |
| `tray.rs`                                   | Tray icon, tooltip, menu, notifications.                                                                |
| `settings/`                                 | The main window: sidebar, scrolling, controls (`mod.rs`), page layout (`layout.rs`), one file per page. |
| `config.rs`, `credentials.rs`, `startup.rs` | Config file, Credential Manager token, Run key.                                                         |
| `icons.rs`                                  | Tray icons drawn in code.                                                                               |
