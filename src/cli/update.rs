//! `seavx update`: replace the running binary with the latest GitHub release.

pub async fn run(force: bool) -> anyhow::Result<()> {
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
