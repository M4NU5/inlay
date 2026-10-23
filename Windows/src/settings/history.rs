//! The History page: shared history from the server, filtered by device and
//! source, with the list beside the selected dictation, as on the Mac. The
//! list holds summaries; the selected dictation's whole record loads on its
//! own. The page fits the window; the list and the dictation scroll on their own.

use super::layout::{Builder, Control, Icon, Item};
use super::{
    Context, DELETE, DELETE_AUDIO, DEVICE, LOAD, OPEN_AUDIO, OPEN_ORIGINAL, RECORD_COPY, REFRESH,
    REPROCESS, RETRY_DETAIL, ROW, SOURCE, WM_HISTORY, server_line,
};
use crate::api::{Api, Detail, Summary};
use crate::app::{self, Server};
use crate::paste;
use crate::theme::{self, Font, px};
use crate::win::wide;
use std::sync::{Mutex, MutexGuard};
use std::time::UNIX_EPOCH;
use std::{env, fs, thread};
use windows_sys::Win32::Foundation::{FILETIME, HWND, RECT, SYSTEMTIME};
use windows_sys::Win32::Graphics::Gdi::{
    DT_CENTER, DT_EDITCONTROL, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DT_WORDBREAK,
};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO, MessageBoxW, PostMessageW, SW_SHOWNORMAL,
};

/// The source filter's choices, and what the server's `source` takes for each.
const SOURCES: [(&str, Option<&str>); 3] = [
    ("All sources", None),
    ("Inlay", Some("inlay")),
    ("Wispr Flow", Some("wispr-flow")),
];
const LINE: u32 = DT_SINGLELINE | DT_VCENTER;
const WRAPPED: u32 = DT_LEFT | DT_WORDBREAK | DT_NOPREFIX;

/// The panes that scroll, by id.
pub const LIST: usize = 0;
pub const DETAIL: usize = 1;

struct History {
    records: Vec<Summary>,
    /// Where the next older page starts, when there is one.
    next: Option<String>,
    loading: bool,
    /// Counts requests, so a reply to an older one is dropped.
    request: u64,
    source: usize,
    /// A device ID, or `None` for all devices.
    device: Option<String>,
    selected: Option<String>,
    /// The selected dictation's whole record, once loaded.
    detail: Option<Detail>,
    /// The row as it was when its detail was last asked for, so the detail
    /// loads again only when the row changes. Timestamps are whole seconds,
    /// so `updatedAt` alone can miss a change; the whole summary can't.
    detail_for: Option<Summary>,
    detail_failed: bool,
    /// Counts detail requests, so a reply to an older one is dropped.
    detail_request: u64,
    /// The IDs of the rows as last painted, so a click selects what it hit
    /// even if a reply changed the records since.
    rows: Vec<String>,
    scroll: [i32; 2],
    /// The result of the last action, and whether it failed.
    message: Option<(String, bool)>,
    copied: Option<String>,
    /// The app's finished-take count when history last loaded.
    seen: u64,
}

static HISTORY: Mutex<History> = Mutex::new(History {
    records: Vec::new(),
    next: None,
    loading: false,
    request: 0,
    source: 0,
    device: None,
    selected: None,
    detail: None,
    detail_for: None,
    detail_failed: false,
    detail_request: 0,
    rows: Vec::new(),
    scroll: [0, 0],
    message: None,
    copied: None,
    seen: 0,
});

fn history() -> MutexGuard<'static, History> {
    HISTORY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Loads the newest page, or with `older` the next older one. The reply
/// arrives as `WM_HISTORY`.
pub fn load(window: HWND, older: bool) {
    let settings = app::shared().settings();
    let (before, source, request) = {
        let mut history = history();
        if older && history.next.is_none() {
            return;
        }
        history.loading = true;
        history.request += 1;
        history.message = None;
        let before = if older { history.next.clone() } else { None };
        (before, SOURCES[history.source].1, history.request)
    };
    let target = window as usize;
    thread::spawn(move || {
        let result = Api::new(&settings.config.endpoint, &settings.token).and_then(|api| {
            api.history(before.as_deref(), source)
                .map_err(|error| error.to_string())
        });
        {
            let mut history = history();
            if history.request != request {
                return;
            }
            history.loading = false;
            match result {
                Ok(page) => {
                    if before.is_some() {
                        let fresh: Vec<Summary> = page
                            .items
                            .into_iter()
                            .filter(|record| !history.records.iter().any(|old| old.id == record.id))
                            .collect();
                        history.records.extend(fresh);
                    } else {
                        history.records = page.items;
                    }
                    history.next = page.next_cursor;
                    let selected = history.selected.clone();
                    if !history
                        .records
                        .iter()
                        .any(|record| Some(&record.id) == selected.as_ref())
                    {
                        history.selected = None;
                    }
                }
                Err(message) => history.message = Some((message, true)),
            }
        }
        // A summary that changed, say after Transcribe again, reloads its
        // detail. So does loading the newest page, as Refresh does: deleting
        // audio leaves the summary as it was.
        load_detail(target, before.is_none());
        post(target);
    });
}

/// Loads the selected row's whole record, unless it is loaded or loading
/// already for the row as the list last showed it. With `again`, loads it
/// regardless. The reply arrives as `WM_HISTORY`.
fn load_detail(window: usize, again: bool) {
    let (id, request) = {
        let mut history = history();
        let Some(summary) = selected(&history) else {
            return;
        };
        let wanted = summary.clone();
        if !again && history.detail_for.as_ref() == Some(&wanted) {
            return;
        }
        let id = wanted.id.clone();
        history.detail_for = Some(wanted);
        history.detail_failed = false;
        history.detail_request += 1;
        (id, history.detail_request)
    };
    let settings = app::shared().settings();
    thread::spawn(move || {
        let result = Api::new(&settings.config.endpoint, &settings.token)
            .and_then(|api| api.detail(&id).map_err(|error| error.to_string()));
        {
            let mut history = history();
            if history.detail_request != request {
                return;
            }
            match result {
                Ok(detail) => history.detail = Some(detail),
                Err(message) => {
                    // Drop the old record, so Try again shows instead of stale content.
                    history.detail = None;
                    history.detail_for = None;
                    history.detail_failed = true;
                    history.message = Some((message, true));
                }
            }
        }
        post(window);
    });
}

/// The selected row's loaded record. `None` while it loads.
fn detail(history: &History) -> Option<&Detail> {
    history
        .detail
        .as_ref()
        .filter(|detail| history.selected.as_ref() == Some(&detail.id))
}

fn post(window: usize) {
    unsafe { PostMessageW(window as HWND, WM_HISTORY, 0, 0) };
}

/// The page was opened: load the newest history.
pub fn opened(window: HWND, finished: u64) {
    history().seen = finished;
    load(window, false);
}

/// Loads again when a take has finished since history last loaded.
pub fn catch_up(window: HWND, finished: u64) {
    let stale = {
        let mut history = history();
        let stale = history.seen != finished && !history.loading;
        if stale {
            history.seen = finished;
        }
        stale
    };
    if stale {
        load(window, false);
    }
}

/// The records the device filter lets through, newest first.
fn shown(history: &History) -> Vec<&Summary> {
    history
        .records
        .iter()
        .filter(|record| {
            history
                .device
                .as_ref()
                .is_none_or(|device| record.device.id == *device)
        })
        .collect()
}

/// Each device in the loaded history once, by name.
fn devices(history: &History) -> Vec<(String, String)> {
    let mut devices: Vec<(String, String)> = Vec::new();
    for record in &history.records {
        if !devices.iter().any(|(id, _)| *id == record.device.id) {
            devices.push((record.device.id.clone(), record.device.name.clone()));
        }
    }
    devices.sort_by(|a, b| a.1.cmp(&b.1));
    devices
}

/// The labels for the device or source popup, and which is current.
pub fn choices(id: i32) -> (Vec<String>, Option<usize>) {
    let history = history();
    if id == SOURCE {
        let labels = SOURCES.iter().map(|(label, _)| label.to_string()).collect();
        (labels, Some(history.source))
    } else {
        let devices = devices(&history);
        let mut labels = vec!["All devices".to_string()];
        labels.extend(devices.iter().map(|(_, name)| name.clone()));
        let selected = match &history.device {
            None => Some(0),
            Some(device) => devices
                .iter()
                .position(|(id, _)| id == device)
                .map(|index| index + 1),
        };
        (labels, selected)
    }
}

pub fn choose_source(window: HWND, index: usize) {
    {
        let mut history = history();
        if index >= SOURCES.len() || index == history.source {
            return;
        }
        history.source = index;
        history.device = None;
        history.selected = None;
        history.records.clear();
        history.next = None;
        history.scroll = [0, 0];
    }
    load(window, false);
}

pub fn choose_device(index: usize) {
    let mut history = history();
    let device = match index {
        0 => None,
        index => devices(&history).get(index - 1).map(|(id, _)| id.clone()),
    };
    history.device = device;
    history.selected = None;
    history.scroll = [0, 0];
}

/// Selects the record painted in row `index`, and loads its whole record.
pub fn select(window: HWND, index: usize) {
    {
        let mut history = history();
        let id = history.rows.get(index).cloned();
        if id.is_none() || id == history.selected {
            return;
        }
        history.selected = id;
        history.scroll[DETAIL] = 0;
        history.copied = None;
    }
    load_detail(window as usize, false);
}

/// Tries again to load a dictation that could not be loaded.
pub fn retry_detail(window: HWND) {
    history().message = None;
    load_detail(window as usize, true);
}

/// Scrolls pane `pane` by `delta` device pixels, up to `limit`.
pub fn scroll(pane: usize, delta: i32, limit: i32) -> bool {
    let mut history = history();
    let Some(scroll) = history.scroll.get_mut(pane) else {
        return false;
    };
    let next = (*scroll + delta).clamp(0, limit.max(0));
    let moved = next != *scroll;
    *scroll = next;
    moved
}

fn selected(history: &History) -> Option<&Summary> {
    let id = history.selected.as_ref()?;
    history.records.iter().find(|record| record.id == *id)
}

/// Copies the selected dictation's transcript.
pub fn copy() {
    let (id, text) = {
        let history = history();
        match detail(&history) {
            Some(record) => (record.id.clone(), record.final_text.clone()),
            None => return,
        }
    };
    let result = paste::copy(&text);
    let mut history = history();
    match result {
        Ok(()) => history.copied = Some(id),
        Err(message) => history.message = Some((message, true)),
    }
}

/// Asks, then deletes the selected dictation from the server.
pub fn delete(window: HWND) {
    let Some(id) = history().selected.clone() else {
        return;
    };
    let (text, title) = (
        wide(
            "Delete this dictation from the server?\n\nIts archived text and files will be removed from every device's history.",
        ),
        wide("Inlay"),
    );
    let answer = unsafe {
        MessageBoxW(
            window,
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    if answer != IDYES {
        return;
    }
    let settings = app::shared().settings();
    let target = window as usize;
    thread::spawn(move || {
        let result = Api::new(&settings.config.endpoint, &settings.token)
            .and_then(|api| api.delete(&id).map_err(|error| error.to_string()));
        let reload = {
            let mut history = history();
            match result {
                Ok(()) => {
                    log!("history: deleted a dictation");
                    history.records.retain(|record| record.id != id);
                    if history.selected.as_ref() == Some(&id) {
                        history.selected = None;
                    }
                    // The next page starts after the last loaded record, so
                    // it can't be the one just deleted.
                    if history.next.as_ref() == Some(&id) {
                        history.next = history.records.last().map(|record| record.id.clone());
                    }
                    // A load already under way may still list it: drop that
                    // reply and load again. Load again too when nothing is
                    // left loaded, as older records may remain past the cursor.
                    let reload = history.loading || history.records.is_empty();
                    if reload {
                        history.request += 1;
                        history.loading = false;
                    }
                    reload
                }
                Err(message) => {
                    history.message = Some((message, true));
                    false
                }
            }
        };
        if reload {
            load(target as HWND, false);
        }
        post(target);
    });
}

/// Asks, then deletes the selected dictation's audio. Its text stays.
pub fn delete_audio(window: HWND) {
    let Some(id) = history().selected.clone() else {
        return;
    };
    let (text, title) = (
        wide(
            "Delete this recording's audio from the server?\n\nThe transcript stays in history. This cannot be undone.",
        ),
        wide("Inlay"),
    );
    let answer = unsafe {
        MessageBoxW(
            window,
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    if answer != IDYES {
        return;
    }
    let settings = app::shared().settings();
    let target = window as usize;
    thread::spawn(move || {
        let result = Api::new(&settings.config.endpoint, &settings.token)
            .and_then(|api| api.delete_audio(&id).map_err(|error| error.to_string()));
        let deleted = result.is_ok();
        history().message = Some(match result {
            Ok(()) => {
                log!("history: deleted a recording's audio");
                (
                    "Deleted the audio. The transcript stays in history.".into(),
                    false,
                )
            }
            Err(message) => (message, true),
        });
        // The summary doesn't change, so reload the record itself.
        if deleted && history().selected.as_ref() == Some(&id) {
            load_detail(target, true);
        }
        post(target);
    });
}

/// Queues a failed or cancelled take's audio to be transcribed again, then
/// follows it and loads History again when it ends.
pub fn transcribe_again(window: HWND) {
    let Some(id) = history().selected.clone() else {
        return;
    };
    history().message = Some(("Transcribing again…".into(), false));
    let settings = app::shared().settings();
    let target = window as usize;
    thread::spawn(move || {
        let api = match Api::new(&settings.config.endpoint, &settings.token) {
            Ok(api) => api,
            Err(message) => {
                history().message = Some((message, true));
                return post(target);
            }
        };
        if let Err(error) = api.reprocess(&id) {
            history().message = Some((error.to_string(), true));
            return post(target);
        }
        log!("history: queued a take to transcribe again");
        // Loading clears the page's message, so set it after.
        load(target as HWND, false);
        history().message = Some(("Transcribing again…".into(), false));
        post(target);
        let ended = api.events(&id, |_| {});
        let outcome = match &ended {
            Ok(record) if record.status == "completed" => None,
            Ok(record) => Some((
                record
                    .error
                    .clone()
                    .unwrap_or_else(|| "It could not be transcribed.".into()),
                true,
            )),
            Err(error) => Some((error.to_string(), true)),
        };
        load(target as HWND, false);
        history().message = outcome;
        post(target);
    });
}

/// Downloads the selected dictation's audio and opens it in the default app.
pub fn open_audio(window: HWND, original: bool) {
    let Some(id) = history().selected.clone() else {
        return;
    };
    let filename = if original {
        "original.wav"
    } else {
        "inference.wav"
    };
    history().message = Some(("Opening audio…".into(), false));
    let settings = app::shared().settings();
    let target = window as usize;
    thread::spawn(move || {
        let result = Api::new(&settings.config.endpoint, &settings.token)
            .and_then(|api| {
                api.artifact(&id, filename)
                    .map_err(|error| error.to_string())
            })
            .and_then(|bytes| {
                let folder = env::temp_dir().join("Inlay");
                let path = folder.join(format!("{id}-{filename}"));
                fs::create_dir_all(&folder)
                    .and_then(|()| fs::write(&path, bytes))
                    .map_err(|error| format!("Could not save the audio: {error}"))?;
                Ok(path)
            })
            .and_then(|path| {
                let (verb, file) = (wide("open"), wide(&path.display().to_string()));
                let opened = unsafe {
                    // ShellExecute may hand the file to a shell extension, which needs COM.
                    CoInitializeEx(
                        std::ptr::null(),
                        (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
                    );
                    ShellExecuteW(
                        std::ptr::null_mut(),
                        verb.as_ptr(),
                        file.as_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        SW_SHOWNORMAL,
                    )
                };
                // Values above 32 mean success.
                if opened as usize > 32 {
                    Ok(())
                } else {
                    Err("Windows has no app to play WAV audio.".into())
                }
            });
        history().message = result.err().map(|message| (message, true));
        post(target);
    });
}

pub fn build(page: &mut Builder, context: &Context) {
    page.fix();
    let mut history = history();
    let canvas = theme::CANVAS;

    let heading = page.band(px(40));
    page.text("History", heading, Font::Title, theme::INK, DT_LEFT | LINE);
    let middle = (heading.top + heading.bottom) / 2;
    let refresh = page.place_right(
        REFRESH,
        Control::icon(Icon::Refresh, "Refresh shared history"),
        heading.right,
        middle,
        canvas,
    );
    let source = page.place_right(
        SOURCE,
        Control::choice(SOURCES[history.source].0, true),
        refresh.left - px(10),
        middle,
        canvas,
    );
    let devices = devices(&history);
    let device = history
        .device
        .as_ref()
        .and_then(|device| devices.iter().find(|(id, _)| id == device))
        .map_or("All devices", |(_, name)| name.as_str());
    page.place_right_within(
        DEVICE,
        Control::choice(device, true),
        source.left - px(10),
        middle,
        canvas,
        px(200),
    );
    page.gap(12);

    let (dot, status) = server_line(&context.server);
    page.status(dot, status, &context.settings.config.endpoint, None);
    if let Some((message, error)) = &history.message {
        let colour = if *error { theme::ORANGE } else { theme::MUTED };
        page.note(message, colour);
    }
    page.gap(12);

    let top = page.y();
    let footer = px(44);
    let bottom = (page.view() - px(20) - footer).max(top + px(140));
    let list_width = (page.width() * 2 / 5).clamp(px(220), px(320));
    let list = RECT {
        left: page.left(),
        top,
        right: page.left() + list_width,
        bottom,
    };
    let detail = RECT {
        left: list.right + px(24),
        top,
        right: page.right(),
        bottom,
    };
    page.push(Item::Rule(RECT {
        left: list.right + px(11),
        top,
        right: list.right + px(12),
        bottom,
    }));
    let shown = shown(&history);
    let count = shown.len();
    let rows = shown.iter().map(|record| record.id.clone()).collect();
    let list_scroll = list_pane(page, &history, &shown, list);
    let detail_scroll = detail_pane(page, &history, context, detail);
    // Keep the offsets painted, so a larger window can't leave them past the end.
    history.rows = rows;
    history.scroll = [list_scroll, detail_scroll];

    let foot = RECT {
        left: page.left(),
        top: bottom + px(12),
        right: page.right(),
        bottom: bottom + px(12) + px(34),
    };
    let more = history.next.is_some();
    let count = format!(
        "{} sessions{}{}",
        count,
        if more { " loaded" } else { "" },
        if history.loading {
            " · Loading…"
        } else {
            ""
        }
    );
    page.text(&count, foot, Font::Small, theme::MUTED, DT_LEFT | LINE);
    if more {
        page.place_right(
            LOAD,
            Control::button("Load older").enabled(!history.loading),
            foot.right,
            (foot.top + foot.bottom) / 2,
            canvas,
        );
    }
}

/// Returns the list's scroll offset as painted.
fn list_pane(page: &mut Builder, history: &History, shown: &[&Summary], area: RECT) -> i32 {
    let row = px(96);
    let content = shown.len() as i32 * row;
    let limit = (content - (area.bottom - area.top)).max(0);
    let scroll = history.scroll[LIST].min(limit);
    page.pane(LIST, area, limit);
    page.push(Item::Clip(Some(area)));
    if shown.is_empty() {
        let middle = (area.top + area.bottom) / 2;
        let (title, detail) = if history.loading {
            ("Loading…", "")
        } else {
            (
                "No dictations",
                "Record while connected to add a dictation.",
            )
        };
        page.text(
            title,
            RECT {
                top: middle - px(24),
                bottom: middle,
                ..area
            },
            Font::Strong,
            theme::MUTED,
            DT_CENTER | LINE,
        );
        page.text(
            detail,
            RECT {
                top: middle + px(2),
                bottom: middle + px(22),
                ..area
            },
            Font::Small,
            theme::MUTED,
            DT_CENTER | LINE,
        );
    }
    for (index, record) in shown.iter().enumerate() {
        let top = area.top + index as i32 * row - scroll;
        if top + row < area.top || top > area.bottom {
            continue;
        }
        let whole = RECT {
            left: area.left,
            top,
            right: area.right - px(4),
            bottom: top + row,
        };
        let chosen = history.selected.as_ref() == Some(&record.id);
        if chosen {
            page.push(Item::Fill {
                area: RECT {
                    top: whole.top + px(2),
                    bottom: whole.bottom - px(2),
                    ..whole
                },
                colour: theme::SIDEBAR_SELECTED,
            });
        } else if index + 1 < shown.len() {
            page.push(Item::Rule(RECT {
                left: whole.left + px(12),
                top: whole.bottom - 1,
                right: whole.right - px(12),
                bottom: whole.bottom,
            }));
        }
        let (left, right) = (whole.left + px(12), whole.right - px(12));
        let date = RECT {
            left,
            top: top + px(10),
            right,
            bottom: top + px(28),
        };
        page.text(
            &date_label(&record.created_at, false),
            date,
            Font::Small,
            theme::MUTED,
            DT_LEFT | LINE,
        );
        if record.status != "completed" {
            page.text(
                &row_status(record),
                date,
                Font::Small,
                theme::ORANGE,
                DT_RIGHT | LINE,
            );
        }
        page.text(
            &summary(record),
            RECT {
                left,
                top: top + px(30),
                right,
                bottom: top + px(68),
            },
            Font::Body,
            theme::INK,
            WRAPPED | DT_EDITCONTROL | DT_END_ELLIPSIS,
        );
        page.push(Item::Icon {
            icon: Icon::Laptop,
            x: left,
            y: top + px(72),
            size: px(16),
            colour: theme::MUTED,
        });
        page.text(
            &source_label(record.imported_source.is_some(), &record.device.name),
            RECT {
                left: left + px(22),
                top: top + px(70),
                right,
                bottom: top + px(90),
            },
            Font::Small,
            theme::MUTED,
            DT_LEFT | LINE | DT_END_ELLIPSIS,
        );
        page.hit(
            RECT {
                top: whole.top.max(area.top),
                bottom: whole.bottom.min(area.bottom),
                ..whole
            },
            ROW + index as i32,
        );
    }
    page.push(Item::Clip(None));
    scroll
}

/// Returns the dictation's scroll offset as painted.
fn detail_pane(page: &mut Builder, history: &History, context: &Context, area: RECT) -> i32 {
    let canvas = theme::CANVAS;
    if selected(history).is_none() {
        let middle = (area.top + area.bottom) / 2;
        let size = px(44);
        page.push(Item::Icon {
            icon: Icon::Paragraph,
            x: (area.left + area.right - size) / 2,
            y: middle - px(64),
            size,
            colour: theme::FAINT,
        });
        page.text(
            "Select a dictation",
            RECT {
                top: middle - px(12),
                bottom: middle + px(30),
                ..area
            },
            Font::Title,
            theme::FAINT,
            DT_CENTER | LINE,
        );
        return 0;
    }
    let Some(record) = detail(history) else {
        let middle = (area.top + area.bottom) / 2;
        let text = RECT {
            top: middle - px(30),
            bottom: middle - px(6),
            ..area
        };
        if history.detail_failed {
            page.text(
                "This dictation could not be loaded.",
                text,
                Font::Body,
                theme::MUTED,
                DT_CENTER | LINE,
            );
            let button = Control::button("Try again");
            let width = button.width();
            let left = (area.left + area.right - width) / 2;
            page.place(
                RETRY_DETAIL,
                button,
                RECT {
                    left,
                    top: middle + px(4),
                    right: left + width,
                    bottom: middle + px(38),
                },
                canvas,
            );
        } else {
            page.text(
                "Loading dictation…",
                text,
                Font::Body,
                theme::MUTED,
                DT_CENTER | LINE,
            );
        }
        return 0;
    };

    let header = RECT {
        bottom: area.top + px(36),
        ..area
    };
    let middle = (header.top + header.bottom) / 2;
    let online = context.server == Server::Ready;
    let delete = page.place_right(
        DELETE,
        Control::icon(Icon::Trash, "Delete dictation").enabled(record.is_terminal() && online),
        header.right,
        middle,
        canvas,
    );
    let copied = history.copied.as_ref() == Some(&record.id);
    let copy = page.place_right(
        RECORD_COPY,
        Control::icon(
            if copied { Icon::Check } else { Icon::Copy },
            "Copy transcript",
        )
        .enabled(!record.final_text.is_empty()),
        delete.left - px(8),
        middle,
        canvas,
    );
    page.text(
        &date_label(&record.created_at, true),
        RECT {
            right: copy.left - px(12),
            ..header
        },
        Font::Strong,
        theme::INK,
        DT_LEFT | LINE | DT_END_ELLIPSIS,
    );

    let mut facts = vec![source_label(
        record.imported_source.is_some(),
        &record.device.name,
    )];
    match &record.imported_source {
        None => facts.push(status_label(&record.status).into()),
        Some(source) => {
            if let Some(status) = source
                .source_status
                .as_ref()
                .filter(|status| !status.is_empty())
            {
                facts.push(format!("Flow status: {status}"));
            }
        }
    }
    let seconds = record.seconds().round() as u64;
    if seconds > 0 {
        facts.push(format!("{}:{:02}", seconds / 60, seconds % 60));
    }
    let meta = RECT {
        top: header.bottom,
        bottom: header.bottom + px(22),
        ..area
    };
    page.text(
        &facts.join("  ·  "),
        meta,
        Font::Small,
        theme::MUTED,
        DT_LEFT | LINE | DT_END_ELLIPSIS,
    );
    let rule = meta.bottom + px(10);
    page.push(Item::Rule(RECT {
        top: rule,
        bottom: rule + 1,
        ..area
    }));

    // The actions sit below the text, which scrolls between the hairlines.
    let actions = RECT {
        top: area.bottom - px(34),
        ..area
    };
    page.push(Item::Rule(RECT {
        top: actions.top - px(12),
        bottom: actions.top - px(11),
        ..area
    }));
    let has_audio = record.inference_audio.is_some() || record.original_audio.is_some();
    let buttons: Vec<(i32, &str, &str)> = [
        (
            REPROCESS,
            "Transcribe again",
            "Again",
            record.can_transcribe_again(),
        ),
        (
            OPEN_AUDIO,
            "Open audio",
            "Audio",
            record.inference_audio.is_some(),
        ),
        (
            OPEN_ORIGINAL,
            "Open original",
            "Original",
            record.original_audio.is_some(),
        ),
        (
            DELETE_AUDIO,
            "Delete audio",
            "Delete",
            has_audio && record.is_terminal(),
        ),
    ]
    .into_iter()
    .filter(|button| button.3)
    .map(|(id, long, short, _)| (id, long, short))
    .collect();
    // Short labels when the pane is narrow, and never wider than a fair share.
    let room = actions.right - actions.left;
    let gaps = px(10) * (buttons.len() as i32 - 1).max(0);
    let fits = buttons
        .iter()
        .map(|(_, long, _)| Control::button(long).width())
        .sum::<i32>()
        + gaps
        <= room;
    let share = (room - gaps) / (buttons.len() as i32).max(1);
    let mut left = actions.left;
    for (id, long, short) in buttons {
        let control = Control::button(if fits { long } else { short }).enabled(online);
        let width = control.width().min(share);
        page.place(
            id,
            control,
            RECT {
                left,
                right: left + width,
                ..actions
            },
            canvas,
        );
        left += width + px(10);
    }

    let body = RECT {
        top: rule + px(14),
        bottom: actions.top - px(24),
        ..area
    };
    let width = body.right - body.left;
    let mut blocks: Vec<(String, Font, u32, i32)> = Vec::new();
    if let Some(error) = &record.error {
        blocks.push((error.clone(), Font::Body, theme::ORANGE, px(14)));
    }
    if matches!(record.status.as_str(), "failed" | "cancelled") {
        let note = if has_audio {
            format!(
                "This recording was {}. Its audio is kept in shared history until the audio retention period ends or you delete it.",
                record.status
            )
        } else {
            "Audio was removed by the retention setting.".into()
        };
        blocks.push((note, Font::Small, theme::MUTED, px(14)));
    }
    let (text, colour) = if record.final_text.is_empty() {
        ("No transcript available.".to_string(), theme::MUTED)
    } else {
        (record.final_text.clone(), theme::INK)
    };
    blocks.push((text, Font::Body, colour, px(18)));
    if !record.raw_text.is_empty() && record.raw_text != record.final_text {
        let title = if record.imported_source.is_some() {
            "Wispr Flow ASR"
        } else {
            "Original transcript"
        };
        blocks.push((title.into(), Font::Strong, theme::MUTED, px(6)));
        blocks.push((record.raw_text.clone(), Font::Small, theme::MUTED, px(18)));
    }
    if let (None, Some(reason)) = (
        &record.original_audio,
        &record.original_audio_omitted_reason,
    ) {
        blocks.push((reason.clone(), Font::Small, theme::FAINT, px(6)));
    }
    if let Some(speech) = &record.speech {
        blocks.push((
            format!("{} · {}", speech.model_id, speech.backend),
            Font::Small,
            theme::FAINT,
            0,
        ));
    }
    let heights: Vec<i32> = blocks
        .iter()
        .map(|(text, font, _, _)| theme::text_height(text, *font, width, WRAPPED))
        .collect();
    let content: i32 =
        heights.iter().sum::<i32>() + blocks.iter().map(|block| block.3).sum::<i32>();
    let limit = (content - (body.bottom - body.top)).max(0);
    let scroll = history.scroll[DETAIL].min(limit);
    page.pane(DETAIL, body, limit);
    page.push(Item::Clip(Some(body)));
    let mut y = body.top - scroll;
    for ((text, font, colour, after), height) in blocks.iter().zip(heights) {
        page.text(
            text,
            RECT {
                top: y,
                bottom: y + height,
                ..body
            },
            *font,
            *colour,
            WRAPPED,
        );
        y += height + after;
    }
    page.push(Item::Clip(None));
    scroll
}

fn status_label(status: &str) -> &'static str {
    match status {
        "receiving" => "Recording",
        "queued" => "Queued",
        "transcribing" => "Transcribing",
        "proofreading" => "Proofreading",
        "completed" => "Completed",
        "failed" => "Failed",
        "cancelled" => "Cancelled",
        _ => "Unknown",
    }
}

/// A row's status beside its date, with its place in the queue when it waits.
fn row_status(record: &Summary) -> String {
    match (record.status.as_str(), record.queue_position) {
        ("queued", Some(position)) => format!("Queued #{position}"),
        (status, _) => status_label(status).into(),
    }
}

fn source_label(imported: bool, device: &str) -> String {
    if imported {
        "Imported from Wispr Flow".into()
    } else {
        device.to_string()
    }
}

fn summary(record: &Summary) -> String {
    if !record.preview_text.is_empty() {
        record.preview_text.clone()
    } else if record.imported_source.is_some() {
        "No transcript recovered".into()
    } else {
        status_label(&record.status).into()
    }
}

/// "20 Oct at 12:15", or with `long` "20 October at 12:15", in local time.
fn date_label(created: &str, long: bool) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let Some(time) = local_time(created) else {
        return created.to_string();
    };
    let month = MONTHS[(time.wMonth.clamp(1, 12) - 1) as usize];
    let month = if long { month } else { &month[..3] };
    format!(
        "{} {month} at {:02}:{:02}",
        time.wDay, time.wHour, time.wMinute
    )
}

/// The day of an RFC 3339 time, such as "23 Oct 2026".
pub fn day_label(time: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let time = local_time(time)?;
    let month = MONTHS[(time.wMonth.clamp(1, 12) - 1) as usize];
    Some(format!("{} {month} {}", time.wDay, time.wYear))
}

/// An RFC 3339 time from the server, in this PC's time zone.
fn local_time(created: &str) -> Option<SYSTEMTIME> {
    let since = humantime::parse_rfc3339(created)
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?;
    // FILETIME counts 100 ns intervals from 1601.
    let ticks = since.as_nanos() / 100 + 116_444_736_000_000_000;
    let file = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        if FileTimeToSystemTime(&file, &mut utc) == 0
            || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0
        {
            return None;
        }
    }
    Some(local)
}
