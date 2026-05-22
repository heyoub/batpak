const BATPAK_DEPENDENCY_PREFIX: &str = "batpak = { path = ";
const BATPAK_FEATURES: &[&str] = &["blake3"];

pub(super) fn batpak_path_dependency_line(path: &str) -> String {
    let features = BATPAK_FEATURES
        .iter()
        .map(|feature| format!("\"{feature}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "batpak = {{ path = \"{}\", features = [{}] }}",
        escape_toml_basic_string(path),
        features
    )
}

pub(super) fn rewrite_batpak_path_dependency(content: &str, path: &str) -> String {
    content
        .lines()
        .map(|line| {
            if line.trim_start().starts_with(BATPAK_DEPENDENCY_PREFIX) {
                batpak_path_dependency_line(path)
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn escape_toml_basic_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::{batpak_path_dependency_line, rewrite_batpak_path_dependency};

    #[test]
    fn batpak_path_dependency_line_carries_feature_policy() {
        assert_eq!(
            batpak_path_dependency_line("../crates/core"),
            "batpak = { path = \"../crates/core\", features = [\"blake3\"] }"
        );
    }

    #[test]
    fn rewrite_batpak_path_dependency_updates_only_batpak_path_row() {
        let input = "[dependencies]\nbatpak = { path = \"old\", features = [] }\nserde = \"1\"";

        let updated = rewrite_batpak_path_dependency(input, "../repo/crates/core");

        assert_eq!(
            updated,
            "[dependencies]\nbatpak = { path = \"../repo/crates/core\", features = [\"blake3\"] }\nserde = \"1\""
        );
    }

    #[test]
    fn batpak_path_dependency_line_escapes_toml_strings() {
        assert_eq!(
            batpak_path_dependency_line("repo\\\"core\""),
            "batpak = { path = \"repo\\\\\\\"core\\\"\", features = [\"blake3\"] }"
        );
    }
}
