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

    if ($pointer.channel -ne 'stable') {
        throw "the pointer is for the $($pointer.channel) channel, not stable."
    }
    if ($pointer.target -ne $Target) {
        throw "the pointer is for target $($pointer.target), not $Target."
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
    if ($actual -ne $pointer.sha256) {
        throw "the download does not match the published checksum: expected $($pointer.sha256), got $actual"
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
