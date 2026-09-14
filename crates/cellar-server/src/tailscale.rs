use std::process::Stdio;

/// Return the current host's Tailscale IPv4 address, if the daemon is online.
pub async fn ip() -> Option<String> {
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
                    .find(|line| !line.is_empty())
                    .map(str::to_owned)
            })
        {
            return Some(ip);
        }
    }
    None
}
