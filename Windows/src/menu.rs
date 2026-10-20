//! A themed popup list: the tray menu and the settings dropdowns. Choosing an
//! item posts `WM_COMMAND` with its id to the owner, like `TrackPopupMenu`.

use crate::theme::{self, Font, px};
use crate::win::wide;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, GetMonitorInfoW,
    InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, MonitorFromWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_SPACE,
    VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub enum Entry {
    Item {
        id: usize,
        label: String,
        enabled: bool,
        checked: bool,
    },
    Separator,
}

/// The tray menu's top: the app name and state, and a line about what to do next.
pub struct Header {
    pub state: String,
    pub dot: u32,
    pub message: String,
    pub ready: bool,
}

pub enum Anchor {
    /// Next to the mouse pointer, as for a tray click.
    Cursor,
    /// Under (or over, near the screen edge) a control, in screen coordinates.
    Below(RECT),
}

struct Menu {
    owner: usize,
    header: Option<Header>,
    entries: Vec<Entry>,
    hot: Option<usize>,
    width: i32,
    above: bool,
    /// The window's height, and how far the list is scrolled when it is taller.
    view: i32,
    scroll: i32,
}

struct Layout {
    height: i32,
    top: RECT,
    message: RECT,
    rows: Vec<RECT>,
}

static WINDOW: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static MENU: Mutex<Option<Menu>> = Mutex::new(None);
static CLOSED: Mutex<Option<Instant>> = Mutex::new(None);

const ROW: i32 = 32;
const SEPARATOR: i32 = 9;

fn menu() -> MutexGuard<'static, Option<Menu>> {
    MENU.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Opens the popup, replacing any open one. `width` is in device pixels.
pub fn show(owner: HWND, header: Option<Header>, entries: Vec<Entry>, width: i32, anchor: Anchor) {
    close();
    let hot = entries.iter().position(|entry| {
        matches!(
            entry,
            Entry::Item {
                checked: true,
                enabled: true,
                ..
            }
        )
    });
    let mut state = Menu {
        owner: owner as usize,
        header,
        entries,
        hot,
        width,
        above: false,
        view: 0,
        scroll: 0,
    };
    let (x, y, above, height) = place(&anchor, width, layout(&state).height);
    state.above = above;
    state.view = height;
    reveal(&mut state);
    *menu() = Some(state);

    let class_name = wide("InlayMenu");
    unsafe {
        let instance = GetModuleHandleW(null());
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            let class = WNDCLASSW {
                style: CS_DROPSHADOW,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class_name.as_ptr(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                ..std::mem::zeroed()
            };
            RegisterClassW(&class);
        });
        let window = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name.as_ptr(),
            null(),
            WS_POPUP,
            x,
            y,
            width,
            height,
            owner,
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            *menu() = None;
            return;
        }
        WINDOW.store(window, Ordering::SeqCst);
        theme::frame(window, None, true);
        ShowWindow(window, SW_SHOW);
        SetForegroundWindow(window);
        SetFocus(window);
    }
}

pub fn close() {
    let window = WINDOW.load(Ordering::SeqCst);
    if !window.is_null() {
        unsafe { DestroyWindow(window) };
    }
}

/// True just after the popup closed. A click on the tray icon closes the menu
/// before the click arrives, and should not open it again.
pub fn recently_closed() -> bool {
    CLOSED
        .lock()
        .ok()
        .and_then(|closed| *closed)
        .is_some_and(|closed| closed.elapsed() < Duration::from_millis(300))
}

/// Replaces the header of an open menu owned by `owner`.
pub fn set_header(owner: HWND, header: Header) {
    let window = WINDOW.load(Ordering::SeqCst);
    if window.is_null() {
        return;
    }
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    unsafe {
        GetMonitorInfoW(
            MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST),
            &mut info,
        )
    };
    let work = info.rcWork;
    let (height, above) = {
        let mut guard = menu();
        let Some(state) = guard.as_mut().filter(|state| state.owner == owner as usize) else {
            return;
        };
        state.header = Some(header);
        state.view = layout(state).height.min(work.bottom - work.top);
        scroll(state, 0);
        (state.view, state.above)
    };
    unsafe {
        let mut frame = std::mem::zeroed();
        GetWindowRect(window, &mut frame);
        let top = if above {
            frame.bottom - height
        } else {
            frame.top
        }
        .clamp(work.top, (work.bottom - height).max(work.top));
        SetWindowPos(
            window,
            null_mut(),
            frame.left,
            top,
            frame.right - frame.left,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        InvalidateRect(window, null(), 0);
    }
}

fn padding(state: &Menu) -> i32 {
    if state.header.is_some() {
        px(12)
    } else {
        px(6)
    }
}

fn layout(state: &Menu) -> Layout {
    let pad = padding(state);
    let (left, right) = (pad, state.width - pad);
    let mut y = pad - state.scroll;
    let mut top = RECT::default();
    let mut message = RECT::default();
    if let Some(header) = &state.header {
        top = RECT {
            left,
            top: y,
            right,
            bottom: y + px(26),
        };
        y = top.bottom + px(8);
        let text_width = right - left - px(14 + 20 + 10 + 14);
        let text = theme::text_height(&header.message, Font::Strong, text_width, DT_WORDBREAK);
        let height = (text + px(24)).max(px(46));
        message = RECT {
            left,
            top: y,
            right,
            bottom: y + height,
        };
        y = message.bottom + px(6);
    }
    let rows = state
        .entries
        .iter()
        .map(|entry| {
            let height = match entry {
                Entry::Item { .. } => px(ROW),
                Entry::Separator => px(SEPARATOR),
            };
            let row = RECT {
                left,
                top: y,
                right,
                bottom: y + height,
            };
            y += height;
            row
        })
        .collect();
    Layout {
        height: y + pad + state.scroll,
        top,
        message,
        rows,
    }
}

/// Where the popup goes, kept inside the work area of the anchor's monitor.
/// Returns the position, whether it opened upwards, and the height, cut to
/// fit the work area.
fn place(anchor: &Anchor, width: i32, height: i32) -> (i32, i32, bool, i32) {
    let gap = px(6);
    let (point, below) = match anchor {
        Anchor::Cursor => {
            let mut point = POINT { x: 0, y: 0 };
            unsafe { GetCursorPos(&mut point) };
            (point, None)
        }
        Anchor::Below(field) => (
            POINT {
                x: field.right,
                y: field.bottom,
            },
            Some(*field),
        ),
    };
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    unsafe { GetMonitorInfoW(MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST), &mut info) };
    let work = info.rcWork;
    let height = height.min(work.bottom - work.top);
    let (mut x, mut y, above);
    match below {
        None => {
            // Open up and to the left of the pointer, as the taskbar is usually at the bottom right.
            x = point.x - width;
            above = point.y - height - gap >= work.top;
            y = if above {
                point.y - height - gap
            } else {
                point.y + gap
            };
        }
        Some(field) => {
            x = field.right - width;
            above = field.bottom + gap + height > work.bottom;
            y = if above {
                field.top - gap - height
            } else {
                field.bottom + gap
            };
        }
    }
    x = x.clamp(work.left, (work.right - width).max(work.left));
    y = y.clamp(work.top, (work.bottom - height).max(work.top));
    (x, y, above, height)
}

/// Scrolls by `by` pixels, within the list.
fn scroll(state: &mut Menu, by: i32) {
    let most = (layout(state).height - state.view).max(0);
    state.scroll = (state.scroll + by).clamp(0, most);
}

/// Scrolls the highlighted item into view.
fn reveal(state: &mut Menu) {
    let Some(hot) = state.hot else { return };
    let row = layout(state).rows[hot];
    let pad = padding(state);
    if row.top < pad {
        scroll(state, row.top - pad);
    } else if row.bottom > state.view - pad {
        scroll(state, row.bottom - (state.view - pad));
    }
}

fn paint(window: HWND) {
    let guard = menu();
    let Some(state) = guard.as_ref() else { return };
    let layout = layout(state);
    theme::paint(window, |canvas, client| {
        canvas.fill(client, theme::CANVAS);
        if let Some(header) = &state.header {
            let top = layout.top;
            let mark = px(18);
            canvas.mark(
                top.left + px(2),
                top.top + (px(26) - mark) / 2,
                mark,
                theme::ACCENT,
            );
            canvas.text(
                "Inlay",
                RECT {
                    left: top.left + px(28),
                    ..top
                },
                Font::Strong,
                theme::INK,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            );
            let state_width = canvas.text_width(&header.state, Font::Small);
            canvas.text(
                &header.state,
                RECT {
                    right: top.right - px(2),
                    ..top
                },
                Font::Small,
                theme::MUTED,
                DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
            );
            canvas.circle(
                (top.right - px(2) - state_width - px(8)) as f32,
                (top.top + top.bottom) as f32 / 2.0,
                3.5 * theme::scale(),
                header.dot,
            );

            let area = layout.message;
            canvas.round(area, 9.0 * theme::scale(), theme::TINT);
            canvas.outline(area, 9.0 * theme::scale(), 0x2E4556, theme::scale());
            let colour = if header.ready {
                theme::ACCENT
            } else {
                theme::INK
            };
            let glyph = px(20);
            canvas.microphone(
                area.left + px(14),
                (area.top + area.bottom - glyph) / 2,
                glyph,
                if header.ready {
                    theme::ACCENT
                } else {
                    header.dot
                },
            );
            let text_left = area.left + px(14 + 20 + 10);
            let text_width = area.right - px(14) - text_left;
            let text_height =
                theme::text_height(&header.message, Font::Strong, text_width, DT_WORDBREAK);
            let text_top = (area.top + area.bottom - text_height) / 2;
            canvas.text(
                &header.message,
                RECT {
                    left: text_left,
                    top: text_top,
                    right: area.right - px(14),
                    bottom: text_top + text_height,
                },
                Font::Strong,
                colour,
                DT_LEFT | DT_WORDBREAK,
            );
        }
        for (index, (entry, row)) in state.entries.iter().zip(&layout.rows).enumerate() {
            match entry {
                Entry::Separator => canvas.fill(
                    RECT {
                        left: row.left + px(6),
                        right: row.right - px(6),
                        top: (row.top + row.bottom) / 2,
                        bottom: (row.top + row.bottom) / 2 + 1,
                    },
                    theme::LINE,
                ),
                Entry::Item {
                    label,
                    enabled,
                    checked,
                    ..
                } => {
                    if state.hot == Some(index) {
                        canvas.round(*row, 6.0 * theme::scale(), theme::HOVER);
                    }
                    let colour = match (enabled, checked) {
                        (false, _) => theme::FAINT,
                        (true, true) => theme::ACCENT,
                        (true, false) => theme::INK,
                    };
                    canvas.text(
                        label,
                        RECT {
                            left: row.left + px(10),
                            right: row.right - px(if *checked { 30 } else { 10 }),
                            ..*row
                        },
                        Font::Body,
                        colour,
                        DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                    );
                    if *checked {
                        canvas.check(
                            (row.right - px(16)) as f32,
                            (row.top + row.bottom) as f32 / 2.0,
                            10.0 * theme::scale(),
                            theme::ACCENT,
                        );
                    }
                }
            }
        }
    });
}

/// The enabled item under `y`, if any.
fn hit(state: &Menu, y: i32) -> Option<usize> {
    let layout = layout(state);
    state
        .entries
        .iter()
        .zip(&layout.rows)
        .position(|(entry, row)| {
            matches!(entry, Entry::Item { enabled: true, .. }) && y >= row.top && y < row.bottom
        })
}

/// Moves the highlight to the next enabled item in `step`'s direction.
fn step(state: &mut Menu, step: isize) {
    let count = state.entries.len() as isize;
    let mut index = state
        .hot
        .map_or(if step > 0 { -1 } else { count }, |hot| hot as isize);
    for _ in 0..count {
        index = (index + step).rem_euclid(count);
        if matches!(
            state.entries[index as usize],
            Entry::Item { enabled: true, .. }
        ) {
            state.hot = Some(index as usize);
            return;
        }
    }
}

/// Closes the popup and sends the owner the highlighted item's id.
fn choose(window: HWND) {
    let chosen = menu().as_ref().and_then(|state| {
        let index = state.hot?;
        match &state.entries[index] {
            Entry::Item {
                id, enabled: true, ..
            } => Some((state.owner, *id)),
            _ => None,
        }
    });
    unsafe { DestroyWindow(window) };
    if let Some((owner, id)) = chosen {
        unsafe { PostMessageW(owner as HWND, WM_COMMAND, id, 0) };
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
        WM_MOUSEMOVE => {
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let changed = {
                let mut guard = menu();
                let Some(state) = guard.as_mut() else {
                    return 0;
                };
                let hot = hit(state, y);
                let changed = hot != state.hot;
                state.hot = hot;
                changed
            };
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: window,
                dwHoverTime: 0,
            };
            unsafe {
                TrackMouseEvent(&mut track);
                if changed {
                    InvalidateRect(window, null(), 0);
                }
            }
            0
        }
        WM_MOUSELEAVE => {
            if let Some(state) = menu().as_mut() {
                state.hot = None;
            }
            unsafe { InvalidateRect(window, null(), 0) };
            0
        }
        WM_MOUSEWHEEL => {
            let notches = ((wparam >> 16) & 0xFFFF) as i16 as i32;
            if let Some(state) = menu().as_mut() {
                scroll(state, -notches * px(ROW) / WHEEL_DELTA as i32 * 3);
                state.hot = None;
            }
            unsafe { InvalidateRect(window, null(), 0) };
            0
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let hit = menu().as_mut().and_then(|state| {
                state.hot = hit(state, y);
                state.hot
            });
            if hit.is_some() {
                choose(window);
            }
            0
        }
        WM_KEYDOWN => {
            match wparam as u16 {
                VK_DOWN | VK_UP => {
                    if let Some(state) = menu().as_mut() {
                        step(state, if wparam as u16 == VK_DOWN { 1 } else { -1 });
                        reveal(state);
                    }
                    unsafe { InvalidateRect(window, null(), 0) };
                }
                VK_RETURN | VK_SPACE => choose(window),
                VK_ESCAPE => unsafe {
                    DestroyWindow(window);
                },
                _ => {}
            }
            0
        }
        WM_ACTIVATE => {
            // Clicking anywhere else closes the menu. Post rather than destroy
            // here, because this also arrives while the window is being destroyed.
            if (wparam & 0xFFFF) as u32 == WA_INACTIVE {
                unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
            }
            0
        }
        WM_CLOSE => {
            unsafe { DestroyWindow(window) };
            0
        }
        WM_DESTROY => {
            let _ = WINDOW.compare_exchange(window, null_mut(), Ordering::SeqCst, Ordering::SeqCst);
            *menu() = None;
            if let Ok(mut closed) = CLOSED.lock() {
                *closed = Some(Instant::now());
            }
            0
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}
