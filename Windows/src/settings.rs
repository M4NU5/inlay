//! The settings window: a sidebar with Dictation, Server and About pages in the
//! Glacier palette. Text boxes are borderless Win32 edits inside painted
//! fields. Choices, the switch and the buttons are owner-drawn buttons, and
//! choices open the shared popup list.

use crate::app::{self, Server};
use crate::icons::Look;
use crate::menu::{self, Anchor, Entry};
use crate::theme::{self, Font, px, rect};
use crate::win::{from_wide, wide};
use crate::{audio, config, credentials, hotkey, startup, tray};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::thread;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, HDC, InvalidateRect,
    SetBkColor, SetTextColor,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_SETMARGINS, ODS_FOCUS, ODS_NOFOCUSRECT, ODS_SELECTED, WM_MOUSELEAVE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

static WINDOW: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static TEST_RESULT: Mutex<Option<String>> = Mutex::new(None);
/// The button class's own window procedure, wrapped to track hover.
static BUTTON_PROC: AtomicUsize = AtomicUsize::new(0);
static HOT: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

struct State {
    page: usize,
    /// Microphone names behind the list entries after "System default".
    microphones: Vec<String>,
    microphone_labels: Vec<String>,
    microphone: usize,
    hotkey: usize,
    launch: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    page: 0,
    microphones: Vec::new(),
    microphone_labels: Vec::new(),
    microphone: 0,
    hotkey: 0,
    launch: false,
});

const ENDPOINT: i32 = 101;
const TOKEN: i32 = 102;
const NAME: i32 = 103;
const MICROPHONE: i32 = 104;
const HOTKEY: i32 = 105;
const LAUNCH: i32 = 106;
const STATUS: i32 = 107;
const TEST: i32 = 108;
const VERSION: i32 = 109;
const FOLDER: i32 = 110;
/// Sidebar buttons: `NAV + page`.
const NAV: i32 = 120;
/// Row labels: `LABEL + page * 10 + row`.
const LABEL: i32 = 200;
/// Popup list choices: `PICK_MICROPHONE + index` and `PICK_HOTKEY + index`.
const PICK_MICROPHONE: usize = 1000;
const PICK_HOTKEY: usize = 2000;
const WM_TEST_DONE: u32 = WM_APP + 20;
// Static control styles, which windows-sys keeps behind an unrelated feature.
const SS_RIGHT: u32 = 0x2;
const SS_NOPREFIX: u32 = 0x80;
const SS_PATHELLIPSIS: u32 = 0x8000;

// Layout in logical pixels.
const WIDTH: i32 = 640;
const HEIGHT: i32 = 440;
const SIDE: i32 = 184;
const MARGIN: i32 = 28;
const CONTENT: i32 = SIDE + MARGIN;
const ROW: i32 = 44;
const FIELD_WIDTH: i32 = 212;
const FIELD_HEIGHT: i32 = 30;
const BUTTON_HEIGHT: i32 = 32;

#[derive(Clone, Copy)]
enum Field {
    Edit(i32, bool),
    Choice(i32),
    Switch(i32),
    Value(i32),
}

struct Row {
    label: &'static str,
    field: Field,
}

struct Group {
    label: &'static str,
    rows: &'static [Row],
}

struct Page {
    title: &'static str,
    groups: &'static [Group],
}

static PAGES: [Page; 3] = [
    Page {
        title: "Dictation",
        groups: &[
            Group {
                label: "Shortcut",
                rows: &[Row {
                    label: "Hold to talk",
                    field: Field::Choice(HOTKEY),
                }],
            },
            Group {
                label: "This PC",
                rows: &[
                    Row {
                        label: "Device name",
                        field: Field::Edit(NAME, false),
                    },
                    Row {
                        label: "Microphone",
                        field: Field::Choice(MICROPHONE),
                    },
                    Row {
                        label: "Start when I sign in",
                        field: Field::Switch(LAUNCH),
                    },
                ],
            },
        ],
    },
    Page {
        title: "Server",
        groups: &[Group {
            label: "Connection",
            rows: &[
                Row {
                    label: "Server address",
                    field: Field::Edit(ENDPOINT, false),
                },
                Row {
                    label: "Server token",
                    field: Field::Edit(TOKEN, true),
                },
            ],
        }],
    },
    Page {
        title: "About",
        groups: &[Group {
            label: "Inlay for Windows",
            rows: &[
                Row {
                    label: "Version",
                    field: Field::Value(VERSION),
                },
                Row {
                    label: "Settings folder",
                    field: Field::Value(FOLDER),
                },
            ],
        }],
    },
];

/// A group's label and box, and its rows, in logical pixels.
struct Placed {
    label: RECT,
    frame: RECT,
    rows: Vec<RECT>,
}

fn logical(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

fn scaled(area: RECT) -> RECT {
    rect(area.left, area.top, area.right, area.bottom)
}

fn place(page: &Page) -> Vec<Placed> {
    let (left, right) = (CONTENT, WIDTH - MARGIN);
    let mut y = 76;
    page.groups
        .iter()
        .map(|group| {
            let label = logical(left + 2, y, right, y + 16);
            y += 22;
            let rows: Vec<RECT> = (0..group.rows.len() as i32)
                .map(|index| logical(left, y + index * ROW, right, y + (index + 1) * ROW))
                .collect();
            let frame = logical(left, y, right, y + group.rows.len() as i32 * ROW);
            y = frame.bottom + 20;
            Placed { label, frame, rows }
        })
        .collect()
}

/// Where a row's control goes, in logical pixels.
fn field_area(row: RECT, field: Field) -> RECT {
    let right = row.right - 14;
    let middle = (row.top + row.bottom) / 2;
    match field {
        Field::Edit(..) | Field::Choice(_) => logical(
            right - FIELD_WIDTH,
            middle - FIELD_HEIGHT / 2,
            right,
            middle + FIELD_HEIGHT / 2,
        ),
        Field::Switch(_) => logical(right - 40, middle - 11, right, middle + 11),
        Field::Value(_) => logical(right - 250, middle - 10, right, middle + 10),
    }
}

fn label_area(row: RECT) -> RECT {
    let middle = (row.top + row.bottom) / 2;
    logical(
        row.left + 14,
        middle - 10,
        row.right - 14 - FIELD_WIDTH - 8,
        middle + 10,
    )
}

fn test_area() -> RECT {
    let bottom = place(&PAGES[1])
        .last()
        .map_or(200, |group| group.frame.bottom);
    logical(
        CONTENT,
        bottom + 16,
        CONTENT + 150,
        bottom + 16 + BUTTON_HEIGHT,
    )
}

fn save_area() -> RECT {
    let right = WIDTH - MARGIN;
    let top = HEIGHT - 24 - BUTTON_HEIGHT;
    logical(right - 90, top, right, top + BUTTON_HEIGHT)
}

fn cancel_area() -> RECT {
    let save = save_area();
    logical(save.left - 8 - 90, save.top, save.left - 8, save.bottom)
}

fn nav_area(page: usize) -> RECT {
    let top = 76 + page as i32 * 36;
    logical(12, top, SIDE - 12, top + 32)
}

fn state() -> MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The open settings window, or null. The message loop routes dialog keys to it.
pub fn window() -> HWND {
    WINDOW.load(Ordering::SeqCst)
}

pub fn close() {
    let window = window();
    if !window.is_null() {
        unsafe { DestroyWindow(window) };
    }
}

/// Redraws the server status in the sidebar.
pub fn refresh() {
    let window = window();
    if !window.is_null() {
        unsafe { InvalidateRect(window, null(), 0) };
    }
}

pub fn open() {
    let existing = window();
    if !existing.is_null() {
        unsafe {
            ShowWindow(existing, SW_SHOWNORMAL);
            SetForegroundWindow(existing);
        }
        return;
    }
    let class_name = wide("InlaySettings");
    unsafe {
        let instance = GetModuleHandleW(null());
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class_name.as_ptr(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                ..std::mem::zeroed()
            };
            RegisterClassW(&class);
        });
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        let mut frame = rect(0, 0, WIDTH, HEIGHT);
        AdjustWindowRectEx(&mut frame, style, 0, 0);
        let (width, height) = (frame.right - frame.left, frame.bottom - frame.top);
        let x = (GetSystemMetrics(SM_CXSCREEN) - width) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - height) / 2;
        let title = wide("Inlay settings");
        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            style,
            x,
            y,
            width,
            height,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            return;
        }
        WINDOW.store(window, Ordering::SeqCst);
        theme::frame(window, Some(theme::CANVAS), false);
        let icon = tray::icon(Look::Idle) as LPARAM;
        SendMessageW(window, WM_SETICON, ICON_SMALL as WPARAM, icon);
        SendMessageW(window, WM_SETICON, ICON_BIG as WPARAM, icon);

        // Creation order is tab order.
        for (page, each) in PAGES.iter().enumerate() {
            button(window, NAV + page as i32, each.title, nav_area(page), true);
        }
        for (index, page) in PAGES.iter().enumerate() {
            for (group, placed) in page.groups.iter().zip(place(page)) {
                for (row, area) in group.rows.iter().zip(placed.rows) {
                    let number = page_row(index, row);
                    control(
                        window,
                        "STATIC",
                        row.label,
                        SS_NOPREFIX,
                        LABEL + number,
                        scaled(label_area(area)),
                        Font::Body,
                    );
                    let field = field_area(area, row.field);
                    match row.field {
                        Field::Edit(id, password) => {
                            // Borderless, centred in the painted field.
                            let inner = logical(
                                field.left + 10,
                                field.top + (FIELD_HEIGHT - 20) / 2,
                                field.right - 10,
                                field.top + (FIELD_HEIGHT - 20) / 2 + 20,
                            );
                            let style = WS_TABSTOP
                                | ES_AUTOHSCROLL as u32
                                | if password { ES_PASSWORD as u32 } else { 0 };
                            let edit =
                                control(window, "EDIT", "", style, id, scaled(inner), Font::Body);
                            SendMessageW(edit, EM_SETMARGINS, 3, 0);
                        }
                        Field::Choice(id) | Field::Switch(id) => {
                            button(window, id, row.label, field, false);
                        }
                        Field::Value(id) => {
                            control(
                                window,
                                "STATIC",
                                "",
                                SS_RIGHT | SS_NOPREFIX | SS_PATHELLIPSIS,
                                id,
                                scaled(field),
                                Font::Body,
                            );
                        }
                    }
                }
            }
        }
        button(window, TEST, "Test connection", test_area(), false);
        let cancel = cancel_area();
        control(
            window,
            "STATIC",
            "",
            SS_NOPREFIX | WS_VISIBLE,
            STATUS,
            scaled(logical(
                CONTENT,
                cancel.top - 4,
                cancel.left - 16,
                HEIGHT - 20,
            )),
            Font::Small,
        );
        button(window, IDCANCEL, "Cancel", cancel, true);
        button(window, IDOK, "Save", save_area(), true);

        let page = fill(window);
        show_page(window, page);
        ShowWindow(window, SW_SHOWNORMAL);
        SetForegroundWindow(window);
        SetFocus(GetDlgItem(window, if page == 1 { ENDPOINT } else { NAME }));
    }
}

/// The number of `row` among all rows of page `index`, for its label's id.
fn page_row(index: usize, row: &Row) -> i32 {
    let position = PAGES[index]
        .groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .position(|other| std::ptr::eq(other, row))
        .unwrap_or(0);
    index as i32 * 10 + position as i32
}

fn control(
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    id: i32,
    RECT {
        left,
        top,
        right,
        bottom,
    }: RECT,
    font: Font,
) -> HWND {
    let (class, text) = (wide(class), wide(text));
    unsafe {
        let control = CreateWindowExW(
            0,
            class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | style,
            left,
            top,
            right - left,
            bottom - top,
            parent,
            id as usize as HMENU,
            GetModuleHandleW(null()),
            null(),
        );
        SendMessageW(control, WM_SETFONT, theme::font(font) as WPARAM, 1);
        control
    }
}

/// An owner-drawn button whose hover state this module tracks. `area` is logical.
fn button(parent: HWND, id: i32, text: &str, area: RECT, visible: bool) -> HWND {
    let style = WS_TABSTOP | BS_OWNERDRAW as u32 | if visible { WS_VISIBLE } else { 0 };
    let button = control(parent, "BUTTON", text, style, id, scaled(area), Font::Body);
    let original =
        unsafe { SetWindowLongPtrW(button, GWLP_WNDPROC, button_proc as *const () as isize) };
    BUTTON_PROC.store(original as usize, Ordering::SeqCst);
    button
}

unsafe extern "system" fn button_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_MOUSEMOVE => {
            let previous = HOT.swap(window, Ordering::SeqCst);
            if previous != window {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: window,
                    dwHoverTime: 0,
                };
                unsafe {
                    TrackMouseEvent(&mut track);
                    InvalidateRect(window, null(), 0);
                    if !previous.is_null() {
                        InvalidateRect(previous, null(), 0);
                    }
                }
            }
        }
        WM_MOUSELEAVE
            if HOT
                .compare_exchange(window, null_mut(), Ordering::SeqCst, Ordering::SeqCst)
                .is_ok() =>
        {
            unsafe { InvalidateRect(window, null(), 0) };
        }
        _ => {}
    }
    let original: WNDPROC = unsafe { std::mem::transmute(BUTTON_PROC.load(Ordering::SeqCst)) };
    unsafe { CallWindowProcW(original, window, message, wparam, lparam) }
}

/// Loads the saved settings into the controls. Returns the page to open on:
/// Server until a token is saved, otherwise Dictation.
fn fill(window: HWND) -> usize {
    let app = app::shared();
    let settings = app.settings();
    set_text(window, ENDPOINT, &settings.config.endpoint);
    set_text(window, TOKEN, &settings.token);
    set_text(window, NAME, &settings.config.device_name);
    set_text(window, VERSION, env!("CARGO_PKG_VERSION"));
    set_text(window, FOLDER, &config::data_dir().display().to_string());

    let connected = audio::input_names();
    let saved = settings.config.microphone.clone();
    let mut names = connected.clone();
    if let Some(saved) = &saved
        && !names.contains(saved)
    {
        names.push(saved.clone());
    }
    let mut labels = vec!["System default".to_string()];
    labels.extend(names.iter().map(|name| {
        if connected.contains(name) {
            name.clone()
        } else {
            format!("{name} (not connected)")
        }
    }));
    let microphone = saved
        .as_ref()
        .and_then(|saved| names.iter().position(|name| name == saved))
        .map_or(0, |index| index + 1);
    let hotkey = hotkey::KEYS
        .iter()
        .position(|key| key.id == settings.config.hotkey)
        .unwrap_or(0);
    set_text(window, MICROPHONE, &labels[microphone]);
    set_text(window, HOTKEY, hotkey::KEYS[hotkey].label);
    {
        let mut state = state();
        state.microphones = names;
        state.microphone_labels = labels;
        state.microphone = microphone;
        state.hotkey = hotkey;
        state.launch = startup::is_enabled();
    }

    let status = match app.status(|status| status.server.clone()) {
        Server::Ready => "The server is ready.".to_string(),
        Server::Checking => "Checking the server…".to_string(),
        Server::Unavailable(message) => message,
    };
    set_text(window, STATUS, &status);
    if settings.token.is_empty() { 1 } else { 0 }
}

fn show_page(window: HWND, page: usize) {
    state().page = page;
    for (index, each) in PAGES.iter().enumerate() {
        let visible = if index == page { SW_SHOW } else { SW_HIDE };
        for row in each.groups.iter().flat_map(|group| group.rows.iter()) {
            let id = match row.field {
                Field::Edit(id, _) | Field::Choice(id) | Field::Switch(id) | Field::Value(id) => id,
            };
            unsafe {
                ShowWindow(GetDlgItem(window, id), visible);
                ShowWindow(GetDlgItem(window, LABEL + page_row(index, row)), visible);
            }
        }
    }
    unsafe {
        ShowWindow(
            GetDlgItem(window, TEST),
            if page == 1 { SW_SHOW } else { SW_HIDE },
        );
        InvalidateRect(window, null(), 0);
        for index in 0..PAGES.len() {
            InvalidateRect(GetDlgItem(window, NAV + index as i32), null(), 0);
        }
    }
}

fn paint(window: HWND) {
    let page = state().page;
    let focus = unsafe { GetDlgCtrlID(GetFocus()) };
    let (status, dot) = match app::shared().status(|status| status.server.clone()) {
        Server::Ready => ("Server ready", theme::GREEN),
        Server::Checking => ("Connecting…", theme::ORANGE),
        Server::Unavailable(_) => ("Server offline", theme::RED),
    };
    theme::paint(window, |canvas, client| {
        let s = theme::scale();
        canvas.fill(client, theme::CANVAS);
        canvas.fill(rect(0, 0, SIDE, HEIGHT), theme::SIDEBAR);
        canvas.fill(
            RECT {
                left: px(SIDE) - 1,
                right: px(SIDE),
                ..client
            },
            theme::LINE,
        );
        canvas.mark(px(22), px(25), px(22), theme::ACCENT);
        canvas.text(
            "Inlay",
            rect(54, 18, SIDE - 12, 54),
            Font::Wordmark,
            theme::INK,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );
        canvas.circle(px(27) as f32, px(HEIGHT - 30) as f32, 3.5 * s, dot);
        canvas.text(
            status,
            rect(38, HEIGHT - 40, SIDE - 12, HEIGHT - 20),
            Font::Small,
            theme::MUTED,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
        );

        let current = &PAGES[page];
        canvas.text(
            current.title,
            rect(CONTENT, 20, WIDTH - MARGIN, 58),
            Font::Title,
            theme::INK,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );
        for (group, placed) in current.groups.iter().zip(place(current)) {
            canvas.text(
                group.label,
                scaled(placed.label),
                Font::SmallStrong,
                theme::MUTED,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            );
            let frame = scaled(placed.frame);
            canvas.round(frame, 10.0 * s, theme::SURFACE);
            canvas.outline(frame, 10.0 * s, theme::LINE, s);
            for (index, (row, area)) in group.rows.iter().zip(&placed.rows).enumerate() {
                if index > 0 {
                    let line = scaled(*area);
                    canvas.fill(
                        RECT {
                            left: line.left + px(14),
                            right: line.right - px(14),
                            top: line.top,
                            bottom: line.top + 1,
                        },
                        theme::LINE,
                    );
                }
                if let Field::Edit(id, _) = row.field {
                    let field = scaled(field_area(*area, row.field));
                    canvas.round(field, 7.0 * s, theme::FIELD);
                    let border = if focus == id {
                        theme::ACCENT
                    } else {
                        theme::LINE
                    };
                    canvas.outline(field, 7.0 * s, border, s);
                }
            }
        }
    });
}

fn draw_item(item: &DRAWITEMSTRUCT) {
    let id = item.CtlID as i32;
    let hot = HOT.load(Ordering::SeqCst) == item.hwndItem;
    let pressed = item.itemState & ODS_SELECTED != 0;
    let focused = item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0;
    let text = window_text(item.hwndItem);
    let (page, launch) = {
        let state = state();
        (state.page, state.launch)
    };
    theme::buffered(item.hDC, item.rcItem, |canvas, area| {
        let s = theme::scale();
        let ring = |radius: f32| {
            if focused {
                canvas.outline(area, radius, theme::ACCENT, 1.5 * s);
            }
        };
        match id {
            _ if (NAV..NAV + PAGES.len() as i32).contains(&id) => {
                canvas.fill(area, theme::SIDEBAR);
                let selected = (id - NAV) as usize == page;
                let (fill, colour, font) = if selected {
                    (Some(theme::TINT), theme::ACCENT, Font::Strong)
                } else if hot {
                    (Some(theme::HOVER), theme::INK, Font::Body)
                } else {
                    (None, theme::MUTED, Font::Body)
                };
                if let Some(fill) = fill {
                    canvas.round(area, 7.0 * s, fill);
                }
                ring(7.0 * s);
                canvas.text(
                    &text,
                    RECT {
                        left: area.left + px(12),
                        ..area
                    },
                    font,
                    colour,
                    DT_LEFT | DT_SINGLELINE | DT_VCENTER,
                );
            }
            MICROPHONE | HOTKEY => {
                canvas.fill(area, theme::SURFACE);
                canvas.round(area, 7.0 * s, if hot { theme::HOVER } else { theme::FIELD });
                let border = if focused {
                    theme::ACCENT
                } else if hot {
                    theme::RAISED
                } else {
                    theme::LINE
                };
                canvas.outline(area, 7.0 * s, border, s);
                canvas.text(
                    &text,
                    RECT {
                        left: area.left + px(10),
                        right: area.right - px(28),
                        ..area
                    },
                    Font::Body,
                    theme::INK,
                    DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                );
                canvas.chevron(
                    (area.right - px(15)) as f32,
                    (area.top + area.bottom) as f32 / 2.0,
                    8.0 * s,
                    theme::MUTED,
                );
            }
            LAUNCH => {
                canvas.fill(area, theme::SURFACE);
                let (track, knob) = if launch {
                    (
                        if hot {
                            theme::ACCENT_HOT
                        } else {
                            theme::ACCENT
                        },
                        theme::ON_ACCENT,
                    )
                } else {
                    (if hot { theme::RAISED } else { theme::LINE }, theme::MUTED)
                };
                canvas.round(area, area.bottom as f32 / 2.0, track);
                ring(area.bottom as f32 / 2.0);
                let radius = (area.bottom - area.top) as f32 / 2.0 - 3.0 * s;
                let x = if launch {
                    area.right as f32 - 3.0 * s - radius
                } else {
                    area.left as f32 + 3.0 * s + radius
                };
                canvas.circle(x, area.bottom as f32 / 2.0, radius, knob);
            }
            _ => {
                canvas.fill(area, theme::CANVAS);
                let primary = id == IDOK;
                let (fill, colour, font) = match (primary, pressed, hot) {
                    (true, true, _) => (theme::ACCENT_DOWN, theme::ON_ACCENT, Font::Strong),
                    (true, false, true) => (theme::ACCENT_HOT, theme::ON_ACCENT, Font::Strong),
                    (true, false, false) => (theme::ACCENT, theme::ON_ACCENT, Font::Strong),
                    (false, true, _) => (theme::LINE, theme::INK, Font::Body),
                    (false, false, true) => (theme::RAISED, theme::INK, Font::Body),
                    (false, false, false) => (theme::HOVER, theme::INK, Font::Body),
                };
                canvas.round(area, 7.0 * s, fill);
                if focused {
                    canvas.outline(
                        area,
                        7.0 * s,
                        if primary { theme::INK } else { theme::ACCENT },
                        1.5 * s,
                    );
                }
                canvas.text(
                    &text,
                    area,
                    font,
                    colour,
                    DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                );
            }
        }
    });
}

/// Opens the popup list for a choice under its field.
fn open_choice(window: HWND, id: i32) {
    if menu::recently_closed() {
        return;
    }
    let (labels, selected, base) = {
        let state = state();
        if id == MICROPHONE {
            (
                state.microphone_labels.clone(),
                state.microphone,
                PICK_MICROPHONE,
            )
        } else {
            (
                hotkey::KEYS
                    .iter()
                    .map(|key| key.label.to_string())
                    .collect(),
                state.hotkey,
                PICK_HOTKEY,
            )
        }
    };
    let entries = labels
        .into_iter()
        .enumerate()
        .map(|(index, label)| Entry::Item {
            id: base + index,
            label,
            enabled: true,
            checked: index == selected,
        })
        .collect();
    let mut field = RECT::default();
    unsafe { GetWindowRect(GetDlgItem(window, id), &mut field) };
    let width = (field.right - field.left).max(if id == MICROPHONE { px(300) } else { 0 });
    menu::show(window, None, entries, width, Anchor::Below(field));
}

/// Acts on a click or Enter on control `id`.
fn command(window: HWND, id: i32) {
    match id {
        IDOK => match save(window) {
            Ok(()) => unsafe {
                DestroyWindow(window);
            },
            Err(message) => set_text(window, STATUS, &message),
        },
        IDCANCEL => unsafe {
            DestroyWindow(window);
        },
        TEST => test(window),
        MICROPHONE | HOTKEY => open_choice(window, id),
        LAUNCH => {
            {
                let mut state = state();
                state.launch = !state.launch;
            }
            unsafe { InvalidateRect(GetDlgItem(window, LAUNCH), null(), 0) };
        }
        _ if (NAV..NAV + PAGES.len() as i32).contains(&id) => {
            show_page(window, (id - NAV) as usize);
        }
        _ => {}
    }
}

/// Applies a choice from the popup list.
fn pick(window: HWND, id: usize) {
    if id >= PICK_HOTKEY {
        let index = id - PICK_HOTKEY;
        if let Some(key) = hotkey::KEYS.get(index) {
            state().hotkey = index;
            set_text(window, HOTKEY, key.label);
        }
    } else {
        let index = id - PICK_MICROPHONE;
        let label = {
            let mut state = state();
            let label = state.microphone_labels.get(index).cloned();
            if label.is_some() {
                state.microphone = index;
            }
            label
        };
        if let Some(label) = label {
            set_text(window, MICROPHONE, &label);
        }
    }
    let field = if id >= PICK_HOTKEY {
        HOTKEY
    } else {
        MICROPHONE
    };
    unsafe {
        let control = GetDlgItem(window, field);
        SetFocus(control);
        InvalidateRect(control, null(), 0);
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            paint(window);
            0
        }
        WM_ERASEBKGND => 1,
        WM_DRAWITEM => {
            draw_item(unsafe { &*(lparam as *const DRAWITEMSTRUCT) });
            1
        }
        WM_CTLCOLOREDIT => {
            let dc = wparam as HDC;
            unsafe {
                SetTextColor(dc, theme::colorref(theme::INK));
                SetBkColor(dc, theme::colorref(theme::FIELD));
            }
            theme::brush(theme::FIELD) as LRESULT
        }
        WM_CTLCOLORSTATIC => {
            let dc = wparam as HDC;
            let id = unsafe { GetDlgCtrlID(lparam as HWND) };
            let (background, colour) = match id {
                STATUS => (theme::CANVAS, theme::MUTED),
                VERSION | FOLDER => (theme::SURFACE, theme::MUTED),
                _ => (theme::SURFACE, theme::INK),
            };
            unsafe {
                SetTextColor(dc, theme::colorref(colour));
                SetBkColor(dc, theme::colorref(background));
            }
            theme::brush(background) as LRESULT
        }
        WM_COMMAND => {
            let id = wparam & 0xFFFF;
            let code = ((wparam >> 16) & 0xFFFF) as u32;
            match id as i32 {
                ENDPOINT | TOKEN | NAME => {
                    if code == EN_SETFOCUS || code == EN_KILLFOCUS {
                        unsafe { InvalidateRect(window, null(), 0) };
                    }
                }
                _ if code != BN_CLICKED && code != BN_DOUBLECLICKED => {}
                _ if id >= PICK_MICROPHONE => pick(window, id),
                // Enter sends IDOK. On a focused push button it should press that button.
                IDOK => {
                    let focus = unsafe { GetDlgCtrlID(GetFocus()) };
                    let pushes = [IDCANCEL, TEST, MICROPHONE, HOTKEY];
                    if pushes.contains(&focus) || (NAV..NAV + PAGES.len() as i32).contains(&focus) {
                        command(window, focus);
                    } else {
                        command(window, IDOK);
                    }
                }
                id => command(window, id),
            }
            0
        }
        WM_TEST_DONE => {
            if let Some(result) = TEST_RESULT.lock().ok().and_then(|mut result| result.take()) {
                set_text(window, STATUS, &result);
            }
            0
        }
        WM_DESTROY => {
            WINDOW.store(null_mut(), Ordering::SeqCst);
            HOT.store(null_mut(), Ordering::SeqCst);
            0
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn save(window: HWND) -> Result<(), String> {
    let endpoint = config::normalize_endpoint(&text(window, ENDPOINT))?;
    let token = config::validate_token(&text(window, TOKEN))?;
    let name = config::validate_device_name(&text(window, NAME))?;
    let (microphone, hotkey, launch) = {
        let state = state();
        let microphone = match state.microphone {
            0 => None,
            index => state.microphones.get(index - 1).cloned(),
        };
        let hotkey = hotkey::KEYS
            .get(state.hotkey)
            .unwrap_or(&hotkey::KEYS[0])
            .id;
        (microphone, hotkey, state.launch)
    };

    credentials::write(&endpoint, &token)?;
    let app = app::shared();
    let mut settings = app.settings();
    settings.config.endpoint = endpoint;
    settings.config.device_name = name;
    settings.config.microphone = microphone;
    settings.config.hotkey = hotkey.to_string();
    settings.token = token;
    settings
        .config
        .save()
        .map_err(|error| format!("Could not save settings: {error}"))?;
    if launch != startup::is_enabled() {
        startup::set(launch)?;
    }
    log!("settings: saved");
    app.apply_settings(settings);
    Ok(())
}

/// Checks the address and token as typed, without saving them.
fn test(window: HWND) {
    let endpoint = match config::normalize_endpoint(&text(window, ENDPOINT)) {
        Ok(endpoint) => endpoint,
        Err(message) => return set_text(window, STATUS, &message),
    };
    let token = match config::validate_token(&text(window, TOKEN)) {
        Ok(token) => token,
        Err(message) => return set_text(window, STATUS, &message),
    };
    set_text(window, STATUS, "Checking…");
    let target = window as usize;
    thread::spawn(move || {
        let result = match app::probe(&endpoint, &token) {
            Ok(message) => format!("Connected, and the token works. {message}"),
            Err(message) => message,
        };
        if let Ok(mut slot) = TEST_RESULT.lock() {
            *slot = Some(result);
        }
        unsafe { PostMessageW(target as HWND, WM_TEST_DONE, 0, 0) };
    });
}

fn window_text(control: HWND) -> String {
    unsafe {
        let length = GetWindowTextLengthW(control).max(0) as usize;
        let mut buffer = vec![0u16; length + 1];
        GetWindowTextW(control, buffer.as_mut_ptr(), buffer.len() as i32);
        from_wide(&buffer)
    }
}

fn text(window: HWND, id: i32) -> String {
    window_text(unsafe { GetDlgItem(window, id) })
}

fn set_text(window: HWND, id: i32, value: &str) {
    let value = wide(value);
    unsafe { SetWindowTextW(GetDlgItem(window, id), value.as_ptr()) };
}
