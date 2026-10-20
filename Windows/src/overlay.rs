//! The recording pill above the taskbar. A small orb breathes while the take
//! starts, stretches into a black pill with a live waveform while recording,
//! shrinks back to an orb with a spinner while transcribing. Once pasted the
//! spinner closes into a green ring and a tick draws itself inside it.
//! Each change morphs: the shape dips, then grows into the next stage's size
//! and colour while the old content fades out and the new fades in. It is a
//! click-through layered window that never takes focus.

use crate::app::{self, App};
use crate::theme::{self, Canvas, Font, px};
use crate::{audio, win::wide};
use std::f32::consts::{PI, TAU};
use std::ffi::c_void;
use std::mem::discriminant;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

static WINDOW: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
/// The pill's bottom centre in screen pixels, fixed while it is showing.
static ANCHOR: Mutex<(i32, i32)> = Mutex::new((0, 0));
/// The waveform's recent loudness, scrolling right to left.
static STREAM: Mutex<Stream> = Mutex::new(Stream {
    samples: [0.0; BARS + 1],
    next: 0.0,
});
/// The animation while the pill is on screen.
static MOTION: Mutex<Option<Motion>> = Mutex::new(None);

const FRAME_TIMER: usize = 1;
/// How long "Pasted" stays up.
const PASTED_FOR: Duration = Duration::from_millis(1400);
/// Seconds for one stage to morph into the next.
const MORPH: f32 = 0.35;
/// Starting to recording takes longer, so the orb visibly opens into the pill.
const MORPH_INTO_RECORDING: f32 = 0.5;
/// The spinner closing and the tick drawing in.
const FINISH: f32 = 0.7;
/// Seconds to grow in on appearing and shrink out on leaving.
const FADE: f32 = 0.2;
const PILL: u32 = 0x05080B;
const ORB: u32 = 0x0E161D;
const BORDER: u32 = 0x2E4556;
/// Bars in the waveform, one loudness sample each, as on the Mac.
const BARS: usize = 9;
/// Seconds between samples: a little slower than the Mac's twenty a second.
const SAMPLE: f32 = 0.055;

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Hidden,
    /// The hold was accepted; admission and the microphone are starting.
    Starting,
    Recording(Instant),
    Processing,
    Pasted,
}

/// The waveform's samples, oldest first. The last one is on its way in from
/// the right, due to land at `next`.
#[derive(Clone, Copy)]
struct Stream {
    samples: [f32; BARS + 1],
    next: f32,
}

impl Stream {
    /// Takes a sample for each step that has passed. Only live audio counts.
    fn advance(&mut self, now: f32, level: f32) {
        let steps = (((now - self.next) / SAMPLE).floor() + 1.0).max(0.0) as usize;
        for _ in 0..steps.min(BARS + 1) {
            self.samples.rotate_left(1);
            self.samples[BARS] = level;
        }
        self.next += steps as f32 * SAMPLE;
    }

    /// Silence, with the first sample due now.
    fn clear(&mut self, now: f32) {
        *self = Stream {
            samples: [0.0; BARS + 1],
            next: now,
        };
    }

    /// How far the newest sample has moved in, 0 to 1.
    fn slide(&self, now: f32) -> f32 {
        (1.0 - (self.next - now) / SAMPLE).clamp(0.0, 1.0)
    }
}

/// Where a morph started, and when.
struct Motion {
    stage: Stage,
    previous: Stage,
    since: f32,
    duration: f32,
    from: Look,
    shown_at: f32,
    leaving_at: Option<f32>,
}

/// A stage's shape. Colours are 0xAARRGGBB so they can be mixed.
#[derive(Clone, Copy)]
struct Look {
    width: f32,
    fill: u32,
    glow: u32,
    border: u32,
}

/// One frame of the animation.
struct Frame {
    stage: Stage,
    previous: Stage,
    width: f32,
    scale: f32,
    look: Look,
    /// How far through the morph, 0 to 1.
    t: f32,
    /// How visible the outgoing and incoming content are, 0 to 1.
    old: f32,
    new: f32,
    alpha: f32,
}

fn stage(app: &App) -> Stage {
    app.status(|status| {
        if status.recording {
            status
                .capturing_since
                .map_or(Stage::Starting, Stage::Recording)
        } else if status
            .pasted_at
            .is_some_and(|pasted| pasted.elapsed() < PASTED_FOR)
        {
            // Ahead of processing: the tick starts as the paste goes out,
            // while the take is still finishing.
            Stage::Pasted
        } else if status.processing > 0 {
            Stage::Processing
        } else {
            Stage::Hidden
        }
    })
}

/// Seconds since the process started, for animation.
fn clock() -> f32 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f32()
}

fn stream() -> MutexGuard<'static, Stream> {
    STREAM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn motion() -> MutexGuard<'static, Option<Motion>> {
    MOTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Shows, morphs or hides the pill for the current state. UI thread only.
pub fn update() {
    let stage = stage(app::shared());
    let window = ensure_window();
    if window.is_null() {
        return;
    }
    let now = clock();
    let mut guard = motion();
    let mut appearing = false;
    match (guard.as_mut(), stage) {
        (None, Stage::Hidden) => return,
        (None, _) => {
            *ANCHOR
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = anchor();
            stream().clear(now);
            *guard = Some(Motion {
                stage,
                previous: stage,
                since: now - MORPH,
                duration: MORPH,
                from: look(stage),
                shown_at: now,
                leaving_at: None,
            });
            appearing = true;
        }
        (Some(motion), Stage::Hidden) => match motion.leaving_at {
            None => motion.leaving_at = Some(now),
            Some(leaving) if now - leaving >= FADE => {
                *guard = None;
                unsafe {
                    KillTimer(window, FRAME_TIMER);
                    ShowWindow(window, SW_HIDE);
                }
                return;
            }
            Some(_) => {}
        },
        (Some(motion), _) => {
            // A new take while shrinking away: grow back in.
            if motion.leaving_at.take().is_some() {
                motion.shown_at = now;
                stream().clear(now);
            }
            if discriminant(&motion.stage) != discriminant(&stage) {
                if matches!(stage, Stage::Recording(_)) {
                    stream().clear(now);
                }
                let current = frame(motion, now);
                motion.from = Look {
                    width: current.width,
                    ..current.look
                };
                motion.duration = match (motion.stage, stage) {
                    (Stage::Starting, Stage::Recording(_)) => MORPH_INTO_RECORDING,
                    (Stage::Processing, Stage::Pasted) => FINISH,
                    _ => MORPH,
                };
                motion.previous = motion.stage;
                motion.since = now;
            }
            motion.stage = stage;
        }
    }
    let Some(frame) = guard.as_ref().map(|motion| frame(motion, now)) else {
        return;
    };
    drop(guard);
    render(window, &frame, now);
    if appearing {
        unsafe {
            ShowWindow(window, SW_SHOWNOACTIVATE);
            SetTimer(window, FRAME_TIMER, 16, None);
        }
    }
}

fn ensure_window() -> HWND {
    let existing = WINDOW.load(Ordering::SeqCst);
    if !existing.is_null() {
        return existing;
    }
    let class_name = wide("InlayPill");
    unsafe {
        let instance = GetModuleHandleW(null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..std::mem::zeroed()
        };
        RegisterClassW(&class);
        let window = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class_name.as_ptr(),
            wide("Inlay recording").as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        WINDOW.store(window, Ordering::SeqCst);
        window
    }
}

/// Bottom centre of the work area on the monitor the person is typing on.
fn anchor() -> (i32, i32) {
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    unsafe {
        let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
        GetMonitorInfoW(monitor, &mut info);
    }
    let work = info.rcWork;
    ((work.left + work.right) / 2, work.bottom - px(24))
}

/// A stage's resting shape. Only recording is wider than the orb.
fn look(stage: Stage) -> Look {
    let height = px(36) as f32;
    let glow = 0x26A7_D6F5;
    let border = 0xFF00_0000 | BORDER;
    match stage {
        Stage::Recording(_) => Look {
            width: (px(14 * 2 + 8 + 12 + BARS as i32 * 6 - 3 + 12)
                + theme::text_width("0:00", Font::Body)) as f32,
            fill: 0xFF00_0000 | PILL,
            glow: 0x30A7_D6F5,
            border,
        },
        Stage::Pasted => Look {
            width: height,
            fill: 0xFF00_0000 | ORB,
            glow: 0x2630_D158,
            border,
        },
        Stage::Hidden | Stage::Starting | Stage::Processing => Look {
            width: height,
            fill: 0xFF00_0000 | ORB,
            glow,
            border,
        },
    }
}

/// Overshoots a little before settling, so shapes grow into place.
fn ease_out_back(t: f32) -> f32 {
    let (c1, c3) = (1.2, 2.2);
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

fn smooth_step(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Mixes two 0xAARRGGBB colours channel by channel.
fn mix(from: u32, to: u32, t: f32) -> u32 {
    (0..4).fold(0, |colour, channel| {
        let shift = channel * 8;
        let (a, b) = ((from >> shift) & 0xFF, (to >> shift) & 0xFF);
        let value = (a as f32 + (b as f32 - a as f32) * t).round() as u32;
        colour | (value.min(255) << shift)
    })
}

fn frame(motion: &Motion, now: f32) -> Frame {
    let target = look(motion.stage);
    let t = ((now - motion.since) / motion.duration).clamp(0.0, 1.0);
    let eased = smooth_step(t);
    let width = motion.from.width + (target.width - motion.from.width) * ease_out_back(t);
    // Dip mid-morph, then grow back into the new shape. Finishing keeps
    // still so the tick grows out of the spinner instead.
    let dip = if finishing(motion.previous, motion.stage) {
        0.0
    } else {
        0.1
    };
    let mut scale = 1.0 - dip * (PI * t).sin();
    let appear = ((now - motion.shown_at) / FADE).clamp(0.0, 1.0);
    scale *= 0.6 + 0.4 * ease_out_back(appear);
    let mut alpha = appear;
    if let Some(leaving) = motion.leaving_at {
        let gone = ((now - leaving) / FADE).clamp(0.0, 1.0);
        scale *= 1.0 - 0.4 * gone * gone;
        alpha *= 1.0 - gone;
    }
    Frame {
        stage: motion.stage,
        previous: motion.previous,
        width,
        scale,
        look: Look {
            width,
            fill: mix(motion.from.fill, target.fill, eased),
            glow: mix(motion.from.glow, target.glow, eased),
            border: mix(motion.from.border, target.border, eased),
        },
        t,
        old: if t >= 1.0 {
            0.0
        } else {
            1.0 - (t / 0.45).min(1.0)
        },
        new: ((t - 0.35) / 0.55).clamp(0.0, 1.0),
        alpha,
    }
}

fn finishing(previous: Stage, stage: Stage) -> bool {
    matches!((previous, stage), (Stage::Processing, Stage::Pasted))
}

/// The spinner's start angle and length in degrees.
fn spinner(now: f32) -> (f32, f32) {
    let sweep = 60.0 + 180.0 * (0.5 - 0.5 * (TAU * now / 1.4).cos());
    ((now * 420.0).rem_euclid(360.0), sweep)
}

/// A tick drawn `drawn` of the way along, 0 to 1, centred on `(x, y)`.
fn tick(canvas: &Canvas, (x, y): (f32, f32), size: f32, drawn: f32, rgb: u32) {
    if drawn <= 0.0 {
        return;
    }
    let half = size / 2.0;
    let points = [
        (x - half, y),
        (x - half / 3.0, y + half * 0.65),
        (x + half, y - half * 0.6),
    ];
    let length = |a: (f32, f32), b: (f32, f32)| (b.0 - a.0).hypot(b.1 - a.1);
    let total = length(points[0], points[1]) + length(points[1], points[2]);
    let mut left = total * drawn.min(1.0);
    let mut path = vec![points[0]];
    for pair in points.windows(2) {
        let step = length(pair[0], pair[1]);
        let part = (left / step).min(1.0);
        path.push((
            pair[0].0 + (pair[1].0 - pair[0].0) * part,
            pair[0].1 + (pair[1].1 - pair[0].1) * part,
        ));
        left -= step;
        if left <= 0.0 {
            break;
        }
    }
    canvas.lines(&path, rgb, 2.0 * theme::scale());
}

/// Processing turning into pasted: the spinner closes into a green ring that
/// widens and fades back while the tick draws in.
fn finish(canvas: &Canvas, pill: RECT, t: f32, now: f32) {
    let s = theme::scale();
    let centre = (
        (pill.left + pill.right) as f32 / 2.0,
        (pill.top + pill.bottom) as f32 / 2.0,
    );
    let close = smooth_step((t / 0.45).min(1.0));
    let open = smooth_step(((t - 0.35) / 0.65).clamp(0.0, 1.0));
    canvas.arc(
        centre,
        7.0 * s,
        (0.0, 360.0),
        alpha(theme::ACCENT, 0.16 * (1.0 - close)),
        2.0 * s,
    );
    let (start, sweep) = spinner(now);
    let colour = mix(
        0xFF00_0000 | theme::ACCENT,
        0xFF00_0000 | theme::GREEN,
        close,
    );
    canvas.arc(
        centre,
        (7.0 + 3.0 * open) * s,
        (start, sweep + (360.0 - sweep) * close),
        alpha(colour, 1.0 - 0.8 * open),
        2.0 * s,
    );
    tick(
        canvas,
        (centre.0, centre.1 + 0.5 * s),
        10.0 * s,
        smooth_step(((t - 0.4) / 0.5).clamp(0.0, 1.0)),
        0xFF00_0000 | theme::GREEN,
    );
}

/// `rgb` at `opacity` from 0 to 1, as 0xAARRGGBB.
fn alpha(rgb: u32, opacity: f32) -> u32 {
    (((opacity.clamp(0.004, 1.0) * 255.0).round() as u32) << 24) | (rgb & 0xFF_FFFF)
}

/// Draws a stage's content inside `pill` at `opacity`. Text goes to `labels`
/// for GDI to draw afterwards; outgoing content passes `None` and drops it.
fn content(
    canvas: &Canvas,
    stage: Stage,
    pill: RECT,
    opacity: f32,
    (now, stream): (f32, &Stream),
    labels: Option<&mut Vec<(RECT, String, u32)>>,
) {
    if opacity <= 0.0 {
        return;
    }
    let s = theme::scale();
    let middle = (pill.top + pill.bottom) as f32 / 2.0;
    let centre = (pill.left + pill.right) as f32 / 2.0;
    let pulse = |period: f32| 0.35 + 0.65 * (0.5 + 0.5 * (TAU * now / period).cos());
    match stage {
        Stage::Hidden => {}
        Stage::Starting => {
            let breathe = 0.5 + 0.5 * (TAU * now / 0.7).cos();
            canvas.arc(
                (centre, middle),
                (6.0 + 1.2 * breathe) * s,
                (0.0, 360.0),
                alpha(theme::ACCENT, (0.6 + 0.4 * breathe) * opacity),
                2.0 * s,
            );
        }
        Stage::Recording(since) => {
            let mut x = pill.left as f32 + 14.0 * s;
            canvas.circle(
                x + 4.0 * s,
                middle,
                4.0 * s,
                alpha(theme::RED, pulse(1.2) * opacity),
            );
            x += 20.0 * s;
            // The bars stay put while the loudness flows through them from
            // right to left: each bar eases towards its right neighbour's
            // sample until the next one arrives.
            let slide = stream.slide(now);
            for (bar, pair) in stream.samples.windows(2).enumerate() {
                let level = pair[0] + (pair[1] - pair[0]) * slide;
                let tall = 3.0 * s + 19.0 * s * level;
                let left = x + bar as f32 * 6.0 * s;
                canvas.round(
                    RECT {
                        left: left.round() as i32,
                        top: (middle - tall / 2.0).round() as i32,
                        right: (left + 3.0 * s).round() as i32,
                        bottom: (middle + tall / 2.0).round() as i32,
                    },
                    1.5 * s,
                    alpha(theme::ACCENT, opacity),
                );
            }
            x += (BARS as f32 * 6.0 - 3.0 + 12.0) * s;
            if let Some(labels) = labels {
                let seconds = since.elapsed().as_secs();
                let left = x.round() as i32;
                labels.push((
                    RECT {
                        left,
                        top: pill.top + px(4),
                        right: (left + theme::text_width("0:00", Font::Body) + px(2))
                            .min(pill.right - px(4)),
                        bottom: pill.bottom - px(4),
                    },
                    format!("{}:{:02}", seconds / 60, seconds % 60),
                    mix(0xFF00_0000 | PILL, 0xFF00_0000 | theme::FAINT, opacity) & 0xFF_FFFF,
                ));
            }
        }
        Stage::Processing => {
            // A faint track with an arc chasing round it, its length breathing.
            let radius = 7.0 * s;
            canvas.arc(
                (centre, middle),
                radius,
                (0.0, 360.0),
                alpha(theme::ACCENT, 0.16 * opacity),
                2.0 * s,
            );
            canvas.arc(
                (centre, middle),
                radius,
                spinner(now),
                alpha(theme::ACCENT, opacity),
                2.0 * s,
            );
        }
        Stage::Pasted => {
            // Where `finish` comes to rest.
            canvas.arc(
                (centre, middle),
                10.0 * s,
                (0.0, 360.0),
                alpha(theme::GREEN, 0.2 * opacity),
                2.0 * s,
            );
            tick(
                canvas,
                (centre, middle + 0.5 * s),
                10.0 * s,
                opacity,
                alpha(theme::GREEN, opacity),
            );
        }
    }
}

fn render(window: HWND, frame: &Frame, now: f32) {
    let s = theme::scale();
    let (full_height, margin) = (px(36), px(16));
    let height = (full_height as f32 * frame.scale).round().max(2.0) as i32;
    let pill_width = (frame.width * frame.scale).round().max(2.0) as i32;
    let (width, total) = (pill_width + margin * 2, full_height + margin * 2);
    let top = margin + (full_height - height) / 2;
    let pill = RECT {
        left: margin,
        top,
        right: margin + pill_width,
        bottom: top + height,
    };
    let radius = height as f32 / 2.0;

    let stream = {
        let mut stream = stream();
        let live = matches!(frame.stage, Stage::Recording(_));
        stream.advance(now, if live { audio::level() } else { 0.0 });
        *stream
    };

    unsafe {
        let screen = GetDC(null_mut());
        let memory = CreateCompatibleDC(screen);
        let mut header: BITMAPINFO = std::mem::zeroed();
        header.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -total,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits = null_mut();
        let bitmap = CreateDIBSection(memory, &header, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bitmap.is_null() || bits.is_null() {
            DeleteDC(memory);
            ReleaseDC(null_mut(), screen);
            return;
        }
        let pixels = bits.cast::<u8>();
        std::ptr::write_bytes(pixels, 0, (width * total * 4) as usize);
        let previous = SelectObject(memory, bitmap);

        let mut labels = Vec::new();
        {
            let canvas = Canvas::over_pixels(memory, pixels, width, total);
            // A soft shadow, then a faint glow.
            for step in 1..=7 {
                let grow = step as f32 * 1.7 * s;
                let shadow = RECT {
                    left: pill.left - grow as i32,
                    top: pill.top - grow as i32 + px(3),
                    right: pill.right + grow as i32,
                    bottom: pill.bottom + grow as i32 + px(3),
                };
                canvas.round(shadow, radius + grow, 0x0C00_0000);
            }
            canvas.outline(
                RECT {
                    left: pill.left - px(2),
                    top: pill.top - px(2),
                    right: pill.right + px(2),
                    bottom: pill.bottom + px(2),
                },
                radius + 2.0 * s,
                frame.look.glow,
                2.0 * s,
            );
            canvas.round(pill, radius, frame.look.fill);
            canvas.outline(pill, radius, frame.look.border, s);
            canvas.clip_round(pill, radius);
            if finishing(frame.previous, frame.stage) && frame.t < 1.0 {
                finish(&canvas, pill, frame.t, now);
            } else {
                if frame.old > 0.0 {
                    content(
                        &canvas,
                        frame.previous,
                        pill,
                        frame.old,
                        (now, &stream),
                        None,
                    );
                }
                content(
                    &canvas,
                    frame.stage,
                    pill,
                    frame.new,
                    (now, &stream),
                    Some(&mut labels),
                );
            }
            canvas.flush();
            for (area, text, colour) in &labels {
                canvas.text(
                    text,
                    *area,
                    Font::Body,
                    *colour,
                    DT_LEFT | DT_SINGLELINE | DT_VCENTER,
                );
            }
        }
        // GDI text leaves alpha at zero. Labels sit on the opaque pill, so
        // their whole area is opaque.
        for (area, _, _) in &labels {
            for y in area.top.max(0)..area.bottom.min(total) {
                for x in area.left.max(0)..area.right.min(width) {
                    *pixels.add(((y * width + x) * 4 + 3) as usize) = 255;
                }
            }
        }

        let (centre, bottom) = *ANCHOR
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let position = POINT {
            x: centre - width / 2,
            y: bottom - full_height - margin,
        };
        let size = SIZE {
            cx: width,
            cy: total,
        };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (frame.alpha * 255.0).round() as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        UpdateLayeredWindow(
            window, screen, &position, &size, memory, &source, 0, &blend, ULW_ALPHA,
        );
        SelectObject(memory, previous);
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(null_mut(), screen);
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_TIMER => {
            update();
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}
