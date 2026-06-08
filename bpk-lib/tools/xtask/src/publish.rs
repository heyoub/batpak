pub(crate) const PUBLISH_CRATES: &[&str] = &["batpak", "syncbat", "netbat"];

pub(crate) const FAMILY_CRATES: &[&str] = &["syncbat", "netbat"];

pub(crate) const RELEASE_CHAIN: &[&str] = &[
    "batpak-macros-support",
    "batpak-macros",
    "batpak-bench-support",
    "syncbat-macros",
    "batpak",
    "syncbat",
    "netbat",
];

pub(crate) fn local_patch_overrides(package: &str) -> &'static [(&'static str, &'static str)] {
    match package {
        "batpak-macros" => &[("batpak-macros-support", "crates/macros-support")],
        "batpak" => &[
            ("batpak-macros-support", "crates/macros-support"),
            ("batpak-macros", "crates/macros"),
            ("batpak-bench-support", "crates/bench-support"),
        ],
        "syncbat" => &[
            ("batpak-macros-support", "crates/macros-support"),
            ("batpak-macros", "crates/macros"),
            ("syncbat-macros", "crates/syncbat-macros"),
            ("batpak", "crates/core"),
        ],
        "netbat" => &[
            ("batpak-macros-support", "crates/macros-support"),
            ("batpak-macros", "crates/macros"),
            ("syncbat-macros", "crates/syncbat-macros"),
            ("batpak", "crates/core"),
            ("syncbat", "crates/syncbat"),
        ],
        _ => &[],
    }
}
