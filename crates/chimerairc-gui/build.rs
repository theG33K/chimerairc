use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");

    // The resource script is written here (not kept as a file) so that the version in the executable's
    // properties is always the one in Cargo.toml.
    let assets = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets");
    let path = |f: &str| assets.join(f).to_string_lossy().replace('\\', "/");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut nums: Vec<u32> = version.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect();
    nums.resize(4, 0);
    let comma = nums.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let dotted = nums.iter().map(u32::to_string).collect::<Vec<_>>().join(".");

    let rc = format!(
        r#"1 ICON "{icon}"
1 24 "{manifest}"
100 BITMAP "{toolbar}"
110 BITMAP "{dark16}"
111 BITMAP "{dark32}"
112 BITMAP "{dark48}"
101 BITMAP "{tabs}"

1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "ChimeraIRC contributors"
      VALUE "FileDescription", "ChimeraIRC - an IRC client for Windows"
      VALUE "FileVersion", "{dotted}"
      VALUE "InternalName", "chimerairc"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
      VALUE "OriginalFilename", "chimerairc.exe"
      VALUE "ProductName", "ChimeraIRC"
      VALUE "ProductVersion", "{dotted}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 0x04b0
  END
END
"#,
        icon = path("chimerairc.ico"),
        manifest = path("chimerairc.manifest"),
        toolbar = path("toolbar.bmp"),
        dark16 = path("toolbar_dark_16.bmp"),
        dark32 = path("toolbar_dark_32.bmp"),
        dark48 = path("toolbar_dark_48.bmp"),
        tabs = path("tabicons.bmp"),
    );
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("chimerairc.rc");
    fs::write(&out, rc).unwrap();

    // Embeds the icon, the version, and the comctl32 v6 / per-monitor-DPI manifest. Fail loudly: without
    // the manifest the UI silently loses visual styles and DPI awareness.
    embed_resource::compile(&out, embed_resource::NONE).manifest_required().unwrap();
}
