#Requires -Version 5.1
# Seavx installer for Windows: fetches the newest CI build from cdn.seavx.com.
$ErrorActionPreference = 'Stop'

$InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\seavx'
$Exe = Join-Path $InstallDir 'seavx.exe'

Write-Host '=== Seavx Installer (Windows) ==='
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

Write-Host 'Downloading the newest build from cdn.seavx.com ...'
try {
    Invoke-WebRequest -Uri 'https://cdn.seavx.com/latest/windows/x86_64' -OutFile $Exe -UseBasicParsing
} catch {
    Write-Host "No prebuilt build is available right now ($($_.Exception.Message))."
    Write-Host 'Build from source instead: cargo install --path .'
    exit 1
}

# User PATH only: no administrator rights needed.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $InstallDir) {
    $newPath = (@($userPath, $InstallDir) | Where-Object { $_ }) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host "Added $InstallDir to your user PATH (open a new terminal to use it)."
}

Write-Host "Installed $(& $Exe --version) -> $Exe"
