#Requires -Version 5.1
# Seavx installer for Windows: fetches and verifies the newest CI build.
$ErrorActionPreference = 'Stop'

# Test seam, shared with the client (`SEAVX_UPDATE_BASE`): it moves the origin only.
$Base = if ($env:SEAVX_UPDATE_BASE) { $env:SEAVX_UPDATE_BASE } else { 'https://cdn.seavx.com' }
# Trimmed once: every URL below is built with a single `/`.
$Base = $Base.TrimEnd('/')

# TODO(update): paste the Task 0 release public key here, as in `PINNED_KEYS`
# (src/cli/update.rs). While it is empty the install refuses, because a checksum
# that arrives from an unsigned pointer authenticates nothing.
$UpdateKey = ''

$Target = 'windows-x86_64'
# The last segment of the signed route, i.e. `Target::binary`.
$Binary = 'seavx-windows-x86_64'
$Pointer = "$Base/latest/windows/x86_64.json"

$InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\seavx'
$Exe = Join-Path $InstallDir 'seavx.exe'
# Replaced after every check passed, and deleted on every failure, so the
# installed exe is only ever written by Move-Item below.
$Temp = "$Exe.tmp"
$ManifestFile = "$Temp.manifest"
$SigFile = "$Temp.sig"

Write-Host '=== Seavx Installer (Windows) ==='
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

try {
    if (-not $UpdateKey) {
        throw "this installer pins no release key, so the signed pointer cannot be verified.`n`       The release signing key is not published yet (SDD Task 0); refusing to install.`n`       Build from source instead: cargo install --path ."
    }

    Write-Host "Verifying the release pointer at $Pointer ..."
    $pointer = (Invoke-WebRequest -Uri $Pointer -UseBasicParsing).Content | ConvertFrom-Json

    if ($pointer.channel -cne 'stable') {
        throw "the pointer is for the $($pointer.channel) channel, not stable."
    }
    if ($pointer.target -cne $Target) {
        throw "the pointer is for target $($pointer.target), not $Target."
    }
    # The same shapes `Manifest::parse` enforces: a pointer the client rejects must
    # not be installable either, or the binary can never update itself.
    # A missing field makes `-notmatch` true, so absent values are refused here.
    if ($pointer.sha -notmatch '^[0-9a-f]{7}$') {
        throw "sha: expected 7 lowercase hex characters, got `"$($pointer.sha)`"."
    }
    if ($pointer.sha256 -notmatch '^[0-9a-f]{64}$') {
        throw "sha256: expected 64 lowercase hex characters, got `"$($pointer.sha256)`"."
    }
    if ($pointer.version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$') {
        throw "version: expected a SemVer version, got `"$($pointer.version)`"."
    }
    if ($pointer.built_at -notmatch '^\d{4}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])T([01]\d|2[0-3]):[0-5]\d:[0-5]\dZ$') {
        throw "built_at: expected YYYY-MM-DDTHH:MM:SSZ, got `"$($pointer.built_at)`"."
    }
    # `key_matches_route`: the download path is exactly the one the digest names.
    if ($pointer.key -cne "builds/$($pointer.sha)/$($pointer.sha256)/$Binary") {
        throw "key: expected builds/$($pointer.sha)/$($pointer.sha256)/$Binary, got `"$($pointer.key)`"."
    }

    # The exact bytes minisign signed: `.github/scripts/pointer-manifest.sh` order,
    # which `manifest.rs::signed_bytes` reproduces byte for byte.
    $manifest = "SEAVX-UPDATE-v1`n" +
        "channel=$($pointer.channel)`n" +
        "target=$($pointer.target)`n" +
        "sha=$($pointer.sha)`n" +
        "key=$($pointer.key)`n" +
        "version=$($pointer.version)`n" +
        "built_at=$($pointer.built_at)`n" +
        "sha256=$($pointer.sha256)`n"
    [IO.File]::WriteAllText($ManifestFile, $manifest)

    if (Get-Command minisign -ErrorAction SilentlyContinue) {
        # `sig` is one JSON string with `\n` escapes, which ConvertFrom-Json has
        # already turned into the 4 lines minisign reads.
        [IO.File]::WriteAllText($SigFile, $pointer.sig)
        & minisign -V -m $ManifestFile -x $SigFile -P $UpdateKey
        if ($LASTEXITCODE -ne 0) { throw 'signature: not signed by the pinned release key.' }
    }
    else {
        Write-Host 'warning: minisign is not installed; the checksum only detects transfer corruption, not a forged pointer or publisher'
    }

    Write-Host "Downloading $($pointer.version) ($($pointer.sha)) ..."
    Invoke-WebRequest -Uri "$Base/$($pointer.key)" -OutFile $Temp -UseBasicParsing

    $actual = (Get-FileHash -Path $Temp -Algorithm SHA256).Hash.ToLowerInvariant()
    $expected = $pointer.sha256.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "the download does not match the published checksum: expected $expected, got $actual"
    }

    Move-Item -Force -Path $Temp -Destination $Exe
}
catch {
    # `exit` does not reliably run a finally block on 5.1, so cleanup is explicit.
    Remove-Item -Force -ErrorAction SilentlyContinue $SigFile, $ManifestFile, $Temp
    Write-Host "error: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}

Remove-Item -Force -ErrorAction SilentlyContinue $SigFile, $ManifestFile

# User PATH only: no administrator rights needed.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $InstallDir) {
    $newPath = (@($userPath, $InstallDir) | Where-Object { $_ }) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host "Added $InstallDir to your user PATH (open a new terminal to use it)."
}

Write-Host "Installed $(& $Exe --version) -> $Exe"
