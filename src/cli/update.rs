//! `seavx update`: replace the running binary with the newest CI build served by cdn.seavx.com.

mod manifest;

use manifest::{
    Channel, Manifest, Ordering, Target, digest_matches, hex_lower, ordering, probe_version,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The keys a release pointer must be signed by. A rotation adds a second entry.
const PINNED_KEYS: &[&str] = &[]; // TODO(update): paste the Task 0 public key here before the first release build

const CDN: &str = "https://cdn.seavx.com";
const POINTER_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The binary this process is: what a published pointer is ordered against.
pub(crate) struct Current {
    pub version: semver::Version,
    pub sha: Option<&'static str>,
}

pub async fn run(force: bool, channel: &str) -> anyhow::Result<()> {
    let channel =
        Channel::parse(channel).ok_or_else(|| anyhow::anyhow!("unknown channel {channel:?}"))?;
    let current = Current {
        version: semver::Version::parse(env!("CARGO_PKG_VERSION"))?,
        // Absent when the build ran outside a git checkout.
        sha: option_env!("SEAVX_GIT_SHA"),
    };
    let exe = std::env::current_exe()?;
    let base = resolve_base(std::env::var("SEAVX_UPDATE_BASE").ok());
    install(force, channel, &exe, &base, &current, PINNED_KEYS).await
}

/// The CDN root. `SEAVX_UPDATE_BASE` moves the origin and nothing else, so a test
/// can serve the committed fixtures without weakening what the pointer must prove.
fn resolve_base(from_env: Option<String>) -> String {
    from_env.map_or_else(
        || CDN.to_string(),
        |base| base.strip_suffix('/').unwrap_or(&base).to_string(),
    )
}

/// Verify the signed pointer for this channel and platform, then install the
/// artifact it names. Everything the pointer claims is checked before the
/// artifact is requested; `force` only relaxes the version ordering.
async fn install(
    force: bool,
    channel: Channel,
    exe: &Path,
    base: &str,
    current: &Current,
    keys: &[&str],
) -> anyhow::Result<()> {
    let target = Target::detect().ok_or_else(|| {
        anyhow::anyhow!(
            "Unsupported platform for binary self-update: {}-{}. Please update via cargo install or build from source.",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;

    println!(
        "Checking for updates... (current version: v{})",
        current.version
    );

    let client = reqwest::Client::builder()
        .user_agent("seavx-updater")
        .build()?;

    let pointer_url = format!("{base}/{}", channel.pointer_path(&target));
    let pointer = client
        .get(&pointer_url)
        .timeout(POINTER_TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await
        .map_err(|error| anyhow::anyhow!("{pointer_url}: {error}"))?;
    let manifest =
        Manifest::parse(&pointer).map_err(|error| anyhow::anyhow!("{pointer_url}: {error}"))?;

    // Signature first, then the bindings it signs for: a pointer that names
    // another channel or target is refused before its `key` is ever fetched.
    manifest
        .verify(keys)
        .and_then(|()| manifest.check_channel(channel))
        .and_then(|()| manifest.check_target(&target))
        .map_err(|error| anyhow::anyhow!("{pointer_url}: {error}"))?;
    // Defence in depth: the unit test is the only proof of this branch, since a
    // pointer failing it cannot carry a valid signature.
    if !manifest.key_matches_route() {
        anyhow::bail!(
            "pointer: signed key {} should be builds/{}/{}/{}",
            manifest.key,
            manifest.sha,
            manifest.sha256,
            target.binary
        );
    }

    match ordering(&current.version, current.sha, &manifest) {
        Ordering::UpToDate if !force => {
            println!(
                "SEAVX is already up to date (v{}, {})",
                manifest.version, manifest.sha
            );
            return Ok(());
        }
        Ordering::Downgrade if !force => {
            anyhow::bail!(
                "refusing to downgrade from v{} to v{}; pass --force to install it anyway",
                current.version,
                manifest.version
            );
        }
        _ => {}
    }

    println!("Downloading v{} ({})...", manifest.version, manifest.sha);
    let response = client
        .get(format!("{base}/{}", manifest.key))
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await?
        .error_for_status()?;
    let pb = progress(response.content_length());

    let dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Unable to get parent dir of executable"))?;
    // Owns the temp path: every failure below leaves the directory as it was.
    let guard = TempFile::new(dir);
    let temp = guard.path().to_path_buf();

    // Streamed straight into the temp file: the download is never held whole in
    // memory, and a broken transfer leaves no partial bytes at the target.
    use futures::StreamExt;
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(&temp).await.map_err(|error| {
        anyhow::anyhow!(
            "cannot write {} in {}: {error}",
            temp.display(),
            dir.display()
        )
    })?;
    let mut digest = Sha256::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        pb.inc(chunk.len() as u64);
        digest.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.sync_all().await?;
    drop(file);

    let actual = hex_lower(&digest.finalize());
    if let Err(message) = digest_matches(&manifest.sha256, &actual) {
        pb.abandon();
        anyhow::bail!(message);
    }
    pb.finish_with_message("Download complete");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
    }

    // The downloaded file is run to prove it is the build the pointer signed,
    // not merely bytes with the right hash.
    let stdout = probe_exe(&temp, PROBE_TIMEOUT)
        .await
        .map_err(|error| anyhow::anyhow!("{}: {error}", temp.display()))?;
    let reported = probe_version(&stdout);
    if reported.as_ref() != Some(&manifest.version) {
        anyhow::bail!(
            "the downloaded binary reports {}, the pointer expects v{}",
            reported.map_or_else(|| "no version".to_string(), |version| format!("v{version}")),
            manifest.version
        );
    }

    replace_exe(&temp, exe).map_err(|error| {
        anyhow::anyhow!(
            "Failed to replace binary at {}: {error}. You may need elevated permissions (e.g. sudo).",
            exe.display()
        )
    })?;
    guard.keep();

    let transition = if manifest.version == current.version {
        format!("v{}", manifest.version)
    } else {
        format!("v{} to v{}", current.version, manifest.version)
    };
    println!(
        "Successfully updated SEAVX {transition} ({}, {})\nsignature  ok\nchecksum   ok",
        manifest.sha,
        exe.display()
    );

    Ok(())
}

/// Run the candidate with `--version`, killing it if it hangs: a stuck probe must
/// not hold the temp file open when the guard tries to remove it.
async fn probe_exe(bin: &Path, timeout: Duration) -> anyhow::Result<String> {
    let mut command = tokio::process::Command::new(bin);
    command.arg("--version").kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| anyhow::anyhow!("did not answer --version within {timeout:?}"))??;
    if !output.status.success() {
        anyhow::bail!(
            "--version exited with {}",
            output
                .status
                .code()
                .map_or_else(|| "a signal".to_string(), |code| code.to_string())
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The temp path next to the binary, removed on every failure path unless kept.
struct TempFile(PathBuf);

impl TempFile {
    fn new(dir: &Path) -> TempFile {
        let mut name = format!(".seavx-update-{}", std::process::id());
        // Windows runs the temp file as a process, which needs the extension.
        if cfg!(windows) {
            name.push_str(".exe");
        }
        TempFile(dir.join(name))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn keep(self) {
        std::mem::forget(self);
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn progress(size: Option<u64>) -> indicatif::ProgressBar {
    // A template is a constant, so a rejected one falls back instead of panicking.
    match size {
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
    }
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
