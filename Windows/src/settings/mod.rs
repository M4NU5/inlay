//! The main window, after the Mac app's: a sidebar of pages beside the page
//! itself, which scrolls. Pages are laid out afresh from the app's state
//! whenever it changes (see `layout.rs`). Text boxes are borderless Win32
//! edits; every other control is an owner-drawn button, and choices open the
//! shared popup list. Changes apply as they are made, except the connection,
//! which applies on Connect.

mod dictation;
mod history;
mod layout;
mod microphone;
mod this_pc;

use self::layout::{Builder, Control, Icon, Layout, Look};
use crate::app::{self, Dictation, Server, Settings};
use crate::controller::Event;
use crate::icons::Look as TrayLook;
use crate::menu::{self, Anchor, Entry};
use crate::theme::{self, Font, px, rect};
use crate::win::{from_wide, wide};
use crate::{audio, config, credentials, hotkey, startup, tray};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, HDC, InvalidateRect, ScreenToClient,
    SetBkColor, SetTextColor,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_SETMARGINS, ODS_FOCUS, ODS_NOFOCUSRECT, ODS_SELECTED, WM_MOUSELEAVE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, IsWindowEnabled, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

static WINDOW: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
/// The button class's own window procedure, wrapped to track hover.
static BUTTON_PROC: AtomicUsize = AtomicUsize::new(0);
static HOT: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Dictation,
    History,
    Microphone,
    ThisPc,
}

/// The sidebar, in the Mac app's order.
const PAGES: [Page; 4] = [
    Page::Dictation,
    Page::History,
    Page::Microphone,
    Page::ThisPc,
];

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Dictation => "Dictation",
            Page::History => "History",
            Page::Microphone => "Microphone",
            Page::ThisPc => "This PC",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Page::Dictation => Icon::Waveform,
            Page::History => Icon::Clock,
            Page::Microphone => Icon::Microphone,
            Page::ThisPc => Icon::Laptop,
        }
    }

    fn index(self) -> usize {
        PAGES.iter().position(|page| *page == self).unwrap_or(0)
    }

    fn build(self, page: &mut Builder, context: &Context) {
        match self {
            Page::Dictation => dictation::build(page, context),
            Page::History => history::build(page, context),
            Page::Microphone => microphone::build(page, context),
            Page::ThisPc => this_pc::build(page, context),
        }
    }
}

/// What a page is built from: the app's state and the window's own.
pub struct Context {
    pub settings: Settings,
    pub server: Server,
    pub recording: bool,
    pub testing: bool,
    pub processing: bool,
    /// A test take is recording or transcribing.
    pub test_busy: bool,
    pub last: Option<Dictation>,
    pub error: Option<String>,
    /// The latest test's transcript, or why it stopped without one.
    pub test: Option<Result<Dictation, String>>,
    pub microphones: Vec<String>,
    pub default_microphone: Option<String>,
    pub launch: bool,
    pub message: Option<Message>,
}

/// The result of the last change made on the page, when it needs saying.
#[derive(Clone)]
pub struct Message {
    pub text: String,
    pub error: bool,
}

struct State {
    page: Page,
    /// How far the page is scrolled, in device pixels.
    scroll: i32,
    layout: Layout,
    microphones: Vec<String>,
    default_microphone: Option<String>,
    /// Behind the microphone popup's entries: `None` is the system default.
    microphone_choices: Vec<Option<String>>,
    launch: bool,
    message: Option<Message>,
}

static STATE: Mutex<State> = Mutex::new(State {
    page: Page::Dictation,
    scroll: 0,
    layout: Layout::EMPTY,
    microphones: Vec::new(),
    default_microphone: None,
    microphone_choices: Vec::new(),
    launch: false,
    message: None,
});

// Controls. A control can appear on more than one page, such as Test
// microphone; each page's layout says where, and hides the rest.
const ENDPOINT: i32 = 101;
const TOKEN: i32 = 102;
const NAME: i32 = 103;
const CONNECT: i32 = 104;
const HOTKEY: i32 = 105;
const LAUNCH: i32 = 106;
const MICROPHONE: i32 = 107;
const CHECK: i32 = 108;
const COPY: i32 = 109;
const TEST: i32 = 110;
const GEAR: i32 = 111;
const HISTORY: i32 = 140;
const DEVICE: i32 = 141;
const SOURCE: i32 = 142;
const REFRESH: i32 = 143;
const LOAD: i32 = 144;
const RECORD_COPY: i32 = 145;
const DELETE: i32 = 146;
const OPEN_AUDIO: i32 = 147;
const OPEN_ORIGINAL: i32 = 148;
/// History's rows, which are painted, not controls: `ROW + index`.
const ROW: i32 = 6000;
/// Sidebar buttons: `NAV + index`.
const NAV: i32 = 120;
const EDITS: [i32; 3] = [ENDPOINT, TOKEN, NAME];
const BUTTONS: [i32; 17] = [
    CHECK,
    COPY,
    TEST,
    HISTORY,
    GEAR,
    DEVICE,
    SOURCE,
    REFRESH,
    RECORD_COPY,
    DELETE,
    OPEN_AUDIO,
    OPEN_ORIGINAL,
    LOAD,
    MICROPHONE,
    CONNECT,
    HOTKEY,
    LAUNCH,
];
/// Popup list choices: `PICK_MICROPHONE + index` and `PICK_HOTKEY + index`.
const PICK_MICROPHONE: usize = 1000;
const PICK_HOTKEY: usize = 2000;
const PICK_DEVICE: usize = 3000;
const PICK_SOURCE: usize = 4000;
/// A History request finished; lay the page out again.
const WM_HISTORY: u32 = WM_APP + 30;
/// `EM_SETCUEBANNER`: grey hint text in an empty text box.
const EM_SETCUEBANNER: u32 = 0x1501;
/// `WM_DEVICECHANGE` with `DBT_DEVNODES_CHANGED`: a device came or went.
const DBT_DEVNODES_CHANGED: usize = 7;

// Layout in logical pixels.
const WIDTH: i32 = 880;
const HEIGHT: i32 = 680;
const MIN_WIDTH: i32 = 700;
const MIN_HEIGHT: i32 = 480;
const SIDE: i32 = 216;
const PADDING: i32 = 36;
const TOP: i32 = 30;
const MAX_WIDTH: i32 = 780;

fn state() -> MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The open window, or null. The message loop routes dialog keys to it.
pub fn window() -> HWND {
    WINDOW.load(Ordering::SeqCst)
}

pub fn close() {
    let window = window();
    if !window.is_null() {
        unsafe { DestroyWindow(window) };
    }
}

/// Lays the page out again from the app's current state.
pub fn refresh() {
    let window = window();
    if !window.is_null() {
        if state().page == Page::History {
            history::catch_up(window, finished());
        }
        relayout(window);
    }
}

fn finished() -> u64 {
    app::shared().status(|status| status.finished)
}

/// Opens the window on `page`, or brings it forward and shows that page.
pub fn open(page: Page) {
    let existing = window();
    if !existing.is_null() {
        unsafe {
            ShowWindow(existing, SW_SHOWNORMAL);
            SetForegroundWindow(existing);
        }
        show_page(existing, page);
        return;
    }
    let class_name = wide("InlayWindow");
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
        let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
        let mut frame = rect(0, 0, WIDTH, HEIGHT);
        AdjustWindowRectEx(&mut frame, style, 0, 0);
        let (width, height) = (frame.right - frame.left, frame.bottom - frame.top);
        let mut work = RECT::default();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut work).cast(), 0);
        let width = width.min(work.right - work.left);
        let height = height.min(work.bottom - work.top);
        let x = work.left + (work.right - work.left - width) / 2;
        let y = work.top + (work.bottom - work.top - height) / 2;
        let title = wide("Inlay");
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
        let icon = tray::icon(TrayLook::Idle) as LPARAM;
        SendMessageW(window, WM_SETICON, ICON_SMALL as WPARAM, icon);
        SendMessageW(window, WM_SETICON, ICON_BIG as WPARAM, icon);

        for (index, page) in PAGES.iter().enumerate() {
            let button = button(window, NAV + index as i32, page.title());
            let area = nav_area(index);
            MoveWindow(
                button,
                area.left,
                area.top,
                area.right - area.left,
                area.bottom - area.top,
                0,
            );
            ShowWindow(button, SW_SHOW);
        }
        for id in EDITS {
            let password = if id == TOKEN { ES_PASSWORD as u32 } else { 0 };
            let style = WS_TABSTOP | ES_AUTOHSCROLL as u32 | ES_RIGHT as u32 | password;
            let edit = control(window, "EDIT", "", style, id);
            SendMessageW(edit, EM_SETMARGINS, 3, 0);
        }
        let cue = |id: i32, text: &str| {
            let text = wide(text);
            SendMessageW(
                GetDlgItem(window, id),
                EM_SETCUEBANNER,
                1,
                text.as_ptr() as LPARAM,
            );
        };
        cue(ENDPOINT, config::DEFAULT_ENDPOINT);
        cue(TOKEN, "None");
        for id in BUTTONS {
            button(window, id, "");
        }

        fill(window);
        ShowWindow(window, SW_SHOWNORMAL);
        SetForegroundWindow(window);
        show_page(window, page);
    }
}

fn control(parent: HWND, class: &str, text: &str, style: u32, id: i32) -> HWND {
    let (class, text) = (wide(class), wide(text));
    unsafe {
        let control = CreateWindowExW(
            0,
            class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | style,
            0,
            0,
            0,
            0,
            parent,
            id as usize as HMENU,
            GetModuleHandleW(null()),
            null(),
        );
        SendMessageW(control, WM_SETFONT, theme::font(Font::Body) as WPARAM, 1);
        control
    }
}

/// An owner-drawn button whose hover state this module tracks. `BS_NOTIFY`
/// reports focus, so a button tabbed to below the fold scrolls into view.
fn button(parent: HWND, id: i32, text: &str) -> HWND {
    let style = WS_TABSTOP | BS_OWNERDRAW as u32 | BS_NOTIFY as u32;
    let button = control(parent, "BUTTON", text, style, id);
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

/// Loads the saved connection into the text boxes, and this PC's state.
fn fill(window: HWND) {
    let settings = app::shared().settings();
    set_text(window, ENDPOINT, &settings.config.endpoint);
    set_text(window, TOKEN, &settings.token);
    set_text(window, NAME, &settings.config.device_name);
    state().launch = startup::is_enabled();
    refresh_microphones();
}

fn refresh_microphones() {
    let microphones = audio::input_names();
    let default_microphone = audio::default_input_name();
    let mut state = state();
    state.microphones = microphones;
    state.default_microphone = default_microphone;
}

fn context() -> Context {
    let app = app::shared();
    let settings = app.settings();
    let mut context = app.status(|status| Context {
        settings,
        server: status.server.clone(),
        recording: status.recording,
        testing: status.testing,
        processing: status.processing > 0,
        test_busy: status.tests > 0,
        last: status.last.clone(),
        error: status.error.clone(),
        test: status.test.clone(),
        microphones: Vec::new(),
        default_microphone: None,
        launch: false,
        message: None,
    });
    let state = state();
    context.microphones = state.microphones.clone();
    context.default_microphone = state.default_microphone.clone();
    context.launch = state.launch;
    context.message = state.message.clone();
    context
}

/// A short description of the server's state and its dot colour.
fn server_line(server: &Server) -> (u32, &'static str) {
    match server {
        Server::Ready => (theme::GREEN, "Server online"),
        Server::Checking => (theme::ORANGE, "Connecting…"),
        Server::Unavailable(message) if message.starts_with("Cannot reach") => {
            (theme::RED, "Server offline")
        }
        Server::Unavailable(_) => (theme::RED, "Server unavailable"),
    }
}

/// The Test microphone button, which finishes a test while one is recording.
fn test_button(context: &Context) -> Control {
    if context.testing {
        Control::primary("Finish dictation", Icon::Stop)
    } else {
        Control::primary("Test microphone", Icon::Microphone)
            .enabled(!context.recording && context.server == Server::Ready)
    }
}

fn client(window: HWND) -> RECT {
    let mut client = RECT::default();
    unsafe { GetClientRect(window, &mut client) };
    client
}

/// Builds the current page from the app's state and puts its controls in place.
fn relayout(window: HWND) {
    let context = context();
    let page = state().page;
    let client = client(window);
    // The page keeps to MAX_WIDTH and sits centred in whatever room is left.
    let start = px(SIDE + PADDING);
    let room = client.right - px(PADDING) - start;
    let width = room.min(px(MAX_WIDTH)).max(px(200));
    let left = start + (room - width).max(0) / 2;
    let mut builder = Builder::new(left, left + width, px(TOP), client.bottom);
    page.build(&mut builder, &context);
    let layout = builder.finish();
    {
        let mut state = state();
        state.scroll = state
            .scroll
            .clamp(0, (layout.height - client.bottom).max(0));
        state.layout = layout;
    }
    place_controls(window);
}

/// Moves the page's controls to its layout and scroll, and hides the others.
/// Tab order follows the layout.
fn place_controls(window: HWND) {
    let (placed, scroll) = {
        let state = state();
        (state.layout.controls.clone(), state.scroll)
    };
    unsafe {
        let mut after = GetDlgItem(window, NAV + PAGES.len() as i32 - 1);
        for each in &placed {
            let control = GetDlgItem(window, each.id);
            let area = each.area;
            SetWindowPos(
                control,
                after,
                area.left,
                area.top - scroll,
                area.right - area.left,
                area.bottom - area.top,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            after = control;
            if let Some(label) = each.control.label()
                && window_text(control) != label
            {
                set_text(window, each.id, label);
            }
            EnableWindow(control, each.control.is_enabled() as i32);
            InvalidateRect(control, null(), 0);
        }
        for id in EDITS.into_iter().chain(BUTTONS) {
            if !placed.iter().any(|each| each.id == id) {
                ShowWindow(GetDlgItem(window, id), SW_HIDE);
            }
        }
        // Keep the keyboard somewhere visible, such as after a button it was
        // on is disabled. Only while this window is in front: SetFocus would
        // otherwise activate it and take the keyboard from the app being
        // dictated into.
        let focus = GetFocus();
        if GetForegroundWindow() == window
            && (focus.is_null() || IsWindowVisible(focus) == 0 || IsWindowEnabled(focus) == 0)
        {
            let page = state().page.index() as i32;
            SetFocus(GetDlgItem(window, NAV + page));
        }
        InvalidateRect(window, null(), 0);
    }
}

/// The scrolling pane under a point in the window, if any.
fn pane_at(point: POINT) -> Option<layout::Pane> {
    let state = state();
    let y = point.y + state.scroll;
    state.layout.panes.iter().copied().find(|pane| {
        (pane.area.left..pane.area.right).contains(&point.x)
            && (pane.area.top..pane.area.bottom).contains(&y)
    })
}

/// The painted area's id under a point in the window, if any.
fn hit_at(point: POINT) -> Option<i32> {
    let state = state();
    let y = point.y + state.scroll;
    state
        .layout
        .hits
        .iter()
        .find(|(area, _)| {
            (area.left..area.right).contains(&point.x) && (area.top..area.bottom).contains(&y)
        })
        .map(|(_, id)| *id)
}

/// Scrolls by `delta` device pixels, within the page.
fn scroll_by(window: HWND, delta: i32) {
    let view = client(window).bottom;
    {
        let mut state = state();
        let limit = (state.layout.height - view).max(0);
        let scroll = (state.scroll + delta).clamp(0, limit);
        if scroll == state.scroll {
            return;
        }
        state.scroll = scroll;
    }
    place_controls(window);
}

/// Scrolls just enough to show control `id`, as when it is tabbed to.
fn reveal(window: HWND, id: i32) {
    let view = client(window).bottom;
    let delta = {
        let state = state();
        let Some(placed) = state.layout.control(id) else {
            return;
        };
        let (top, bottom) = (
            placed.area.top - state.scroll,
            placed.area.bottom - state.scroll,
        );
        if top < px(12) {
            top - px(12)
        } else if bottom > view - px(12) {
            bottom - view + px(12)
        } else {
            0
        }
    };
    if delta != 0 {
        scroll_by(window, delta);
    }
}

fn show_page(window: HWND, page: Page) {
    {
        let mut state = state();
        state.page = page;
        state.scroll = 0;
        state.message = None;
    }
    if page == Page::Microphone {
        refresh_microphones();
    }
    if page == Page::History {
        history::opened(window, finished());
    }
    set_window_title(window, &format!("Inlay · {}", page.title()));
    unsafe {
        SetFocus(GetDlgItem(window, NAV + page.index() as i32));
        for index in 0..PAGES.len() {
            InvalidateRect(GetDlgItem(window, NAV + index as i32), null(), 0);
        }
    }
    relayout(window);
}

fn nav_area(index: usize) -> RECT {
    let top = 86 + index as i32 * 42;
    rect(14, top, SIDE - 14, top + 38)
}

fn paint(window: HWND) {
    let focus = unsafe { GetDlgCtrlID(GetFocus()) };
    let (dot, status) = server_line(&app::shared().status(|status| status.server.clone()));
    let state = state();
    theme::paint(window, |canvas, client| {
        let s = theme::scale();
        canvas.fill(client, theme::CANVAS);
        layout::paint(canvas, &state.layout.items, state.scroll, focus);

        // The scroll position, when the page is taller than the window.
        let (height, view) = (state.layout.height, client.bottom);
        if height > view {
            let thumb = (view * view / height).max(px(32));
            let top = state.scroll * (view - thumb) / (height - view);
            let bar = RECT {
                left: client.right - px(8),
                top: top + px(4),
                right: client.right - px(4),
                bottom: top + thumb - px(4),
            };
            canvas.round(bar, 2.0 * s, theme::RAISED);
        }

        let side = RECT {
            right: px(SIDE),
            ..client
        };
        canvas.fill(side, theme::SIDEBAR);
        canvas.fill(
            RECT {
                left: side.right - 1,
                ..side
            },
            theme::SIDEBAR_EDGE,
        );
        // The app's tile and wordmark, as at the top of the Mac sidebar.
        let tile = rect(22, 24, 58, 60);
        canvas.round(tile, 9.0 * s, 0xF4EFE6);
        canvas.mark(px(26), px(28), px(28), theme::CANVAS);
        canvas.text(
            "Inlay",
            rect(70, 20, SIDE - 12, 64),
            Font::Wordmark,
            theme::INK,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
        );
        let bottom = client.bottom;
        canvas.circle(px(30) as f32, (bottom - px(30)) as f32, 4.0 * s, dot);
        canvas.text(
            status,
            RECT {
                left: px(42),
                top: bottom - px(42),
                right: side.right - px(12),
                bottom: bottom - px(18),
            },
            Font::Body,
            theme::INK,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
        );
    });
}

fn draw_item(item: &DRAWITEMSTRUCT) {
    let id = item.CtlID as i32;
    let look = Look {
        hot: HOT.load(Ordering::SeqCst) == item.hwndItem,
        pressed: item.itemState & ODS_SELECTED != 0,
        focused: item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0,
    };
    let state = state();
    theme::buffered(item.hDC, item.rcItem, |canvas, area| {
        if (NAV..NAV + PAGES.len() as i32).contains(&id) {
            let page = PAGES[(id - NAV) as usize];
            draw_nav(canvas, area, page, page == state.page, look);
        } else if let Some(placed) = state.layout.control(id) {
            layout::draw(canvas, area, placed, look);
        } else {
            canvas.fill(area, theme::CANVAS);
        }
    });
}

fn draw_nav(canvas: &theme::Canvas, area: RECT, page: Page, selected: bool, look: Look) {
    let s = theme::scale();
    canvas.fill(area, theme::SIDEBAR);
    let (fill, colour, font) = if selected {
        (Some(theme::SIDEBAR_SELECTED), theme::INK, Font::Strong)
    } else if look.hot {
        (Some(theme::SIDEBAR_HOVER), theme::INK, Font::Body)
    } else {
        (None, theme::MUTED, Font::Body)
    };
    if let Some(fill) = fill {
        canvas.round(area, 8.0 * s, fill);
    }
    if look.focused {
        canvas.outline(area, 8.0 * s, theme::ACCENT, 1.5 * s);
    }
    let size = px(20);
    page.icon().draw(
        canvas,
        area.left + px(14),
        (area.top + area.bottom - size) / 2,
        size,
        colour,
    );
    canvas.text(
        page.title(),
        RECT {
            left: area.left + px(46),
            ..area
        },
        font,
        colour,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
    );
}

/// Opens the popup list for a choice under its control.
fn open_choice(window: HWND, id: i32) {
    if menu::recently_closed() {
        return;
    }
    let (labels, selected, base) = if id == MICROPHONE {
        refresh_microphones();
        let saved = app::shared().settings().config.microphone;
        let mut state = state();
        let mut choices: Vec<Option<String>> = vec![None];
        choices.extend(state.microphones.iter().cloned().map(Some));
        if let Some(saved) = &saved
            && !state.microphones.contains(saved)
        {
            choices.push(Some(saved.clone()));
        }
        let labels: Vec<String> = choices
            .iter()
            .map(|choice| match choice {
                None => "System default".to_string(),
                Some(name) if state.microphones.contains(name) => name.clone(),
                Some(name) => format!("{name} (disconnected)"),
            })
            .collect();
        let selected = choices.iter().position(|choice| *choice == saved);
        state.microphone_choices = choices;
        (labels, selected, PICK_MICROPHONE)
    } else if id == DEVICE || id == SOURCE {
        let (labels, selected) = history::choices(id);
        let base = if id == DEVICE {
            PICK_DEVICE
        } else {
            PICK_SOURCE
        };
        (labels, selected, base)
    } else {
        let current = app::shared().settings().config.hotkey;
        (
            hotkey::KEYS
                .iter()
                .map(|key| key.label.to_string())
                .collect(),
            hotkey::KEYS.iter().position(|key| key.id == current),
            PICK_HOTKEY,
        )
    };
    let entries = labels
        .into_iter()
        .enumerate()
        .map(|(index, label)| Entry::Item {
            id: base + index,
            label,
            enabled: true,
            checked: Some(index) == selected,
        })
        .collect();
    let mut field = RECT::default();
    unsafe { GetWindowRect(GetDlgItem(window, id), &mut field) };
    let width = (field.right - field.left).max(px(if id == MICROPHONE { 320 } else { 200 }));
    menu::show(window, None, entries, width, Anchor::Below(field));
}

/// Applies a choice from the popup list.
fn pick(window: HWND, id: usize) {
    if id >= PICK_DEVICE {
        if id >= PICK_SOURCE {
            history::choose_source(window, id - PICK_SOURCE);
        } else {
            history::choose_device(id - PICK_DEVICE);
        }
        let field = if id >= PICK_SOURCE { SOURCE } else { DEVICE };
        unsafe { SetFocus(GetDlgItem(window, field)) };
        return relayout(window);
    }
    let app = app::shared();
    let (result, field) = if id >= PICK_HOTKEY {
        let key = hotkey::KEYS.get(id - PICK_HOTKEY);
        let result = key.map_or(Ok(()), |key| {
            app.change_config(|config| config.hotkey = key.id.into())
        });
        (result, HOTKEY)
    } else {
        let choice = state()
            .microphone_choices
            .get(id - PICK_MICROPHONE)
            .cloned();
        let result = choice.map_or(Ok(()), |choice| {
            app.change_config(|config| config.microphone = choice)
        });
        (result, MICROPHONE)
    };
    if result.is_ok() {
        log!("settings: saved a change on this PC");
    }
    say(result.err().map(|text| Message { text, error: true }));
    unsafe { SetFocus(GetDlgItem(window, field)) };
    relayout(window);
}

fn say(message: Option<Message>) {
    state().message = message;
}

/// Saves and applies the server address, token and device name together.
fn connect(window: HWND) {
    let result = (|| {
        let endpoint = config::normalize_endpoint(&text(window, ENDPOINT))?;
        let token = config::validate_token(&text(window, TOKEN))?;
        let name = config::validate_device_name(&text(window, NAME))?;
        credentials::write(&endpoint, &token)?;
        let app = app::shared();
        let mut settings = app.settings();
        settings.config.endpoint = endpoint;
        settings.config.device_name = name;
        settings.token = token;
        settings
            .config
            .save()
            .map_err(|error| format!("Could not save settings: {error}"))?;
        log!("settings: saved the connection");
        app.apply_settings(settings);
        Ok(())
    })();
    match result {
        Ok(()) => {
            // Show the address as saved, without a trailing slash.
            let saved = app::shared().settings().config.endpoint;
            set_text(window, ENDPOINT, &saved);
            say(None);
        }
        Err(text) => say(Some(Message { text, error: true })),
    }
    relayout(window);
}

fn toggle_launch() {
    let enabled = !state().launch;
    match startup::set(enabled) {
        Ok(()) => {
            state().launch = enabled;
            say(None);
        }
        Err(text) => say(Some(Message { text, error: true })),
    }
}

/// Acts on a click or Enter on control `id`.
fn command(window: HWND, id: i32) {
    let app = app::shared();
    match id {
        CHECK => app.recheck(),
        COPY => app.copy_last(),
        TEST => {
            let _ = app.events.send(Event::Test);
        }
        GEAR => return show_page(window, Page::ThisPc),
        HISTORY => return show_page(window, Page::History),
        CONNECT => return connect(window),
        MICROPHONE | HOTKEY | DEVICE | SOURCE => return open_choice(window, id),
        REFRESH => history::load(window, false),
        LOAD => history::load(window, true),
        RECORD_COPY => history::copy(),
        DELETE => history::delete(window),
        OPEN_AUDIO | OPEN_ORIGINAL => history::open_audio(window, id == OPEN_ORIGINAL),
        _ if id >= ROW => history::select((id - ROW) as usize),
        LAUNCH => toggle_launch(),
        _ if (NAV..NAV + PAGES.len() as i32).contains(&id) => {
            return show_page(window, PAGES[(id - NAV) as usize]);
        }
        _ => return,
    }
    relayout(window);
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
        // Sizes during CreateWindowExW come before the controls exist.
        WM_SIZE if window == self::window() => {
            relayout(window);
            0
        }
        WM_GETMINMAXINFO => {
            let mut frame = rect(0, 0, MIN_WIDTH, MIN_HEIGHT);
            unsafe {
                AdjustWindowRectEx(&mut frame, WS_OVERLAPPEDWINDOW, 0, 0);
                let info = &mut *(lparam as *mut MINMAXINFO);
                info.ptMinTrackSize = POINT {
                    x: frame.right - frame.left,
                    y: frame.bottom - frame.top,
                };
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam >> 16) & 0xFFFF) as u16 as i16 as i32;
            let delta = -delta * px(48) / WHEEL_DELTA as i32;
            // The wheel's position is on the screen.
            let mut point = POINT {
                x: (lparam & 0xFFFF) as u16 as i16 as i32,
                y: ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32,
            };
            unsafe { ScreenToClient(window, &mut point) };
            match pane_at(point) {
                Some(pane) => {
                    if history::scroll(pane.id, delta, pane.limit) {
                        relayout(window);
                    }
                }
                None => scroll_by(window, delta),
            }
            0
        }
        WM_LBUTTONDOWN => {
            let point = POINT {
                x: (lparam & 0xFFFF) as u16 as i16 as i32,
                y: ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32,
            };
            if let Some(id) = hit_at(point) {
                command(window, id);
            }
            0
        }
        WM_HISTORY => {
            relayout(window);
            0
        }
        WM_DEVICECHANGE => {
            if wparam == DBT_DEVNODES_CHANGED && state().page == Page::Microphone {
                refresh_microphones();
                relayout(window);
            }
            1
        }
        WM_DRAWITEM => {
            draw_item(unsafe { &*(lparam as *const DRAWITEMSTRUCT) });
            1
        }
        WM_CTLCOLOREDIT => {
            let dc = wparam as HDC;
            let background = if lparam as HWND == unsafe { GetFocus() } {
                theme::FIELD
            } else {
                theme::SURFACE
            };
            unsafe {
                SetTextColor(dc, theme::colorref(theme::INK));
                SetBkColor(dc, theme::colorref(background));
            }
            theme::brush(background) as LRESULT
        }
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as i32;
            let code = ((wparam >> 16) & 0xFFFF) as u32;
            match code {
                EN_SETFOCUS | EN_KILLFOCUS => unsafe {
                    if code == EN_SETFOCUS {
                        reveal(window, id);
                    }
                    InvalidateRect(lparam as HWND, null(), 1);
                    InvalidateRect(window, null(), 0);
                },
                BN_SETFOCUS => reveal(window, id),
                BN_CLICKED | BN_DOUBLECLICKED => match id {
                    _ if id as usize >= PICK_MICROPHONE => pick(window, id as usize),
                    // Enter sends IDOK: press the focused button, or Connect from a text box.
                    IDOK => {
                        let focus = unsafe { GetDlgCtrlID(GetFocus()) };
                        if EDITS.contains(&focus) {
                            command(window, CONNECT);
                        } else {
                            command(window, focus);
                        }
                    }
                    id => command(window, id),
                },
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            WINDOW.store(null_mut(), Ordering::SeqCst);
            HOT.store(null_mut(), Ordering::SeqCst);
            state().layout = Layout::EMPTY;
            0
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
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

fn set_window_title(window: HWND, value: &str) {
    let value = wide(value);
    unsafe { SetWindowTextW(window, value.as_ptr()) };
}
