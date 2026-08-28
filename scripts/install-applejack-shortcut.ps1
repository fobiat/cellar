[CmdletBinding()]
param(
    [string] $Launcher,
    [string] $TrayScript,
    [string] $Icon,
    [switch] $NoStartup
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ( [string]::IsNullOrWhiteSpace( $Launcher ) ) { $Launcher = Join-Path $PSScriptRoot 'start-applejack.ps1' }
if ( [string]::IsNullOrWhiteSpace( $TrayScript ) ) { $TrayScript = Join-Path $PSScriptRoot 'Cellar-Tray.ps1' }
if ( [string]::IsNullOrWhiteSpace( $Icon ) ) { $Icon = Join-Path $PSScriptRoot '..\assets\cellar-applejack.ico' }

foreach ( $required in @($Launcher, $TrayScript, $Icon) ) {
    if ( -not (Test-Path -LiteralPath $required) ) { throw "Required launcher asset not found: $required" }
}

$shell = New-Object -ComObject WScript.Shell
$powerShell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$workingDirectory = Split-Path -Parent $Launcher

function New-AppleJackShortcut([string] $Destination, [string] $TargetScript, [string] $Arguments) {
    $shortcut = $shell.CreateShortcut($Destination)
    $shortcut.TargetPath = $powerShell
    $shortcut.Arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$TargetScript`" $Arguments"
    $shortcut.WorkingDirectory = $workingDirectory
    $shortcut.IconLocation = "$Icon,0"
    $shortcut.Description = 'Start and manage the AppleJackRP s&box server.'
    $shortcut.Save()
}

$desktop = [Environment]::GetFolderPath('Desktop')
$desktopShortcut = Join-Path $desktop 'AppleJackRP.lnk'
New-AppleJackShortcut $desktopShortcut $Launcher '-OpenDashboard'

if ( -not $NoStartup ) {
    $startup = [Environment]::GetFolderPath('Startup')
    $trayShortcut = Join-Path $startup 'AppleJackRP Cellar Tray.lnk'
    New-AppleJackShortcut $trayShortcut $TrayScript ''
}

Write-Output "Created $desktopShortcut"
if ( -not $NoStartup ) { Write-Output "Created the AppleJackRP tray shortcut in the Windows Startup folder." }
