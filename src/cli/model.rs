//! `seavx model`: set model and reasoning effort, either by flag or interactively.

use super::prompt;
use crate::agent::engine::{AgentConfig, EFFORT_LEVELS};
use std::path::{Path, PathBuf};

pub async fn run(model: Option<String>, effort: Option<String>) -> anyhow::Result<()> {
    let config_path = PathBuf::from(".seavx/config.toml");
    let mut config = AgentConfig::load(Path::new("."));
    let base_url = config
        .base_url
        .clone()
        .unwrap_or_else(|| crate::agent::engine::DEFAULT_BASE_URL.to_string());
    let model_mgr = crate::agent::models::ModelManager::new(&PathBuf::from("."));

    // 1. If explicit model name given
    if let Some(chosen_model) = model {
        config.model = chosen_model;
        if let Some(eff) = effort {
            config.reasoning_effort = Some(eff);
        }
        config.write_to(&config_path)?;
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

    // Reasoning effort selection from the shared level list
    println!("\nReasoning Effort options:");
    for (idx, level) in EFFORT_LEVELS.iter().enumerate() {
        println!("  [{}] {}", idx + 1, level);
    }

    let current_effort = config.reasoning_effort.as_deref().unwrap_or("none");
    let eff_input = prompt::line(
        &format!(
            "Select effort 1-{} (or press Enter to keep '{current_effort}')",
            EFFORT_LEVELS.len()
        ),
        "",
    )?;

    if !eff_input.is_empty() {
        let chosen_effort = match eff_input.parse::<usize>() {
            Ok(n) if (1..=EFFORT_LEVELS.len()).contains(&n) => EFFORT_LEVELS[n - 1],
            _ => eff_input.as_str(),
        };
        config.reasoning_effort = Some(chosen_effort.to_string());
    }

    config.write_to(&config_path)?;
    println!("\nSuccessfully saved to .seavx/config.toml!");
    println!("  Model : {}", config.model);
    println!(
        "  Effort: {}",
        config.reasoning_effort.as_deref().unwrap_or("none")
    );

    Ok(())
}
