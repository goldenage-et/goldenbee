# Knowledge Worker Package

One independently versioned Worker executable (`knowledge`) that composes the
shared runtime with the pinned native ADK `adk-rag` pipeline. Agents, prompts,
skills, source roots, and storage are operator configuration; nothing is
resolved against the installation directory or the current working directory.

## Source adapter contract

The package-owned adapter is `FileSource` in `src/source.rs`.

- A URI is absolute or relative to the **first** configured `source_roots`
  entry. Roots are resolved against the configuration file's directory at
  startup and canonicalized once.
- The canonical target must stay inside a configured root and must be a regular
  file. Symlink targets are refused (`knowledge.source_symlink`).
- Supported media types are UTF-8 `text/plain` (`.txt`) and `text/markdown`
  (`.md`). Other extensions return `knowledge.unsupported_format` with the
  detected media type; no bytes are indexed.
- The adapter checks the file length before reading and never reads more than
  `max_document_bytes + 1`. Oversize input returns
  `knowledge.document_too_large` with the configured limit. Nothing is silently
  truncated.
- Invalid UTF-8 returns `knowledge.partial_extraction` with the first invalid
  byte offset. No partial text is indexed.
- NUL-containing input is treated as binary and returns
  `knowledge.unsupported_format`.

## Native retrieval backend

- Chunking: pinned `adk_rag::FixedSizeChunker` with the configured
  `chunk_size` and zero overlap, so deterministic chunk ids cover the source
  exactly. Coverage is validated before indexing; a gap returns
  `knowledge.chunk_gap`.
- Embeddings: package-local `HashingEmbeddingProvider`, a deterministic offline
  fixture. It is not claimed as semantic production retrieval. It exercises the
  real native pipeline without credentials or network.
- Vector store: pinned `adk_rag::InMemoryVectorStore`, one collection per
  namespace. Chunks are a derived cache and are rebuilt from durable source
  bytes and job records after restart. This is not claimed as durable vector
  storage.
- Retrieval: `adk_rag::RagPipeline::query`; namespace, job status, current
  source digest, and allowed-principal checks run **before** any chunk text is
  returned.

## Durable provenance and jobs

`<data_dir>/knowledge/jobs.json` holds one record per source revision:
`job_id`, namespace, canonical URI, exact source digest (also the source
revision), title, media type, allowed principals, creator, status, chunk count,
collection, derived memory ids, and timestamps. Source bytes live at
`<data_dir>/knowledge/sources/sha256/<hex>`.

Writes use a temporary file, `fsync`, and an atomic rename. The dedup key is
`sha256(namespace | canonical_uri | source_digest)`, so an identical retry
returns the existing job/converged result without chunking again. A changed
byte creates a new revision and job, and the superseded revision becomes a
tombstone whose chunks and derived memory are removed after the new revision
indexes successfully.

## Permissions

The package declares `task:submit`, `session:write`, `artifact:write`, and per-tool
`knowledge:ingest`, `knowledge:read`, and `knowledge:remove`. Namespace and
principal come from the native session identity bound at admission, or from the
operator namespace for the upstream development launcher's fixed `user`
identity. A caller never supplies another namespace.

Every returned chunk is checked against the job ACL and the live source digest.
Denied candidates contribute no text, URI, or count. Explicit removal writes a
tombstone and deletes both index chunks and derived native memory rows; deleting
or changing a source without an explicit removal makes later retrieval report
`source_missing` or `source_changed` in `stale`.

## Configuration

See `config/knowledge.toml` and `config/knowledge/`. Required setting:

```toml
[settings]
source_roots = "knowledge/sources"
```

Optional settings: `max_document_bytes` (default 16777216), `chunk_size`
(default 512), `max_chunks` (default 10000), `top_k` (default 5), `max_top_k`
(default 20), and `deny_source_contains` (package policy enforced by the native
before-tool callback as well as the adapter).

## Native launcher modes

```sh
knowledge probe --json
knowledge capabilities --config config/knowledge.toml --json
knowledge run --config config/knowledge.toml --input "knowledge-ask:citation"
knowledge dev --config config/knowledge.toml      # upstream native console
knowledge serve --config config/knowledge.toml    # native web server and A2A
```

The full `adk_cli::Launcher --serve` surface is not enabled by this workspace;
`dev` is the upstream `adk_runner::Launcher` console and `serve` is the native
`adk_server` composition. Run the package tests for commands and observed
results.
