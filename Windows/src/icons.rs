//! Tray icons drawn in code: a white microphone on a coloured disc. The colour
//! carries the state, and a slash marks errors and an unreachable server.

use std::ptr::{null, null_mut};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Idle,
    Recording,
    Processing,
    Error,
}

pub fn create(look: Look) -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let pixels = draw(look, size);
    unsafe {
        let mut header: BITMAPINFO = std::mem::zeroed();
        header.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits = null_mut();
        let color = CreateDIBSection(
            null_mut(),
            &header,
            DIB_RGB_COLORS,
            &mut bits,
            null_mut(),
            0,
        );
        if color.is_null() || bits.is_null() {
            return null_mut();
        }
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u32>(), pixels.len());
        // An all-zero mask lets the colour bitmap's alpha channel decide.
        let mask = CreateBitmap(size, size, 1, 1, null());
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}

fn draw(look: Look, size: i32) -> Vec<u32> {
    let s = size as f32;
    let (red, green, blue) = match look {
        Look::Idle => (0x45, 0x5A, 0x64),
        Look::Recording => (0xE5, 0x39, 0x35),
        Look::Processing => (0xFB, 0x8C, 0x00),
        Look::Error => (0x75, 0x75, 0x75),
    };
    let center = s / 2.0;
    let line = s * 0.045;
    let mut pixels = Vec::with_capacity((size * size) as usize);
    for row in 0..size {
        for column in 0..size {
            let point = (column as f32 + 0.5, row as f32 + 0.5);
            let disc = cover(distance(point, (center, center)) - s * 0.48);
            let body = segment(point, (center, s * 0.27), (center, s * 0.47)) - s * 0.115;
            let ring = (distance(point, (center, s * 0.47)) - s * 0.205).abs() - line;
            let cradle = if point.1 >= s * 0.47 { ring } else { f32::MAX };
            let stem = segment(point, (center, s * 0.675), (center, s * 0.77)) - line;
            let base = segment(
                point,
                (center - s * 0.12, s * 0.77),
                (center + s * 0.12, s * 0.77),
            ) - line;
            let mut glyph = body.min(cradle).min(stem).min(base);
            if look == Look::Error {
                glyph = glyph
                    .min(segment(point, (s * 0.24, s * 0.24), (s * 0.76, s * 0.76)) - s * 0.05);
            }
            let white = cover(glyph);
            let mix =
                |channel: u32| (channel as f32 + (255.0 - channel as f32) * white).round() as u32;
            let alpha = (disc * 255.0).round() as u32;
            pixels.push(alpha << 24 | mix(red) << 16 | mix(green) << 8 | mix(blue));
        }
    }
    pixels
}

/// Anti-aliased coverage of a signed distance measured in pixels.
fn cover(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

fn distance(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn segment(point: (f32, f32), start: (f32, f32), end: (f32, f32)) -> f32 {
    let along = (end.0 - start.0, end.1 - start.1);
    let offset = (point.0 - start.0, point.1 - start.1);
    let length = along.0 * along.0 + along.1 * along.1;
    let t = if length == 0.0 {
        0.0
    } else {
        ((offset.0 * along.0 + offset.1 * along.1) / length).clamp(0.0, 1.0)
    };
    distance(point, (start.0 + along.0 * t, start.1 + along.1 * t))
}
