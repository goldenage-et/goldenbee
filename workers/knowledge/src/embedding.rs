//! Deterministic offline embedding adapter for the pinned native RAG pipeline.
//!
//! This is a hashing fixture, not a semantic production provider. It exists so
//! tests and offline operator development can exercise the real native
//! `RagPipeline` without credentials or network access. Production deployments
//! may enable a native embedding feature in a later slice; this package does
//! not implement an HTTP embedding client.

use adk_rust::rag::{EmbeddingProvider, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Fixed dimensionality of the hashing embedding.
pub const HASHING_DIMENSIONS: usize = 256;

/// Token-hashing embedding with L2 normalization.
#[derive(Debug, Clone, Copy, Default)]
pub struct HashingEmbeddingProvider;

impl HashingEmbeddingProvider {
    /// Construct the offline provider.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl EmbeddingProvider for HashingEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(embed_text(text))
    }

    fn dimensions(&self) -> usize {
        HASHING_DIMENSIONS
    }
}

fn embed_text(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0_f32; HASHING_DIMENSIONS];
    let mut tokens: BTreeMap<String, u32> = BTreeMap::new();
    for token in text
        .split(|character: char| !character.is_alphanumeric())
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        *tokens.entry(token.to_lowercase()).or_default() += 1;
    }
    for (token, count) in tokens {
        let digest = Sha256::digest(token.as_bytes());
        let bucket = u64::from_le_bytes(digest[..8].try_into().unwrap_or([0; 8])) as usize
            % HASHING_DIMENSIONS;
        let sign = if digest[8] & 1 == 0 { 1.0 } else { -1.0 };
        vector[bucket] += sign * count as f32;
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}
