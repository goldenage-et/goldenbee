//! Runtime result and error helpers.

pub(crate) use common::ApiError as Error;

/// Result type used by runtime operations.
pub type Result<T> = std::result::Result<T, common::ApiError>;
