//! `seavx update`: replace the running binary with the newest CI build served by cdn.seavx.com.

pub async fn run(force: bool) -> anyhow::Result<()> {
    const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

    let (os, arch) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => ("linux", "x86_64"),
        ("windows", "x86_64") => ("windows", "x86_64"),
        (os, arch) => {
            anyhow::bail!(
                "Unsupported platform for binary self-update: {os}-{arch}. Please update via cargo install or build from source."
            );
        }
    };

    println!("Checking for updates... (current version: v{CURRENT_VERSION})");

    let client = reqwest::Client::builder()
        .user_agent("seavx-updater")
        .build()?;

    /// Pointer object CI publishes next to each build.
    #[derive(serde::Deserialize)]
    struct Latest {
        sha: String,
        key: String,
        version: String,
    }

    let latest: Latest = client
        .get(format!("https://cdn.seavx.com/latest/{os}/{arch}.json"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if !force && latest.version == CURRENT_VERSION {
        println!(
            "SEAVX is already up to date (v{}, {})",
            latest.version, latest.sha
        );
        return Ok(());
    }

    println!("Downloading v{} ({})...", latest.version, latest.sha);
    let response = client
        .get(format!("https://cdn.seavx.com/{}", latest.key))
        .send()
        .await?
        .error_for_status()?;

    let pb = match response.content_length() {
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

    let current_exe = std::env::current_exe()?;
    let current_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Unable to get parent dir of executable"))?;

    let temp_path = current_dir.join(format!(".seavx-update-{}.tmp", std::process::id()));
    tokio::fs::write(&temp_path, &downloaded_bytes).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        let _ = std::fs::set_permissions(&temp_path, perms);
    }

    // Atomically replace current executable
    if let Err(e) = replace_exe(&temp_path, &current_exe) {
        let _ = tokio::fs::remove_file(&temp_path).await;
        anyhow::bail!(
            "Failed to replace binary at {}: {e}. You may need elevated permissions (e.g. sudo).",
            current_exe.display()
        );
    }

    println!(
        "Successfully updated SEAVX from v{} to v{} ({}, {})",
        CURRENT_VERSION,
        latest.version,
        latest.sha,
        current_exe.display()
    );

    Ok(())
}

/// Windows cannot overwrite a running exe, so the old one is moved aside first and kept
/// as `<name>.old` until the next update clears it.
fn replace_exe(temp: &std::path::Path, exe: &std::path::Path) -> std::io::Result<()> {
    #[cfg(not(windows))]
    return std::fs::rename(temp, exe);

    #[cfg(windows)]
    {
        let old = exe.with_extension("old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old)?;
        if let Err(e) = std::fs::rename(temp, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(e);
        }
        Ok(())
    }
}
