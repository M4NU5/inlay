//! Embeds an application manifest that selects Common Controls 6, so the
//! settings window uses current Windows visual styles. The linker writes the
//! manifest itself, which works with both link.exe and lld-link.
//!
//! Also embeds `inlay.ico` (from `scripts/make-windows-icon.py`) as icon
//! resource 1, which Explorer and the taskbar show for `inlay.exe`. The `.res`
//! file is written here and handed to the linker, so no resource compiler is
//! needed.
use std::path::Path;

/// Resource ID of the app icon group. `settings/mod.rs` loads it by this ID.
const APP_ICON: u16 = 1;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=inlay.ico");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' \
             name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
             processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
        let ico = std::fs::read("inlay.ico").expect("read inlay.ico");
        let res = Path::new(&std::env::var("OUT_DIR").unwrap()).join("inlay.res");
        std::fs::write(&res, icon_resources(&ico)).expect("write inlay.res");
        println!("cargo:rustc-link-arg-bins={}", res.display());
    }
}

/// Converts an `.ico` file into a `.res` file holding one `RT_ICON` per image
/// and an `RT_GROUP_ICON` that lists them.
fn icon_resources(ico: &[u8]) -> Vec<u8> {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    let word = |at: usize| u16::from_le_bytes([ico[at], ico[at + 1]]);
    let dword = |at: usize| u32::from_le_bytes(ico[at..at + 4].try_into().unwrap());

    // A .res file starts with an empty entry.
    let mut res = Vec::new();
    entry(&mut res, 0, 0, 0, &[]);
    let count = word(4);
    let mut group = Vec::new();
    group.extend_from_slice(&[0, 0, 1, 0]);
    group.extend_from_slice(&count.to_le_bytes());
    for index in 0..count {
        let at = 6 + 16 * index as usize;
        let size = dword(at + 8) as usize;
        let offset = dword(at + 12) as usize;
        let id = index + 1;
        entry(&mut res, RT_ICON, id, 0x1010, &ico[offset..offset + size]);
        // The group entry is the file's directory entry with the offset
        // replaced by the resource ID.
        group.extend_from_slice(&ico[at..at + 12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    entry(&mut res, RT_GROUP_ICON, APP_ICON, 0x1030, &group);
    res
}

fn entry(res: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
    res.extend_from_slice(&(data.len() as u32).to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes()); // header size
    res.extend_from_slice(&[0xFF, 0xFF]);
    res.extend_from_slice(&kind.to_le_bytes());
    res.extend_from_slice(&[0xFF, 0xFF]);
    res.extend_from_slice(&name.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // data version
    res.extend_from_slice(&flags.to_le_bytes());
    let language: u16 = if kind == 0 { 0 } else { 0x0409 };
    res.extend_from_slice(&language.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // version
    res.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    res.extend_from_slice(data);
    res.resize(res.len().next_multiple_of(4), 0);
}
