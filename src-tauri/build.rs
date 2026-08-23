use std::{fs, path::Path};

fn main() {
    // Read the single source of truth for the bundled Harness version and
    // expose it to Rust code via `env!("HARNESS_VERSION")`. Also generate a
    // TypeScript constants file so the frontend uses the same value without
    // hardcoding it in multiple places.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let version_path = manifest_dir.parent().unwrap().join("HARNESS_VERSION");
    let harness_version = fs::read_to_string(&version_path)
        .unwrap_or_else(|error| panic!("failed to read HARNESS_VERSION at {}: {error}", version_path.display()))
        .trim()
        .to_owned();

    println!("cargo:rustc-env=HARNESS_VERSION={harness_version}");
    println!("cargo:rerun-if-changed=../HARNESS_VERSION");

    // Generate `src/generated-version.ts` so the frontend imports the same
    // constant. This file is gitignored and regenerated on every build.
    let ts_path = manifest_dir.parent().unwrap().join("src").join("generated-version.ts");
    let ts_content = format!(
        "// Auto-generated from HARNESS_VERSION — do not edit.\n\
         export const HARNESS_VERSION = {harness_version:?};\n",
    );
    if let Err(error) = fs::write(&ts_path, ts_content) {
        panic!("failed to write {}: {error}", ts_path.display());
    }

    tauri_build::build();
}
