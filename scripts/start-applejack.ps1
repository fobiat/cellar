[CmdletBinding()]
param(
    [string] $Cellar,
    [string] $Config,
    [string] $AppleJackRoot,
    [string] $RuntimeRoot,
    [string] $Web = 'http://127.0.0.1:8081/',
    [switch] $CheckOnly,
    [switch] $SkipSync,
    [switch] $NoTray,
    [switch] $Development,
    [switch] $Published,
    [switch] $Watch,
    [switch] $OpenDashboard
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ( $Development -and $Published ) { throw 'Choose either -Development or -Published, not both.' }

$script:RepoRoot = Split-Path -Parent $PSScriptRoot
if ( [string]::IsNullOrWhiteSpace( $Cellar ) ) {
    $Cellar = Join-Path $env:LOCALAPPDATA 'Programs\Cellar\cellar.exe'
}
if ( [string]::IsNullOrWhiteSpace( $Config ) ) {
    $Config = Join-Path $env:ProgramData 'Cellar\cellar.toml'
}
if ( [string]::IsNullOrWhiteSpace( $AppleJackRoot ) ) {
    $AppleJackRoot = Join-Path (Split-Path -Parent $script:RepoRoot) 'AppleJackRP-sandbox'
}
if ( [string]::IsNullOrWhiteSpace( $RuntimeRoot ) ) {
    $RuntimeRoot = 'C:\AppleJackServer\applejackrp-runtime'
}

$templateName = if ( $Published ) { 'applejackrp-public-windows.toml' } else { 'applejackrp-windows.toml' }
$template = Join-Path $script:RepoRoot "configs\$templateName"
$syncScript = Join-Path $AppleJackRoot 'tools\sync-cellar-runtime.ps1'
$trayScript = Join-Path $PSScriptRoot 'Cellar-Tray.ps1'
$watchScript = Join-Path $PSScriptRoot 'watch-applejack.ps1'

function Show-Notice([string] $Message, [string] $Title = 'AppleJackRP') {
    Add-Type -AssemblyName System.Windows.Forms
    [System.Windows.Forms.MessageBox]::Show(
        $Message, $Title, 'OK', 'Information') | Out-Null
}

function Ensure-Config {
    if ( Test-Path -LiteralPath $Config ) { return }
    if ( -not ( Test-Path -LiteralPath $template ) ) {
        throw "Cellar config not found at $Config and no AppleJackRP template exists at $template."
    }
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Config) | Out-Null
    Copy-Item -LiteralPath $template -Destination $Config
    Show-Notice "Created $Config from the AppleJackRP template. Review its server paths before starting."
}

function Repair-LegacyDatabaseUrlSetting {
    if ( -not (Test-Path -LiteralPath $Config) ) { return }
    $encoding = [Text.Encoding]::UTF8
    $text = $encoding.GetString([IO.File]::ReadAllBytes($Config))
    $match = [regex]::Match($text, "(?m)^\s*#\s*url_file\s*=\s*'([^']+)'\s*$")
    if ( -not $match.Success ) { return }

    $urlFile = $match.Groups[1].Value
    if ( -not (Test-Path -LiteralPath $urlFile) ) { return }

    Copy-Item -LiteralPath $Config -Destination "$Config.bak" -Force
    $replacement = "url_file = '$urlFile'"
    $text = $text.Remove($match.Index, $match.Length).Insert($match.Index, $replacement)
    [IO.File]::WriteAllBytes($Config, $encoding.GetBytes($text))
}

function Invoke-Cellar([string[]] $Arguments) {
    $previousErrorAction = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $output = @(& $Cellar -c $Config @Arguments 2>&1)
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorAction
    }
    [pscustomobject]@{
        ExitCode = $exitCode
        Output = ($output -join [Environment]::NewLine)
    }
}

function Test-CellarWeb {
    try {
        $response = Invoke-WebRequest -Uri ($Web.TrimEnd('/') + '/healthz') -UseBasicParsing -TimeoutSec 2
        return $response.StatusCode -eq 200
    } catch {
        return $false
    }
}

function Wait-CellarWeb([int] $TimeoutSeconds = 30) {
    $deadline = (Get-Date).AddSeconds( $TimeoutSeconds )
    do {
        if ( Test-CellarWeb ) { return $true }
        Start-Sleep -Milliseconds 500
    } while ( (Get-Date) -lt $deadline )
    return $false
}

function Ensure-CellarTray {
    if ( $NoTray -or -not (Test-Path -LiteralPath $trayScript) ) { return }
    $trayProcesses = @(Get-CimInstance Win32_Process -Filter "Name = 'powershell.exe'" |
        Where-Object { $_.CommandLine -like '*Cellar-Tray.ps1*' })
    if ( $trayProcesses.Count -gt 0 ) { return }
    Start-Process powershell.exe -WindowStyle Hidden -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $trayScript,
        '-Cellar', $Cellar, '-Config', $Config, '-Web', $Web)
}

function Ensure-AppleJackWatch {
    if ( -not $Watch -or -not (Test-Path -LiteralPath $watchScript) ) { return }
    $watchProcesses = @(Get-CimInstance Win32_Process -Filter "Name = 'powershell.exe'" |
        Where-Object { $_.CommandLine -like '*watch-applejack.ps1*' })
    if ( $watchProcesses.Count -gt 0 ) { return }
    Start-Process powershell.exe -WindowStyle Hidden -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $watchScript,
        '-Cellar', $Cellar, '-Config', $Config, '-AppleJackRoot', $AppleJackRoot,
        '-RuntimeRoot', $RuntimeRoot, '-Web', $Web)
}

function Test-UpdateAvailable([string] $Output) {
    return $Output -match '(?i)update is available|is available \(running|published\s+build .+available|remote\s+.+differs'
}

function Confirm-Update([string] $Label, [string] $CheckOutput, [string[]] $ApplyArguments) {
    if ( -not ( Test-UpdateAvailable $CheckOutput ) ) { return }
    Add-Type -AssemblyName System.Windows.Forms
    $answer = [System.Windows.Forms.MessageBox]::Show(
        "$Label update is available.`n`n$CheckOutput`n`nInstall it now?",
        'AppleJackRP update available', 'YesNo', 'Question')
    if ( $answer -ne [System.Windows.Forms.DialogResult]::Yes ) { return }

    $result = Invoke-Cellar $ApplyArguments
    if ( $result.ExitCode -ne 0 ) {
        Show-Notice "$Label update failed.`n`n$($result.Output)" 'AppleJackRP update failed'
        return
    }
    Show-Notice "$Label update completed.`n`n$($result.Output)"
}

function Check-For-Updates([switch] $AllowApply) {
    $cellarCheck = Invoke-Cellar @('self-update', '--check')
    if ( $cellarCheck.ExitCode -eq 0 ) {
        if ( $AllowApply ) {
            Confirm-Update 'Cellar' $cellarCheck.Output @('self-update')
        } elseif ( Test-UpdateAvailable $cellarCheck.Output ) {
            Show-Notice 'A Cellar update is available. Stop Cellar and launch AppleJackRP from the desktop to install it.'
        }
    }

    $gameCheck = Invoke-Cellar @('update', '--check')
    if ( $gameCheck.ExitCode -eq 0 ) {
        if ( $AllowApply ) {
            Confirm-Update 'AppleJackRP' $gameCheck.Output @('update', '--now')
        } elseif ( Test-UpdateAvailable $gameCheck.Output ) {
            Show-Notice 'An AppleJackRP update is available. Stop the server and launch AppleJackRP from the desktop to install it.'
        }
    }
}

try {
    Ensure-Config
    Repair-LegacyDatabaseUrlSetting
    if ( -not (Test-Path -LiteralPath $Cellar) ) {
        throw "Cellar executable not found at $Cellar. Install Cellar first or pass -Cellar."
    }

    $configCheck = Invoke-Cellar @('config')
    if ( $configCheck.ExitCode -ne 0 ) {
        throw "Cellar configuration is invalid.`n`n$($configCheck.Output)"
    }

    if ( $CheckOnly ) {
        Check-For-Updates
        exit 0
    }

    if ( Test-CellarWeb ) {
        Ensure-CellarTray
        Ensure-AppleJackWatch
        Start-Process $Web
        Show-Notice 'Cellar is already running. Opened the dashboard instead of starting a second instance.'
        exit 0
    }

    Check-For-Updates -AllowApply

    if ( -not $SkipSync -and (Test-Path -LiteralPath $syncScript) ) {
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $syncScript `
            -SourceRoot $AppleJackRoot -RuntimeRoot $RuntimeRoot
        if ( $LASTEXITCODE -gt 7 ) {
            throw "AppleJackRP runtime sync failed with robocopy exit code $LASTEXITCODE."
        }
    }

    $process = Start-Process -FilePath $Cellar -ArgumentList @('-c', $Config, 'run') `
        -WorkingDirectory (Split-Path -Parent $Cellar) -WindowStyle Hidden -PassThru

    if ( -not (Wait-CellarWeb) ) {
        $doctor = Invoke-Cellar @('doctor')
        $details = if ( $doctor.Output ) { $doctor.Output } else { 'Cellar did not expose its health endpoint within 30 seconds.' }
        throw "AppleJackRP did not start.`n`n$details"
    }

    $doctor = Invoke-Cellar @('doctor')
    if ( $doctor.ExitCode -ne 0 ) {
        Show-Notice "Cellar started, but its health check found a problem.`n`n$($doctor.Output)" 'AppleJackRP needs attention'
    }

    Ensure-CellarTray
    Ensure-AppleJackWatch

    if ( $OpenDashboard ) {
        Start-Sleep -Milliseconds 750
        Start-Process $Web
    }

    Write-Output "AppleJackRP Cellar started with process id $($process.Id)."
} catch {
    $message = ($_ | Out-String).Trim()
    try {
        Show-Notice $message 'AppleJackRP cannot start'
    } catch {
        Write-Error $message
    }
    exit 1
}
