//! Durable Gateway lifecycle operations.
//!
//! One record per caller-supplied operation id retains the request digest and
//! the terminal outcome. A repeated command after a lost acknowledgment or a
//! Gateway restart converges on the original host operation instead of
//! creating another instance or approval. Host credentials are never stored.

use common::{ApiError, HostOperationOutcome};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

const LIFECYCLE_FILE: &str = "lifecycle.json";
const LIFECYCLE_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Record {
    pub(crate) operation_id: String,
    pub(crate) kind: String,
    pub(crate) host: String,
    pub(crate) instance: String,
    pub(crate) worker: String,
    pub(crate) digest: String,
    /// `in_flight`, `succeeded`, `failed`, or `unknown`.
    pub(crate) status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) outcome: Option<HostOperationOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<ApiError>,
    pub(crate) updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Document {
    schema: u32,
    #[serde(default)]
    operations: BTreeMap<String, Record>,
}

pub(crate) struct Store {
    path: PathBuf,
    document: Document,
}

impl Store {
    pub(crate) fn open(directory: &Path) -> Result<Self, ApiError> {
        std::fs::create_dir_all(directory).map_err(|_| {
            ApiError::new(
                "storage.unavailable",
                "Gateway lifecycle directory could not be created",
            )
        })?;
        let path = directory.join(LIFECYCLE_FILE);
        let document = match std::fs::read(&path) {
            Ok(bytes) => {
                let document: Document = serde_json::from_slice(&bytes).map_err(|_| {
                    ApiError::new("storage.corrupt", "Gateway lifecycle state is malformed")
                })?;
                if document.schema > LIFECYCLE_SCHEMA {
                    return Err(ApiError::new(
                        "storage.schema_unsupported",
                        "Gateway lifecycle state is newer than this process",
                    ));
                }
                document
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Document {
                schema: LIFECYCLE_SCHEMA,
                operations: BTreeMap::new(),
            },
            Err(_) => {
                return Err(ApiError::new(
                    "storage.unavailable",
                    "Gateway lifecycle state could not be read",
                ))
            }
        };
        Ok(Self { path, document })
    }

    pub(crate) fn get(&self, operation: &str) -> Option<Record> {
        self.document.operations.get(operation).cloned()
    }

    pub(crate) fn put(&mut self, record: Record) -> Result<(), ApiError> {
        self.document
            .operations
            .insert(record.operation_id.clone(), record);
        self.write()
    }

    fn write(&self) -> Result<(), ApiError> {
        let bytes = serde_json::to_vec(&self.document).map_err(|_| {
            ApiError::new(
                "storage.unavailable",
                "Gateway lifecycle state could not be serialized",
            )
        })?;
        let temporary = self.path.with_extension("tmp");
        std::fs::write(&temporary, bytes).map_err(|_| {
            ApiError::new(
                "storage.unavailable",
                "Gateway lifecycle state could not be written",
            )
        })?;
        std::fs::rename(&temporary, &self.path).map_err(|_| {
            ApiError::new(
                "storage.unavailable",
                "Gateway lifecycle state could not be committed",
            )
        })
    }
}

pub(crate) struct Lifecycle {
    store: Mutex<Store>,
}

impl Lifecycle {
    pub(crate) fn open(directory: &Path) -> Result<Self, ApiError> {
        Ok(Self {
            store: Mutex::new(Store::open(directory)?),
        })
    }

    pub(crate) fn get(&self, operation: &str) -> Option<Record> {
        self.lock().get(operation)
    }

    pub(crate) fn put(&self, record: Record) -> Result<(), ApiError> {
        self.lock().put(record)
    }

    fn lock(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|error| error.into_inner())
    }
}
