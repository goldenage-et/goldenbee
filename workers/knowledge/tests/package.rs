//! Package metadata, permissions, and single-executable checks.

#[test]
fn manifest_declares_rag_permissions_and_one_executable() {
    let manifest = knowledge::manifest();
    assert_eq!(manifest.name, "knowledge");
    assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest.runtime, runtime::RUNTIME_VERSION);
    assert!(manifest.features.contains(&"rag".to_string()));
    assert!(manifest.features.contains(&"agents".to_string()));
    assert!(manifest.features.contains(&"server".to_string()));
    assert_eq!(
        manifest.launcher_modes,
        vec!["dev".to_string(), "serve".to_string()]
    );
    assert_eq!(
        manifest.hooks,
        vec![
            "startup".to_string(),
            "before_task".to_string(),
            "after_task".to_string(),
            "shutdown".to_string()
        ]
    );
    let ids: Vec<&str> = manifest
        .capabilities
        .iter()
        .map(|capability| capability.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["knowledge_ask", "knowledge_ingest", "knowledge_remove"]
    );
    let ingest = manifest
        .capabilities
        .iter()
        .find(|capability| capability.id == "knowledge_ingest")
        .unwrap();
    assert!(ingest.permissions.contains(&"knowledge:ingest".to_string()));
    assert!(ingest.permissions.contains(&"artifact:write".to_string()));
    let ask = manifest
        .capabilities
        .iter()
        .find(|capability| capability.id == "knowledge_ask")
        .unwrap();
    assert_eq!(ask.permissions, vec!["knowledge:read".to_string()]);

    // Roles are profiles/agents inside one package, never one executable per
    // role. The package manifest declares exactly one binary.
    let cargo = include_str!("../Cargo.toml");
    assert_eq!(cargo.matches("[[bin]]").count(), 1, "{cargo}");
    assert!(cargo.contains("name = \"knowledge\""));
}

#[test]
fn probe_reports_the_same_compiled_inventory() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_knowledge"))
        .arg("probe")
        .arg("--json")
        .output()
        .expect("probe executes");
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("probe is JSON");
    assert_eq!(report["package"], "knowledge");
    assert_eq!(report["serve_ready"], true);
    assert_eq!(
        report["registered"],
        serde_json::json!(["knowledge_ask", "knowledge_ingest", "knowledge_remove"])
    );
}
