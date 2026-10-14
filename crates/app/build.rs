use std::path::Path;

const ICON: &str = "assets/icon.ico";
// Resource types of a Windows icon: each image, and the group naming them.
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
// Moveable, pure and discardable, as resource compilers mark icons.
const ICON_MEMORY_FLAGS: u16 = 0x1030;
const LANGUAGE_NEUTRAL: u16 = 0;
const VERSION_MEMORY_FLAGS: u16 = 0x0030;
const ENGLISH_US: u16 = 0x0409;
// The code page the version strings declare: UTF-16.
const UNICODE: u16 = 1200;

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/main.slint", config).expect("ui/main.slint compiles");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        embed_resources();
    }
}

/// Gives the Windows executable its icon and version details. The MSVC
/// linker takes compiled resources directly, so the `.res` file is written
/// here rather than by a resource compiler, which cross builds often lack.
fn embed_resources() {
    println!("cargo::rerun-if-changed={ICON}");
    let ico = std::fs::read(ICON).expect("the icon exists");
    let mut res = icon_resources(&ico);
    push_resource(
        &mut res,
        RT_VERSION,
        1,
        VERSION_MEMORY_FLAGS,
        ENGLISH_US,
        &version_info(),
    );
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let path = Path::new(&out_dir).join("vivace.res");
    std::fs::write(&path, res).expect("OUT_DIR is writable");
    println!("cargo::rustc-link-arg-bins={}", path.display());
}

/// The `VS_VERSIONINFO` block Explorer's Details tab reads, from the
/// package's version.
fn version_info() -> Vec<u8> {
    let part = |name: &str| -> u32 {
        std::env::var(name)
            .ok()
            .and_then(|part| part.parse().ok())
            .unwrap_or(0)
    };
    let (major, minor, patch) = (
        part("CARGO_PKG_VERSION_MAJOR"),
        part("CARGO_PKG_VERSION_MINOR"),
        part("CARGO_PKG_VERSION_PATCH"),
    );
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();

    // VS_FIXEDFILEINFO: signature, structure version, file and product
    // versions, flags, Windows NT, an application.
    let mut fixed = Vec::new();
    let high = (major << 16) | minor;
    let low = patch << 16;
    for word in [
        0xFEEF_04BD,
        0x0001_0000,
        high,
        low,
        high,
        low,
        0x3F,
        0,
        0x0004_0004,
        1,
        0,
        0,
        0,
    ] {
        fixed.extend_from_slice(&u32::to_le_bytes(word));
    }

    let strings: Vec<Vec<u8>> = [
        ("CompanyName", "Vivace"),
        ("FileDescription", "Vivace"),
        ("FileVersion", version.as_str()),
        ("InternalName", "vivace"),
        ("LegalCopyright", "MIT License"),
        ("OriginalFilename", "vivace.exe"),
        ("ProductName", "Vivace"),
        ("ProductVersion", version.as_str()),
    ]
    .into_iter()
    .map(|(key, value)| {
        let text = utf16(value);
        version_node(key, 1, (text.len() / 2) as u16, &text, &[])
    })
    .collect();
    let table = version_node(
        &format!("{ENGLISH_US:04x}{UNICODE:04x}"),
        1,
        0,
        &[],
        &strings,
    );
    let string_info = version_node("StringFileInfo", 1, 0, &[], &[table]);

    let mut translation = Vec::new();
    translation.extend_from_slice(&ENGLISH_US.to_le_bytes());
    translation.extend_from_slice(&UNICODE.to_le_bytes());
    let var = version_node("Translation", 0, 4, &translation, &[]);
    let var_info = version_node("VarFileInfo", 1, 0, &[], &[var]);

    version_node(
        "VS_VERSION_INFO",
        0,
        fixed.len() as u16,
        &fixed,
        &[string_info, var_info],
    )
}

/// One block of version information: a header, its key, its value and
/// the blocks inside it, each starting on a 4-byte boundary. Text values
/// count their length in characters, binary ones in bytes.
fn version_node(
    key: &str,
    kind: u16,
    value_length: u16,
    value: &[u8],
    children: &[Vec<u8>],
) -> Vec<u8> {
    let mut node = vec![0; 2];
    node.extend_from_slice(&value_length.to_le_bytes());
    node.extend_from_slice(&kind.to_le_bytes());
    node.extend_from_slice(&utf16(key));
    node.resize(node.len().next_multiple_of(4), 0);
    node.extend_from_slice(value);
    for child in children {
        node.resize(node.len().next_multiple_of(4), 0);
        node.extend_from_slice(child);
    }
    let length = node.len() as u16;
    node[..2].copy_from_slice(&length.to_le_bytes());
    node
}

/// NUL-terminated UTF-16, as resources store text.
fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
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
