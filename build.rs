//! Embeds the icon, manifest and Win32 version info. The version resource is
//! what Explorer (Properties > Details), Task Manager and SmartScreen show as
//! publisher / product, so it is generated from Cargo.toml and never drifts.

const COMPANY: &str = "Emulsion Labs";
const PRODUCT: &str = "Resource Monitor";

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out = std::env::var("OUT_DIR").unwrap();
    let res = |f: &str| format!("{dir}/res/{f}").replace('\\', "/");
    let num = |k: &str| std::env::var(k).unwrap().parse::<u16>().unwrap_or(0);
    let (ma, mi, pa) = (num("CARGO_PKG_VERSION_MAJOR"), num("CARGO_PKG_VERSION_MINOR"), num("CARGO_PKG_VERSION_PATCH"));
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap();
    let desc = std::env::var("CARGO_PKG_DESCRIPTION").unwrap();
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {ma},{mi},{pa},0
PRODUCTVERSION {ma},{mi},{pa},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "{COMPANY}"
      VALUE "FileDescription", "{PRODUCT}"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "ResourceMonitor"
      VALUE "LegalCopyright", "Copyright (C) 2026 {COMPANY}"
      VALUE "OriginalFilename", "ResourceMonitor.exe"
      VALUE "ProductName", "{PRODUCT}"
      VALUE "ProductVersion", "{ver}"
      VALUE "Comments", "{desc}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = res("app.ico"),
        manifest = res("resmon.manifest"),
    );
    let path = format!("{out}/resmon.rc");
    std::fs::write(&path, rc).unwrap();
    embed_resource::compile(&path, embed_resource::NONE).manifest_optional().unwrap();
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=res/resmon.manifest");
    println!("cargo:rerun-if-changed=res/app.ico");
}
