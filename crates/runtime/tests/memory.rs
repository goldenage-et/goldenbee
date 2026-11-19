//! Ticket 29: persistent, scoped native ADK memory behavior.
//!
//! These tests use the pinned `adk_memory::SqliteMemoryService` through the
//! runtime adapter. They observe retrieval, provenance, erasure, scoped denial,
//! cache invalidation, and truthful storage failures without asserting private
//! SQLite tables.

use common::Principal;
use runtime::native::NativeMemory;
use std::path::Path;
use std::sync::Arc;

fn principal(id: &str, namespace: &str) -> Principal {
    Principal::new(id, namespace, &["*"])
}

async fn open(dir: impl AsRef<Path>) -> Arc<NativeMemory> {
    NativeMemory::open("general", dir)
        .await
        .expect("native memory opens")
}

fn texts(items: &[common::MemoryItem]) -> Vec<&str> {
    items.iter().map(|item| item.text.as_str()).collect()
}

#[tokio::test]
async fn remembered_fact_survives_reopen_with_source_and_scope() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let alice = principal("alice", "local");
    let first = open(dir.path()).await;
    let stored = first
        .remember(&alice, "local", "conversation/42", "alpha secret")
        .await
        .expect("memory is stored");

    assert_eq!(stored.namespace, "local");
    assert_eq!(stored.source, "conversation/42");
    assert_eq!(stored.text, "alpha secret");
    assert!(!stored.id.is_empty());
    assert!(stored.valid_from > 0);
    drop(first);

    // A new connection represents a Worker restart against the same data
    // directory. The native SQLite file is the canonical store.
    let second = open(dir.path()).await;
    let found = second
        .search(&alice, "local", "alpha", 10)
        .await
        .expect("memory searches after reopen");
    assert_eq!(texts(&found), vec!["alpha secret"]);
    assert_eq!(found[0].id, stored.id);
    assert_eq!(found[0].source, "conversation/42");
    assert_eq!(found[0].namespace, "local");

    let bytes = std::fs::metadata(dir.path().join(runtime::native::MEMORY_STORE_FILE))
        .expect("native memory file exists")
        .len();
    assert!(bytes > 0, "the native backend persisted real bytes");
}

#[tokio::test]
async fn identical_text_is_isolated_by_namespace_and_principal() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let memory = open(dir.path()).await;
    let local_alice = principal("alice", "local");
    let local_bob = principal("bob", "local");
    let other_alice = principal("alice", "other");

    let a = memory
        .remember(&local_alice, "local", "alice-source", "same phrase")
        .await
        .expect("alice stores");
    let b = memory
        .remember(&local_bob, "local", "bob-source", "same phrase")
        .await
        .expect("bob stores");
    let c = memory
        .remember(&other_alice, "other", "other-source", "same phrase")
        .await
        .expect("other namespace stores");

    assert_ne!(a.id, b.id);
    assert_ne!(a.id, c.id);

    for _ in 0..2 {
        let a_hits = memory
            .search(&local_alice, "local", "same", 10)
            .await
            .expect("alice reads");
        assert_eq!(texts(&a_hits), vec!["same phrase"]);
        assert_eq!(a_hits[0].id, a.id);
        assert_eq!(a_hits[0].source, "alice-source");

        let b_hits = memory
            .search(&local_bob, "local", "same", 10)
            .await
            .expect("bob reads");
        assert_eq!(texts(&b_hits), vec!["same phrase"]);
        assert_eq!(b_hits[0].id, b.id);
        assert_eq!(b_hits[0].source, "bob-source");

        let c_hits = memory
            .search(&other_alice, "other", "same", 10)
            .await
            .expect("other namespace reads");
        assert_eq!(texts(&c_hits), vec!["same phrase"]);
        assert_eq!(c_hits[0].id, c.id);
        assert_eq!(c_hits[0].source, "other-source");
    }

    let denied = memory
        .search(&local_alice, "other", "same", 10)
        .await
        .expect_err("namespace mismatch is refused before retrieval");
    assert_eq!(denied.code, "scope.namespace_denied");
}

#[tokio::test]
async fn unauthorized_erase_is_denied_and_cannot_remove_foreign_memory() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let memory = open(dir.path()).await;
    let alice = principal("alice", "local");
    let bob = principal("bob", "local");
    let stored = memory
        .remember(&alice, "local", "alice-source", "alpha secret")
        .await
        .expect("alice stores");

    let denied = memory
        .erase(&bob, &stored.id)
        .await
        .expect_err("another principal cannot erase the memory");
    assert_eq!(denied.code, "memory.not_found");

    let hits = memory
        .search(&alice, "local", "alpha", 10)
        .await
        .expect("alice still reads");
    assert_eq!(hits[0].id, stored.id);

    memory
        .erase(&alice, &stored.id)
        .await
        .expect("the owner erases the memory");
    assert!(memory
        .search(&alice, "local", "alpha", 10)
        .await
        .expect("search after erasure")
        .is_empty());

    drop(memory);
    let reopened = open(dir.path()).await;
    assert!(
        reopened
            .search(&alice, "local", "alpha", 10)
            .await
            .expect("search after reopen")
            .is_empty(),
        "erasure is durable"
    );
}

#[tokio::test]
async fn erase_invalidates_cached_results() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let memory = open(dir.path()).await;
    let alice = principal("alice", "local");
    let stored = memory
        .remember(&alice, "local", "cache-source", "cacheable fact")
        .await
        .expect("memory is stored");

    // Populate the adapter cache.
    assert_eq!(
        texts(
            &memory
                .search(&alice, "local", "cacheable", 5)
                .await
                .expect("first search")
        ),
        vec!["cacheable fact"]
    );
    memory
        .erase(&alice, &stored.id)
        .await
        .expect("erase succeeds");
    assert!(
        memory
            .search(&alice, "local", "cacheable", 5)
            .await
            .expect("cached search after erase")
            .is_empty(),
        "authorized erase must invalidate the retrieval cache"
    );
}

#[tokio::test]
async fn document_derived_memory_keeps_source_restrictions() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let memory = open(dir.path()).await;
    let alice = principal("alice", "local");
    let bob = principal("bob", "local");

    memory
        .remember_with_allowed(
            &alice,
            "local",
            "report.pdf#doc-1",
            "restricted figure",
            vec!["alice".into()],
        )
        .await
        .expect("derived memory is stored");
    memory
        .remember_with_allowed(
            &alice,
            "local",
            "report.pdf#doc-1",
            "club only figure",
            vec!["carol".into()],
        )
        .await
        .expect("another derived memory is stored");

    // Search twice so the cache path is exercised as well as the direct path.
    for _ in 0..2 {
        let alice_hits = memory
            .search(&alice, "local", "figure", 10)
            .await
            .expect("alice reads");
        assert_eq!(texts(&alice_hits), vec!["restricted figure"]);
        assert_eq!(alice_hits[0].source, "report.pdf#doc-1");

        let bob_hits = memory
            .search(&bob, "local", "figure", 10)
            .await
            .expect("bob reads");
        assert!(
            bob_hits.is_empty(),
            "a derived copy cannot widen its source restriction: {bob_hits:?}"
        );
    }
}

#[tokio::test]
async fn unavailable_or_corrupt_storage_is_reported() {
    let root = tempfile::tempdir().expect("temporary directory");

    // A regular file blocks the data directory the native store needs.
    let blocked = root.path().join("blocked");
    std::fs::write(&blocked, "not a directory").expect("blocking file");
    let error = NativeMemory::open("general", &blocked)
        .await
        .expect_err("unavailable storage fails");
    assert_eq!(error.code, "memory.storage_unavailable");

    // A file that is not SQLite is never replaced silently.
    let corrupt = root.path().join("corrupt");
    std::fs::create_dir_all(&corrupt).expect("data directory");
    std::fs::write(
        corrupt.join(runtime::native::MEMORY_STORE_FILE),
        "not sqlite",
    )
    .expect("corrupt store");
    let error = NativeMemory::open("general", &corrupt)
        .await
        .expect_err("corrupt storage fails");
    assert_eq!(error.code, "memory.storage_corrupt");
}
