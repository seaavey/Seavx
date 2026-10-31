//! `seavx model`: set model and reasoning effort, either by flag or interactively.

use super::prompt;
use crate::agent::engine::{AgentConfig, EFFORT_LEVELS};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// Write the flags as one targeted update: model and effort change together or
/// not at all, unknown keys survive, and an unparsable file is repaired while its
/// bytes are kept. Production and the tests both go through here.
pub fn apply_flags(
    path: &std::path::Path,
    model: String,
    effort: Option<String>,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    if let Some(effort) = effort.as_deref()
        && crate::agent::engine::parse_reasoning_effort(effort).is_none()
    {
        anyhow::bail!("unknown reasoning effort '{effort}'");
    }
    crate::cli::config::set_model_and_effort_repairing(path, &model, effort.as_deref())
        .map_err(anyhow::Error::msg)
}

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
        let preserved = apply_flags(&config_path, chosen_model.clone(), effort)?;
        if let Some(preserved) = preserved {
            println!(
                "note: the previous config could not be parsed and was kept at {}",
                preserved.display()
            );
        }
        println!("Updated .seavx/config.toml: model = {chosen_model}");
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

    let interactive = IsTerminal::is_terminal(&std::io::stdin());

    if interactive {
        // The fetched list is the choice; a typed name is the piped fallback.
        let Some(chosen) = prompt::pick("Model", &available_models, Some(&config.model)) else {
            anyhow::bail!("cancelled; nothing written");
        };
        config.model = chosen;
    } else {
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
    }

    let current_effort = config.reasoning_effort.as_deref().unwrap_or("none");
    if interactive {
        let levels: Vec<String> = EFFORT_LEVELS
            .iter()
            .map(|level| (*level).to_string())
            .collect();
        let Some(chosen) = prompt::pick("Reasoning effort", &levels, Some(current_effort)) else {
            anyhow::bail!("cancelled; nothing written");
        };
        config.reasoning_effort = Some(chosen);
    } else {
        // Reasoning effort selection from the shared level list
        println!("\nReasoning Effort options:");
        for (idx, level) in EFFORT_LEVELS.iter().enumerate() {
            println!("  [{}] {}\n", idx + 1, level);
        }

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
    }

    crate::cli::config::write_agent_config(&config_path, &config).map_err(anyhow::Error::msg)?;
    println!("\nSuccessfully saved to .seavx/config.toml!");
    println!("  Model : {}", config.model);
    println!(
        "  Effort: {}",
        config.reasoning_effort.as_deref().unwrap_or("none")
    );

    Ok(())
}
