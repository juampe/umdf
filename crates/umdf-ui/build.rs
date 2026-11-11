fn main() {
    slint_build::compile("ui/app.slint").expect("failed to compile ui/app.slint");

    #[cfg(windows)]
    {
        let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icon.ico");
        let mut resource = winres::WindowsResource::new();
        resource.set_icon(icon.to_string_lossy().as_ref());
        resource.set("ProductName", "UMDF — Universal Media Duplicate Finder");
        resource.set("FileDescription", "Universal Media Duplicate Finder");
        resource.set(
            "LegalCopyright",
            "Copyright (C) 2026 Juan Pedro Paredes Caballero. GPL-3.0-only.",
        );
        resource
            .compile()
            .expect("failed to embed Windows resources");
    }
}
