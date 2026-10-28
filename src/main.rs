pub mod agent;
mod tui;

use clap::{Parser, Subcommand};
use std::fs::OpenOptions;
use std::path::PathBuf;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "seavx", version, about = "A customizable AI coding agent")]
struct Cli {
    /// Enable file-based debug logging to .seavx/debug.log in current repository
    #[arg(short, long, global = true)]
    debug: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

fn init_tracing(_debug: bool) -> anyhow::Result<Option<PathBuf>> {
    let log_dir = PathBuf::from(".seavx");
    std::fs::create_dir_all(&log_dir)?;
    let log_file_path = log_dir.join("debug.log");

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)?;

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file)
        .with_ansi(false);

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("seavx=debug,genai=debug"));

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .init();

    tracing::info!(
        "Tracing initialized. Writing debug log to {:?}",
        log_file_path
    );
    Ok(Some(log_file_path))
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start an interactive agent session
    Run {
        /// Task to give the agent
        prompt: Vec<String>,
    },

    /// Run a task directly in headless CLI mode without TUI
    Exec {
        /// Prompt/task to execute
        prompt: Vec<String>,
    },

    /// Initialize SEAVX configuration
    Init,

    /// Check the installation
    Doctor,

    /// List or interactively select model and reasoning effort
    Model {
        /// Optional model name to set directly
        model: Option<String>,

        /// Optional reasoning effort: none, minimal, medium, high, highx, max
        #[arg(short, long)]
        effort: Option<String>,
    },

    /// Inspect agent memory
    Memory,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();
    let debug_enabled = cli.debug
        || cfg!(debug_assertions)
        || std::env::var("SEAVX_DEBUG")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);

    let _log_path = init_tracing(debug_enabled)?;

    match cli.command {
        Some(Commands::Run { prompt }) => {
            let initial_prompt = if prompt.is_empty() {
                None
            } else {
                Some(prompt.join(" "))
            };
            tui::app::start_tui(initial_prompt).await?;
        }
        Some(Commands::Exec { prompt }) => {
            let task = prompt.join(" ");
            if task.trim().is_empty() {
                eprintln!("Error: no task specified for exec");
                return Ok(());
            }

            let security = crate::agent::security::WorkspaceSecurity::new(".");
            let config = crate::agent::engine::AgentConfig::load(security.root());
            let runtime = crate::agent::engine::AgentRuntime::new(config, security);

            println!("Executing task: {task}\n");
            let result = runtime
                .execute_task(&task, |evt| {
                    // Reasoning blocks are a TUI concern; headless runs skip them.
                    if !evt.starts_with("THINKING:") {
                        println!("{evt}");
                    }
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            println!("\nFinal Response:\n{result}");
        }
        Some(Commands::Init) => {
            let seavx_dir = PathBuf::from(".seavx");
            std::fs::create_dir_all(&seavx_dir)?;
            let config_path = seavx_dir.join("config.toml");
            if !config_path.exists() {
                let default_toml = "model = \"openai/gpt-4o-mini\"\nmax_turns = 20\nbase_url = \"https://api.openai.com/v1/\"\napi_key = \"\"\n";
                std::fs::write(&config_path, default_toml)?;
                println!("Created default config at {}", config_path.display());
            } else {
                println!("Config already exists at {}", config_path.display());
            }
        }
        Some(Commands::Doctor) => {
            println!("SEAVX environment check");
        }
        Some(Commands::Model { model, effort }) => {
            let config_path = PathBuf::from(".seavx/config.toml");
            let mut config = crate::agent::engine::AgentConfig::load(&PathBuf::from("."));
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "https://seavx.ai.id/v1/".to_string());
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

            print!(
                "\nSelect model number (or press Enter to keep '{}'): ",
                config.model
            );
            use std::io::{self, Write};
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let trimmed = input.trim();

            if let Ok(num) = trimmed.parse::<usize>()
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
            print!(
                "Select effort 1-6 (or press Enter to keep '{}'): ",
                current_effort
            );
            io::stdout().flush()?;

            let mut eff_input = String::new();
            io::stdin().read_line(&mut eff_input)?;
            let eff_trimmed = eff_input.trim();

            if !eff_trimmed.is_empty() {
                let chosen_effort = match eff_trimmed {
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
        }
        Some(Commands::Memory) => {
            let memory_candidates = [
                PathBuf::from(".seavx/MEMORY.md"),
                PathBuf::from("MEMORY.md"),
            ];
            let mut found_mem = false;
            for memory_path in &memory_candidates {
                if memory_path.exists() {
                    println!("--- Project Memory ({}) ---", memory_path.display());
                    match std::fs::read_to_string(memory_path) {
                        Ok(c) => println!("{c}"),
                        Err(e) => eprintln!("Error reading {}: {e}", memory_path.display()),
                    }
                    found_mem = true;
                    break;
                }
            }
            if !found_mem {
                println!("No MEMORY.md found in current workspace.");
            }

            if let Ok(home) = std::env::var("HOME") {
                let user_candidates = [
                    PathBuf::from(".seavx/USER.md"),
                    PathBuf::from("USER.md"),
                    PathBuf::from(home).join(".seavx/USER.md"),
                ];
                for u in &user_candidates {
                    if u.exists() {
                        println!("\n--- User Preferences ({}) ---", u.display());
                        match std::fs::read_to_string(u) {
                            Ok(c) => println!("{c}"),
                            Err(e) => eprintln!("Error reading {}: {e}", u.display()),
                        }
                        break;
                    }
                }
            }

            let skill_mgr = crate::agent::skills::SkillManager::new(&PathBuf::from("."));
            let skills = skill_mgr.list_skills();
            if !skills.is_empty() {
                println!(
                    "\n--- Installed Skills (.seavx/skills, ~/.seavx/skills, ~/.agents/skills) ---"
                );
                for s in skills {
                    println!("- {}: {}", s.name, s.description);
                }
            }
        }
        None => {
            tui::app::start_tui(None).await?;
        }
    }

    Ok(())
}
