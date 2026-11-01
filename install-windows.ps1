# Vercelify hosting on Windows: the host manager (vercelify-host) that makes
# and runs this PC's Vercelify environment (one Linux VM shared by every app,
# run by QEMU with the Windows Hypervisor Platform).
#
#   Run in an elevated PowerShell (Run as administrator):
#     Set-ExecutionPolicy -Scope Process Bypass; .\install-windows.ps1
#
# Windows hosting is in qualification: it is built and tested in CI, and
# must pass real-hardware runs before Vercelify advertises it (see
# docs/CROSS-PLATFORM-HOSTING.md). Managing other servers from Windows works
# without this script (the Vercelify installer).
#
# What it changes, after asking:
#   - turns on the "Windows Hypervisor Platform" feature (a restart follows)
#   - installs QEMU with winget
#   - %ProgramData%\Vercelify\host, readable by SYSTEM, Administrators and you
#   - a scheduled task "Vercelify host" that runs vercelify-host at startup
#   - firewall rules for a main server's two mesh ports (Private networks)
# Safe to run again.

[CmdletBinding()]
param(
    [switch]$Yes,
    # The vercelify-host.exe to install (default: next to the Vercelify app).
    [string]$HostBinary = ""
)

$ErrorActionPreference = "Stop"
$TaskName = "Vercelify host"
$Data = Join-Path $env:ProgramData "Vercelify\host"

function Step($m) { Write-Host "`n==> $m" -ForegroundColor Cyan }
function Ok($m) { Write-Host "  + $m" -ForegroundColor Green }
function Warn($m) { Write-Host "  ! $m" -ForegroundColor Yellow }
function Fail($m) { Write-Host "  x $m" -ForegroundColor Red; exit 1 }
function Confirm($q) {
    if ($Yes) { return $true }
    $a = Read-Host "  $q [y/N]"
    return $a -match '^(y|yes)$'
}

Step "Checking this PC"
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Fail "run this in an elevated PowerShell (right-click PowerShell, Run as administrator)"
}
if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -ne "AMD64") {
    Fail "Vercelify hosting needs 64-bit Windows on an Intel or AMD processor (Windows on ARM can manage servers but not host yet)"
}
$build = [int](Get-CimInstance Win32_OperatingSystem).BuildNumber
if ($build -lt 19041) { Fail "Windows 10 version 2004 (build 19041) or newer is needed; this is build $build" }
Ok "Windows build $build, x64"

# The person who ran this (the desktop app runs as them and reads the token).
$owner = [Security.Principal.WindowsIdentity]::GetCurrent().Name

Step "Windows Hypervisor Platform"
$restart = $false
$whp = Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform
if ($whp.State -eq "Enabled") {
    Ok "on"
} else {
    $fw = (Get-CimInstance Win32_Processor | Select-Object -First 1).VirtualizationFirmwareEnabled
    if ($fw -eq $false) {
        Warn "virtualization is off in this PC's firmware (BIOS/UEFI): turn on Intel VT-x / AMD-V there first"
    }
    if (Confirm "Turn on the Windows Hypervisor Platform (a restart is needed afterwards)?") {
        Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform -NoRestart -All | Out-Null
        $restart = $true
        Ok "turned on (takes effect after a restart)"
    } else {
        Fail "the environment can't run without it"
    }
}

Step "QEMU"
$qemu = Join-Path $env:ProgramFiles "qemu\qemu-system-x86_64.exe"
if (Test-Path $qemu) {
    Ok "already installed"
} else {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        Fail "winget isn't available: install QEMU from https://qemu.weilnetz.de/w64/ into $env:ProgramFiles\qemu, then run this again"
    }
    if (-not (Confirm "Install QEMU (GPL, from the winget catalogue)?")) { Fail "the environment can't run without QEMU" }
    winget install --id SoftwareFreedomConservancy.QEMU --exact --silent --accept-package-agreements --accept-source-agreements
    if (-not (Test-Path $qemu)) { Fail "QEMU didn't install to $env:ProgramFiles\qemu" }
    Ok "installed"
}

Step "vercelify-host"
if (-not $HostBinary) {
    foreach ($d in @("$env:ProgramFiles\Vercelify", "${env:ProgramFiles(x86)}\Vercelify", "$env:LOCALAPPDATA\Programs\Vercelify")) {
        if (Test-Path (Join-Path $d "vercelify-host.exe")) { $HostBinary = Join-Path $d "vercelify-host.exe"; break }
    }
}
if (-not $HostBinary -or -not (Test-Path $HostBinary)) {
    Fail "vercelify-host.exe not found: install the Vercelify app first, or pass -HostBinary PATH"
}
Ok $HostBinary

Step "Data folder and owner token"
New-Item -ItemType Directory -Force -Path $Data | Out-Null
# Only SYSTEM, Administrators and the owner: the VM's disk and its keys are here.
& icacls $Data /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" "${owner}:(OI)(CI)RX" | Out-Null
$tokenFile = Join-Path $Data "owner.token"
if (-not (Test-Path $tokenFile) -or (Get-Content $tokenFile -Raw).Trim().Length -lt 32) {
    $bytes = New-Object byte[] 32
    [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    $token = "vfyh_" + (($bytes | ForEach-Object { $_.ToString("x2") }) -join "")
    [IO.File]::WriteAllText($tokenFile, $token)
}
& icacls $tokenFile /inheritance:r /grant:r "*S-1-5-18:F" "*S-1-5-32-544:F" "${owner}:R" | Out-Null
Ok "$Data (SYSTEM, Administrators, $owner)"

Step "Firewall (a main server's mesh ports, Private networks only)"
foreach ($r in @(@{ Name = "Vercelify mesh control (TCP 7443)"; Proto = "TCP"; Port = 7443 },
                 @{ Name = "Vercelify mesh (UDP 51820)"; Proto = "UDP"; Port = 51820 })) {
    Get-NetFirewallRule -DisplayName $r.Name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    New-NetFirewallRule -DisplayName $r.Name -Direction Inbound -Action Allow -Profile Private `
        -Protocol $r.Proto -LocalPort $r.Port -Program $qemu | Out-Null
}
Ok "only used when this PC's environment is your main server"

Step "Startup task"
$action = New-ScheduledTaskAction -Execute $HostBinary -Argument "serve"
$trigger = New-ScheduledTaskTrigger -AtStartup
$who = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -StartWhenAvailable
Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Principal $who -Settings $settings -Force | Out-Null
Ok "vercelify-host starts with Windows (before anyone signs in) and restarts if it stops"

if ($restart) {
    Warn "restart Windows to finish turning on the Hypervisor Platform; then open Vercelify > Servers > Vercelify environment"
} else {
    Start-ScheduledTask -TaskName $TaskName
    Ok "running: open Vercelify > Servers > Vercelify environment"
}
Write-Host ""
Write-Host "  A sleeping PC serves nothing: in Settings > System > Power, keep it awake on power."
Write-Host "  Windows Update restarts it from time to time; your apps come back on their own afterwards."
