//! `seavx model`: set model and reasoning effort, either by flag or interactively.

use super::prompt;
use crate::agent::engine::{AgentConfig, EFFORT_LEVELS};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// What to do with a model name the endpoint did not list. Pure, so a test can
/// pin both outcomes without a terminal.
#[derive(Debug, PartialEq, Eq)]
pub enum ModelGate {
    /// It is listed, or nothing could be listed to compare against.
    Writable,
    /// A terminal can still say yes; a pipeline cannot.
    Ask(String),
    /// Refuse, with the message the user sees.
    Refused(String),
}

/// The endpoint `seavx model` may talk to. There is no default to fall back on.
pub fn require_endpoint(config: &AgentConfig) -> Result<String, String> {
    config
        .base_url
        .clone()
        .ok_or_else(|| crate::cli::config::ENDPOINT_NOT_SET.to_string())
}

/// Gate a typed model name on the endpoint's own list.
pub fn model_gate(interactive: bool, host: &str, list: &[String], name: &str) -> ModelGate {
    if list.is_empty()
        || crate::agent::models::check_model(list, name) == crate::agent::models::ModelCheck::Listed
    {
        return ModelGate::Writable;
    }
    let message = format!("'{name}' is not in the {} models {host} lists", list.len());
    if interactive {
        ModelGate::Ask(format!("{message}; use it anyway? [y/N]"))
    } else {
        ModelGate::Refused(message)
    }
}

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
    let base_url = match require_endpoint(&config) {
        Ok(base_url) => base_url,
        Err(reason) => {
            println!("endpoint: {reason}");
            anyhow::bail!("no endpoint configured: run `seavx setup`");
        }
    };
    let model_mgr = crate::agent::models::ModelManager::new(&PathBuf::from("."));

    // 1. If explicit model name given
    if let Some(chosen_model) = model {
        // The endpoint's own list decides, when it can be reached at all.
        let host = crate::agent::models::ModelManager::domain_slug(&base_url);
        match model_mgr
            .fetch_or_cache_models(&base_url, config.api_key.as_deref())
            .await
        {
            Ok(list) => {
                let interactive = IsTerminal::is_terminal(&std::io::stdin());
                match model_gate(interactive, &host, &list, &chosen_model) {
                    ModelGate::Writable => {}
                    ModelGate::Ask(question) => {
                        let answer = prompt::line(&question, "")?;
                        let answer = answer.trim().to_ascii_lowercase();
                        if answer != "y" && answer != "yes" {
                            anyhow::bail!("nothing written");
                        }
                    }
                    ModelGate::Refused(reason) => anyhow::bail!("{reason}; nothing written"),
                }
            }
            // Nothing to compare against: refusing here would block the only
            // way to configure a gateway that is briefly unreachable.
            Err(error) => println!("note: could not list models from {host} ({error})"),
        }
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
        println!(
            "No models discovered. Current model is: {}",
            crate::cli::config::model_label(&config)
        );
        return Ok(());
    }

    let interactive = IsTerminal::is_terminal(&std::io::stdin());

    if interactive {
        // The fetched list is the choice; a typed name is the piped fallback.
        let Some(chosen) = prompt::pick("Model", &available_models, config.model.as_deref()) else {
            anyhow::bail!("cancelled; nothing written");
        };
        config.model = Some(chosen);
    } else {
        println!("\nAvailable Models:");
        for (idx, m) in available_models.iter().enumerate() {
            let mark = if config.model.as_deref() == Some(m.as_str()) {
                " [active]"
            } else {
                ""
            };
            println!("  [{}] {}{}", idx + 1, m, mark);
        }

        println!();
        let input = prompt::line(
            &format!(
                "Select model number (or press Enter to keep '{}')",
                crate::cli::config::model_label(&config)
            ),
            "",
        )?;

        if let Ok(num) = input.parse::<usize>()
            && num > 0
            && num <= available_models.len()
        {
            config.model = Some(available_models[num - 1].clone());
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
    println!("  Model : {}", crate::cli::config::model_label(&config));
    println!(
        "  Effort: {}",
        config.reasoning_effort.as_deref().unwrap_or("none")
    );

    Ok(())
}

#[cfg(test)]
#[path = "../tests/cli/model.rs"]
mod tests;
