//! Local Gateway profiles.
//!
//! Profiles live in a SQLite database under the user configuration directory.
//! Tokens are stored as supplied; the file and directory are restricted to the
//! current user. Nothing is sent anywhere until a command uses a profile.

use common::ApiError;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::Row;
use std::path::{Path, PathBuf};

/// Stored Gateway connection.
#[derive(Debug, Clone)]
pub struct Profile {
    /// Operator-chosen name.
    pub name: String,
    /// Gateway base URL.
    pub url: String,
    /// Caller token.
    pub token: String,
    /// Whether this is the active profile.
    pub active: bool,
}

/// SQLite-backed profile store.
pub struct Store {
    pool: SqlitePool,
    path: PathBuf,
}

impl Store {
    /// Open the store in the configured CLI home.
    pub async fn open() -> Result<Self, ApiError> {
        Self::open_in(home()).await
    }

    /// Open a store in an explicit directory.
    pub async fn open_in(directory: impl AsRef<Path>) -> Result<Self, ApiError> {
        let directory = directory.as_ref();
        std::fs::create_dir_all(directory)
            .map_err(|error| storage("cannot create CLI directory", error))?;
        restrict(directory, 0o700)?;
        let path = directory.join("cli.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(|error| storage("cannot open CLI database", error))?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS gateways (
                 name TEXT PRIMARY KEY,
                 url TEXT NOT NULL,
                 token TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             )",
        )
        .execute(&pool)
        .await
        .map_err(|error| storage("cannot initialize CLI database", error))?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS settings (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             )",
        )
        .execute(&pool)
        .await
        .map_err(|error| storage("cannot initialize CLI database", error))?;
        restrict(&path, 0o600)?;
        Ok(Self { pool, path })
    }

    /// Insert or update a profile. Returns true when it was created.
    pub async fn add(&self, name: &str, url: &str, token: &str) -> Result<bool, ApiError> {
        validate_name(name)?;
        if url.trim().is_empty() || token.trim().is_empty() {
            return Err(ApiError::new(
                "cli.profile_invalid",
                "gateway URL and token must not be empty",
            ));
        }
        let now = common::now_ms();
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gateways WHERE name = ?")
            .bind(name)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| storage("cannot read CLI database", error))?;
        if exists == 0 {
            sqlx::query(
                "INSERT INTO gateways (name, url, token, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
            )
            .bind(name)
            .bind(url.trim_end_matches('/'))
            .bind(token)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(|error| storage("cannot save gateway profile", error))?;
        } else {
            sqlx::query(
                "UPDATE gateways SET url = ?2, token = ?3, updated_at = ?4 WHERE name = ?1",
            )
            .bind(name)
            .bind(url.trim_end_matches('/'))
            .bind(token)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(|error| storage("cannot save gateway profile", error))?;
        }
        if self.active().await?.is_none() {
            self.set_active(name).await?;
        }
        Ok(exists == 0)
    }

    /// All profiles, ordered by name.
    pub async fn list(&self) -> Result<Vec<Profile>, ApiError> {
        let active = self.active_name().await?;
        let rows = sqlx::query("SELECT name, url, token FROM gateways ORDER BY name")
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage("cannot read CLI database", error))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let name: String = row.get(0);
                Profile {
                    url: row.get(1),
                    token: row.get(2),
                    active: active.as_deref() == Some(name.as_str()),
                    name,
                }
            })
            .collect())
    }

    /// One profile by name.
    pub async fn get(&self, name: &str) -> Result<Profile, ApiError> {
        let active = self.active_name().await?;
        let row = sqlx::query("SELECT name, url, token FROM gateways WHERE name = ?1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| storage("cannot read CLI database", error))?;
        let Some(row) = row else {
            return Err(ApiError::new(
                "cli.profile_not_found",
                format!("gateway '{name}' is not registered; add it first"),
            ));
        };
        let stored_name: String = row.get(0);
        Ok(Profile {
            active: active.as_deref() == Some(stored_name.as_str()),
            name: stored_name,
            url: row.get(1),
            token: row.get(2),
        })
    }

    /// Active profile, if one is selected.
    pub async fn active(&self) -> Result<Option<Profile>, ApiError> {
        match self.active_name().await? {
            Some(name) => self.get(&name).await.map(Some),
            None => Ok(None),
        }
    }

    /// Select the active profile.
    pub async fn set_active(&self, name: &str) -> Result<(), ApiError> {
        self.get(name).await?;
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ('active_gateway', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
        )
        .bind(name)
        .execute(&self.pool)
        .await
        .map_err(|error| storage("cannot save active gateway", error))?;
        Ok(())
    }

    /// Database path, for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn active_name(&self) -> Result<Option<String>, ApiError> {
        sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = 'active_gateway'")
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| storage("cannot read CLI database", error))
    }
}

/// Hide most of a token for display.
pub fn mask(token: &str) -> String {
    let count = token.chars().count();
    if count <= 8 {
        return "********".into();
    }
    let head: String = token.chars().take(4).collect();
    let tail: String = token.chars().skip(count - 4).collect();
    format!("{head}…{tail}")
}

/// Default CLI home.
pub fn home() -> PathBuf {
    if let Some(path) = std::env::var_os("GOLDENBEE_CLI_HOME") {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("goldenbee")
}

fn validate_name(name: &str) -> Result<(), ApiError> {
    if name.trim().is_empty()
        || name.len() > 64
        || name
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(ApiError::new(
            "cli.profile_invalid",
            "gateway name must be 1-64 characters without whitespace",
        ));
    }
    Ok(())
}

fn restrict(path: &Path, mode: u32) -> Result<(), ApiError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|error| storage("cannot restrict CLI storage permissions", error))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

fn storage(message: &str, error: impl std::fmt::Display) -> ApiError {
    ApiError::new("cli.storage", format!("{message}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn profiles_round_trip_and_active_selection() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open_in(directory.path()).await.unwrap();
        assert!(store
            .add("local", "http://127.0.0.1:8080", "token-one")
            .await
            .unwrap());
        assert!(
            store.active().await.unwrap().is_some(),
            "first profile becomes active"
        );
        assert!(store
            .add("team", "http://127.0.0.1:8090", "token-two")
            .await
            .unwrap());
        assert!(!store
            .add("team", "http://127.0.0.1:8090", "token-two")
            .await
            .unwrap());
        store.set_active("team").await.unwrap();
        assert_eq!(store.active().await.unwrap().unwrap().name, "team");
        assert_eq!(store.list().await.unwrap().len(), 2);
        assert_eq!(
            store.get("local").await.unwrap().url,
            "http://127.0.0.1:8080"
        );
    }

    #[tokio::test]
    async fn missing_profile_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open_in(directory.path()).await.unwrap();
        assert_eq!(
            store.get("absent").await.unwrap_err().code,
            "cli.profile_not_found"
        );
    }

    #[test]
    fn token_display_is_masked() {
        assert_eq!(mask("short"), "********");
        assert_eq!(mask("abcdefghijkl"), "abcd…ijkl");
    }
}
