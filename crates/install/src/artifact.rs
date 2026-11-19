//! OCI image-layout artifact reader and writer.
//!
//! The selected package format is an offline OCI image layout. Manifest and
//! blob digests bind every byte, while an ed25519 signature over the metadata
//! config digest establishes provenance. Layers are raw payload blobs with
//! registered media types; no second archive format is introduced.

use crate::error::InstallError;
use base64::Engine;
use common::{bytes_digest, ReleaseMetadata, SeedAsset};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// OCI layout version written by the tool.
pub const LAYOUT_VERSION: &str = "1.0.0";
/// OCI image index media type.
pub const INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";
/// OCI image manifest media type.
pub const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
/// Goldenbee release artifact type.
pub const ARTIFACT_TYPE: &str = "application/vnd.goldenbee.worker.release.v1";
/// Signed metadata config media type.
pub const METADATA_MEDIA_TYPE: &str = "application/vnd.goldenbee.worker.metadata.v1+json";
/// Raw executable layer media type.
pub const BINARY_MEDIA_TYPE: &str = "application/vnd.goldenbee.worker.binary.v1";
/// Source descriptor layer media type.
pub const SOURCE_MEDIA_TYPE: &str = "application/vnd.goldenbee.worker.source.v1+json";
/// Optional seed-asset catalog layer media type.
pub const SEED_MEDIA_TYPE: &str = "application/vnd.goldenbee.worker.seed.v1+json";
/// Index annotation carrying the detached ed25519 signature.
pub const SIGNATURE_ANNOTATION: &str = "org.goldenbee.signature.ed25519";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Layout {
    image_layout_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    schema_version: u32,
    #[serde(default)]
    media_type: String,
    manifests: Vec<Descriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    #[serde(default)]
    media_type: String,
    #[serde(default)]
    artifact_type: String,
    config: Descriptor,
    layers: Vec<Descriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    #[serde(default)]
    media_type: String,
    digest: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    annotations: std::collections::BTreeMap<String, String>,
}

/// One optional seed asset supplied by the package author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedFile {
    /// Relative path below the instance root.
    pub path: String,
    /// Raw asset bytes.
    pub bytes: Vec<u8>,
}

impl SeedFile {
    /// Construct one seed asset from a relative path and its bytes.
    pub fn new(path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
        }
    }
}

/// Digests produced by writing one layout.
#[derive(Debug, Clone)]
pub struct ArtifactDigests {
    /// Manifest digest used as the immutable release identity.
    pub artifact_digest: String,
    /// Metadata config digest.
    pub metadata_digest: String,
    /// Binary layer digest.
    pub binary_digest: String,
    /// Source layer digest.
    pub source_digest: String,
    /// Seed catalog digest when the release carries seeds.
    pub seed_digest: Option<String>,
}

/// Verified bytes and metadata from one offline layout.
#[derive(Debug, Clone)]
pub struct VerifiedLayout {
    /// Parsed release metadata.
    pub metadata: ReleaseMetadata,
    /// Exact metadata bytes that were signed.
    pub metadata_bytes: Vec<u8>,
    /// Raw executable bytes.
    pub binary: Vec<u8>,
    /// Raw source descriptor bytes.
    pub source: Vec<u8>,
    /// Index manifest digest.
    pub artifact_digest: String,
    /// Detached signature annotation.
    pub signature: String,
    /// Raw manifest bytes.
    pub manifest_bytes: Vec<u8>,
    /// Verified seed assets carried by the release.
    pub seed_files: Vec<SeedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SeedCatalog {
    schema: u32,
    entries: Vec<SeedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SeedEntry {
    path: String,
    digest: String,
    /// Standard base64 encoded bytes.
    content: String,
}

/// Write one offline OCI image layout. The caller supplies the signature over
/// [`common::bytes_digest`] of the encoded metadata.
pub fn write_layout(
    root: &Path,
    metadata: &ReleaseMetadata,
    binary: &[u8],
    source: &[u8],
    signature: &str,
) -> Result<ArtifactDigests, InstallError> {
    write_layout_with_seeds(root, metadata, binary, source, &[], signature)
}

/// Write one offline OCI image layout with verified optional seed assets.
///
/// The seed catalog is one JSON layer. Each entry carries the raw bytes so the
/// installer can materialize the exact signed content; metadata and catalog
/// must agree on every path and digest.
pub fn write_layout_with_seeds(
    root: &Path,
    metadata: &ReleaseMetadata,
    binary: &[u8],
    source: &[u8],
    seeds: &[SeedFile],
    signature: &str,
) -> Result<ArtifactDigests, InstallError> {
    metadata
        .package
        .validate()
        .map_err(|error| InstallError::new("install.metadata_invalid", error.message))?;
    metadata
        .validate_protocol_range()
        .map_err(|error| InstallError::new("install.metadata_invalid", error.message))?;
    if metadata.source_digest != bytes_digest(source) {
        return Err(InstallError::new(
            "install.metadata_invalid",
            "source digest does not match the supplied source bytes",
        ));
    }
    let source_descriptor = validate_source(source)?;
    if !metadata.source_revision.is_empty()
        && metadata.source_revision != source_descriptor.revision
    {
        return Err(InstallError::new(
            "install.metadata_invalid",
            "release source revision does not match the source descriptor",
        ));
    }
    if metadata.binary_digest != bytes_digest(binary) {
        return Err(InstallError::new(
            "install.metadata_invalid",
            "binary digest does not match the supplied binary bytes",
        ));
    }
    let metadata_bytes = encode_metadata(metadata)?;
    let metadata_digest = bytes_digest(&metadata_bytes);
    let binary_digest = bytes_digest(binary);
    let source_digest = bytes_digest(source);

    let (seed_catalog, seed_digest) = build_seed_catalog(metadata, seeds)?;
    let mut layers = vec![
        Descriptor {
            media_type: BINARY_MEDIA_TYPE.into(),
            digest: binary_digest.clone(),
            size: binary.len() as u64,
            annotations: Default::default(),
        },
        Descriptor {
            media_type: SOURCE_MEDIA_TYPE.into(),
            digest: source_digest.clone(),
            size: source.len() as u64,
            annotations: Default::default(),
        },
    ];
    if let (Some(catalog), Some(digest)) = (&seed_catalog, &seed_digest) {
        layers.push(Descriptor {
            media_type: SEED_MEDIA_TYPE.into(),
            digest: digest.clone(),
            size: catalog.len() as u64,
            annotations: Default::default(),
        });
    }

    let manifest = Manifest {
        schema_version: 2,
        media_type: MANIFEST_MEDIA_TYPE.into(),
        artifact_type: ARTIFACT_TYPE.into(),
        config: Descriptor {
            media_type: METADATA_MEDIA_TYPE.into(),
            digest: metadata_digest.clone(),
            size: metadata_bytes.len() as u64,
            annotations: Default::default(),
        },
        layers,
    };
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|error| InstallError::new("install.layout_invalid", error.to_string()))?;
    let artifact_digest = bytes_digest(&manifest_bytes);

    let mut annotations = std::collections::BTreeMap::new();
    annotations.insert(SIGNATURE_ANNOTATION.into(), signature.into());
    annotations.insert(
        "org.goldenbee.package".into(),
        metadata.package.name.clone(),
    );
    annotations.insert(
        "org.goldenbee.version".into(),
        metadata.package.version.clone(),
    );
    let index = Index {
        schema_version: 2,
        media_type: INDEX_MEDIA_TYPE.into(),
        manifests: vec![Descriptor {
            media_type: MANIFEST_MEDIA_TYPE.into(),
            digest: artifact_digest.clone(),
            size: manifest_bytes.len() as u64,
            annotations,
        }],
    };
    let index_bytes = serde_json::to_vec_pretty(&index)
        .map_err(|error| InstallError::new("install.layout_invalid", error.to_string()))?;

    let blob_dir = root.join("blobs").join("sha256");
    std::fs::create_dir_all(&blob_dir).map_err(storage_error)?;
    write_blob(root, &metadata_bytes, &metadata_digest)?;
    write_blob(root, binary, &binary_digest)?;
    write_blob(root, source, &source_digest)?;
    if let (Some(catalog), Some(digest)) = (&seed_catalog, &seed_digest) {
        write_blob(root, catalog, digest)?;
    }
    write_blob(root, &manifest_bytes, &artifact_digest)?;
    std::fs::write(root.join("index.json"), index_bytes).map_err(storage_error)?;
    std::fs::write(
        root.join("oci-layout"),
        serde_json::to_vec_pretty(&Layout {
            image_layout_version: LAYOUT_VERSION.into(),
        })
        .map_err(|error| InstallError::new("install.layout_invalid", error.to_string()))?,
    )
    .map_err(storage_error)?;

    Ok(ArtifactDigests {
        artifact_digest,
        metadata_digest,
        binary_digest,
        source_digest,
        seed_digest,
    })
}

fn build_seed_catalog(
    metadata: &ReleaseMetadata,
    seeds: &[SeedFile],
) -> Result<(Option<Vec<u8>>, Option<String>), InstallError> {
    if metadata.seed_assets.is_empty() && seeds.is_empty() {
        return Ok((None, None));
    }
    let mut declared: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for asset in &metadata.seed_assets {
        validate_seed_path(&asset.path)?;
        if declared.insert(&asset.path, &asset.digest).is_some() {
            return Err(InstallError::new(
                "install.seed_invalid",
                "metadata declares the same seed path more than once",
            ));
        }
    }
    let mut entries = Vec::new();
    let mut supplied = std::collections::BTreeSet::new();
    for seed in seeds {
        validate_seed_path(&seed.path)?;
        if !supplied.insert(seed.path.clone()) {
            return Err(InstallError::new(
                "install.seed_invalid",
                "the same seed asset was supplied more than once",
            ));
        }
        let digest = bytes_digest(&seed.bytes);
        match declared.get(seed.path.as_str()) {
            Some(declared_digest) if *declared_digest == digest => {}
            Some(_) => {
                return Err(InstallError::new(
                    "install.seed_invalid",
                    format!("seed '{}' does not match its signed digest", seed.path),
                ));
            }
            None => {
                return Err(InstallError::new(
                    "install.seed_invalid",
                    format!(
                        "seed '{}' is not declared by the signed metadata",
                        seed.path
                    ),
                ));
            }
        }
        entries.push(SeedEntry {
            path: seed.path.clone(),
            digest,
            content: base64::engine::general_purpose::STANDARD.encode(&seed.bytes),
        });
    }
    if supplied.len() != declared.len() {
        return Err(InstallError::new(
            "install.seed_missing",
            "signed metadata declares a seed asset that was not supplied",
        ));
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let catalog = SeedCatalog { schema: 1, entries };
    let bytes = serde_json::to_vec(&catalog)
        .map_err(|error| InstallError::new("install.layout_invalid", error.to_string()))?;
    let digest = bytes_digest(&bytes);
    Ok((Some(bytes), Some(digest)))
}

fn validate_seed_path(path: &str) -> Result<(), InstallError> {
    let valid = !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !valid {
        return Err(InstallError::new(
            "install.seed_invalid",
            format!("seed path '{path}' must stay relative to the instance root"),
        ));
    }
    Ok(())
}

fn read_seed_catalog(
    root: &Path,
    manifest: &Manifest,
    metadata: &ReleaseMetadata,
) -> Result<Vec<SeedFile>, InstallError> {
    let seed_layer = manifest
        .layers
        .iter()
        .find(|layer| layer.media_type == SEED_MEDIA_TYPE);
    if metadata.seed_assets.is_empty() {
        if seed_layer.is_some() {
            return Err(InstallError::new(
                "install.seed_invalid",
                "release carries a seed layer that its metadata does not declare",
            ));
        }
        return Ok(Vec::new());
    }
    let layer = seed_layer.ok_or_else(|| {
        InstallError::new(
            "install.seed_missing",
            "release declares seed assets but carries no seed catalog",
        )
    })?;
    let bytes = read_blob(root, &layer.digest, layer.size)?;
    if bytes_digest(&bytes) != layer.digest {
        return Err(InstallError::new(
            "install.layer_digest_mismatch",
            "seed catalog bytes do not match the manifest digest",
        ));
    }
    let catalog: SeedCatalog = serde_json::from_slice(&bytes)
        .map_err(|_| InstallError::new("install.seed_invalid", "seed catalog is malformed"))?;
    if catalog.schema != 1 {
        return Err(InstallError::new(
            "install.seed_invalid",
            "seed catalog schema is unsupported",
        ));
    }
    let mut declared: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for asset in &metadata.seed_assets {
        validate_seed_path(&asset.path)?;
        if declared.insert(&asset.path, &asset.digest).is_some() {
            return Err(InstallError::new(
                "install.seed_invalid",
                "metadata declares the same seed path more than once",
            ));
        }
    }
    let mut files = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for entry in &catalog.entries {
        validate_seed_path(&entry.path)?;
        if !seen.insert(entry.path.clone()) {
            return Err(InstallError::new(
                "install.seed_invalid",
                "seed catalog contains the same path more than once",
            ));
        }
        let content = base64::engine::general_purpose::STANDARD
            .decode(&entry.content)
            .map_err(|_| {
                InstallError::new("install.seed_invalid", "seed content is not valid base64")
            })?;
        let digest = bytes_digest(&content);
        if digest != entry.digest {
            return Err(InstallError::new(
                "install.seed_invalid",
                format!(
                    "seed '{}' content does not match its catalog digest",
                    entry.path
                ),
            ));
        }
        match declared.get(entry.path.as_str()) {
            Some(declared_digest) if *declared_digest == digest => {}
            _ => {
                return Err(InstallError::new(
                    "install.seed_invalid",
                    format!("seed '{}' does not match the signed metadata", entry.path),
                ));
            }
        }
        files.push(SeedFile {
            path: entry.path.clone(),
            bytes: content,
        });
    }
    if files.len() != metadata.seed_assets.len() {
        return Err(InstallError::new(
            "install.seed_missing",
            "seed catalog does not carry every declared asset",
        ));
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

pub(crate) fn write_seed_files(directory: &Path, seeds: &[SeedFile]) -> Result<(), InstallError> {
    if seeds.is_empty() {
        return Ok(());
    }
    let root = directory.join("seeds");
    for seed in seeds {
        validate_seed_path(&seed.path)?;
        let path = root.join(&seed.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(storage_error)?;
        }
        std::fs::write(&path, &seed.bytes).map_err(storage_error)?;
    }
    Ok(())
}

/// Seed assets recorded by an installed release.
pub fn seed_assets(seeds: &[SeedFile]) -> Vec<SeedAsset> {
    seeds
        .iter()
        .map(|seed| SeedAsset {
            path: seed.path.clone(),
            digest: bytes_digest(&seed.bytes),
        })
        .collect()
}

/// Encode release metadata deterministically. Pretty JSON is not used because
/// whitespace changes would change the signed digest.
pub fn encode_metadata(metadata: &ReleaseMetadata) -> Result<Vec<u8>, InstallError> {
    serde_json::to_vec(metadata)
        .map_err(|error| InstallError::new("install.metadata_invalid", error.to_string()))
}

/// Verify every digest in an offline layout and parse its signed metadata.
pub fn read_layout(root: &Path) -> Result<VerifiedLayout, InstallError> {
    let layout_bytes = std::fs::read(root.join("oci-layout")).map_err(|_| {
        InstallError::new(
            "install.layout_invalid",
            "artifact does not contain an OCI layout marker",
        )
    })?;
    let layout: Layout = serde_json::from_slice(&layout_bytes)
        .map_err(|_| InstallError::new("install.layout_invalid", "layout marker is malformed"))?;
    if layout.image_layout_version != LAYOUT_VERSION {
        return Err(InstallError::new(
            "install.layout_invalid",
            "unsupported OCI layout version",
        ));
    }
    let index_bytes = std::fs::read(root.join("index.json"))
        .map_err(|_| InstallError::new("install.layout_invalid", "artifact has no index"))?;
    let index: Index = serde_json::from_slice(&index_bytes)
        .map_err(|_| InstallError::new("install.layout_invalid", "index is malformed"))?;
    if index.schema_version != 2 || index.manifests.len() != 1 {
        return Err(InstallError::new(
            "install.layout_invalid",
            "index must contain exactly one manifest",
        ));
    }
    let descriptor = &index.manifests[0];
    let manifest_bytes = read_blob(root, &descriptor.digest, descriptor.size)?;
    if bytes_digest(&manifest_bytes) != descriptor.digest {
        return Err(InstallError::new(
            "install.manifest_digest_mismatch",
            "manifest bytes do not match the index digest",
        ));
    }
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| InstallError::new("install.layout_invalid", "manifest is malformed"))?;
    if manifest.schema_version != 2 || manifest.artifact_type != ARTIFACT_TYPE {
        return Err(InstallError::new(
            "install.layout_invalid",
            "manifest is not a Goldenbee Worker release",
        ));
    }
    if manifest.config.media_type != METADATA_MEDIA_TYPE {
        return Err(InstallError::new(
            "install.layout_invalid",
            "metadata config media type is unsupported",
        ));
    }
    let metadata_bytes = read_blob(root, &manifest.config.digest, manifest.config.size)?;
    if bytes_digest(&metadata_bytes) != manifest.config.digest {
        return Err(InstallError::new(
            "install.config_digest_mismatch",
            "metadata bytes do not match the manifest digest",
        ));
    }
    let metadata: ReleaseMetadata = serde_json::from_slice(&metadata_bytes)
        .map_err(|_| InstallError::new("install.metadata_invalid", "metadata is malformed"))?;
    if metadata.schema != common::RELEASE_METADATA_SCHEMA {
        return Err(InstallError::new(
            "install.metadata_invalid",
            "release metadata schema is unsupported",
        ));
    }
    metadata
        .package
        .validate()
        .map_err(|error| InstallError::new("install.metadata_invalid", error.message))?;
    metadata
        .validate_protocol_range()
        .map_err(|error| InstallError::new("install.metadata_invalid", error.message))?;

    let binary_descriptor = single_layer(&manifest, BINARY_MEDIA_TYPE, "binary")?;
    let source_descriptor = single_layer(&manifest, SOURCE_MEDIA_TYPE, "source")?;
    if manifest
        .layers
        .iter()
        .any(|layer| layer.media_type == SEED_MEDIA_TYPE)
        && manifest
            .layers
            .iter()
            .filter(|layer| layer.media_type == SEED_MEDIA_TYPE)
            .count()
            > 1
    {
        return Err(InstallError::new(
            "install.layout_invalid",
            "release declares more than one seed-asset layer",
        ));
    }
    let binary = read_blob(root, &binary_descriptor.digest, binary_descriptor.size)?;
    if bytes_digest(&binary) != binary_descriptor.digest
        || metadata.binary_digest != binary_descriptor.digest
    {
        return Err(InstallError::new(
            "install.layer_digest_mismatch",
            "binary bytes do not match the signed digest",
        ));
    }
    let source = read_blob(root, &source_descriptor.digest, source_descriptor.size)?;
    if bytes_digest(&source) != source_descriptor.digest
        || metadata.source_digest != source_descriptor.digest
    {
        return Err(InstallError::new(
            "install.layer_digest_mismatch",
            "source bytes do not match the signed digest",
        ));
    }
    let source_descriptor = validate_source(&source)?;
    if !metadata.source_revision.is_empty()
        && metadata.source_revision != source_descriptor.revision
    {
        return Err(InstallError::new(
            "install.metadata_invalid",
            "release source revision does not match the source descriptor",
        ));
    }
    for layer in &manifest.layers {
        if !matches!(
            layer.media_type.as_str(),
            BINARY_MEDIA_TYPE | SOURCE_MEDIA_TYPE | SEED_MEDIA_TYPE
        ) {
            return Err(InstallError::new(
                "install.layout_invalid",
                "release contains an unsupported layer media type",
            ));
        }
    }
    let signature = descriptor
        .annotations
        .get(SIGNATURE_ANNOTATION)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            InstallError::new(
                "install.signature_invalid",
                "artifact does not carry a detached signature",
            )
        })?
        .clone();
    let seed_files = read_seed_catalog(root, &manifest, &metadata)?;

    Ok(VerifiedLayout {
        metadata,
        metadata_bytes,
        binary,
        source,
        artifact_digest: descriptor.digest.clone(),
        signature,
        manifest_bytes,
        seed_files,
    })
}

fn single_layer<'a>(
    manifest: &'a Manifest,
    media_type: &str,
    label: &str,
) -> Result<&'a Descriptor, InstallError> {
    let mut matches = manifest
        .layers
        .iter()
        .filter(|layer| layer.media_type == media_type);
    let layer = matches.next().ok_or_else(|| {
        InstallError::new(
            "install.layout_invalid",
            format!("release does not contain a {label} layer"),
        )
    })?;
    if matches.next().is_some() {
        return Err(InstallError::new(
            "install.layout_invalid",
            format!("release contains more than one {label} layer"),
        ));
    }
    Ok(layer)
}

fn validate_source(source: &[u8]) -> Result<SourceDescriptor, InstallError> {
    let source: SourceDescriptor = serde_json::from_slice(source).map_err(|_| {
        InstallError::new("install.metadata_invalid", "source descriptor is malformed")
    })?;
    if source.revision.trim().is_empty() || !is_digest(&source.tree) {
        return Err(InstallError::new(
            "install.metadata_invalid",
            "source descriptor lacks a revision or tree digest",
        ));
    }
    Ok(source)
}

#[derive(Debug, Deserialize)]
struct SourceDescriptor {
    revision: String,
    tree: String,
}

fn write_blob(root: &Path, bytes: &[u8], digest: &str) -> Result<PathBuf, InstallError> {
    let hex = digest_hex(digest)?;
    let path = root.join("blobs").join("sha256").join(hex);
    std::fs::write(&path, bytes).map_err(storage_error)?;
    Ok(path)
}

fn read_blob(root: &Path, digest: &str, _size: u64) -> Result<Vec<u8>, InstallError> {
    let hex = digest_hex(digest)?;
    let path = root.join("blobs").join("sha256").join(hex);
    std::fs::read(&path).map_err(|_| {
        InstallError::new(
            "install.layout_invalid",
            "artifact references a missing blob",
        )
    })
}

fn digest_hex(digest: &str) -> Result<&str, InstallError> {
    digest
        .strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| {
            InstallError::new(
                "install.layout_invalid",
                "artifact digest is not a sha256 digest",
            )
        })
}

/// Whether a value is a `sha256:` digest.
pub fn is_digest(value: &str) -> bool {
    digest_hex(value).is_ok()
}

fn storage_error(error: std::io::Error) -> InstallError {
    InstallError::new(
        "install.storage_unavailable",
        format!("artifact layout could not be written: {error}"),
    )
}
