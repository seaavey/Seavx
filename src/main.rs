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

    /// Configure providers and other SEAVX settings
    Setup {
        #[command(subcommand)]
        section: SetupSection,
    },

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

    /// Self-update SEAVX to the latest release
    Update {
        /// Force update even if already up to date
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum SetupSection {
    /// Configure the API provider: base URL, key, model, reasoning effort
    Providers,
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
                let default_toml = format!(
                    "model = \"openai/gpt-4o-mini\"\nmax_turns = 1000\nbase_url = \"{}\"\napi_key = \"\"\n",
                    crate::agent::engine::DEFAULT_BASE_URL
                );
                std::fs::write(&config_path, default_toml)?;
                println!("Created default config at {}", config_path.display());
            } else {
                println!("Config already exists at {}", config_path.display());
            }
        }
        Some(Commands::Setup { section }) => match section {
            SetupSection::Providers => setup_providers().await?,
        },
        Some(Commands::Doctor) => {
            println!("SEAVX environment check");
        }
        Some(Commands::Model { model, effort }) => {
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
        Some(Commands::Update { force }) => {
            handle_update(force).await?;
        }
        None => {
            tui::app::start_tui(None).await?;
        }
    }

    Ok(())
}

async fn handle_update(force: bool) -> anyhow::Result<()> {
    const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
    println!("Checking for updates... (current version: v{CURRENT_VERSION})");

    let client = reqwest::Client::builder()
        .user_agent("seavx-updater")
        .build()?;

    let url = "https://api.github.com/repos/seaavey/Seavx/releases/latest";
    let resp = client
        .get(url)
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "Failed to query latest release from GitHub: HTTP {}",
            resp.status()
        );
    }

    #[derive(serde::Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }

    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
        assets: Vec<Asset>,
    }

    let release: Release = resp.json().await?;
    let latest_tag = release.tag_name.trim();
    let latest_version = latest_tag.trim_start_matches('v');

    if !force && latest_version == CURRENT_VERSION {
        println!("SEAVX is already up to date ({latest_tag}).");
        return Ok(());
    }

    // Determine target binary name
    let target_asset_name = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "seavx-linux-x86_64",
        (os, arch) => {
            anyhow::bail!(
                "Unsupported platform for binary self-update: {os}-{arch}. Please update via cargo install or build from source."
            );
        }
    };

    let asset = release
        .assets
        .iter()
        .find(|a| a.name == target_asset_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No release binary artifact '{}' found for release {}",
                target_asset_name,
                latest_tag
            )
        })?;

    println!("Downloading {} ({})...", latest_tag, asset.name);
    let response = client
        .get(&asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?;

    let total_size = response.content_length();
    let pb = match total_size {
        Some(size) => {
            let pb = indicatif::ProgressBar::new(size);
            pb.set_style(
                indicatif::ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                    .unwrap()
                    .progress_chars("#>-"),
            );
            pb
        }
        None => {
            let pb = indicatif::ProgressBar::new_spinner();
            pb.set_style(
                indicatif::ProgressStyle::default_spinner()
                    .template("{spinner:.green} [{elapsed_precise}] {bytes} downloaded")
                    .unwrap(),
            );
            pb
        }
    };

    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    let mut downloaded_bytes = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result?;
        pb.inc(chunk.len() as u64);
        downloaded_bytes.extend_from_slice(&chunk);
    }
    pb.finish_with_message("Download complete");

    let bytes = downloaded_bytes;

    let current_exe = std::env::current_exe()?;
    let current_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Unable to get parent dir of executable"))?;

    let temp_path = current_dir.join(format!(".seavx-update-{}.tmp", std::process::id()));
    tokio::fs::write(&temp_path, &bytes).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        let _ = std::fs::set_permissions(&temp_path, perms);
    }

    // Atomically replace current executable
    if let Err(e) = std::fs::rename(&temp_path, &current_exe) {
        let _ = tokio::fs::remove_file(&temp_path).await;
        anyhow::bail!(
            "Failed to replace binary at {}: {e}. You may need elevated permissions (e.g. sudo).",
            current_exe.display()
        );
    }

    println!(
        "Successfully updated SEAVX from v{} to {} ({})",
        CURRENT_VERSION,
        latest_tag,
        current_exe.display()
    );

    Ok(())
}

/// Read one stdin line; an empty answer keeps `current`.
fn prompt_line(label: &str, current: &str) -> anyhow::Result<String> {
    use std::io::{self, Write};
    if current.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{current}]: ");
    }
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();
    Ok(if input.is_empty() {
        current.to_string()
    } else {
        input.to_string()
    })
}

/// Masked variant of `prompt_line`, for values that must not land in scrollback.
fn prompt_secret(label: &str, current: &str) -> anyhow::Result<String> {
    use std::io::{self, IsTerminal, Write};

    if current.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [****]: ");
    }
    io::stdout().flush()?;

    let input = if io::stdin().is_terminal() {
        read_masked_line()?
    } else {
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        println!();
        line.trim_end().to_string()
    };

    Ok(if input.is_empty() {
        current.to_string()
    } else {
        input
    })
}

fn read_masked_line() -> anyhow::Result<String> {
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    use std::io::{self, Write};

    crossterm::terminal::enable_raw_mode()?;
    let mut value = String::new();
    let mut out = io::stdout();

    let result = loop {
        match event::read() {
            Ok(Event::Key(key)) => match key.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("cancelled"));
                }
                KeyCode::Char(c) => {
                    value.push(c);
                    let _ = out.write_all(b"*");
                }
                KeyCode::Backspace if value.pop().is_some() => {
                    let _ = out.write_all(b"\x08 \x08");
                }
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(anyhow::anyhow!(e)),
        }
        let _ = out.flush();
    };

    crossterm::terminal::disable_raw_mode()?;
    println!();
    result?;
    Ok(value)
}

/// Redraw the wizard as a form between steps; piped output stays linear.
fn setup_screen(config_path: &std::path::Path, rows: &[(&str, String)]) {
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

async fn setup_providers() -> anyhow::Result<()> {
    use std::io::{self, Write};

    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("HOME is not set, cannot locate config"))?;
    let config_path = PathBuf::from(home).join(".seavx/config.toml");

    // A malformed file is replaced, matching AgentConfig::load's fallback to defaults.
    let mut config: crate::agent::engine::AgentConfig = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|content| toml::from_str(&content).ok())
        .unwrap_or(crate::agent::engine::AgentConfig {
            model: "openai/gpt-4o-mini".to_string(),
            max_turns: 1000,
            base_url: None,
            api_key: None,
            reasoning_effort: None,
            system_prompt: None,
        });

    let mask = |key: &str| {
        if key.is_empty() {
            "none".to_string()
        } else {
            "****".to_string()
        }
    };

    let model_mgr = crate::agent::models::ModelManager::new(&PathBuf::from("."));
    let mut available_models: Vec<String> = Vec::new();
    let mut url_default = config
        .base_url
        .clone()
        .unwrap_or_else(|| crate::agent::engine::DEFAULT_BASE_URL.to_string());
    let mut key_default = config.api_key.clone().unwrap_or_default();

    // Ask the endpoint for its model list, so a wrong URL or key fails here instead of in the TUI.
    let (base_url, api_key) = loop {
        setup_screen(&config_path, &[]);
        println!(
            "Accepts an OpenAI- or Anthropic-compatible endpoint; /v1/ is added when missing.\n"
        );
        let url = prompt_line("Base URL", &url_default)?;

        setup_screen(&config_path, &[("base_url", url.clone())]);
        let key = prompt_secret("API key (optional)", &key_default)?;

        let (normalized, _) = crate::agent::engine::normalize_base_url(&url);
        setup_screen(
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
                let keep = prompt_line("Save this endpoint anyway? [y/N]", "")?;
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
    setup_screen(&config_path, &rows);
    let model = prompt_line("Model", &config.model)?;
    if !available_models.is_empty() && !available_models.contains(&model) {
        println!("warning: '{model}' is not in the endpoint's model list");
    }

    rows.push(("model", model.clone()));
    setup_screen(&config_path, &rows);
    let effort = prompt_line(
        "Reasoning effort (none|minimal|medium|high|highx|max)",
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
            anyhow::bail!(
                "Unknown reasoning effort '{effort}', expected none|minimal|medium|high|highx|max"
            );
        }
        config.reasoning_effort = Some(effort.to_lowercase());
    }

    if let Some(dir) = config_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&config_path, toml::to_string_pretty(&config)?)?;

    // The file holds an API key, so keep it private to the user.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))?;
    }

    setup_screen(
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
    if PathBuf::from(".seavx/config.toml").exists() {
        println!("note: ./.seavx/config.toml overrides this file in the current workspace.");
    }

    Ok(())
}
