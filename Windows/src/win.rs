//! Small helpers for Win32 strings.

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

pub fn from_wide(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// Copies `text` into a fixed Win32 buffer, truncating and keeping the terminator.
pub fn fill(buffer: &mut [u16], text: &str) {
    let units: Vec<u16> = text.encode_utf16().take(buffer.len() - 1).collect();
    buffer[..units.len()].copy_from_slice(&units);
    buffer[units.len()] = 0;
}
