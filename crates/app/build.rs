use std::path::Path;

const ICON: &str = "assets/icon.ico";
// Resource types of a Windows icon: each image, and the group naming them.
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
// Moveable, pure and discardable, as resource compilers mark icons.
const ICON_MEMORY_FLAGS: u16 = 0x1030;
const LANGUAGE_NEUTRAL: u16 = 0;

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/main.slint", config).expect("ui/main.slint compiles");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        embed_icon();
    }
}

/// Gives the Windows executable its icon. The MSVC linker takes compiled
/// resources directly, so the `.res` file is written here rather than by a
/// resource compiler, which cross builds often lack.
fn embed_icon() {
    println!("cargo::rerun-if-changed={ICON}");
    let ico = std::fs::read(ICON).expect("the icon exists");
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let res = Path::new(&out_dir).join("icon.res");
    std::fs::write(&res, icon_resources(&ico)).expect("OUT_DIR is writable");
    println!("cargo::rustc-link-arg-bins={}", res.display());
}

/// Compiled resources holding each image of an `.ico` file and the icon
/// group that lists them, the form a resource compiler produces.
fn icon_resources(ico: &[u8]) -> Vec<u8> {
    let u16_at = |at: usize| u16::from_le_bytes([ico[at], ico[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([ico[at], ico[at + 1], ico[at + 2], ico[at + 3]]);

    // A compiled resource file starts with an empty entry.
    let mut res = Vec::new();
    push_resource(&mut res, 0, 0, 0, 0, &[]);

    let count = u16_at(4);
    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&count.to_le_bytes());
    for index in 0..count {
        let entry = 6 + 16 * usize::from(index);
        let size = u32_at(entry + 8) as usize;
        let offset = u32_at(entry + 12) as usize;
        let id = index + 1;
        push_resource(
            &mut res,
            RT_ICON,
            id,
            ICON_MEMORY_FLAGS,
            LANGUAGE_NEUTRAL,
            &ico[offset..offset + size],
        );
        // The group entry is the file's entry with the image's id in place
        // of its offset.
        group.extend_from_slice(&ico[entry..entry + 12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    push_resource(
        &mut res,
        RT_GROUP_ICON,
        1,
        ICON_MEMORY_FLAGS,
        LANGUAGE_NEUTRAL,
        &group,
    );
    res
}

/// Appends one resource with a numeric type and id.
fn push_resource(res: &mut Vec<u8>, kind: u16, id: u16, flags: u16, language: u16, data: &[u8]) {
    const HEADER_SIZE: u32 = 32;
    res.extend_from_slice(&(data.len() as u32).to_le_bytes());
    res.extend_from_slice(&HEADER_SIZE.to_le_bytes());
    for ordinal in [kind, id] {
        res.extend_from_slice(&0xFFFFu16.to_le_bytes());
        res.extend_from_slice(&ordinal.to_le_bytes());
    }
    res.extend_from_slice(&0u32.to_le_bytes()); // data version
    res.extend_from_slice(&flags.to_le_bytes());
    res.extend_from_slice(&language.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // version
    res.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    res.extend_from_slice(data);
    // Each resource starts on a 4-byte boundary.
    res.resize(res.len().next_multiple_of(4), 0);
}
