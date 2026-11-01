use crate::util::epoch_secs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize)]
pub struct ModelCache {
    pub fetched_at: u64,
    pub domain: String,
    pub models: Vec<String>,
}

/// Whether a name the user wants to write is in the list the provider returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCheck {
    Listed,
    NotListed,
}

/// One comparison, used by every surface that writes a model id.
pub fn check_model(list: &[String], name: &str) -> ModelCheck {
    if list.iter().any(|model| model == name) {
        ModelCheck::Listed
    } else {
        ModelCheck::NotListed
    }
}

/// The message a refused credential carries, so a caller can re-ask the key
/// instead of offering to save an endpoint that cannot work.
const KEY_REJECTED: &str = "the gateway rejected the key";

/// True when `/models` refused the credential rather than the endpoint.
pub fn key_rejected(error: &str) -> bool {
    error.starts_with(KEY_REJECTED)
}

/// A non-success status as the error the wizard prints: 401/403 name the key,
/// every other status may still be the URL.
fn status_error(status: reqwest::StatusCode) -> String {
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        format!("{KEY_REJECTED}: {status}")
    } else {
        format!("Gateway returned error status: {status}")
    }
}

pub struct ModelManager {
    cache_dir: PathBuf,
    ttl: Duration,
}

impl ModelManager {
    pub fn new(workspace_root: &Path) -> Self {
        Self {
            cache_dir: workspace_root.join(".seavx/cache"),
            ttl: Duration::from_secs(24 * 3600),
        }
    }

    /// Extract clean domain/slug for cache filename: .seavx/cache/models-[domain].json
    pub fn domain_slug(base_url: &str) -> String {
        let clean = base_url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split(['/', ':', '?'])
            .next()
            .unwrap_or("default");

        clean
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn cache_path(&self, domain_slug: &str) -> PathBuf {
        self.cache_dir.join(format!("models-{domain_slug}.json"))
    }

    /// Load models from cache if still valid within TTL
    pub fn get_cached_models(&self, domain_slug: &str) -> Option<Vec<String>> {
        let cache = self.read_cache(domain_slug)?;
        let now = epoch_secs();

        if now.saturating_sub(cache.fetched_at) < self.ttl.as_secs() {
            Some(cache.models)
        } else {
            None // TTL expired
        }
    }

    /// Load the cached models whatever their age. The picker prefers a stale
    /// list over none, while `fetch_or_cache_models` still refreshes on TTL
    /// expiry: the two questions are different and so are the two methods.
    pub fn get_cached_models_allow_stale(&self, domain_slug: &str) -> Option<Vec<String>> {
        self.read_cache(domain_slug).map(|cache| cache.models)
    }

    fn read_cache(&self, domain_slug: &str) -> Option<ModelCache> {
        let content = std::fs::read_to_string(self.cache_path(domain_slug)).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Save fetched models into cache file
    pub fn save_cache(&self, domain_slug: &str, models: &[String]) -> Result<(), String> {
        let _ = std::fs::create_dir_all(&self.cache_dir);
        let path = self.cache_path(domain_slug);

        let now = epoch_secs();

        let cache = ModelCache {
            fetched_at: now,
            domain: domain_slug.to_string(),
            models: models.to_vec(),
        };

        let json = serde_json::to_string_pretty(&cache)
            .map_err(|e| format!("Serialization error: {e}"))?;

        // Same atomic writer as every other file this process owns: a reader
        // never sees a half-written cache.
        crate::atomic_file::write_atomic(
            &path,
            json.as_bytes(),
            crate::atomic_file::ModePolicy::Preserve,
        )
        .map_err(|e| format!("Failed to write cache {}: {e}", path.display()))?;

        Ok(())
    }

    /// Fetch live models directly bypassing cache and update local cache file
    pub async fn refresh_remote_models(
        &self,
        base_url: &str,
        api_key: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let domain_slug = Self::domain_slug(base_url);

        // Endpoint: standard OpenAI/Anthropic gateway /models
        let mut endpoint = base_url.trim_end_matches('/').to_string();
        if !endpoint.ends_with("/models") {
            endpoint.push_str("/models");
        }

        tracing::info!(endpoint = %endpoint, "Refreshing live models from remote gateway");

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

        let mut req = client.get(&endpoint);
        if let Some(key) = api_key.filter(|k| !k.is_empty()) {
            req = req.header("Authorization", format!("Bearer {key}"));
            req = req.header("x-api-key", key);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(status_error(resp.status()));
        }

        let json_val: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse JSON response: {e}"))?;

        let mut model_list = Vec::new();
        if let Some(arr) = json_val.get("data").and_then(|d| d.as_array()) {
            for item in arr {
                if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                    model_list.push(id.to_string());
                }
            }
        } else if let Some(arr) = json_val.as_array() {
            for item in arr {
                if let Some(s) = item.as_str() {
                    model_list.push(s.to_string());
                } else if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                    model_list.push(id.to_string());
                }
            }
        }

        if model_list.is_empty() {
            return Err("No models discovered in remote response".to_string());
        }

        model_list.sort();
        // A cache that cannot be written costs one extra request next time; it
        // must not fail a refresh that already succeeded.
        if let Err(error) = self.save_cache(&domain_slug, &model_list) {
            tracing::warn!(%error, "model cache not written");
        }
        Ok(model_list)
    }

    /// Fetch models from remote /models endpoint or use cache
    pub async fn fetch_or_cache_models(
        &self,
        base_url: &str,
        api_key: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let domain_slug = Self::domain_slug(base_url);

        if let Some(cached) = self.get_cached_models(&domain_slug) {
            tracing::info!(domain = %domain_slug, "Loaded models from local cache");
            return Ok(cached);
        }

        self.refresh_remote_models(base_url, api_key).await
    }
}

/// Where the public catalog lives: context windows for the model ids a gateway
/// lists but never describes.
const CATALOG_URL: &str = "https://models.dev/api.json";
/// A window changes when a model ships, not when a session starts.
const CATALOG_TTL_SECS: u64 = 7 * 24 * 3600;

/// Model-name tail → window, installed from the cache and swapped after a
/// refresh. `BTreeMap` so the cache file keeps a stable order.
static WINDOWS: OnceLock<RwLock<BTreeMap<String, i32>>> = OnceLock::new();

fn windows() -> &'static RwLock<BTreeMap<String, i32>> {
    WINDOWS.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Model-name tail → the effort levels the catalog publishes, in menu order.
static EFFORTS: OnceLock<RwLock<BTreeMap<String, Vec<String>>>> = OnceLock::new();

fn efforts() -> &'static RwLock<BTreeMap<String, Vec<String>>> {
    EFFORTS.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// The key a model id is looked up by: `rv/vendor/DeepSeek-V4.1-Flash-1017` and
/// `amd/deepseek-v4.1-flash-1017:free` are one model to the catalog.
fn window_key(id: &str) -> String {
    let tail = id.rsplit('/').next().unwrap_or(id);
    tail.split(':').next().unwrap_or(tail).trim().to_lowercase()
}

/// The window the catalog publishes for this id. `None` means it does not know
/// the model, and the caller falls back to the family table.
pub fn catalog_context(model: &str) -> Option<i32> {
    windows().read().ok()?.get(&window_key(model)).copied()
}

/// The effort levels the catalog publishes for this id, in menu order. `None`
/// means it publishes none for the model, and the caller falls back to the
/// static list.
pub fn catalog_efforts(model: &str) -> Option<Vec<String>> {
    efforts().read().ok()?.get(&window_key(model)).cloned()
}

#[derive(Debug, Serialize, Deserialize)]
struct ContextCatalog {
    fetched_at: u64,
    windows: BTreeMap<String, i32>,
    /// Absent from a cache written before the catalog kept effort levels.
    #[serde(default)]
    efforts: BTreeMap<String, Vec<String>>,
}

fn catalog_path(root: &Path) -> PathBuf {
    root.join(".seavx/cache/contexts.json")
}

/// Install the cached catalog and report whether a refresh is due. A cache that
/// cannot be read is no catalog: the family table still answers.
pub fn load_context_catalog(root: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(catalog_path(root)) else {
        return true;
    };
    let Ok(cache) = serde_json::from_str::<ContextCatalog>(&text) else {
        return true;
    };
    if let Ok(mut installed) = windows().write() {
        *installed = cache.windows;
    }
    if let Ok(mut installed) = efforts().write() {
        *installed = cache.efforts.clone();
    }
    // A cache written before the catalog kept effort levels makes the picker
    // fall back to the static list; one refresh fills it.
    cache.efforts.is_empty() || epoch_secs().saturating_sub(cache.fetched_at) >= CATALOG_TTL_SECS
}

/// Fetch the catalog and keep only what answers a lookup. Callers spawn this:
/// nothing waits on it, and a failed download leaves the previous numbers alone.
pub async fn refresh_context_catalog(root: &Path) -> Result<usize, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;
    let resp = client
        .get(CATALOG_URL)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("catalog returned {}", resp.status()));
    }
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse catalog: {e}"))?;
    let fetched = catalog_windows(&json);
    if fetched.is_empty() {
        return Err("catalog carried no context limits".to_string());
    }
    let levels = catalog_effort_levels(&json);

    let cache = ContextCatalog {
        fetched_at: epoch_secs(),
        windows: fetched.clone(),
        efforts: levels.clone(),
    };
    let text = serde_json::to_string(&cache).map_err(|e| format!("Serialization error: {e}"))?;
    if let Err(error) = crate::atomic_file::write_atomic(
        &catalog_path(root),
        text.as_bytes(),
        crate::atomic_file::ModePolicy::Preserve,
    ) {
        // The numbers are good for this run; only the next start pays again.
        tracing::warn!(%error, "context catalog not written");
    }
    let count = fetched.len();
    if let Ok(mut installed) = windows().write() {
        *installed = fetched;
    }
    if let Ok(mut installed) = efforts().write() {
        *installed = levels;
    }
    Ok(count)
}

/// Keep `limit.context` per model name. Providers disagree on one model's
/// window — a free tier clamps it — so the largest wins: the budget guesses
/// high, and a request the provider refuses is already retried once against a
/// smaller one.
fn catalog_windows(json: &serde_json::Value) -> BTreeMap<String, i32> {
    let mut windows = BTreeMap::new();
    let Some(providers) = json.as_object() else {
        return windows;
    };
    for provider in providers.values() {
        let Some(models) = provider.get("models").and_then(|models| models.as_object()) else {
            continue;
        };
        for (id, model) in models {
            let Some(context) = model
                .get("limit")
                .and_then(|limit| limit.get("context"))
                .and_then(|context| context.as_i64())
                .filter(|context| *context > 0 && *context <= i32::MAX as i64)
            else {
                continue;
            };
            let window = context as i32;
            windows
                .entry(window_key(id))
                .and_modify(|known: &mut i32| *known = (*known).max(window))
                .or_insert(window);
        }
    }
    windows
}

/// The seven levels a request can carry, ascending. `default` is not one.
fn effort_rank(level: &str) -> Option<u8> {
    match level {
        "none" => Some(0),
        "minimal" => Some(1),
        "low" => Some(2),
        "medium" => Some(3),
        "high" => Some(4),
        "xhigh" => Some(5),
        "max" => Some(6),
        _ => None,
    }
}

/// Keep the effort levels each model accepts, in menu order. Providers clamp one
/// model differently, so the union wins: the picker offers every level some
/// gateway takes, while a saved level is never refused for being absent here.
fn catalog_effort_levels(json: &serde_json::Value) -> BTreeMap<String, Vec<String>> {
    let mut levels: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Some(providers) = json.as_object() else {
        return levels;
    };
    for provider in providers.values() {
        let Some(models) = provider.get("models").and_then(|models| models.as_object()) else {
            continue;
        };
        for (id, model) in models {
            let Some(options) = model.get("reasoning_options").and_then(|o| o.as_array()) else {
                continue;
            };
            let entry = levels.entry(window_key(id)).or_default();
            for option in options
                .iter()
                .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("effort"))
            {
                for value in option
                    .get("values")
                    .and_then(|values| values.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.as_str())
                    .filter(|value| effort_rank(value).is_some())
                {
                    if !entry.iter().any(|known| known == value) {
                        entry.push(value.to_string());
                    }
                }
            }
        }
    }
    levels.retain(|_, list| !list.is_empty());
    for list in levels.values_mut() {
        list.sort_by_key(|level| effort_rank(level).unwrap_or(u8::MAX));
    }
    levels
}

#[cfg(test)]
#[path = "../tests/core/models.rs"]
mod tests;
