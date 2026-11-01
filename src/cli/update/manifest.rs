//! Signed release pointer: the fields CI publishes, the bytes minisign signs, and
//! the binding between a pointer and the binary this process is running.

use serde_json::Value;

/// Update stream: `stable` is a tagged release, `edge` is the newest master build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Edge,
}

impl Channel {
    pub fn parse(raw: &str) -> Option<Channel> {
        match raw {
            "stable" => Some(Channel::Stable),
            "edge" => Some(Channel::Edge),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Edge => "edge",
        }
    }

    /// Where the signed pointer for this channel lives, relative to the CDN root.
    /// Stable is served from `latest/`, which the worker rewrites on a tag.
    pub fn pointer_path(&self, target: &Target) -> String {
        let channel = match self {
            Channel::Stable => "latest",
            Channel::Edge => "edge",
        };
        format!("{channel}/{}/{}.json", target.os, target.arch)
    }
}

/// A CI build this updater knows how to install.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub binary: &'static str,
}

impl Target {
    pub fn from_name(name: &str) -> Option<Target> {
        TARGETS.iter().copied().find(|target| target.name == name)
    }

    /// The target this binary was compiled for, if CI publishes one for it.
    /// `std::env::consts::ARCH` spells arm64 `aarch64`; the CDN spells it `arm64`.
    pub fn detect() -> Option<Target> {
        let os = std::env::consts::OS;
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            arch => arch,
        };
        TARGETS
            .iter()
            .copied()
            .find(|target| target.os == os && target.arch == arch)
    }
}

/// Every `platform` in ci.yml's build matrix.
const TARGETS: &[Target] = &[
    Target {
        name: "linux-x86_64",
        os: "linux",
        arch: "x86_64",
        binary: "seavx-linux-x86_64",
    },
    Target {
        name: "windows-x86_64",
        os: "windows",
        arch: "x86_64",
        binary: "seavx-windows-x86_64.exe",
    },
    Target {
        name: "macos-arm64",
        os: "macos",
        arch: "arm64",
        binary: "seavx-macos-arm64",
    },
];

/// A pointer CI signed: the fields, plus the signature over them.
#[derive(Debug)]
pub struct Manifest {
    pub channel: Channel,
    pub target: Target,
    pub sha: String,
    pub key: String,
    pub version: semver::Version,
    pub built_at: String,
    pub sha256: String,
    sig: String,
}

impl Manifest {
    /// Parse the pointer JSON, naming every field that is missing or malformed.
    pub fn parse(json: &str) -> Result<Manifest, String> {
        let raw: Value = serde_json::from_str(json).map_err(|error| format!("pointer: {error}"))?;
        let field = |name: &str| -> Result<&str, String> {
            raw.get(name)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{name}: missing or not a string"))
        };

        let channel = field("channel")?;
        let channel = Channel::parse(channel)
            .ok_or_else(|| format!("channel: unknown channel {channel:?}"))?;
        let target = field("target")?;
        let target = Target::from_name(target)
            .ok_or_else(|| format!("target: unknown target {target:?}"))?;
        let sha = field("sha")?;
        // CI names the build with a truncated `git rev-parse`, always lowercase.
        if !lowercase_hex(sha, 7) {
            return Err(format!(
                "sha: expected 7 lowercase hex characters, got {sha:?}"
            ));
        }
        let sha256 = field("sha256")?;
        if !lowercase_hex(sha256, 64) {
            return Err(format!(
                "sha256: expected 64 lowercase hex characters, got {sha256:?}"
            ));
        }
        let built_at = field("built_at")?;
        if !valid_built_at(built_at) {
            return Err(format!(
                "built_at: expected YYYY-MM-DDTHH:MM:SSZ, got {built_at:?}"
            ));
        }
        let version = field("version")?;
        let version =
            semver::Version::parse(version).map_err(|error| format!("version: {error}"))?;
        let sig = field("sig")?;
        minisign_verify::Signature::decode(sig).map_err(|error| format!("sig: {error}"))?;

        Ok(Manifest {
            channel,
            target,
            sha: sha.to_string(),
            key: field("key")?.to_string(),
            version,
            built_at: built_at.to_string(),
            sha256: sha256.to_string(),
            sig: sig.to_string(),
        })
    }

    /// Accept the pointer only when one of `keys` signed its canonical bytes.
    /// An empty key set refuses: a build with no pinned key cannot update.
    pub fn verify(&self, keys: &[&str]) -> Result<(), String> {
        if keys.is_empty() {
            return Err("no pinned update key".to_string());
        }
        let signature = minisign_verify::Signature::decode(&self.sig)
            .map_err(|error| format!("sig: {error}"))?;
        let bytes = self.signed_bytes();
        for key in keys {
            let public_key = minisign_verify::PublicKey::from_base64(key)
                .map_err(|error| format!("pinned update key {key}: {error}"))?;
            // `false` rejects a legacy non-prehashed signature; `minisign -S`
            // produces prehashed ones by default.
            if public_key.verify(&bytes, &signature, false).is_ok() {
                return Ok(());
            }
        }
        Err("signature: not signed by a pinned update key".to_string())
    }

    /// Refuse a pointer that was signed for a different channel.
    pub fn check_channel(&self, requested: Channel) -> Result<(), String> {
        if self.channel == requested {
            return Ok(());
        }
        Err(format!(
            "pointer is for the {} channel, not {}",
            self.channel.name(),
            requested.name()
        ))
    }

    /// Refuse a pointer that was signed for another build target.
    pub fn check_target(&self, target: &Target) -> Result<(), String> {
        if self.target.name == target.name {
            return Ok(());
        }
        Err(format!(
            "pointer is for target {}, not {}",
            self.target.name, target.name
        ))
    }

    /// The download path must be the one the digest names, not merely start with
    /// it: `builds/{sha}/{sha256}/{binary}`.
    pub fn key_matches_route(&self) -> bool {
        self.key == format!("builds/{}/{}/{}", self.sha, self.sha256, self.target.binary)
    }

    /// The exact bytes minisign signed, built from the validated fields in the
    /// fixed v1 order: never a re-serialisation of the incoming JSON.
    pub fn signed_bytes(&self) -> Vec<u8> {
        format!(
            "SEAVX-UPDATE-v1\nchannel={}\ntarget={}\nsha={}\nkey={}\nversion={}\nbuilt_at={}\nsha256={}\n",
            self.channel.name(),
            self.target.name,
            self.sha,
            self.key,
            self.version,
            self.built_at,
            self.sha256
        )
        .into_bytes()
    }
}

/// What a verified pointer means for the binary that is running.
#[derive(Debug, PartialEq, Eq)]
pub enum Ordering {
    UpToDate,
    Newer,
    SameVersionNewCommit,
    Downgrade,
}

/// CI republishes every master build under the same unreleased version, so an
/// equal version alone does not mean the running binary is that build. A build
/// made outside git records no commit, and then the version stands alone.
pub fn ordering(current: &semver::Version, current_sha: Option<&str>, next: &Manifest) -> Ordering {
    match next.version.cmp(current) {
        std::cmp::Ordering::Greater => Ordering::Newer,
        std::cmp::Ordering::Less => Ordering::Downgrade,
        std::cmp::Ordering::Equal => match current_sha {
            Some(sha) if !sha.eq_ignore_ascii_case(&next.sha) => Ordering::SameVersionNewCommit,
            _ => Ordering::UpToDate,
        },
    }
}

/// Lowercase hex, the spelling `sha256sum` and the CI pointer use.
pub fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Compare what the pointer published with the bytes actually received.
pub fn digest_matches(expected: &str, actual: &str) -> Result<(), String> {
    if expected.eq_ignore_ascii_case(actual) {
        return Ok(());
    }
    Err(format!(
        "download does not match the published checksum: expected {expected}, got {actual}"
    ))
}

/// The first token of `--version` output that is a version: the binary's own
/// name and the commit suffix are skipped rather than parsed.
pub fn probe_version(stdout: &str) -> Option<semver::Version> {
    stdout
        .split_whitespace()
        .find_map(|token| semver::Version::parse(token).ok())
}

fn lowercase_hex(raw: &str, len: usize) -> bool {
    raw.len() == len
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

// shortcut: no leap second or fractional seconds, upgrade if CI's date format changes
fn valid_built_at(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return false;
    }
    let number = |range: std::ops::Range<usize>| {
        std::str::from_utf8(&bytes[range]).ok()?.parse::<u32>().ok()
    };
    let (Some(_year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0..4),
        number(5..7),
        number(8..10),
        number(11..13),
        number(14..16),
        number(17..19),
    ) else {
        return false;
    };
    (1..=12).contains(&month) && (1..=31).contains(&day) && hour < 24 && minute < 60 && second < 60
}

#[cfg(test)]
#[path = "../../tests/cli/update/manifest.rs"]
mod tests;
