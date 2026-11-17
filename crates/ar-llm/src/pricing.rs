use alloc::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "the public fields are the per-model price-table JSON shape; #[non_exhaustive] \
              would stop callers from building or destructuring a price by struct literal"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the serialized key order of the price table JSON; reordering \
              changes the serialized table"
)]
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
    /// Prices keyed by `<provider base URL>|<model>` or bare model name.
    #[serde(flatten)]
    models: BTreeMap<String, ModelPrice>,
}

impl PriceTable {
    #[inline]
    #[must_use = "the estimate is the only output; dropping it records no cost"]
    #[expect(
        clippy::float_arithmetic,
        reason = "a USD cost estimate from per-million-token f64 rates is inherently floating \
                  point; the result is reported, never used for exact accounting"
    )]
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
            (f64::from(input_tokens) * price.input
                + f64::from(output_tokens) * price.output
                + f64::from(embedding_tokens) * price.embedding)
                / 1_000_000.0,
        )
    }

    /// Replace or add every entry from `overrides`.
    fn extend(&mut self, overrides: PriceTable) {
        self.models.extend(overrides.models);
    }
}

#[derive(Debug, thiserror::Error)]
#[expect(
    clippy::error_impl_error,
    reason = "public error type of load_openai_price_table (used by ar-review); renaming it \
              changes the crate API"
)]
pub enum Error {
    #[error("decode price table at {path}: {source}")]
    Decode {
        path: String,
        source: serde_json::Error,
    },
    #[error("read price table at {path}: {source}")]
    Read { path: String, source: io::Error },
}

/// Key for a provider-qualified price entry.
fn price_key(provider_base_url: &str, model: &str) -> String {
    format!("{}|{}", provider_base_url.trim_end_matches('/'), model)
}

/// Built-in prices, overlaid with the JSON table at `path` when given.
///
/// # Errors
///
/// Returns [`Error::Read`] when the override file cannot be read and
/// [`Error::Decode`] when it is not a valid price table.
#[inline]
pub fn load_openai_price_table(path: Option<&Path>) -> Result<PriceTable, Error> {
    let mut table = default_openai_price_table();
    if let Some(override_path) = path {
        let body = fs::read_to_string(override_path).map_err(|source| Error::Read {
            path: override_path.display().to_string(),
            source,
        })?;
        let overrides: PriceTable =
            serde_json::from_str(&body).map_err(|source| Error::Decode {
                path: override_path.display().to_string(),
                source,
            })?;
        table.extend(overrides);
    }
    Ok(table)
}

#[inline]
#[must_use = "the default table is the only output"]
pub fn default_openai_price_table() -> PriceTable {
    PriceTable {
        models: BTreeMap::from([
            (
                "gpt-4o".to_owned(),
                ModelPrice {
                    input: 5.0,
                    output: 15.0,
                    embedding: 0.0,
                },
            ),
            (
                "gpt-4o-mini".to_owned(),
                ModelPrice {
                    input: 0.15,
                    output: 0.60,
                    embedding: 0.0,
                },
            ),
            (
                "text-embedding-3-small".to_owned(),
                ModelPrice {
                    input: 0.0,
                    output: 0.0,
                    embedding: 0.02,
                },
            ),
        ]),
    }
}
