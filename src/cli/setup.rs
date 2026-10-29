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

pub async fn providers() -> anyhow::Result<()> {
    use std::io::{self, Write};

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

        screen(&config_path, &[("base_url", url.clone())]);
        let key = prompt::secret("API key (optional)", &key_default)?;

        let (normalized, _) = crate::agent::engine::normalize_base_url(&url);
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

    let mut rows = vec![
        ("base_url", base_url.clone()),
        ("api key", mask(&api_key)),
        ("models", format!("{} available", available_models.len())),
    ];
    if available_models.is_empty() {
        rows.pop();
    }
    screen(&config_path, &rows);
    let efforts = crate::agent::engine::EFFORT_LEVELS.join("|");
    let model = prompt::line("Model", &config.model)?;
    if !available_models.is_empty() && !available_models.contains(&model) {
        println!("warning: '{model}' is not in the endpoint's model list");
    }

    rows.push(("model", model.clone()));
    screen(&config_path, &rows);
    let effort = prompt::line(
        &format!("Reasoning effort ({efforts})"),
        config.reasoning_effort.as_deref().unwrap_or(""),
    )?;

    let (stored_url, adapters) = crate::agent::engine::normalize_base_url(&base_url);
    let protocols = adapters
        .iter()
        .map(|a| format!("{a:?}"))
        .collect::<Vec<_>>()
        .join(", ");
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

    // One writer: it validates, preserves unknown keys and forces mode `0600`,
    // which a file holding an API key needs.
    crate::cli::config::write_agent_config(&config_path, &config).map_err(anyhow::Error::msg)?;

    screen(
        &config_path,
        &[
            ("saved", "yes".to_string()),
            ("base_url", config.base_url.clone().unwrap_or_default()),
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
