use crate::util::epoch_secs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize)]
pub struct ModelCache {
    pub fetched_at: u64,
    pub domain: String,
    pub models: Vec<String>,
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
        let path = self.cache_path(domain_slug);
        if !path.exists() {
            return None;
        }

        let content = std::fs::read_to_string(&path).ok()?;
        let cache: ModelCache = serde_json::from_str(&content).ok()?;

        let now = epoch_secs();

        if now.saturating_sub(cache.fetched_at) < self.ttl.as_secs() {
            Some(cache.models)
        } else {
            None // TTL expired
        }
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
            return Err(format!("Gateway returned error status: {}", resp.status()));
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
