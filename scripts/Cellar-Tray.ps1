param(
    [string]$Cellar = 'C:\Users\Shadow\AppData\Local\Programs\Cellar\cellar.exe',
    [string]$Config = 'C:\ProgramData\Cellar\cellar.toml',
    [string]$Web = 'http://127.0.0.1:8081/'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

function Invoke-Cellar([string]$Action) {
    try {
        Invoke-WebRequest -Uri ("http://127.0.0.1:8081/api/control/{0}" -f $Action) `
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
    if (Get-Process -Name cellar -ErrorAction SilentlyContinue) {
        return
    }
    Start-Process -FilePath $Cellar -ArgumentList @('-c', $Config, 'run') `
        -WorkingDirectory (Split-Path -Parent $Cellar) -WindowStyle Hidden
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
$exitCellar = $menu.Items.Add('Exit Cellar')
$exitCellar.Add_Click({ Invoke-Cellar 'stop'; $notify.Visible = $false; $notify.Dispose(); [System.Windows.Forms.Application]::Exit() })
$exitTray = $menu.Items.Add('Exit tray')
$exitTray.Add_Click({ $notify.Visible = $false; $notify.Dispose(); [System.Windows.Forms.Application]::Exit() })

$notify = New-Object System.Windows.Forms.NotifyIcon
$notify.Icon = [System.Drawing.SystemIcons]::Application
$notify.Text = 'Cellar, AppleJackRP server'
$notify.ContextMenuStrip = $menu
$notify.Visible = $true
$notify.Add_DoubleClick({ Start-Process $Web })

[System.Windows.Forms.Application]::Run()
