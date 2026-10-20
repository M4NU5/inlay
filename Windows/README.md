# Inlay for Windows

A hold-to-talk dictation client for Windows. It sits in the notification area, records while you hold a key, sends the audio to an [Inlay server](../Server/README.md), and pastes the transcript at your cursor. It uses the same HTTP API (version 2) as the Mac app and needs no server changes.

It is a small Rust program: raw Win32 for the tray, main window, keyboard hook and paste, WASAPI (through `cpal`) for the microphone, and blocking HTTP (through `ureq`) for the server.

## Use

1. Click into the text field you want to type into.
2. Hold **Right Ctrl**, speak, then release.
3. The transcript is pasted with Ctrl+V. Windows Terminal and console windows get Ctrl+Shift+V.

Details:

- A hold must last about 0.2 seconds before recording starts, so shortcuts that use the key never open the microphone.
- Pressing another key while holding cancels the take. Keys you were already holding, such as a movement key in a game, don't count.
- Takes shorter than 0.25 seconds are discarded. Takes stop at the server's 3-minute limit.
- You can start a new take while earlier ones are still transcribing. Pastes happen in release order, and never while the key is held.
- If the text doesn't appear, open the tray menu and choose **Copy last message**, then paste it yourself. The **Dictation** page shows the last result too, with **Copy**.
- **Test microphone**, on the Dictation and Microphone pages, records until you select **Finish dictation**. It shows the transcript without pasting it.

| Tray icon            | Meaning                                                   |
| -------------------- | --------------------------------------------------------- |
| Slate microphone     | Ready.                                                    |
| Red microphone       | Recording.                                                |
| Orange microphone    | Uploading or transcribing.                                |
| Grey, struck through | The server is unreachable or not ready, or a take failed. |

Hover over the icon for details. Take failures also show a Windows notification.

## Build

### From WSL or Linux

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --target x86_64-pc-windows-msvc
cargo install cargo-xwin --locked
cd Windows
cargo xwin build --release
```

The executable is `Windows/target/x86_64-pc-windows-msvc/release/inlay.exe`. On its first run, `cargo xwin` downloads the Microsoft CRT and Windows SDK libraries into `~/.cache/cargo-xwin`. That download means you accept Microsoft's license for those files. No C compiler is needed. `cargo check` and `cargo clippy` also work from WSL, because `.cargo/config.toml` selects the Windows target.

### On Windows

Install Rust with `rustup-init.exe` and the Visual Studio Build Tools (the "Desktop development with C++" workload). Then, in the `Windows` folder, run:

```powershell
cargo build --release
```

GitHub Actions builds the same executable on `windows-latest` and uploads it as the `inlay-windows-x64` artifact.

## Install

1. Copy `inlay.exe` to a permanent folder, such as `%LOCALAPPDATA%\Programs\Inlay\`.
2. Run it. On the first run it opens the **This PC** page, where you connect to the server.
3. Windows 11 hides new tray icons behind the **^** arrow. To keep Inlay visible, drag its icon onto the taskbar, or turn it on under **Settings › Personalization › Taskbar › Other system tray icons**.

No installer or administrator rights are needed.

## Configure

Choose **Open Inlay…** from the tray menu. The window follows the Mac app: a sidebar with **Dictation**, **History**, **Microphone** and **This PC** pages. Changes apply as you make them, except the connection, which applies when you select **Connect**.

**History** lists shared history from the server, newest first, filtered by device and source. Select a dictation to read its transcript and the original one, copy it, delete it from the server, or open its audio in your default player. **Load older** fetches the next 50.

| Page       | Setting                    | Notes                                                                                                                                                |
| ---------- | -------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| Microphone | Choose input               | "System default", or a specific input. If the chosen microphone is unplugged, the default is used. **Next dictation** shows which one will record.   |
| This PC    | Server address             | Defaults to `http://127.0.0.1:8391`. HTTPS works anywhere. Plain HTTP is only allowed for localhost and Tailscale IPs, matching the Mac app.         |
| This PC    | Access token               | Stored in Windows Credential Manager, scoped to this server address. It is never written to the config file.                                         |
| This PC    | Device name                | Shown in shared history. Defaults to the computer name.                                                                                              |
| This PC    | Dictation key              | Right Ctrl (default), Right Alt, Right Shift, Scroll Lock, Pause, or F13–F24. Right Alt is AltGr on UK and many European layouts, so avoid it there. |
| This PC    | Start Inlay when I sign in | Adds `Inlay` to `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.                                                                                |

**Connect** saves the address, token and device name, then checks the server. The status beside it, on the Dictation page and at the foot of the sidebar shows the result. A server on WSL is reachable from Windows at `http://127.0.0.1:8391`.

Shared settings (language, dictionary, proofreading and original-audio retention) belong to the server. Edit them from the Mac app or the API. Each take uses the server's settings snapshot from when it started.

Files:

- `%APPDATA%\Inlay\config.json` holds the server address, a stable device ID, the device name, the microphone and the hold key.
- `%APPDATA%\Inlay\inlay.log` records events and errors. It never records transcripts, tokens or audio.
- Set `INLAY_CLIENT_DATA_DIR` to use another folder, for example for a test copy. The token is scoped to the folder too.

## How a take works

The flow matches the Mac client and [`docs/client-server-contract.md`](../docs/client-server-contract.md):

1. On an accepted hold, the client checks `/v1/health` (API version 2 and readiness) and creates a generation with a new `requestID`, the device identity and `mode: "dictation"`. If admission fails, it doesn't open the microphone. Test microphone uses `mode: "test"`, pastes nothing, and reports `tested`.
2. It records from the chosen microphone in shared mode. It downmixes to mono and resamples to 16 kHz float32 with a windowed-sinc filter.
3. While recording, it uploads half-second chunks in sequence and waits for each acknowledgement. When the server snapshot has `keepOriginalAudio`, the same 16 kHz audio is also uploaded as the `original` stream.
4. On release, it finishes with the exact frame counts, then follows the NDJSON events until the generation completes.
5. It pastes only `insertionText`, then reports a delivery receipt. Because Windows can't confirm the text landed, a sent paste is reported as `unconfirmed`. No speech is reported as `none`, and a failed paste as `failed`.

## Limitations

- **Elevated windows.** Windows stops a normal app from sending keys to, or watching keys in, windows running as administrator. The hold key doesn't work while one is focused, and pastes into one are blocked. Use **Copy last message** and paste by hand, or run Inlay as administrator too.
- **Pastes are not verified.** Inlay sends Ctrl+V and can't tell whether the app accepted it.
- **Clipboard restore.** The previous clipboard comes back about 500 ms after the paste, unless something else was copied in the meantime. An app that reads the clipboard later than that pastes the old contents. Inlay restores memory-backed formats: text, HTML, RTF, file lists and images. It skips GDI and metafile handles, OLE's private formats, and any single format over 64 MiB.
- **Focus.** The paste goes to whichever window is focused when the result arrives, not necessarily where you started.
- **No list continuation.** It needs a confirmed cursor position, which plain paste can't provide, so the client never sends `continuationID`.
- **Original audio** is a 16 kHz mono copy, not the microphone's native format.
- **Only keys cancel a hold.** Mouse clicks while holding don't.
- **Not included yet:** server preferences, microphone priority lists, Wispr Flow import, output muting while recording.

## Troubleshooting

- Read `%APPDATA%\Inlay\inlay.log`.
- If every take ends with "No speech detected", check that the microphone isn't muted. While Inlay records, hovering over the microphone icon in the taskbar shows "Microphone: Muted" if it is.
- `inlay.exe --paste "some text"` waits two seconds, then pastes the text into the focused window. It uses the same clipboard save, Ctrl+V and restore as a take, which helps you check one app at a time.

## Uninstall

1. Quit Inlay from the tray.
2. Run `reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v Inlay /f`, or turn off **Start Inlay when I sign in** first.
3. Delete `%APPDATA%\Inlay` and the executable.
4. Remove the token: `cmdkey /list | findstr Inlay`, then `cmdkey /delete:<target>`.
