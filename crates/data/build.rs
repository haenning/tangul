//! Embeds the settings files' migration steps and fixtures (`migrations/`), so a
//! new file there needs no code: `migrations.rs` reads the generated lists.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut out = String::new();
    for (name, dir) in [
        ("SETTINGS_STEPS", "settings"),
        ("DATABASE_STEPS", "database"),
        ("SETTINGS_FIXTURES", "fixtures/settings"),
        ("DATABASE_FIXTURES", "fixtures/database"),
    ] {
        let dir = root.join(dir);
        println!("cargo:rerun-if-changed={}", dir.display());
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.retain(|p| p.extension().is_some_and(|e| e == "json"));
        files.sort();
        // Fixtures are only for the tests.
        if dir.ends_with("fixtures/settings") || dir.ends_with("fixtures/database") {
            writeln!(out, "#[cfg(test)]").unwrap();
        }
        writeln!(out, "pub(crate) const {name}: &[(&str, &str)] = &[").unwrap();
        for path in files {
            let version = path.file_stem().unwrap().to_string_lossy();
            writeln!(
                out,
                "    ({version:?}, include_str!({:?})),",
                path.display().to_string()
            )
            .unwrap();
        }
        writeln!(out, "];").unwrap();
    }
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("migrations.rs");
    std::fs::write(dest, out).unwrap();
}
