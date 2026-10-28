//! `seavx model`: set model and reasoning effort, either by flag or interactively.

use super::prompt;
use std::path::PathBuf;

pub async fn run(model: Option<String>, effort: Option<String>) -> anyhow::Result<()> {
    let config_path = PathBuf::from(".seavx/config.toml");
    let mut config = crate::agent::engine::AgentConfig::load(&PathBuf::from("."));
    let base_url = config
        .base_url
        .clone()
        .unwrap_or_else(|| crate::agent::engine::DEFAULT_BASE_URL.to_string());
    let model_mgr = crate::agent::models::ModelManager::new(&PathBuf::from("."));

    // 1. If explicit model name given
    if let Some(chosen_model) = model {
        config.model = chosen_model.clone();
        if let Some(eff) = effort {
            config.reasoning_effort = Some(eff);
        }
        let toml_str = toml::to_string_pretty(&config)?;
        std::fs::write(&config_path, toml_str)?;
        println!(
            "Updated .seavx/config.toml: model = {}, effort = {:?}",
            config.model, config.reasoning_effort
        );
        return Ok(());
    }

    // 2. Interactive Selection from Cached / Remote Gateway Models
    println!("Loading available models for {base_url}...");
    let available_models = match model_mgr
        .fetch_or_cache_models(&base_url, config.api_key.as_deref())
        .await
    {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Failed to fetch remote models: {e}");
            Vec::new()
        }
    };

    if available_models.is_empty() {
        println!("No models discovered. Current model is: {}", config.model);
        return Ok(());
    }

    println!("\nAvailable Models:");
    for (idx, m) in available_models.iter().enumerate() {
        let mark = if *m == config.model { " [active]" } else { "" };
        println!("  [{}] {}{}", idx + 1, m, mark);
    }

    println!();
    let input = prompt::line(
        &format!(
            "Select model number (or press Enter to keep '{}')",
            config.model
        ),
        "",
    )?;

    if let Ok(num) = input.parse::<usize>()
        && num > 0
        && num <= available_models.len()
    {
        config.model = available_models[num - 1].clone();
    }

    // Reasoning effort selection
    println!("\nReasoning Effort options:");
    println!("  [1] None (no reasoning)");
    println!("  [2] Minimal / Low");
    println!("  [3] Medium");
    println!("  [4] High");
    println!("  [5] HighX");
    println!("  [6] Max");

    let current_effort = config.reasoning_effort.as_deref().unwrap_or("none");
    let eff_input = prompt::line(
        &format!("Select effort 1-6 (or press Enter to keep '{current_effort}')"),
        "",
    )?;

    if !eff_input.is_empty() {
        let chosen_effort = match eff_input.as_str() {
            "1" => "none",
            "2" => "minimal",
            "3" => "medium",
            "4" => "high",
            "5" => "highx",
            "6" => "max",
            other => other,
        };
        config.reasoning_effort = Some(chosen_effort.to_string());
    }

    let toml_str = toml::to_string_pretty(&config)?;
    std::fs::write(&config_path, toml_str)?;
    println!("\nSuccessfully saved to .seavx/config.toml!");
    println!("  Model : {}", config.model);
    println!(
        "  Effort: {}",
        config.reasoning_effort.as_deref().unwrap_or("none")
    );

    Ok(())
}
