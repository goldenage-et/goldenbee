//! Package identity, manifest, and source-evidence checks.

use std::path::Path;

#[test]
fn manifest_declares_coding_features_and_permissions() {
    let manifest = coding::manifest();
    assert_eq!(manifest.name, "coding");
    assert_eq!(manifest.runtime, runtime::VERSION);
    assert_eq!(manifest.adk, "2.3.0");
    assert!(manifest.features.iter().any(|feature| feature == "coding"));
    assert!(manifest
        .features
        .iter()
        .any(|feature| feature == "devtools"));
    assert_eq!(
        manifest.launcher_modes,
        vec!["dev".to_string(), "serve".to_string()]
    );
    assert!(manifest.hooks.contains(&"after_task".to_string()));

    let ids: Vec<_> = manifest
        .capabilities
        .iter()
        .map(|capability| capability.id.as_str())
        .collect();
    assert!(ids.contains(&"developer"));
    assert!(ids.contains(&"reviewer"));
    assert!(ids.contains(&"exec_command"));
    assert!(!ids.iter().any(|id| matches!(
        *id,
        "read_file" | "write_file" | "edit_file" | "glob" | "grep" | "bash"
    )));

    let developer = manifest
        .capabilities
        .iter()
        .find(|capability| capability.id == "developer")
        .expect("developer capability");
    assert!(developer
        .permissions
        .contains(&"workspace:write".to_string()));
    let reviewer = manifest
        .capabilities
        .iter()
        .find(|capability| capability.id == "reviewer")
        .expect("reviewer capability");
    assert!(reviewer.permissions.contains(&"workspace:read".to_string()));
    assert!(!reviewer
        .permissions
        .contains(&"workspace:write".to_string()));
    assert!(!reviewer.permissions.contains(&"exec:command".to_string()));
}

#[test]
fn package_uses_native_coding_dependencies_without_a_second_loop() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(manifest.contains("adk-agent"));
    assert!(manifest.contains("adk-devtools"));
    assert!(manifest.contains("coding"));

    let source = std::fs::read_to_string(root.join("src/tool.rs")).unwrap();
    assert!(source.contains("\"exec_command\""));
    assert!(!source.contains("fn write_file"));
    assert!(!source.contains("struct Bash"));

    for entry in std::fs::read_dir(root.join("src")).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().and_then(|name| name.to_str()) == Some("tool.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !text.contains("impl Tool for"),
            "{} must not implement another tool loop",
            path.display()
        );
        assert!(
            !text.contains("Command::new(\"sh\")"),
            "{} must not build a second Bash tool",
            path.display()
        );
    }
}

#[test]
fn coding_composition_is_separate_from_general_and_knowledge() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let own = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(!own.contains("workers/general"));
    assert!(!own.contains("workers/knowledge"));

    let general = root.join("../general/Cargo.toml");
    let general = std::fs::read_to_string(general).unwrap();
    assert!(!general.contains("workers/coding"));

    let knowledge = root.join("../knowledge/Cargo.toml");
    if knowledge.exists() {
        let knowledge = std::fs::read_to_string(knowledge).unwrap();
        assert!(!knowledge.contains("workers/coding"));
    }
}
