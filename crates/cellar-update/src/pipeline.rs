use std::path::Path;
use std::process::Stdio;

use cellar_core::config::ReleaseConfig;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct PipelineResult {
    pub action: String,
    pub ok: bool,
    pub output: String,
}

/// Run one configured project pipeline step without invoking a shell.
pub async fn run(config: &ReleaseConfig, action: &str, project_dir: &Path) -> PipelineResult {
    let command = match action {
        "build" => &config.build_command,
        "publish" => &config.publish_command,
        _ => {
            return PipelineResult {
                action: action.to_owned(),
                ok: false,
                output: "unknown pipeline action".to_owned(),
            };
        }
    };
    let Some(program) = command.first() else {
        return PipelineResult {
            action: action.to_owned(),
            ok: false,
            output: format!("release.{action}_command is not configured"),
        };
    };

    let working_dir = config.working_dir.as_deref().unwrap_or(project_dir);
    let result = tokio::process::Command::new(program)
        .args(&command[1..])
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .output()
        .await;
    match result {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                text.push('\n');
                text.push_str(&stderr);
            }
            PipelineResult {
                action: action.to_owned(),
                ok: output.status.success(),
                output: text.trim().to_owned(),
            }
        }
        Err(error) => PipelineResult {
            action: action.to_owned(),
            ok: false,
            output: error.to_string(),
        },
    }
}
