fn main() {
    embed_resource::compile("res/resmon.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
    println!("cargo:rerun-if-changed=res/resmon.rc");
    println!("cargo:rerun-if-changed=res/resmon.manifest");
    println!("cargo:rerun-if-changed=res/app.ico");
}
