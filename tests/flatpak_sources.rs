//! Das Flatpak baut ohne Netz aus `packaging/flatpak/cargo-sources.json`; jede Abhängigkeit
//! aus `Cargo.lock` muss dort stehen, sonst schlägt der Flatpak-Build fehl.

#[test]
fn cargo_sources_cover_cargo_lock() {
    let lock = include_str!("../Cargo.lock");
    let sources = include_str!("../packaging/flatpak/cargo-sources.json");
    let mut missing = Vec::new();
    for package in lock.split("[[package]]").skip(1) {
        let field = |key: &str| {
            package.lines().find_map(|line| {
                line.strip_prefix(key)
                    .map(|rest| rest.trim().trim_matches('"').to_string())
            })
        };
        // Nur Pakete aus crates.io (das eigene Paket hat keine Quelle)
        if field("source = ").is_none() {
            continue;
        }
        let (Some(name), Some(version)) = (field("name = "), field("version = ")) else {
            continue;
        };
        let crate_file = format!("/{name}-{version}.crate\"");
        if !sources.contains(&crate_file) {
            missing.push(format!("{name} {version}"));
        }
    }
    assert!(
        missing.is_empty(),
        "cargo-sources.json ist veraltet (flatpak-cargo-generator neu ausführen), es fehlen: {}",
        missing.join(", ")
    );
}
