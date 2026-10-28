//! `seavx uninstall`: remove the binary after asking whether `~/.seavx` data goes too.

use super::prompt;

pub fn run() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let data_dir = crate::home::dir().map(|home| home.join(".seavx"));

    let data_label = match &data_dir {
        Some(dir) if dir.exists() => dir.display().to_string(),
        _ => "no data found".to_string(),
    };

    println!("This will remove:");
    println!("  binary  {}", exe.display());
    println!();
    println!("  [1] Keep data (remove binary only)");
    println!("  [2] Force remove data too ({data_label})");
    println!("  [3] Cancel");
    println!();

    // Anything but 1 or 2 cancels, so a stray Enter or a piped EOF wipes nothing.
    let all = match prompt::line("Select 1-3", "3")?.as_str() {
        "1" => false,
        "2" => true,
        _ => {
            println!("Cancelled.");
            return Ok(());
        }
    };

    // Binary first: if it is not removable (e.g. root-owned), no data has been lost yet.
    #[cfg(not(windows))]
    std::fs::remove_file(&exe).map_err(|e| {
        anyhow::anyhow!(
            "Failed to remove {}: {e}. Re-run with sufficient permissions.",
            exe.display()
        )
    })?;

    // Windows refuses to delete a running exe, so it is renamed aside; the leftover is inert.
    #[cfg(windows)]
    {
        let leftover = exe.with_extension("old");
        let _ = std::fs::remove_file(&leftover);
        std::fs::rename(&exe, &leftover)
            .map_err(|e| anyhow::anyhow!("Failed to remove {}: {e}", exe.display()))?;
        println!("Moved {} aside to {}", exe.display(), leftover.display());
        println!("Delete that leftover file once this window closes.");
    }
    println!("Removed {}", exe.display());

    if all
        && let Some(dir) = data_dir
        && dir.exists()
    {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| anyhow::anyhow!("Failed to remove {}: {e}", dir.display()))?;
        println!("Removed {}", dir.display());
    }

    println!("SEAVX uninstalled.");
    Ok(())
}
