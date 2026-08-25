<#
.SYNOPSIS
    Install Cellar on Windows.

.DESCRIPTION
    Downloads the latest release, verifies its SHA-256 against the published
    checksum, installs to a per-user location, and puts it on PATH.

    Per-user by default, under %LOCALAPPDATA%. That needs no administrator, and
    a game server manager has no business writing to Program Files just to run
    as the person who is already logged in. Use -System for an all-users
    install, which does need an elevated shell.

.EXAMPLE
    irm https://raw.githubusercontent.com/fobiat/cellar/main/scripts/install.ps1 | iex

.EXAMPLE
    .\install.ps1 -Version v0.1.0 -Service
#>
[CmdletBinding()]
param(
    # Release tag. Defaults to the latest.
    [string] $Version = 'latest',

    # Install for all users, into Program Files. Needs an elevated shell.
    [switch] $System,

    # Register a Windows service that runs `cellar run` at boot.
    [switch] $Service,

    # Install from a local zip instead of downloading. For testing a build
    # before it is published.
    [string] $FromFile
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = 'fobiat/cellar'
$target = 'x86_64-pc-windows'

function Write-Step { param([string] $Message) Write-Host "  $Message" -ForegroundColor Cyan }
function Write-Done { param([string] $Message) Write-Host "  $Message" -ForegroundColor Green }
function Write-Note { param([string] $Message) Write-Host "  $Message" -ForegroundColor DarkGray }

Write-Host ''
Write-Host '  * CELLAR' -ForegroundColor Blue
Write-Host '    a dedicated server manager for s&box' -ForegroundColor DarkGray
Write-Host ''

# ---------------------------------------------------------------- destination

if ($System) {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw '-System needs an elevated PowerShell. Run as administrator, or drop -System for a per-user install.'
    }
    $installDir = Join-Path $env:ProgramFiles 'Cellar'
    $pathScope = 'Machine'
} else {
    $installDir = Join-Path $env:LOCALAPPDATA 'Programs\Cellar'
    $pathScope = 'User'
}

$dataDir = Join-Path $env:ProgramData 'Cellar'
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
New-Item -ItemType Directory -Force -Path $dataDir | Out-Null

# ----------------------------------------------------------------- get the zip

$temp = Join-Path ([IO.Path]::GetTempPath()) ("cellar-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $temp | Out-Null

try {
    if ($FromFile) {
        Write-Step "Using $FromFile"
        $zip = $FromFile
        $expected = $null
    } else {
        # TLS 1.2 explicitly: Windows PowerShell 5.1 still defaults to SSL3/TLS1
        # on some builds, and GitHub refuses both.
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

        $api = if ($Version -eq 'latest') {
            "https://api.github.com/repos/$repo/releases/latest"
        } else {
            "https://api.github.com/repos/$repo/releases/tags/$Version"
        }

        Write-Step 'Looking up the release'
        $release = Invoke-RestMethod -Uri $api -Headers @{ 'User-Agent' = 'cellar-installer' }

        $asset = $release.assets | Where-Object { $_.name -like "*$target*.zip" } | Select-Object -First 1
        if (-not $asset) { throw "No $target asset in release $($release.tag_name)." }

        $checksumAsset = $release.assets | Where-Object { $_.name -eq "$($asset.name).sha256" } | Select-Object -First 1
        if (-not $checksumAsset) {
            throw "No published checksum for $($asset.name). Refusing to install an unverified binary."
        }

        Write-Step "Downloading $($release.tag_name)"
        $zip = Join-Path $temp $asset.name
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip -UseBasicParsing

        $checksumText = (Invoke-WebRequest -Uri $checksumAsset.browser_download_url -UseBasicParsing).Content
        $expected = ($checksumText -split '\s+')[0].ToLower()
    }

    if ($expected) {
        Write-Step 'Verifying the checksum'
        $actual = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLower()
        if ($actual -ne $expected) {
            throw "Checksum mismatch. Expected $expected, got $actual. Not installing."
        }
        Write-Done 'Checksum matches'
    }

    # ------------------------------------------------------------------ install

    Write-Step "Installing to $installDir"

    # A running cellar.exe cannot be overwritten, but it can be renamed. Same
    # trick `cellar self-update` uses, for the same reason.
    $exe = Join-Path $installDir 'cellar.exe'
    if (Test-Path $exe) {
        $retired = "$exe.old"
        Remove-Item -Force -ErrorAction SilentlyContinue $retired
        Rename-Item -Path $exe -NewName "cellar.exe.old" -Force
    }

    Expand-Archive -Path $zip -DestinationPath $installDir -Force
    Remove-Item -Force -ErrorAction SilentlyContinue "$exe.old"

    # -------------------------------------------------------------------- PATH

    $current = [Environment]::GetEnvironmentVariable('Path', $pathScope)
    if ($current -notlike "*$installDir*") {
        Write-Step "Adding to the $pathScope PATH"
        [Environment]::SetEnvironmentVariable('Path', "$current;$installDir", $pathScope)
    }
    $env:Path = "$env:Path;$installDir"

    # ------------------------------------------------------------------ config

    $config = Join-Path $dataDir 'cellar.toml'
    if (-not (Test-Path $config)) {
        $example = Join-Path $installDir 'cellar.toml.example'
        if (Test-Path $example) {
            Copy-Item $example $config
            Write-Done "Wrote a starting config to $config"
        }
    } else {
        Write-Note "Left your existing config at $config"
    }

    # ----------------------------------------------------------------- service

    if ($Service) {
        if (-not $System) {
            throw '-Service needs -System: a Windows service runs outside your user session.'
        }

        Write-Step 'Registering the Windows service'

        # `binPath` needs the full command line quoted as one argument, and the
        # spaces in the config path are exactly what breaks a naive version.
        $binPath = "`"$exe`" run --config `"$config`""
        & sc.exe create Cellar binPath= $binPath start= auto DisplayName= "Cellar (s&box server manager)" | Out-Null
        & sc.exe description Cellar "Supervises an s&box dedicated server, and serves its persistence bridge." | Out-Null

        Write-Done 'Service registered. Start it with: sc.exe start Cellar'
        Write-Note 'Set CELLAR_DATABASE_URL as a machine environment variable first, or the bridge will not start.'
    }

    # -------------------------------------------------------------------- done

    Write-Host ''
    Write-Done "Installed $(& $exe --version)"
    Write-Host ''
    Write-Note "  Config:  $config"
    Write-Note "  Binary:  $exe"
    Write-Host ''
    Write-Host '  Next:' -ForegroundColor White
    Write-Host "    1. Edit $config" -ForegroundColor DarkGray
    Write-Host '    2. cellar doctor' -ForegroundColor DarkGray
    Write-Host '    3. cellar run' -ForegroundColor DarkGray
    Write-Host ''
    Write-Note '  Open a new terminal for the PATH change to take effect.'
    Write-Host ''
}
finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $temp
}
