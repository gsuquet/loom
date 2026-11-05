fn main() {
    // Without this list every command is open to every window. With it, a capability must allow it.
    let manifest = tauri_build::AppManifest::new().commands(&[
        "loom_api",
        "loom_events",
        "spike_patch",
        "spike_notify",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("tauri build script failed");
}
