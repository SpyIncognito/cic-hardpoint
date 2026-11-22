//! On Windows targets, the exe's resources: the Orbit Reticle icon (Explorer, the taskbar before
//! the window sets its own) and the version information Explorer's Details tab shows.
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/hardpoint.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is set");
    let mut parts: Vec<u16> = version
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    parts.resize(4, 0);
    let numeric = parts
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir is set"));
    let icon = manifest.join("assets").join("hardpoint.ico");
    let icon = icon.to_string_lossy().replace('\\', "/");
    // Numbers in place of winver.h's names, so no Windows SDK headers are needed:
    // VOS_NT_WINDOWS32 = 0x40004, VFT_APP = 1, language 0x0409 (US English), code page 1200.
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Spy Incognito Games"
      VALUE "FileDescription", "Hardpoint"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "hardpoint"
      VALUE "LegalCopyright", "Copyright 2026 Spy Incognito Games. Apache License 2.0."
      VALUE "OriginalFilename", "hardpoint.exe"
      VALUE "ProductName", "Hardpoint"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#
    );
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set")).join("hardpoint.rc");
    fs::write(&out, rc).expect("resource script written");
    embed_resource::compile(&out, embed_resource::NONE)
        .manifest_optional()
        .expect("resources compiled");
}
