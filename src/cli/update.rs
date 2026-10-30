//! `seavx update`: replace the running binary with the newest CI build served by cdn.seavx.com.

/// Pointer object CI publishes next to each build.
#[derive(serde::Deserialize)]
struct Latest {
    sha: String,
    key: String,
    version: String,
    /// Hex SHA-256 of the build. Absent from a pointer published before the
    /// field existed, so a missing value warns instead of blocking an update.
    #[serde(default)]
    sha256: Option<String>,
}

/// Lowercase hex, the spelling `sha256sum` and the CI pointer use.
fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Compare what the pointer published with the bytes actually received.
/// `Ok(true)` on a match, `Ok(false)` when the pointer carries no checksum, and
/// `Err` with both values when they differ.
fn verify_digest(expected: Option<&str>, actual: &str) -> Result<bool, String> {
    match expected {
        Some(expected) if expected.eq_ignore_ascii_case(actual) => Ok(true),
        Some(expected) => Err(format!(
            "download does not match the published checksum: expected {expected}, got {actual}"
        )),
        None => Ok(false),
    }
}

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

    // A template is a constant, so a rejected one falls back instead of panicking.
    let pb = match response.content_length() {
        Some(size) => {
            let pb = indicatif::ProgressBar::new(size);
            let style = indicatif::ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar())
                .progress_chars("#>-");
            pb.set_style(style);
            pb
        }
        None => {
            let pb = indicatif::ProgressBar::new_spinner();
            let style = indicatif::ProgressStyle::default_spinner()
                .template("{spinner:.green} [{elapsed_precise}] {bytes} downloaded")
                .unwrap_or_else(|_| indicatif::ProgressStyle::default_spinner());
            pb.set_style(style);
            pb
        }
    };

    let current_exe = std::env::current_exe()?;
    let current_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Unable to get parent dir of executable"))?;
    let temp_path = current_dir.join(format!(".seavx-update-{}.tmp", std::process::id()));

    // Streamed straight into the temp file: the download is never held whole in
    // memory, and a broken transfer leaves no partial bytes at the target.
    use futures::StreamExt;
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(&temp_path).await?;
    let mut digest = Sha256::new();
    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                pb.inc(chunk.len() as u64);
                digest.update(&chunk);
                if let Err(error) = file.write_all(&chunk).await {
                    let _ = tokio::fs::remove_file(&temp_path).await;
                    return Err(error.into());
                }
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(error.into());
            }
        }
    }
    file.sync_all().await?;
    drop(file);

    // The pointer arrives over TLS from the same origin as the build, so the
    // digest catches a corrupt or swapped object. It cannot prove authenticity
    // to a client whose pointer was already forged: that needs a signature.
    let actual = hex_lower(&digest.finalize());
    match verify_digest(latest.sha256.as_deref(), &actual) {
        Ok(true) => {}
        Ok(false) => eprintln!(
            "warning: this build was published without a checksum; the download was not verified"
        ),
        Err(message) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            pb.abandon();
            anyhow::bail!(message);
        }
    }
    pb.finish_with_message("Download complete");

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

#[cfg(test)]
#[path = "../tests/cli/update.rs"]
mod tests;

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
