[CmdletBinding()]
param(
    [string] $Launcher,
    [string] $TrayScript,
    [string] $Icon,
    [string] $Config,
    [switch] $Development,
    [switch] $Published,
    [switch] $NoWatch,
    [switch] $NoStartup
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ( $Development -and $Published ) { throw 'Choose either -Development or -Published, not both.' }
$developmentMode = $Development -or -not $Published

if ( [string]::IsNullOrWhiteSpace( $Launcher ) ) { $Launcher = Join-Path $PSScriptRoot 'start-applejack.ps1' }
if ( [string]::IsNullOrWhiteSpace( $TrayScript ) ) { $TrayScript = Join-Path $PSScriptRoot 'Cellar-Tray.ps1' }
if ( [string]::IsNullOrWhiteSpace( $Icon ) ) { $Icon = Join-Path $PSScriptRoot '..\assets\cellar-applejack.ico' }
if ( [string]::IsNullOrWhiteSpace( $Config ) ) {
    $Config = Join-Path $env:LOCALAPPDATA 'AppleJackRP\cellar.toml'
}

foreach ( $required in @($Launcher, $TrayScript, $Icon) ) {
    if ( -not (Test-Path -LiteralPath $required) ) { throw "Required launcher asset not found: $required" }
}

if ( -not (Test-Path -LiteralPath $Config) ) {
    $templateName = if ( $developmentMode ) { 'applejackrp-windows.toml' } else { 'applejackrp-public-windows.toml' }
    $profile = Join-Path $PSScriptRoot "..\configs\$templateName"
    if ( -not (Test-Path -LiteralPath $profile) ) { throw "Cellar config not found at $Config." }
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Config) | Out-Null
    Copy-Item -LiteralPath $profile -Destination $Config
    Write-Output "Created $Config from the AppleJackRP $(if ($developmentMode) { 'local development' } else { 'published' }) profile."
}

$profileDirectory = Split-Path -Parent $Config
$profileCopies = @{
    'applejackrp-development.toml' = 'applejackrp-windows.toml'
    'applejackrp-published.toml' = 'applejackrp-public-windows.toml'
}
foreach ( $profileCopy in $profileCopies.GetEnumerator() ) {
    $sourceProfile = Join-Path $PSScriptRoot "..\configs\$($profileCopy.Value)"
    $targetProfile = Join-Path $profileDirectory $profileCopy.Key
    if ( (Test-Path -LiteralPath $sourceProfile) -and -not (Test-Path -LiteralPath $targetProfile) ) {
        Copy-Item -LiteralPath $sourceProfile -Destination $targetProfile
    }
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
$modeArgument = if ( $developmentMode ) { ' -Development' } else { ' -Published' }
$watchArgument = if ( $developmentMode -and -not $NoWatch ) { ' -Watch' } else { '' }
New-AppleJackShortcut $desktopShortcut $Launcher "-Config `"$Config`" -OpenDashboard$modeArgument$watchArgument"

if ( -not $NoStartup ) {
    $startup = [Environment]::GetFolderPath('Startup')
    $trayShortcut = Join-Path $startup 'AppleJackRP Cellar Tray.lnk'
    New-AppleJackShortcut $trayShortcut $TrayScript "-Config `"$Config`""
}

Write-Output "Created $desktopShortcut"
if ( -not $NoStartup ) { Write-Output "Created the AppleJackRP tray shortcut in the Windows Startup folder." }
