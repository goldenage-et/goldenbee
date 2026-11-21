//! Local run source resolution.
//!
//! A Client may ask the runtime to read stdin, a text file, or an image. This
//! module resolves those sources into the bounded, transport-safe
//! [`common::RunInput`] *before* admission. Errors name a position, a limit, or
//! a display name; they never echo prompt text, credentials, image bytes, or a
//! full host path.

use common::{
    bytes_digest, ApiError, RunFile, RunImage, RunInput, RunSource, MAX_RUN_FILES,
    MAX_RUN_FILE_BYTES, MAX_RUN_IMAGES, MAX_RUN_IMAGE_BYTES, MAX_RUN_TEXT_BYTES,
};
use std::io::Read;

/// Resolve local input sources into bounded run input.
///
/// `stdin` is read only when a [`RunSource::Stdin`] part is present. A run may
/// select stdin at most once.
pub fn resolve_sources(sources: &[RunSource], stdin: &mut dyn Read) -> Result<RunInput, ApiError> {
    let mut text = String::new();
    let mut files = Vec::new();
    let mut images = Vec::new();
    let mut stdin_seen = false;

    for (index, source) in sources.iter().enumerate() {
        match source {
            RunSource::Text { text: value } => {
                push_text(&mut text, value, index)?;
            }
            RunSource::Stdin => {
                if stdin_seen {
                    return Err(ApiError::new(
                        "validation.run_source",
                        "stdin may be selected at most once",
                    )
                    .with("position", index.to_string()));
                }
                stdin_seen = true;
                let bytes = read_bounded(stdin, MAX_RUN_TEXT_BYTES, index, "stdin")?;
                let value = String::from_utf8(bytes).map_err(|_| {
                    ApiError::new("validation.run_source", "stdin is not valid UTF-8")
                        .with("position", index.to_string())
                })?;
                push_text(&mut text, &value, index)?;
            }
            RunSource::File { path, media_type } => {
                if files.len() >= MAX_RUN_FILES {
                    return Err(ApiError::new(
                        "validation.run_source",
                        "too many file sources in one run",
                    )
                    .with("limit", MAX_RUN_FILES.to_string()));
                }
                let bytes = read_path(path, MAX_RUN_FILE_BYTES, index)?;
                let value = String::from_utf8(bytes.clone()).map_err(|_| {
                    ApiError::new("validation.run_source", "a file source is not valid UTF-8")
                        .with("position", index.to_string())
                })?;
                let name = display_name(path);
                push_file(&mut text, &name, &value, index)?;
                files.push(RunFile {
                    name,
                    media_type: media_type
                        .clone()
                        .unwrap_or_else(|| "text/plain".to_string()),
                    bytes: bytes.len(),
                    digest: bytes_digest(&bytes),
                });
            }
            RunSource::Image { path, media_type } => {
                if images.len() >= MAX_RUN_IMAGES {
                    return Err(ApiError::new(
                        "validation.run_source",
                        "too many image sources in one run",
                    )
                    .with("limit", MAX_RUN_IMAGES.to_string()));
                }
                let bytes = read_path(path, MAX_RUN_IMAGE_BYTES, index)?;
                let media_type = image_media_type(path, media_type.as_deref(), index)?;
                if bytes.is_empty() {
                    return Err(
                        ApiError::new("validation.run_image", "an image source is empty")
                            .with("position", index.to_string()),
                    );
                }
                images.push(RunImage::from_bytes(
                    media_type,
                    &bytes,
                    Some(display_name(path)),
                ));
            }
        }
    }

    let input = RunInput {
        text,
        files,
        images,
    };
    input.validate()?;
    Ok(input)
}

fn push_text(text: &mut String, value: &str, index: usize) -> Result<(), ApiError> {
    if !text.is_empty() && !value.is_empty() {
        text.push('\n');
    }
    text.push_str(value);
    if text.len() > MAX_RUN_TEXT_BYTES {
        return Err(ApiError::new(
            "validation.run_input",
            "run input text exceeds the published bound",
        )
        .with("limit_bytes", MAX_RUN_TEXT_BYTES.to_string())
        .with("position", index.to_string()));
    }
    Ok(())
}

fn push_file(text: &mut String, name: &str, value: &str, index: usize) -> Result<(), ApiError> {
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&format!("<file name=\"{name}\">\n{value}\n</file>"));
    if text.len() > MAX_RUN_TEXT_BYTES {
        return Err(ApiError::new(
            "validation.run_input",
            "run input text exceeds the published bound",
        )
        .with("limit_bytes", MAX_RUN_TEXT_BYTES.to_string())
        .with("position", index.to_string()));
    }
    Ok(())
}

fn read_bounded(
    reader: &mut dyn Read,
    limit: usize,
    index: usize,
    label: &str,
) -> Result<Vec<u8>, ApiError> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            ApiError::new(
                "validation.run_source",
                format!("{label} could not be read"),
            )
            .with("position", index.to_string())
        })?;
    if bytes.len() > limit {
        return Err(ApiError::new(
            "validation.run_source",
            format!("{label} exceeds the published bound"),
        )
        .with("limit_bytes", limit.to_string())
        .with("position", index.to_string()));
    }
    Ok(bytes)
}

fn read_path(path: &str, limit: usize, index: usize) -> Result<Vec<u8>, ApiError> {
    let metadata = std::fs::metadata(path).map_err(|_| {
        ApiError::new(
            "validation.run_source",
            "a run input source could not be opened",
        )
        .with("position", index.to_string())
    })?;
    if !metadata.is_file() {
        return Err(ApiError::new(
            "validation.run_source",
            "a run input source is not a regular file",
        )
        .with("position", index.to_string()));
    }
    if metadata.len() > limit as u64 {
        return Err(ApiError::new(
            "validation.run_source",
            "a run input source exceeds the published bound",
        )
        .with("limit_bytes", limit.to_string())
        .with("position", index.to_string()));
    }
    let file = std::fs::File::open(path).map_err(|_| {
        ApiError::new(
            "validation.run_source",
            "a run input source could not be opened",
        )
        .with("position", index.to_string())
    })?;
    read_bounded(&mut &file, limit, index, "the run input source")
}

fn display_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("input")
        .to_string()
}

fn image_media_type(path: &str, declared: Option<&str>, index: usize) -> Result<String, ApiError> {
    if let Some(declared) = declared {
        return Ok(declared.to_string());
    }
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let media_type = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => {
            return Err(ApiError::new(
                "validation.run_image",
                "an image source needs a supported media type",
            )
            .with("position", index.to_string()))
        }
    };
    Ok(media_type.to_string())
}
