[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $AppleJackRoot,
    [Parameter(Mandatory)] [string] $OutputDirectory,
    [Parameter(Mandatory)] [string] $Version
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$AppleJackRoot = (Resolve-Path -LiteralPath $AppleJackRoot).Path
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$commit = (git -C $AppleJackRoot rev-parse HEAD).Trim()
$shortCommit = (git -C $AppleJackRoot rev-parse --short HEAD).Trim()
$stamp = Join-Path $AppleJackRoot 'Code\Core\BuildVersion.g.cs'
$buildNumber = if (Test-Path -LiteralPath $stamp) {
    (Select-String -LiteralPath $stamp -Pattern 'BuildNumber = "([^"]+)"' | Select-Object -First 1).Matches.Groups[1].Value
} else {
    'unknown'
}

$stage = Join-Path ([IO.Path]::GetTempPath()) "applejackrp-release-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force -Path $stage | Out-Null
try {
    foreach ( $item in @('applejackrp.sbproj', 'Code', 'Assets', 'ProjectSettings', 'Localization', 'README.md', 'CHANGELOG.md', 'GAMEMODE_CHANGELOG.md', 'CREDITS.md', 'LICENSE', 'LICENSE-MIT') ) {
        $source = Join-Path $AppleJackRoot $item
        if ( Test-Path -LiteralPath $source ) {
            Copy-Item -LiteralPath $source -Destination $stage -Recurse -Force
        }
    }

    $manifest = [ordered]@{
        product = 'AppleJackRP'
        version = $Version
        build_number = $buildNumber
        commit = $commit
        short_commit = $shortCommit
        package_mode = 'local source bundle; s&box editor publish remains separate'
    }
    $manifest | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $stage 'applejackrp-release.json') -Encoding UTF8

    $archive = Join-Path $OutputDirectory "applejackrp-$Version.zip"
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $archive -Force
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $(Split-Path -Leaf $archive)" | Set-Content -LiteralPath "$archive.sha256" -Encoding ASCII
    Write-Output "Created $archive for AppleJackRP $buildNumber ($shortCommit)."
} finally {
    Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
}
