//! `seavx setup providers`: prompt for endpoint, key, model and reasoning effort.

use super::prompt;

/// Redraw the wizard as a form between steps; piped output stays linear.
fn screen(config_path: &std::path::Path, rows: &[(&str, String)]) {
    use std::io::{self, IsTerminal, Write};
    if io::stdout().is_terminal() {
        let _ = crossterm::execute!(
            io::stdout(),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
            crossterm::cursor::MoveTo(0, 0)
        );
    }
    println!("seavx setup providers");
    println!("config: {}\n", config_path.display());
    for (label, value) in rows {
        println!("{:<10} {}", format!("{label}:"), value);
    }
    if !rows.is_empty() {
        println!();
    }
    let _ = io::stdout().flush();
}

/// Persist the wizard's answers: the whole config, plus the key only when the
/// user typed one. `seavx setup` is one of the two surfaces allowed to repair an
/// unparsable file; the malformed bytes are kept beside it.
pub fn persist(
    path: &std::path::Path,
    config: &crate::agent::engine::AgentConfig,
    typed_key: Option<&str>,
) -> Result<Option<std::path::PathBuf>, String> {
    let preserved = crate::cli::config::write_agent_config_repairing(path, config)?;
    if let Some(key) = typed_key.filter(|key| !key.is_empty()) {
        crate::cli::config::set_api_key(path, key)?;
    }
    Ok(preserved)
}

pub async fn providers() -> anyhow::Result<()> {
    use std::io::{self, IsTerminal, Write};

    let config_path = crate::util::global_config_path()
        .ok_or_else(|| anyhow::anyhow!("home directory is not set, cannot locate config"))?;

    // A malformed file is replaced, matching AgentConfig::load's fallback to defaults.
    let mut config: crate::agent::engine::AgentConfig = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|content| toml::from_str(&content).ok())
        .unwrap_or_default();

    let mask = |key: &str| {
        if key.is_empty() {
            "none".to_string()
        } else {
            "****".to_string()
        }
    };

    let model_mgr = crate::agent::models::ModelManager::new(&std::path::PathBuf::from("."));
    let mut available_models: Vec<String> = Vec::new();
    let mut url_default = config
        .base_url
        .clone()
        .unwrap_or_else(|| crate::agent::engine::DEFAULT_BASE_URL.to_string());
    let mut key_default = config.api_key.clone().unwrap_or_default();

    // Ask the endpoint for its model list, so a wrong URL or key fails here instead of in the TUI.
    let (base_url, api_key) = loop {
        screen(&config_path, &[]);
        println!(
            "Accepts an OpenAI- or Anthropic-compatible endpoint; /v1/ is added when missing.\n"
        );
        let url = prompt::line("Base URL", &url_default)?;
        // Checked before the key is asked, so a wrong scheme or a typo does not
        // cost a network round trip and a second prompt.
        let normalized = match crate::agent::engine::validate_base_url(&url) {
            Ok(normalized) => normalized,
            Err(error) => {
                println!("invalid base URL: {error}\n");
                url_default = url;
                continue;
            }
        };

        screen(&config_path, &[("base_url", normalized.clone())]);
        let key = prompt::secret("API key (optional)", &key_default)?;

        screen(
            &config_path,
            &[("base_url", normalized.clone()), ("api key", mask(&key))],
        );
        print!("checking /models ... ");
        io::stdout().flush()?;
        match model_mgr
            .refresh_remote_models(&normalized, Some(key.as_str()))
            .await
        {
            Ok(models) => {
                println!("ok, {} models returned", models.len());
                available_models = models;
                break (normalized, key);
            }
            Err(e) => {
                println!("failed: {e}");
                let keep = prompt::line("Save this endpoint anyway? [y/N]", "")?;
                if keep.eq_ignore_ascii_case("y") {
                    break (normalized, key);
                }
                // A rejected attempt is forgotten, so the retry starts from blank fields.
                url_default.clear();
                key_default.clear();
            }
        }
    };

    // One normalization, used by the live form and by the write below.
    let (stored_url, adapters) = crate::agent::engine::normalize_base_url(&base_url);
    let protocols = adapters
        .iter()
        .map(|a| format!("{a:?}"))
        .collect::<Vec<_>>()
        .join(", ");

    let mut rows = vec![
        ("base_url", base_url.clone()),
        ("api key", mask(&api_key)),
        ("models", format!("{} available", available_models.len())),
        ("protocols", protocols.clone()),
    ];
    if available_models.is_empty() {
        rows.remove(2);
    }
    screen(&config_path, &rows);
    let efforts = crate::agent::engine::EFFORT_LEVELS.join("|");
    let model = if available_models.is_empty() || !io::stdin().is_terminal() {
        let typed = prompt::line("Model", &config.model)?;
        if !available_models.is_empty() && !available_models.contains(&typed) {
            println!("warning: '{typed}' is not in the endpoint's model list");
        }
        typed
    } else {
        // The endpoint answered, so the choice is its list: a typed default here
        // is how a model the gateway does not serve gets saved.
        prompt::pick("Model", &available_models, Some(&config.model))
            .ok_or_else(|| anyhow::anyhow!("cancelled; nothing written"))?
    };

    rows.push(("model", model.clone()));
    screen(&config_path, &rows);
    let effort = if io::stdin().is_terminal() {
        let levels: Vec<String> = crate::agent::engine::EFFORT_LEVELS
            .iter()
            .map(|level| (*level).to_string())
            .collect();
        let current = config.reasoning_effort.clone().unwrap_or_default();
        let current = if current.is_empty() {
            "none".to_string()
        } else {
            current
        };
        prompt::pick("Reasoning effort", &levels, Some(&current))
            .ok_or_else(|| anyhow::anyhow!("cancelled; nothing written"))?
    } else {
        prompt::line(
            &format!("Reasoning effort ({efforts})"),
            config.reasoning_effort.as_deref().unwrap_or(""),
        )?
    };

    config.base_url = Some(stored_url);
    config.model = model;
    if !api_key.is_empty() {
        config.api_key = Some(api_key);
    }
    if !effort.is_empty() {
        if crate::agent::engine::parse_reasoning_effort(&effort).is_none() {
            anyhow::bail!("Unknown reasoning effort '{effort}', expected {efforts}");
        }
        config.reasoning_effort = Some(effort.to_lowercase());
    }

    let preserved =
        persist(&config_path, &config, config.api_key.as_deref()).map_err(anyhow::Error::msg)?;
    if let Some(preserved) = preserved {
        println!(
            "note: the previous config could not be parsed and was kept at {}",
            preserved.display()
        );
    }

    screen(
        &config_path,
        &[
            ("saved", "yes".to_string()),
            (
                "base_url",
                crate::cli::config::redact_url(config.base_url.as_deref().unwrap_or_default()),
            ),
            ("protocols", protocols),
            ("model", config.model.clone()),
            (
                "api key",
                mask(config.api_key.as_deref().unwrap_or_default()),
            ),
            (
                "effort",
                config
                    .reasoning_effort
                    .clone()
                    .unwrap_or_else(|| "none".to_string()),
            ),
        ],
    );
    if std::path::PathBuf::from(".seavx/config.toml").exists() {
        println!("note: ./.seavx/config.toml overrides this file in the current workspace.");
    }

    Ok(())
}
