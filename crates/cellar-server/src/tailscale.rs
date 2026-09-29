use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

type CachedIp = tokio::sync::Mutex<Option<(Instant, Option<String>)>>;

/// Return the current host's Tailscale IPv4 address, if the daemon is online.
pub async fn ip() -> Option<String> {
    static CACHE: OnceLock<CachedIp> = OnceLock::new();
    let mut cached = CACHE.get_or_init(|| tokio::sync::Mutex::new(None)).lock().await;
    if let Some((at, value)) = &*cached
        && at.elapsed() < Duration::from_secs(30)
    {
        return value.clone();
    }
    let value = discover().await;
    *cached = Some((Instant::now(), value.clone()));
    value
}

async fn discover() -> Option<String> {
    let mut commands = vec!["tailscale".to_owned()];
    if cfg!(windows) {
        commands.extend([
            r"C:\Program Files\Tailscale\tailscale.exe".to_owned(),
            r"C:\Program Files (x86)\Tailscale\tailscale.exe".to_owned(),
        ]);
    }

    for command in commands {
        let output = tokio::time::timeout(
            std::time::Duration::from_millis(700),
            tokio::process::Command::new(command)
                .args(["ip", "-4"])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()
        .and_then(Result::ok);
        let Some(output) = output else { continue };
        if output.status.success()
            && let Some(ip) = String::from_utf8(output.stdout).ok().and_then(|value| {
                value
                    .lines()
                    .map(str::trim)
                    .find(|line| line.parse::<std::net::Ipv4Addr>().is_ok())
                    .map(str::to_owned)
            })
        {
            return Some(ip);
        }
    }
    None
}
