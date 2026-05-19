use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub embedding: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PriceTable {
    #[serde(flatten)]
    models: BTreeMap<String, ModelPrice>,
}

impl PriceTable {
    pub fn estimate_usage_usd(
        &self,
        provider_base_url: &str,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
        embedding_tokens: u32,
    ) -> Option<f64> {
        let price = self
            .models
            .get(&price_key(provider_base_url, model))
            .or_else(|| self.models.get(model))?;
        Some(
            (input_tokens as f64 * price.input
                + output_tokens as f64 * price.output
                + embedding_tokens as f64 * price.embedding)
                / 1_000_000.0,
        )
    }

    fn extend(&mut self, overrides: PriceTable) {
        self.models.extend(overrides.models);
    }
}

fn price_key(provider_base_url: &str, model: &str) -> String {
    format!("{}|{}", provider_base_url.trim_end_matches('/'), model)
}

pub fn load_openai_price_table(path: Option<&Path>) -> Result<PriceTable, Error> {
    let mut table = default_openai_price_table();
    if let Some(path) = path {
        let body = std::fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.display().to_string(),
            source,
        })?;
        let overrides: PriceTable =
            serde_json::from_str(&body).map_err(|source| Error::Decode {
                path: path.display().to_string(),
                source,
            })?;
        table.extend(overrides);
    }
    Ok(table)
}

pub fn default_openai_price_table() -> PriceTable {
    PriceTable {
        models: BTreeMap::from([
            (
                "gpt-4o".to_string(),
                ModelPrice {
                    input: 5.0,
                    output: 15.0,
                    embedding: 0.0,
                },
            ),
            (
                "gpt-4o-mini".to_string(),
                ModelPrice {
                    input: 0.15,
                    output: 0.60,
                    embedding: 0.0,
                },
            ),
            (
                "text-embedding-3-small".to_string(),
                ModelPrice {
                    input: 0.0,
                    output: 0.0,
                    embedding: 0.02,
                },
            ),
        ]),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("read price table at {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("decode price table at {path}: {source}")]
    Decode {
        path: String,
        source: serde_json::Error,
    },
}
