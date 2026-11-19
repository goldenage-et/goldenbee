//! Real-byte ingestion, citation, chunk coverage, restart, and removal tests.

mod fixture;

use fixture::{knowledge_payload, payload_from_text, run};

fn write_source(fixture: &fixture::Fixture, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = fixture.sources.join(name);
    std::fs::write(&path, bytes).expect("source bytes");
    path
}

fn jobs(fixture: &fixture::Fixture) -> serde_json::Value {
    let path = fixture.data_dir.join("knowledge").join("jobs.json");
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).expect("jobs.json is JSON"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            serde_json::json!({"jobs": []})
        }
        Err(error) => panic!("jobs.json is unreadable: {error}"),
    }
}

fn indexed_jobs(fixture: &fixture::Fixture) -> Vec<serde_json::Value> {
    jobs(fixture)["jobs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|job| job["status"] == "indexed")
        .collect()
}

#[test]
fn ingests_real_bytes_and_cites_source_digest() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fixture::write(temp.path(), fixture::Options::default());
    let content = b"# Guide\n\nThe release canary is KNOWLEDGE-CANARY-31.\n";
    let source = write_source(&fixture, "guide.md", content);

    let ingested = run(&fixture, &format!("knowledge-ingest:{}", source.display()));
    assert!(
        ingested.status.success(),
        "ingest failed: {}",
        String::from_utf8_lossy(&ingested.stderr)
    );
    let ingest = knowledge_payload(&ingested);
    assert_eq!(ingest["status"], "indexed");
    assert_eq!(
        ingest["source_digest"].as_str().unwrap(),
        common::bytes_digest(content)
    );
    assert_eq!(ingest["coverage"]["uncovered_bytes"], 0);

    let asked = run(&fixture, "knowledge-ask:KNOWLEDGE-CANARY-31");
    assert!(
        asked.status.success(),
        "ask failed: {}",
        String::from_utf8_lossy(&asked.stderr)
    );
    let answer = knowledge_payload(&asked);
    let citations = answer["citations"].as_array().expect("citations array");
    assert!(!citations.is_empty(), "no citations: {answer}");
    let first = &citations[0];
    assert_eq!(
        first["source_digest"].as_str().unwrap(),
        common::bytes_digest(content)
    );
    assert!(first["text"]
        .as_str()
        .unwrap()
        .contains("KNOWLEDGE-CANARY-31"));
    assert_eq!(
        first["source_uri"].as_str().unwrap(),
        source.display().to_string()
    );
    let token = first["token"].as_str().unwrap();
    assert!(token.contains("#"), "token has no chunk id: {token}");
    assert!(token.contains("@sha256:"), "token has no digest: {token}");
}

#[test]
fn chunking_has_no_silent_gaps() {
    let temp = tempfile::tempdir().unwrap();
    let mut options = fixture::Options::default();
    options.settings.push(("chunk_size".into(), "256".into()));
    let fixture = fixture::write(temp.path(), options);
    let mut content = String::new();
    for index in 0..40 {
        content.push_str(&format!(
            "Paragraph {index} contains sentence number {index} and enough words to cross a chunk boundary. \
             It stays inside the source and must be covered exactly once.\n\n"
        ));
    }
    let source = write_source(&fixture, "long.md", content.as_bytes());
    let ingested = run(&fixture, &format!("knowledge-ingest:{}", source.display()));
    let ingest = knowledge_payload(&ingested);
    assert_eq!(ingest["status"], "indexed", "{ingest}");
    assert!(
        ingest["chunk_count"].as_u64().unwrap() > 1,
        "expected multiple chunks: {ingest}"
    );
    assert_eq!(
        ingest["coverage"]["source_bytes"],
        content.len() as u64,
        "{ingest}"
    );
    assert_eq!(ingest["coverage"]["covered_bytes"], content.len() as u64);
    assert_eq!(ingest["coverage"]["uncovered_bytes"], 0);
}

#[test]
fn oversized_unsupported_and_partial_are_actionable() {
    let temp = tempfile::tempdir().unwrap();
    let mut options = fixture::Options::default();
    options
        .settings
        .push(("max_document_bytes".into(), "1024".into()));
    let fixture = fixture::write(temp.path(), options);

    let oversized = write_source(&fixture, "large.txt", &vec![b'x'; 2048]);
    let output = run(
        &fixture,
        &format!("knowledge-ingest:{}", oversized.display()),
    );
    let value = knowledge_payload(&output);
    assert_eq!(value["error"], "knowledge.document_too_large", "{value}");
    assert_eq!(value["limit_bytes"], "1024");

    let pdf = write_source(&fixture, "manual.pdf", b"%PDF-1.7\nnot text\n");
    let output = run(&fixture, &format!("knowledge-ingest:{}", pdf.display()));
    let value = knowledge_payload(&output);
    assert_eq!(value["error"], "knowledge.unsupported_format", "{value}");
    assert_eq!(value["detected_media_type"], "application/pdf");

    let invalid = write_source(&fixture, "invalid.txt", &[0x66, 0x6f, 0xff, 0xfe]);
    let output = run(&fixture, &format!("knowledge-ingest:{}", invalid.display()));
    let value = knowledge_payload(&output);
    assert_eq!(value["error"], "knowledge.partial_extraction", "{value}");
    assert_eq!(value["invalid_offset"], "2");

    assert!(
        indexed_jobs(&fixture).is_empty(),
        "failed extractions must not leave indexed jobs"
    );

    let symlink = fixture.sources.join("link.txt");
    std::os::unix::fs::symlink(&invalid, &symlink).unwrap();
    let output = run(&fixture, &format!("knowledge-ingest:{}", symlink.display()));
    let value = knowledge_payload(&output);
    assert_eq!(value["error"], "knowledge.source_symlink", "{value}");

    let outside = temp.path().join("outside.txt");
    std::fs::write(&outside, b"outside source").unwrap();
    let output = run(&fixture, &format!("knowledge-ingest:{}", outside.display()));
    let value = knowledge_payload(&output);
    assert!(
        value["error"] == "knowledge.source_outside_roots"
            || value["error"] == "knowledge.source_not_found"
            || value["error"] == "knowledge.source_symlink",
        "outside root must be denied: {value}"
    );
}

#[test]
fn retry_deduplicates_source_revision() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fixture::write(temp.path(), fixture::Options::default());
    let source = write_source(&fixture, "dedup.md", b"dedup canary ALPHA\n");
    let worker = fixture::spawn(&fixture.config, &fixture.root, "dev-token");

    let first = payload_from_text(
        worker.run_as(
            "dev-token",
            &format!("knowledge-ingest:{}", source.display()),
            "dedup-1",
        )["output"]
            .as_str()
            .unwrap(),
    );
    let second = payload_from_text(
        worker.run_as(
            "dev-token",
            &format!("knowledge-ingest:{}", source.display()),
            "dedup-2",
        )["output"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(first["job_id"], second["job_id"], "{first} {second}");
    assert_eq!(second["converged"], true);
    assert_eq!(first["source_digest"], second["source_digest"]);
    assert_eq!(indexed_jobs(&fixture).len(), 1);

    std::fs::write(&source, b"dedup canary BETA\n").unwrap();
    let third = payload_from_text(
        worker.run_as(
            "dev-token",
            &format!("knowledge-ingest:{}", source.display()),
            "dedup-3",
        )["output"]
            .as_str()
            .unwrap(),
    );
    assert_ne!(first["job_id"], third["job_id"]);
    assert_ne!(first["source_digest"], third["source_digest"]);
    assert_eq!(
        indexed_jobs(&fixture).len(),
        1,
        "only the current revision stays indexed"
    );
    let all = jobs(&fixture)["jobs"].as_array().unwrap().clone();
    assert_eq!(all.len(), 2, "both revisions retain provenance");
    assert_eq!(
        all.iter().filter(|job| job["status"] == "removed").count(),
        1,
        "the superseded revision is a tombstone"
    );
}

#[test]
fn restart_preserves_provenance_and_reindexes() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fixture::write(temp.path(), fixture::Options::default());
    let source = write_source(
        &fixture,
        "restart.md",
        b"restart canary KNOWLEDGE-RESTART-31\n",
    );

    let ingested = knowledge_payload(&run(
        &fixture,
        &format!("knowledge-ingest:{}", source.display()),
    ));
    let job_id = ingested["job_id"].as_str().unwrap().to_string();
    let digest = ingested["source_digest"].as_str().unwrap().to_string();

    let stored = jobs(&fixture);
    let record = stored["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["job_id"] == job_id)
        .expect("job survives restart");
    assert_eq!(record["source_digest"], digest);
    assert_eq!(record["status"], "indexed");
    assert!(fixture
        .data_dir
        .join("knowledge")
        .join("sources")
        .join("sha256")
        .join(digest.trim_start_matches("sha256:"))
        .exists());

    let asked = knowledge_payload(&run(&fixture, "knowledge-ask:KNOWLEDGE-RESTART-31"));
    let citation = &asked["citations"].as_array().unwrap()[0];
    assert_eq!(citation["document_id"], job_id);
    assert_eq!(citation["source_digest"], digest);
    assert!(citation["text"]
        .as_str()
        .unwrap()
        .contains("KNOWLEDGE-RESTART-31"));
}

#[test]
fn source_removal_and_stale_index_are_observable() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fixture::write(temp.path(), fixture::Options::default());
    let source = write_source(
        &fixture,
        "remove.md",
        b"removal canary KNOWLEDGE-REMOVE-31\n",
    );
    knowledge_payload(&run(
        &fixture,
        &format!("knowledge-ingest:{}", source.display()),
    ));
    let removed = knowledge_payload(&run(
        &fixture,
        &format!("knowledge-remove:{}", source.display()),
    ));
    assert_eq!(removed["removed"][0]["status"], "removed", "{removed}");
    let after = knowledge_payload(&run(&fixture, "knowledge-ask:KNOWLEDGE-REMOVE-31"));
    assert_eq!(after["results"], 0, "{after}");
    let tombstone = jobs(&fixture)["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["status"] == "removed")
        .cloned()
        .expect("removal tombstone");
    assert!(tombstone["removed_at"].as_i64().unwrap() > 0);

    let stale_source = write_source(
        &fixture,
        "stale.md",
        b"stale canary KNOWLEDGE-STALE-31 original\n",
    );
    knowledge_payload(&run(
        &fixture,
        &format!("knowledge-ingest:{}", stale_source.display()),
    ));
    std::fs::write(&stale_source, b"stale canary KNOWLEDGE-STALE-31 changed\n").unwrap();
    let stale = knowledge_payload(&run(&fixture, "knowledge-ask:KNOWLEDGE-STALE-31"));
    assert_eq!(stale["results"], 0, "{stale}");
    assert_eq!(stale["stale"][0]["reason"], "source_changed", "{stale}");

    std::fs::remove_file(&stale_source).unwrap();
    let missing = knowledge_payload(&run(&fixture, "knowledge-ask:KNOWLEDGE-STALE-31"));
    assert_eq!(missing["results"], 0, "{missing}");
    assert_eq!(missing["stale"][0]["reason"], "source_missing", "{missing}");

    // An explicit authorized removal must still leave a tombstone after the
    // source file itself is gone.
    let gone = write_source(
        &fixture,
        "gone.md",
        b"gone canary KNOWLEDGE-GONE-31 original\n",
    );
    knowledge_payload(&run(
        &fixture,
        &format!("knowledge-ingest:{}", gone.display()),
    ));
    std::fs::remove_file(&gone).unwrap();
    let removed_gone = knowledge_payload(&run(
        &fixture,
        &format!("knowledge-remove:{}", gone.display()),
    ));
    assert_eq!(removed_gone["removed"][0]["status"], "removed");
    let after_gone = knowledge_payload(&run(&fixture, "knowledge-ask:KNOWLEDGE-GONE-31"));
    assert_eq!(after_gone["results"], 0, "{after_gone}");
}
