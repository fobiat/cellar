[CmdletBinding()]
param(
    [string] $Cellar,
    [Parameter(Mandatory)] [string] $Config,
    [Parameter(Mandatory)] [string] $AppleJackRoot,
    [Parameter(Mandatory)] [string] $RuntimeRoot,
    [string] $Web = 'http://127.0.0.1:8081',
    [int] $DebounceSeconds = 2
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ( [string]::IsNullOrWhiteSpace( $Cellar ) ) {
    $Cellar = Join-Path $env:LOCALAPPDATA 'Programs\Cellar\cellar.exe'
}

$syncScript = Join-Path $AppleJackRoot 'tools\sync-cellar-runtime.ps1'
$logFile = Join-Path $env:LOCALAPPDATA 'AppleJackRP\hot-reload.log'
$ignoredDirectories = @('.git', '.sbox', 'bin', 'obj', 'node_modules', 'dist')
$watchedExtensions = @('.cs', '.cshtml', '.razor', '.scss', '.json', '.sbproj', '.config', '.prefab', '.scene', '.toml', '.yaml', '.yml')

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $logFile) | Out-Null

function Write-WatchLog([string] $Message) {
    Add-Content -LiteralPath $logFile -Value "$(Get-Date -Format o) $Message"
}

function Test-SourceChange([string] $FullPath) {
    if ( [string]::IsNullOrWhiteSpace( $FullPath ) ) { return $false }
    $root = ([IO.Path]::GetFullPath( $AppleJackRoot )).TrimEnd('\') + '\'
    $full = [IO.Path]::GetFullPath( $FullPath )
    if ( -not $full.StartsWith( $root, [StringComparison]::OrdinalIgnoreCase ) ) { return $false }

    $relative = $full.Substring( $root.Length )
    foreach ( $segment in $relative.Split('\') ) {
        if ( $ignoredDirectories -contains $segment ) { return $false }
    }
    return $watchedExtensions -contains ([IO.Path]::GetExtension( $full ).ToLowerInvariant())
}

function Sync-And-Restart {
    try {
        $status = Invoke-RestMethod -Uri ($Web.TrimEnd('/') + '/api/status') -Method Get -TimeoutSec 5
        if ( $status.mode -eq 'published' ) {
            Write-WatchLog 'source change ignored while published mode is active'
            return
        }
    } catch {
        Write-WatchLog 'could not read active mode, continuing with development sync'
    }

    if ( -not (Test-Path -LiteralPath $syncScript) ) {
        Write-WatchLog 'source change noticed, but sync script is missing'
        return
    }

    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $syncScript `
        -SourceRoot $AppleJackRoot -RuntimeRoot $RuntimeRoot | Out-Null
    $syncExitCode = $LASTEXITCODE
    if ( $syncExitCode -gt 7 ) {
        Write-WatchLog "runtime sync failed with robocopy exit code $syncExitCode"
        return
    }

    try {
        Invoke-WebRequest -Method Post -Uri ($Web.TrimEnd('/') + '/api/control/restart') `
            -UseBasicParsing -TimeoutSec 5 | Out-Null
        Write-WatchLog 'runtime synced and supervised server restarted'
    } catch {
        Write-WatchLog "runtime synced, but supervised server restart failed: $($_.Exception.Message)"
    }
}

$watcher = [IO.FileSystemWatcher]::new($AppleJackRoot)
$watcher.IncludeSubdirectories = $true
$watcher.NotifyFilter = [IO.NotifyFilters]::FileName -bor [IO.NotifyFilters]::LastWrite -bor [IO.NotifyFilters]::Size
$subscriptions = @(
    (Register-ObjectEvent -InputObject $watcher -EventName Changed),
    (Register-ObjectEvent -InputObject $watcher -EventName Created),
    (Register-ObjectEvent -InputObject $watcher -EventName Deleted),
    (Register-ObjectEvent -InputObject $watcher -EventName Renamed)
)
$watcher.EnableRaisingEvents = $true
$pendingAt = $null
$lastChange = $null
Write-WatchLog "watching $AppleJackRoot"

try {
    while ( $true ) {
        $event = Wait-Event -Timeout 1
        if ( $null -ne $event ) {
            $fullPath = $event.SourceEventArgs.FullPath
            Remove-Event -EventIdentifier $event.EventIdentifier
            if ( Test-SourceChange $fullPath ) {
                $pendingAt = Get-Date
                $lastChange = $fullPath
            }
        }

        if ( $null -ne $pendingAt -and ((Get-Date) - $pendingAt).TotalSeconds -ge $DebounceSeconds ) {
            Write-WatchLog "source change detected at $lastChange"
            Sync-And-Restart
            $pendingAt = $null
            $lastChange = $null
        }
    }
} finally {
    $watcher.EnableRaisingEvents = $false
    $subscriptions | ForEach-Object { Unregister-Event -SubscriptionId $_.Id -ErrorAction SilentlyContinue }
    $watcher.Dispose()
}
