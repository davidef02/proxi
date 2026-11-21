fn main() {
    #[cfg(windows)]
    {
        let windows = tauri_build::WindowsAttributes::new().app_manifest(include_str!("app.manifest"));
        let attributes = tauri_build::Attributes::new().windows_attributes(windows);
        tauri_build::try_build(attributes).expect("failed to run tauri-build");
    }
    #[cfg(not(windows))]
    {
        tauri_build::build();
    }
}
