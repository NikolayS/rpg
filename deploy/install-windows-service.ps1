# Install Samo as a Windows service using NSSM (Non-Sucking Service Manager).
#
# Prerequisites:
#   1. Download NSSM from https://nssm.cc/download
#   2. Place nssm.exe in PATH or current directory
#   3. Place samo.exe in C:\Program Files\Samo\
#   4. Create config at C:\ProgramData\Samo\config.toml
#
# Usage (run as Administrator):
#   .\install-windows-service.ps1
#
# To remove:
#   nssm remove Samo confirm

$ErrorActionPreference = "Stop"

$ServiceName = "Samo"
$SamoBin = "C:\Program Files\Samo\samo.exe"
$ConfigPath = "C:\ProgramData\Samo\config.toml"
$LogDir = "C:\ProgramData\Samo\logs"

# Verify prerequisites
if (-not (Test-Path $SamoBin)) {
    Write-Error "Samo binary not found at $SamoBin"
    exit 1
}

# Create log directory
if (-not (Test-Path $LogDir)) {
    New-Item -ItemType Directory -Path $LogDir -Force | Out-Null
}

# Check for nssm
$nssm = Get-Command nssm -ErrorAction SilentlyContinue
if (-not $nssm) {
    Write-Error "NSSM not found. Install from https://nssm.cc/download"
    exit 1
}

# Install service
nssm install $ServiceName $SamoBin "daemon --config `"$ConfigPath`""
nssm set $ServiceName AppDirectory "C:\ProgramData\Samo"
nssm set $ServiceName DisplayName "Samo - Self-Driving Postgres Agent"
nssm set $ServiceName Description "Autonomous Postgres monitoring and management daemon"
nssm set $ServiceName Start SERVICE_AUTO_START
nssm set $ServiceName AppStdout "$LogDir\stdout.log"
nssm set $ServiceName AppStderr "$LogDir\stderr.log"
nssm set $ServiceName AppStdoutCreationDisposition 4  # Append
nssm set $ServiceName AppStderrCreationDisposition 4  # Append
nssm set $ServiceName AppRotateFiles 1
nssm set $ServiceName AppRotateBytes 10485760  # 10MB

Write-Host "Service '$ServiceName' installed successfully."
Write-Host "Start with: nssm start $ServiceName"
Write-Host "Status:     nssm status $ServiceName"
Write-Host "Remove:     nssm remove $ServiceName confirm"
