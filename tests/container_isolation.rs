//! Cargo enforces private API boundaries; these checks keep the dependency
//! direction explicit as new container families are added.

#[test]
fn core_has_no_handler_dependencies() {
    let manifest = include_str!("../crates/asset-io-core/Cargo.toml");
    for line in manifest.lines() {
        assert!(
            !line.trim_start().starts_with("asset-io-"),
            "core must not depend on container crates: {line}"
        );
    }
}

#[test]
fn container_families_depend_on_core_not_the_facade_or_each_other() {
    for (name, manifest) in [
        ("jpeg", include_str!("../crates/asset-io-jpeg/Cargo.toml")),
        ("png", include_str!("../crates/asset-io-png/Cargo.toml")),
        ("bmff", include_str!("../crates/asset-io-bmff/Cargo.toml")),
        ("riff", include_str!("../crates/asset-io-riff/Cargo.toml")),
        ("svg", include_str!("../crates/asset-io-svg/Cargo.toml")),
        ("pdf", include_str!("../crates/asset-io-pdf/Cargo.toml")),
        ("c2pa", include_str!("../crates/asset-io-c2pa/Cargo.toml")),
    ] {
        assert!(manifest.contains("asset-io-core"), "{name} needs core");
        for line in manifest.lines().map(str::trim_start) {
            if line.starts_with("asset-io") {
                assert!(
                    line.starts_with("asset-io-core"),
                    "{name} must depend on core, not another handler or facade: {line}"
                );
            }
        }
    }
}
