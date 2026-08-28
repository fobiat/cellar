param(
    [string]$Cellar,
    [string]$Config,
    [string]$Web = 'http://127.0.0.1:8081/'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

if ( [string]::IsNullOrWhiteSpace( $Cellar ) ) {
    $Cellar = Join-Path $env:LOCALAPPDATA 'Programs\Cellar\cellar.exe'
}
if ( [string]::IsNullOrWhiteSpace( $Config ) ) {
    $Config = Join-Path $env:ProgramData 'Cellar\cellar.toml'
}

$launcher = Join-Path $PSScriptRoot 'start-applejack.ps1'
$iconPath = Join-Path $PSScriptRoot '..\assets\cellar-applejack.ico'

function Get-ServerState {
    try {
        Invoke-WebRequest -Uri ($Web.TrimEnd('/') + '/readyz') -UseBasicParsing -TimeoutSec 2 | Out-Null
        return 'ready'
    } catch {
        try {
            Invoke-WebRequest -Uri ($Web.TrimEnd('/') + '/healthz') -UseBasicParsing -TimeoutSec 2 | Out-Null
            return 'starting'
        } catch {
            return 'offline'
        }
    }
}

function Invoke-Cellar([string]$Action) {
    try {
        Invoke-WebRequest -Uri (("{0}/api/control/{1}" -f $Web.TrimEnd('/'), $Action)) `
            -Method Post -UseBasicParsing -TimeoutSec 5 | Out-Null
        $notify.BalloonTipTitle = 'Cellar'
        $notify.BalloonTipText = "Server $Action requested."
        $notify.ShowBalloonTip(2500)
    } catch {
        [System.Windows.Forms.MessageBox]::Show(
            "Cellar did not accept the request: $($_.Exception.Message)",
            'Cellar', 'OK', 'Warning') | Out-Null
    }
}

function Start-Cellar {
    if (Get-ServerState -ne 'offline') {
        return
    }
    if ( -not (Test-Path -LiteralPath $Cellar) ) {
        [System.Windows.Forms.MessageBox]::Show(
            "Cellar executable not found at $Cellar.", 'AppleJackRP', 'OK', 'Error') | Out-Null
        return
    }
    Start-Process -FilePath $Cellar -ArgumentList @('-c', $Config, 'run') `
        -WorkingDirectory (Split-Path -Parent $Cellar) -WindowStyle Hidden
}

function Check-For-Updates {
    if ( -not (Test-Path -LiteralPath $launcher) ) { return }
    Start-Process powershell.exe -WindowStyle Hidden -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $launcher, '-CheckOnly',
        '-Cellar', $Cellar, '-Config', $Config, '-Web', $Web)
}

$menu = New-Object System.Windows.Forms.ContextMenuStrip
$open = $menu.Items.Add('Open web UI')
$open.Add_Click({ Start-Process $Web })
$menu.Items.Add('-') | Out-Null
$start = $menu.Items.Add('Start server')
$start.Add_Click({ Start-Cellar })
$restart = $menu.Items.Add('Restart server')
$restart.Add_Click({ Invoke-Cellar 'restart' })
$stop = $menu.Items.Add('Stop server')
$stop.Add_Click({ Invoke-Cellar 'stop' })
$menu.Items.Add('-') | Out-Null
$updates = $menu.Items.Add('Check for updates')
$updates.Add_Click({ Check-For-Updates })
$menu.Items.Add('-') | Out-Null
$exitCellar = $menu.Items.Add('Exit Cellar')
$exitCellar.Add_Click({ Invoke-Cellar 'exit'; $notify.Visible = $false; $notify.Dispose(); [System.Windows.Forms.Application]::Exit() })
$exitTray = $menu.Items.Add('Exit tray')
$exitTray.Add_Click({ $notify.Visible = $false; $notify.Dispose(); [System.Windows.Forms.Application]::Exit() })

$notify = New-Object System.Windows.Forms.NotifyIcon
$notify.Icon = if (Test-Path -LiteralPath $iconPath) {
    New-Object System.Drawing.Icon($iconPath)
} else {
    [System.Drawing.SystemIcons]::Application
}
$notify.Text = 'Cellar, s&box server'
$notify.ContextMenuStrip = $menu
$notify.Visible = $true
$notify.Add_DoubleClick({ Start-Process $Web })

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 10000
$timer.Add_Tick({
    $state = Get-ServerState
    $notify.Text = "AppleJackRP: $state"
})
$timer.Start()

[System.Windows.Forms.Application]::Run()
