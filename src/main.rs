pub mod agent;
mod atomic_file;
mod checkpoint;
mod cli;
mod history;
mod home;
pub mod session;
mod tui;
mod util;

use clap::{Parser, Subcommand};
use std::fs::OpenOptions;
use std::path::PathBuf;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "seavx", version, about = "A customizable AI coding agent")]
struct Cli {
    /// Log at trace level to `.seavx/debug.log`; RUST_LOG overrides this
    #[arg(short, long, global = true)]
    debug: bool,

    #[command(subcommand)]
    command: Option<Commands>,
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
    Doctor {
        /// Skip the endpoint network check
        #[arg(long)]
        offline: bool,
    },

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

    /// List, show, resume or export stored sessions
    Sessions {
        #[command(subcommand)]
        command: cli::sessions::SessionsCommand,
    },

    /// Self-update SEAVX to the latest release
    Update {
        /// Force update even if already up to date
        #[arg(short, long)]
        force: bool,
    },

    /// Uninstall SEAVX: remove the binary, with either a data-keeping or a full wipe choice
    Uninstall,
}

#[derive(Subcommand, Debug)]
enum SetupSection {
    /// Configure the API provider: base URL, key, model, reasoning effort
    Providers,
}

fn init_tracing(debug_flag: bool) -> anyhow::Result<Option<PathBuf>> {
    let log_file_path = util::prepare_log_file(std::path::Path::new(".seavx"))?;

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)?;

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(file)
        .with_ansi(false);

    // An explicit `--debug` raises the filter; otherwise the env or a default applies.
    let filter = if debug_flag {
        EnvFilter::new("seavx=trace,genai=debug")
    } else {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("seavx=debug,genai=debug"))
    };

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    let args = Cli::parse();
    let _log_path = init_tracing(args.debug)?;

    match args.command {
        Some(Commands::Run { prompt }) => {
            let initial_prompt = if prompt.is_empty() {
                None
            } else {
                Some(prompt.join(" "))
            };
            tui::app::start_tui(initial_prompt, None).await?;
        }
        Some(Commands::Exec { prompt }) => {
            let task = prompt.join(" ");
            if task.trim().is_empty() {
                eprintln!("Error: no task specified for exec");
                return Ok(());
            }

            let security = agent::security::WorkspaceSecurity::new(".");
            let config = agent::engine::AgentConfig::load(security.root());
            for batch in crate::checkpoint::recover_pending(security.root()).unwrap_or_default() {
                eprintln!("{}", batch.render());
            }

            let runtime = agent::engine::AgentRuntime::new(config, security);
            println!("Executing task: {task}\n");
            let result = runtime
                .execute_task(
                    &task,
                    agent::engine::AgentMode::Build,
                    Vec::new(),
                    &agent::permissions::DenyingConfirmer,
                    |evt| {
                        // Reasoning blocks are a TUI concern; headless runs skip them.
                        if let Some(token) = evt.strip_prefix("TOKEN:") {
                            print!("{token}");
                        } else if !evt.starts_with("THINKING:") {
                            println!("{evt}");
                        }
                    },
                    // `seavx exec` stays session-free: no row is ever written.
                    |_, _| {},
                )
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            println!("\nFinal Response:\n{result}");
        }
        Some(Commands::Init) => {
            let (config_path, created) = cli::config::init_workspace(std::path::Path::new("."))
                .map_err(anyhow::Error::msg)?;
            if created {
                println!("Created default config at {}", config_path.display());
            } else {
                println!("Config already exists at {}", config_path.display());
            }
        }
        Some(Commands::Setup { section }) => match section {
            SetupSection::Providers => cli::setup::providers().await?,
        },
        Some(Commands::Doctor { offline }) => {
            let report = cli::doctor::run(cli::doctor::DoctorOptions {
                offline,
                workspace_root: PathBuf::from("."),
                home: home::dir(),
            })
            .await;
            println!("{}", cli::doctor::render(&report));
            let code = cli::doctor::exit_code(&report);
            if code != 0 {
                std::process::exit(code);
            }
        }
        Some(Commands::Model { model, effort }) => cli::model::run(model, effort).await?,
        Some(Commands::Memory) => cli::memory::run()?,
        Some(Commands::Sessions { command }) => cli::sessions::run(command).await?,
        Some(Commands::Update { force }) => cli::update::run(force).await?,
        Some(Commands::Uninstall) => cli::uninstall::run()?,
        None => {
            tui::app::start_tui(None, None).await?;
        }
    }

    Ok(())
}
