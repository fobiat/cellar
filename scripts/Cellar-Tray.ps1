param(
    [string]$Cellar,
    [string]$Config,
    [string]$Web = 'http://127.0.0.1:8081/'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

if ([string]::IsNullOrWhiteSpace($Cellar)) {
    $Cellar = Join-Path $env:LOCALAPPDATA 'Programs\Cellar\cellar.exe'
}
if ([string]::IsNullOrWhiteSpace($Config)) {
    $Config = Join-Path $env:ProgramData 'Cellar\cellar.toml'
}

function Get-CellarHeaders {
    $headers = @{}
    if ($env:CELLAR_SESSION) {
        $headers['Cookie'] = "cellar_session=$($env:CELLAR_SESSION)"
    }
    return $headers
}

function Get-ServerState {
    try {
        Invoke-WebRequest -Uri ($Web.TrimEnd('/') + '/readyz') -Headers (Get-CellarHeaders) `
            -UseBasicParsing -TimeoutSec 2 | Out-Null
        return 'ready'
    } catch {
        try {
            Invoke-WebRequest -Uri ($Web.TrimEnd('/') + '/healthz') -Headers (Get-CellarHeaders) `
                -UseBasicParsing -TimeoutSec 2 | Out-Null
            return 'starting'
        } catch {
            return 'offline'
        }
    }
}

function Show-Notice([string]$Text) {
    $notify.BalloonTipTitle = 'Cellar'
    $notify.BalloonTipText = $Text
    $notify.ShowBalloonTip(2500)
}

function Invoke-Cellar([string]$Action) {
    try {
        Invoke-WebRequest -Uri ("{0}/api/control/{1}" -f $Web.TrimEnd('/'), $Action) `
            -Headers (Get-CellarHeaders) -Method Post -UseBasicParsing -TimeoutSec 5 | Out-Null
        Show-Notice "Cellar $Action requested."
    } catch {
        [System.Windows.Forms.MessageBox]::Show(
            "Cellar did not accept the request: $($_.Exception.Message)",
            'Cellar', 'OK', 'Warning') | Out-Null
    }
}

function Start-Cellar {
    if (Get-ServerState -ne 'offline') {
        Show-Notice 'Cellar is already running.'
        return
    }
    if (-not (Test-Path -LiteralPath $Cellar)) {
        [System.Windows.Forms.MessageBox]::Show(
            "Cellar executable not found at $Cellar.", 'Cellar', 'OK', 'Error') | Out-Null
        return
    }
    Start-Process -FilePath $Cellar -ArgumentList @('--config', $Config, 'run') `
        -WorkingDirectory (Split-Path -Parent $Cellar) -WindowStyle Hidden
    Show-Notice 'Cellar is starting.'
}

function Open-Tui {
    if (-not (Test-Path -LiteralPath $Cellar)) {
        Show-Notice "Cellar executable not found at $Cellar."
        return
    }
    $arguments = "--config `"$Config`" tui --url `"$Web`""
    $terminal = Get-Command wt.exe -ErrorAction SilentlyContinue
    if ($terminal) {
        Start-Process -FilePath $terminal.Source -ArgumentList @('new-tab', 'powershell.exe', '-NoProfile', '-NoExit', '-Command', "& '$Cellar' $arguments")
    } else {
        Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoProfile', '-NoExit', '-Command', "& '$Cellar' $arguments")
    }
}

function Check-For-Updates {
    if (-not (Test-Path -LiteralPath $Cellar)) {
        Show-Notice "Cellar executable not found at $Cellar."
        return
    }
    $output = & $Cellar --config $Config self-update --check 2>&1 | Out-String
    Show-Notice ($output.Trim())
}

$menu = New-Object System.Windows.Forms.ContextMenuStrip
$open = $menu.Items.Add('Open web UI')
$open.Add_Click({ Start-Process $Web })
$tui = $menu.Items.Add('Open TUI')
$tui.Add_Click({ Open-Tui })
$menu.Items.Add('-') | Out-Null
$start = $menu.Items.Add('Start Cellar')
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
$notify.Icon = [System.Drawing.SystemIcons]::Application
$notify.Text = 'Cellar, s&box server'
$notify.ContextMenuStrip = $menu
$notify.Visible = $true
$notify.Add_DoubleClick({ Start-Process $Web })

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 10000
$timer.Add_Tick({
    $state = Get-ServerState
    $notify.Text = "Cellar: $state"
})
$timer.Start()

[System.Windows.Forms.Application]::Run()
