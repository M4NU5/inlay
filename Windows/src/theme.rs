//! The Glacier look shared by the tray menu and the settings window: colours,
//! fonts and drawing helpers. Shapes use GDI+ for smooth edges; text uses GDI.

use crate::win::fill;
use std::ptr::null_mut;
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::Foundation::{COLORREF, HWND, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Graphics::GdiPlus::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;

// Colours are 0xRRGGBB, from the Mac app's Glacier palette. Canvas methods also
// take 0xAARRGGBB for translucent shapes.
pub const CANVAS: u32 = 0x1B252E;
pub const SIDEBAR: u32 = 0x151E26;
pub const SURFACE: u32 = 0x222C35;
pub const FIELD: u32 = 0x18212A;
pub const LINE: u32 = 0x2E3F4D;
pub const HOVER: u32 = 0x2B3E4D;
pub const RAISED: u32 = 0x3A5265;
pub const TINT: u32 = 0x223949;
pub const INK: u32 = 0xEDF5FA;
pub const MUTED: u32 = 0xA4B8C7;
pub const FAINT: u32 = 0x6F8798;
pub const ACCENT: u32 = 0xA7D6F5;
pub const ACCENT_HOT: u32 = 0xC4E5FA;
pub const ACCENT_DOWN: u32 = 0x8CC3EA;
pub const ON_ACCENT: u32 = 0x192C3A;
pub const GREEN: u32 = 0x30D158;
pub const ORANGE: u32 = 0xFF9F0A;
pub const RED: u32 = 0xFF6B5A;

/// GDI wants 0x00BBGGRR.
pub fn colorref(rgb: u32) -> COLORREF {
    ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF)
}

fn argb(colour: u32) -> u32 {
    if colour >> 24 == 0 {
        0xFF00_0000 | colour
    } else {
        colour
    }
}

/// GDI+'s premultiplied 32-bit format, which layered windows expect.
const PIXEL_FORMAT_PARGB: i32 = 0xE200B;

pub fn scale() -> f32 {
    static SCALE: OnceLock<f32> = OnceLock::new();
    *SCALE.get_or_init(|| unsafe { GetDpiForSystem() } as f32 / 96.0)
}

/// Logical pixels at 96 DPI to device pixels.
pub fn px(value: i32) -> i32 {
    (value as f32 * scale()).round() as i32
}

/// A rectangle given in logical pixels, in device pixels.
pub fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left: px(left),
        top: px(top),
        right: px(right),
        bottom: px(bottom),
    }
}

/// Starts GDI+ for the life of the process. Call once before any window paints.
pub fn start() {
    let input = GdiplusStartupInput {
        GdiplusVersion: 1,
        ..Default::default()
    };
    let mut token = 0;
    let mut output = GdiplusStartupOutput::default();
    unsafe { GdiplusStartup(&mut token, &input, &mut output) };
}

/// Dark title bar and Glacier border. `caption` colours the title bar on
/// Windows 11; `round` asks for rounded corners on popups.
pub fn frame(window: HWND, caption: Option<u32>, round: bool) {
    let set = |attribute: i32, value: u32| unsafe {
        DwmSetWindowAttribute(
            window,
            attribute as u32,
            (&raw const value).cast(),
            size_of::<u32>() as u32,
        );
    };
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, 1);
    set(DWMWA_BORDER_COLOR, colorref(LINE));
    if let Some(caption) = caption {
        set(DWMWA_CAPTION_COLOR, colorref(caption));
    }
    if round {
        set(DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND as u32);
    }
}

#[derive(Clone, Copy)]
pub enum Font {
    Body,
    Strong,
    Small,
    SmallStrong,
    Title,
    Wordmark,
}

pub fn font(font: Font) -> HFONT {
    static FONTS: OnceLock<[usize; 6]> = OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        [
            ("Segoe UI", 14, 400),
            ("Segoe UI", 14, 600),
            ("Segoe UI", 12, 400),
            ("Segoe UI", 12, 600),
            ("Georgia", 24, 400),
            ("Georgia", 22, 400),
        ]
        .map(|(face, size, weight)| {
            let mut description: LOGFONTW = unsafe { std::mem::zeroed() };
            description.lfHeight = -px(size);
            description.lfWeight = weight;
            description.lfQuality = CLEARTYPE_QUALITY;
            fill(&mut description.lfFaceName, face);
            unsafe { CreateFontIndirectW(&description) as usize }
        })
    });
    fonts[font as usize] as HFONT
}

/// A solid brush for `rgb`, kept for the life of the process.
pub fn brush(rgb: u32) -> HBRUSH {
    static BRUSHES: Mutex<Vec<(u32, usize)>> = Mutex::new(Vec::new());
    let mut brushes = BRUSHES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(&(_, brush)) = brushes.iter().find(|(colour, _)| *colour == rgb) {
        return brush as HBRUSH;
    }
    let brush = unsafe { CreateSolidBrush(colorref(rgb)) };
    brushes.push((rgb, brush as usize));
    brush
}

/// Paints a window's client area through an off-screen bitmap, so it never flickers.
pub fn paint(window: HWND, draw: impl FnOnce(&Canvas, RECT)) {
    unsafe {
        let mut paint: PAINTSTRUCT = std::mem::zeroed();
        let dc = BeginPaint(window, &mut paint);
        let mut client = std::mem::zeroed();
        GetClientRect(window, &mut client);
        buffered(dc, client, draw);
        EndPaint(window, &paint);
    }
}

/// Draws into an off-screen bitmap the size of `area`, then copies it to `dc`.
/// `draw` sees the area with its top-left corner at zero.
pub fn buffered(dc: HDC, area: RECT, draw: impl FnOnce(&Canvas, RECT)) {
    let (width, height) = (area.right - area.left, area.bottom - area.top);
    if width <= 0 || height <= 0 {
        return;
    }
    unsafe {
        let memory = CreateCompatibleDC(dc);
        let bitmap = CreateCompatibleBitmap(dc, width, height);
        let previous = SelectObject(memory, bitmap);
        {
            let canvas = Canvas::new(memory);
            draw(
                &canvas,
                RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                },
            );
        }
        BitBlt(
            dc, area.left, area.top, width, height, memory, 0, 0, SRCCOPY,
        );
        SelectObject(memory, previous);
        DeleteObject(bitmap);
        DeleteDC(memory);
    }
}

/// Measures wrapped text in a given width. Returns its height.
pub fn text_height(text: &str, font: Font, width: i32, flags: u32) -> i32 {
    unsafe {
        let dc = GetDC(null_mut());
        let previous = SelectObject(dc, self::font(font));
        let mut area = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: 0,
        };
        let units: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(
            dc,
            units.as_ptr(),
            units.len() as i32,
            &mut area,
            flags | DT_CALCRECT,
        );
        SelectObject(dc, previous);
        ReleaseDC(null_mut(), dc);
        area.bottom
    }
}

/// The width `text` takes on one line, measured on the screen.
pub fn text_width(text: &str, font: Font) -> i32 {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut area = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe {
        let dc = GetDC(null_mut());
        let previous = SelectObject(dc, self::font(font));
        DrawTextW(
            dc,
            units.as_ptr(),
            units.len() as i32,
            &mut area,
            DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(dc, previous);
        ReleaseDC(null_mut(), dc);
    }
    area.right
}

pub struct Canvas {
    dc: HDC,
    graphics: *mut GpGraphics,
    /// Set when drawing straight into a premultiplied pixel buffer.
    bitmap: *mut GpBitmap,
}

impl Canvas {
    fn new(dc: HDC) -> Canvas {
        let mut graphics = null_mut();
        unsafe { GdipCreateFromHDC(dc, &mut graphics) };
        Self::prepare(dc, graphics, null_mut())
    }

    /// Draws into the top-down 32-bit pixels of a DIB section selected into
    /// `dc`, keeping per-pixel alpha for a layered window. GDI text drawn
    /// through this canvas clears alpha, so callers restore it afterwards.
    pub fn over_pixels(dc: HDC, pixels: *mut u8, width: i32, height: i32) -> Canvas {
        let mut bitmap = null_mut();
        let mut graphics = null_mut();
        unsafe {
            GdipCreateBitmapFromScan0(
                width,
                height,
                width * 4,
                PIXEL_FORMAT_PARGB,
                pixels,
                &mut bitmap,
            );
            GdipGetImageGraphicsContext(bitmap.cast(), &mut graphics);
        }
        Self::prepare(dc, graphics, bitmap)
    }

    fn prepare(dc: HDC, graphics: *mut GpGraphics, bitmap: *mut GpBitmap) -> Canvas {
        unsafe {
            GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
            GdipSetPixelOffsetMode(graphics, PixelOffsetModeHalf);
            SetBkMode(dc, TRANSPARENT as i32);
        }
        Canvas {
            dc,
            graphics,
            bitmap,
        }
    }

    /// Finishes pending GDI+ drawing, so GDI can draw over it.
    pub fn flush(&self) {
        unsafe { GdipFlush(self.graphics, FlushIntentionSync) };
    }

    pub fn fill(&self, area: RECT, rgb: u32) {
        unsafe { FillRect(self.dc, &area, brush(rgb)) };
    }

    fn rounded(area: RECT, radius: f32, inset: f32) -> *mut GpPath {
        let (x, y) = (area.left as f32 + inset, area.top as f32 + inset);
        let width = (area.right - area.left) as f32 - inset * 2.0;
        let height = (area.bottom - area.top) as f32 - inset * 2.0;
        let diameter = (radius * 2.0).min(width).min(height).max(0.1);
        let mut path = null_mut();
        unsafe {
            GdipCreatePath(FillModeAlternate, &mut path);
            GdipAddPathArc(path, x, y, diameter, diameter, 180.0, 90.0);
            GdipAddPathArc(
                path,
                x + width - diameter,
                y,
                diameter,
                diameter,
                270.0,
                90.0,
            );
            GdipAddPathArc(
                path,
                x + width - diameter,
                y + height - diameter,
                diameter,
                diameter,
                0.0,
                90.0,
            );
            GdipAddPathArc(
                path,
                x,
                y + height - diameter,
                diameter,
                diameter,
                90.0,
                90.0,
            );
            GdipClosePathFigure(path);
        }
        path
    }

    /// A filled rounded rectangle. `radius` is in device pixels.
    pub fn round(&self, area: RECT, radius: f32, rgb: u32) {
        let path = Self::rounded(area, radius, 0.0);
        unsafe {
            let mut brush = null_mut();
            GdipCreateSolidFill(argb(rgb), &mut brush);
            GdipFillPath(self.graphics, brush.cast(), path);
            GdipDeleteBrush(brush.cast());
            GdipDeletePath(path);
        }
    }

    /// Limits later shapes to a rounded rectangle. GDI text ignores it.
    pub fn clip_round(&self, area: RECT, radius: f32) {
        let path = Self::rounded(area, radius, 0.0);
        unsafe {
            GdipSetClipPath(self.graphics, path, CombineModeReplace);
            GdipDeletePath(path);
        }
    }

    /// A rounded outline drawn inside `area`. `width` is in device pixels.
    pub fn outline(&self, area: RECT, radius: f32, rgb: u32, width: f32) {
        let path = Self::rounded(area, radius - width / 2.0, width / 2.0);
        unsafe {
            let pen = self.pen(rgb, width);
            GdipDrawPath(self.graphics, pen, path);
            GdipDeletePen(pen);
            GdipDeletePath(path);
        }
    }

    pub fn circle(&self, x: f32, y: f32, radius: f32, rgb: u32) {
        unsafe {
            let mut brush = null_mut();
            GdipCreateSolidFill(argb(rgb), &mut brush);
            GdipFillEllipse(
                self.graphics,
                brush.cast(),
                x - radius,
                y - radius,
                radius * 2.0,
                radius * 2.0,
            );
            GdipDeleteBrush(brush.cast());
        }
    }

    /// Part of a circle's outline. Angles are in degrees, clockwise from three o'clock.
    pub fn arc(
        &self,
        (x, y): (f32, f32),
        radius: f32,
        (start, sweep): (f32, f32),
        rgb: u32,
        width: f32,
    ) {
        unsafe {
            let pen = self.pen(rgb, width);
            GdipDrawArc(
                self.graphics,
                pen,
                x - radius,
                y - radius,
                radius * 2.0,
                radius * 2.0,
                start,
                sweep,
            );
            GdipDeletePen(pen);
        }
    }

    fn pen(&self, rgb: u32, width: f32) -> *mut GpPen {
        let mut pen = null_mut();
        unsafe {
            GdipCreatePen1(argb(rgb), width, UnitPixel, &mut pen);
            GdipSetPenLineCap197819(pen, LineCapRound, LineCapRound, DashCapRound);
        }
        pen
    }

    /// Joins the points with a round-capped line of `width` device pixels.
    pub fn lines(&self, points: &[(f32, f32)], rgb: u32, width: f32) {
        unsafe {
            let pen = self.pen(rgb, width);
            for pair in points.windows(2) {
                GdipDrawLine(
                    self.graphics,
                    pen,
                    pair[0].0,
                    pair[0].1,
                    pair[1].0,
                    pair[1].1,
                );
            }
            GdipDeletePen(pen);
        }
    }

    /// The Inlay mark: three fitted strokes forming an I, `size` pixels square.
    pub fn mark(&self, x: i32, y: i32, size: i32, rgb: u32) {
        let unit = size as f32 / 128.0;
        let part = |left: f32, top: f32, width: f32, height: f32| RECT {
            left: x + (left * unit).round() as i32,
            top: y + (top * unit).round() as i32,
            right: x + ((left + width) * unit).round() as i32,
            bottom: y + ((top + height) * unit).round() as i32,
        };
        let radius = 8.0 * unit;
        self.round(part(20.0, 20.0, 88.0, 16.0), radius, rgb);
        self.round(part(56.0, 44.0, 16.0, 40.0), radius, rgb);
        self.round(part(20.0, 92.0, 88.0, 16.0), radius, rgb);
    }

    /// A microphone outline, `size` pixels square.
    pub fn microphone(&self, x: i32, y: i32, size: i32, rgb: u32) {
        let unit = size as f32 / 24.0;
        let at = |px: f32, py: f32| (x as f32 + px * unit, y as f32 + py * unit);
        let width = 2.0 * unit;
        let body = RECT {
            left: (x as f32 + 9.0 * unit).round() as i32,
            top: (y as f32 + 3.0 * unit).round() as i32,
            right: (x as f32 + 15.0 * unit).round() as i32,
            bottom: (y as f32 + 15.0 * unit).round() as i32,
        };
        self.outline(body, 3.0 * unit, rgb, width);
        unsafe {
            let pen = self.pen(rgb, width);
            let (left, top) = at(5.5, 4.5);
            GdipDrawArc(
                self.graphics,
                pen,
                left,
                top,
                13.0 * unit,
                13.0 * unit,
                0.0,
                180.0,
            );
            GdipDeletePen(pen);
        }
        self.lines(&[at(12.0, 17.5), at(12.0, 21.0)], rgb, width);
    }

    pub fn chevron(&self, center_x: f32, center_y: f32, size: f32, rgb: u32) {
        let half = size / 2.0;
        self.lines(
            &[
                (center_x - half, center_y - half / 2.0),
                (center_x, center_y + half / 2.0),
                (center_x + half, center_y - half / 2.0),
            ],
            rgb,
            1.5 * scale(),
        );
    }

    pub fn check(&self, center_x: f32, center_y: f32, size: f32, rgb: u32) {
        let half = size / 2.0;
        self.lines(
            &[
                (center_x - half, center_y),
                (center_x - half / 3.0, center_y + half * 0.65),
                (center_x + half, center_y - half * 0.6),
            ],
            rgb,
            1.8 * scale(),
        );
    }

    pub fn text(&self, text: &str, area: RECT, font: Font, rgb: u32, flags: u32) {
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut area = area;
        unsafe {
            let previous = SelectObject(self.dc, self::font(font));
            SetTextColor(self.dc, colorref(rgb));
            DrawTextW(
                self.dc,
                units.as_ptr(),
                units.len() as i32,
                &mut area,
                flags | DT_NOPREFIX,
            );
            SelectObject(self.dc, previous);
        }
    }

    /// The width `text` takes on one line.
    pub fn text_width(&self, text: &str, font: Font) -> i32 {
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut area = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        unsafe {
            let previous = SelectObject(self.dc, self::font(font));
            DrawTextW(
                self.dc,
                units.as_ptr(),
                units.len() as i32,
                &mut area,
                DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
            );
            SelectObject(self.dc, previous);
        }
        area.right
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            GdipDeleteGraphics(self.graphics);
            if !self.bitmap.is_null() {
                GdipDisposeImage(self.bitmap.cast());
            }
        }
    }
}
