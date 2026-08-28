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
    $version = 'v0.1.11'
    Invoke-WebRequest "https://raw.githubusercontent.com/fobiat/cellar/$version/scripts/install.ps1" -OutFile install-cellar.ps1
    Get-Content .\install-cellar.ps1
    .\install-cellar.ps1 -Version $version
    Remove-Item .\install-cellar.ps1

.EXAMPLE
    .\install.ps1 -Version v0.1.0 -Service

.EXAMPLE
    .\install.ps1 -Run
#>
[CmdletBinding()]
param(
    # Release tag. Defaults to the latest.
    [string] $Version = 'latest',

    # Install for all users, into Program Files. Needs an elevated shell.
    [switch] $System,

    # Register a Windows service that runs `cellar run` at boot.
    [switch] $Service,

    # Run doctor and start Cellar after installation. Cannot be combined with
    # -Service because the service owns the process.
    [switch] $Run,

    # Install from a local zip instead of downloading. For testing a build
    # before it is published.
    [string] $FromFile
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($Run -and $Service) {
    throw '-Run cannot be combined with -Service'
}

$repo = 'fobiat/cellar'
$target = 'x86_64-pc-windows'

function Write-Step { param([string] $Message) Write-Host "  $Message" -ForegroundColor Cyan }
function Write-Done { param([string] $Message) Write-Host "  $Message" -ForegroundColor Green }
function Write-Note { param([string] $Message) Write-Host "  $Message" -ForegroundColor DarkGray }

function Stop-InstalledCellar([string] $Executable) {
    $running = @(Get-Process -Name 'cellar' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $Executable })
    if (-not $running) { return }

    Write-Step 'Stopping the running Cellar process before replacement'
    try {
        Invoke-WebRequest -Uri 'http://127.0.0.1:8081/api/control/exit' -Method Post `
            -UseBasicParsing -TimeoutSec 5 | Out-Null
    } catch {
        Write-Note 'The Cellar exit endpoint was unavailable; requesting a graceful server stop.'
        try {
            Invoke-WebRequest -Uri 'http://127.0.0.1:8081/api/control/stop' -Method Post `
                -UseBasicParsing -TimeoutSec 5 | Out-Null
        } catch { }
    }

    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    do {
        Start-Sleep -Milliseconds 250
        $running = @(Get-Process -Name 'cellar' -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -eq $Executable })
    } while ($running -and [DateTime]::UtcNow -lt $deadline)

    if ($running) {
        Write-Note 'The installed Cellar predates the exit endpoint; terminating it after the graceful stop.'
        $running | Stop-Process -Force
        Start-Sleep -Seconds 1
        $running = @(Get-Process -Name 'cellar' -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -eq $Executable })
        if ($running) {
            throw 'Cellar is still running and its executable cannot be replaced. Stop it from the tray or run the installer again.'
        }
    }
}

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

        # A private repository answers 404 to an anonymous caller, so a token
        # is the difference between installing and appearing to have no
        # releases. `gh auth token` prints one.
        $token = if ($env:CELLAR_GITHUB_TOKEN) { $env:CELLAR_GITHUB_TOKEN } else { $env:GITHUB_TOKEN }
        $headers = @{ 'User-Agent' = 'cellar-installer'; 'Accept' = 'application/vnd.github+json' }
        if ($token) { $headers['Authorization'] = "Bearer $token" }

        Write-Step 'Looking up the release'
        try {
            $release = Invoke-RestMethod -Uri $api -Headers $headers
        } catch {
            throw "No release visible at $api. If the repository is private, set CELLAR_GITHUB_TOKEN (gh auth token)."
        }

        $asset = $release.assets | Where-Object { $_.name -like "*$target*.zip" } | Select-Object -First 1
        if (-not $asset) { throw "No $target asset in release $($release.tag_name)." }

        $checksumAsset = $release.assets | Where-Object { $_.name -eq "$($asset.name).sha256" } | Select-Object -First 1
        if (-not $checksumAsset) {
            throw "No published checksum for $($asset.name). Refusing to install an unverified binary."
        }

        Write-Step "Downloading $($release.tag_name)"

        # The API url, not browser_download_url: the latter needs a web session
        # on a private repository, where this works with the same token.
        $assetHeaders = $headers.Clone()
        $assetHeaders['Accept'] = 'application/octet-stream'

        $zip = Join-Path $temp $asset.name
        Invoke-WebRequest -Uri $asset.url -Headers $assetHeaders -OutFile $zip -UseBasicParsing

        # -OutFile rather than .Content. `Accept: application/octet-stream`
        # makes the response binary, so .Content is a Byte[]; `-split` then
        # splits the array and returns the first byte's decimal value ("57")
        # instead of the digest, which fails every install as a mismatch.
        $checksumFile = "$zip.sha256"
        Invoke-WebRequest -Uri $checksumAsset.url -Headers $assetHeaders -OutFile $checksumFile -UseBasicParsing
        $expected = ((Get-Content -Path $checksumFile -Raw) -split '\s+')[0].ToLower()
    }

    if ($expected) {
        Write-Step 'Verifying the checksum'

        # The guard `cellar self-update` already applies. A checksum that failed
        # to parse otherwise arrives as a mismatch, which reads as a corrupt
        # download and sends you looking at the wrong thing.
        if ($expected -notmatch '^[0-9a-f]{64}$') {
            throw "The published checksum did not parse as a sha256 digest (got '$expected'). Not installing."
        }

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
    $retired = $null
    if (Test-Path $exe) {
        Stop-InstalledCellar $exe
        $retired = "$exe.old"
        if (Test-Path -LiteralPath $retired) {
            try {
                Remove-Item -LiteralPath $retired -Force -ErrorAction Stop
            } catch {
                $retired = "$exe.$([Guid]::NewGuid().ToString('N')).old"
                Write-Note "The previous backup is locked; using $([IO.Path]::GetFileName($retired)) for this update."
            }
        }
        Move-Item -LiteralPath $exe -Destination $retired
    }

    Expand-Archive -Path $zip -DestinationPath $installDir -Force
    if ($retired -and (Test-Path -LiteralPath $retired)) {
        Remove-Item -LiteralPath $retired -Force -ErrorAction Stop
    }

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
        & sc.exe description Cellar "Supervises an s&box dedicated server and serves its operator web UI." | Out-Null

        Write-Done 'Service registered. Start it with: sc.exe start Cellar'
        Write-Note 'Set CELLAR_DATABASE_URL as a machine environment variable first, or database features will stay offline.'
        Write-Note 'No MySQL/MariaDB available? `cellar mariadb provision` hosts one locally and prints a CELLAR_DATABASE_URL for you.'
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

    if ($Run) {
        Write-Step 'Running cellar doctor'
        & $exe --config $config doctor
        if ($LASTEXITCODE -ne 0) {
            throw "cellar doctor failed with exit code $LASTEXITCODE"
        }
        & $exe --config $config run
        exit $LASTEXITCODE
    }
}
finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $temp
}
