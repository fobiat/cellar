//! Cellar's Model Context Protocol adapter.
//!
//! The crate has two small seams. [`CellarBackend`] is the server-side seam,
//! and [`CellarApi`] is the HTTP adapter used by the CLI. Keeping them apart
//! lets the MCP protocol remain independent of Cellar's process state while
//! still making the standard API authentication the source of truth.

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, SET_COOKIE};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use rmcp::{ErrorData as McpError, ServiceExt, schemars, tool, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8081";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// The data operations exposed to MCP clients.
///
/// Implementations are responsible for applying their own authentication. The
/// protocol layer never receives or stores a bearer token or password.
#[async_trait]
pub trait CellarBackend: Send + Sync + 'static {
    async fn status(&self, instance: Option<String>) -> Result<Value, String>;
    async fn logs(&self, query: LogQuery) -> Result<Value, String>;
    async fn resources(&self, instance: Option<String>) -> Result<Value, String>;
    async fn addresses(&self) -> Result<Value, String>;
    async fn versions(&self) -> Result<Value, String>;
    async fn configs(&self) -> Result<Value, String>;
    async fn instances(&self) -> Result<Value, String>;
    async fn command(&self, command: String, instance: Option<String>) -> Result<Value, String>;
}

/// The one thing every server-scoped tool takes.
///
/// Named the same as the query parameter it becomes, and described in a way
/// that tells a model to call `cellar_instances` rather than guess: a model
/// that invents an id gets a 404 listing the real ones, which is recoverable,
/// but a model that omits it silently addresses the primary, which is not.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct InstanceSelector {
    /// Which supervised server this is about. Omit for the primary. Call
    /// `cellar_instances` first when a config declares more than one.
    #[serde(default)]
    pub instance: Option<String>,
}

/// Query parameters accepted by the searchable log tool.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct LogQuery {
    /// Case-insensitive text to find in the rendered log line.
    #[serde(default)]
    pub query: Option<String>,
    /// A parsed log tag/category, such as `network` or `players`.
    #[serde(default)]
    pub tag: Option<String>,
    /// A parsed log level, such as `error` or `warning`.
    #[serde(default)]
    pub level: Option<String>,
    /// Maximum number of matching lines, capped by Cellar's API.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Which supervised server's logs. Omit for the primary.
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CommandRequest {
    /// One console command. Newlines are rejected by Cellar's existing API.
    pub command: String,
    /// Which supervised server to type it into. Omit for the primary.
    ///
    /// The highest-consequence argument in this file. `quit` sent to the wrong
    /// instance stops a server nobody asked to stop, and a wrong id is refused
    /// where an omitted one is silently the primary.
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Clone)]
pub struct CellarMcpServer {
    backend: Arc<dyn CellarBackend>,
}

impl CellarMcpServer {
    pub fn new(backend: Arc<dyn CellarBackend>) -> Self {
        Self { backend }
    }

    /// Serve this adapter over MCP stdio until the client closes the session.
    pub async fn serve_stdio(self) -> Result<(), String> {
        let service = self
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|error| error.to_string())?;
        service
            .waiting()
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[tool_router(server_handler)]
impl CellarMcpServer {
    #[tool(
        description = "Read the current Cellar status, including server state, database health, access, anti-cheat detection, and addresses",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn cellar_status(
        &self,
        Parameters(target): Parameters<InstanceSelector>,
    ) -> Result<CallToolResult, McpError> {
        self.read_tool(self.backend.status(target.instance).await)
            .await
    }

    #[tool(
        description = "List the supervised servers this Cellar declares, with their ids, scopes, gamemodes and whether each is running. Call this before any other tool when more than one server may exist",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn cellar_instances(&self) -> Result<CallToolResult, McpError> {
        self.read_tool(self.backend.instances().await).await
    }

    #[tool(
        description = "Search persistent Cellar logs by text, tag, level, and result limit",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn cellar_logs(
        &self,
        Parameters(query): Parameters<LogQuery>,
    ) -> Result<CallToolResult, McpError> {
        self.read_tool(self.backend.logs(query).await).await
    }

    #[tool(
        description = "Read current process, host, and network resource telemetry",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn cellar_resources(
        &self,
        Parameters(target): Parameters<InstanceSelector>,
    ) -> Result<CallToolResult, McpError> {
        self.read_tool(self.backend.resources(target.instance).await)
            .await
    }

    #[tool(
        description = "Read Cellar web, game, query, bridge, and Tailscale addresses",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn cellar_addresses(&self) -> Result<CallToolResult, McpError> {
        self.read_tool(self.backend.addresses().await).await
    }

    #[tool(
        description = "Read installed, running, and remote gamemode and engine versions, including build drift",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn cellar_versions(&self) -> Result<CallToolResult, McpError> {
        self.read_tool(self.backend.versions().await).await
    }

    #[tool(
        description = "List available Cellar configuration profiles without exposing local file paths",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn cellar_configs(&self) -> Result<CallToolResult, McpError> {
        self.read_tool(self.backend.configs().await).await
    }

    #[tool(
        description = "Run one authenticated Cellar console command. Disabled unless CELLAR_MCP_ENABLE_COMMAND=1 and the existing web operator auth succeeds",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn cellar_command(
        &self,
        Parameters(request): Parameters<CommandRequest>,
    ) -> Result<CallToolResult, McpError> {
        self.read_tool(
            self.backend
                .command(request.command, request.instance)
                .await,
        )
        .await
    }

    async fn read_tool(&self, result: Result<Value, String>) -> Result<CallToolResult, McpError> {
        match result {
            Ok(value) => Ok(CallToolResult::structured(value)),
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(error)])),
        }
    }
}

impl std::fmt::Debug for CellarMcpServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CellarMcpServer")
            .finish_non_exhaustive()
    }
}

/// HTTP adapter for a running Cellar instance.
#[derive(Clone)]
pub struct CellarApi {
    client: reqwest::Client,
    base_url: String,
    external_token: Option<String>,
    web_password: Option<String>,
    command_enabled: bool,
    session_cookie: Arc<tokio::sync::Mutex<Option<String>>>,
}

impl std::fmt::Debug for CellarApi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CellarApi")
            .field("base_url", &self.base_url)
            .field("external_token_configured", &self.external_token.is_some())
            .field("web_password_configured", &self.web_password.is_some())
            .field("command_enabled", &self.command_enabled)
            .finish()
    }
}

impl CellarApi {
    pub fn from_env(base_url: Option<&str>) -> Result<Self, String> {
        let base_url = base_url
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_owned();
        let parsed = reqwest::Url::parse(&base_url)
            .map_err(|error| format!("invalid Cellar URL: {error}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("Cellar URL must use http or https".to_owned());
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|error| format!("building HTTP client: {error}"))?;

        Ok(Self {
            client,
            base_url,
            external_token: env_value("CELLAR_API_TOKEN"),
            web_password: env_value("CELLAR_WEB_PASSWORD"),
            command_enabled: env_flag("CELLAR_MCP_ENABLE_COMMAND"),
            session_cookie: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    /// Append `?instance=` when the caller named one.
    ///
    /// Built rather than concatenated so an id with a space or an ampersand in
    /// it becomes a 404 naming the real ids, rather than a request that quietly
    /// means something else.
    fn scoped(&self, path: &str, instance: Option<String>) -> Result<reqwest::Url, String> {
        let mut url = reqwest::Url::parse(&format!("{}{path}", self.base_url))
            .map_err(|error| error.to_string())?;
        if let Some(id) = instance.filter(|value| !value.trim().is_empty()) {
            url.query_pairs_mut().append_pair("instance", id.trim());
        }
        Ok(url)
    }

    async fn get_external(&self, path: &str) -> Result<Value, String> {
        self.get_external_scoped(path, None).await
    }

    async fn get_external_scoped(
        &self,
        path: &str,
        instance: Option<String>,
    ) -> Result<Value, String> {
        let token = self
            .external_token
            .as_deref()
            .ok_or_else(|| "CELLAR_API_TOKEN is required for MCP read tools".to_owned())?;
        let response = self
            .client
            .get(self.scoped(path, instance)?)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header("User-Agent", "cellar-mcp")
            .send()
            .await
            .map_err(|error| format!("Cellar request failed: {error}"))?;
        decode_json(response).await
    }

    async fn ensure_operator(&self) -> Result<Option<String>, String> {
        if let Some(cookie) = self.session_cookie.lock().await.clone() {
            return Ok(Some(cookie));
        }
        let Some(password) = &self.web_password else {
            return Ok(None);
        };

        let response = self
            .client
            .post(format!("{}/api/login", self.base_url))
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({"password": password}))
            .send()
            .await
            .map_err(|error| format!("Cellar login request failed: {error}"))?;
        let cookie = response
            .headers()
            .get(SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::to_owned);
        if !response.status().is_success() || cookie.is_none() {
            return Err("Cellar operator authentication was not accepted".to_owned());
        }
        *self.session_cookie.lock().await = cookie.clone();
        Ok(cookie)
    }

    async fn command_request(
        &self,
        command: String,
        instance: Option<String>,
    ) -> Result<Value, String> {
        if !self.command_enabled {
            return Err(
                "cellar_command is disabled; set CELLAR_MCP_ENABLE_COMMAND=1 explicitly".to_owned(),
            );
        }
        if command.trim().is_empty() || command.contains(['\r', '\n']) {
            return Err("command must be one non-empty line".to_owned());
        }
        let mut request = self
            .client
            .post(self.scoped("/api/exec", instance)?)
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({"command": command}));
        if let Some(cookie) = self.ensure_operator().await? {
            request = request.header(COOKIE, cookie);
        }
        decode_json(
            request
                .send()
                .await
                .map_err(|error| format!("Cellar command request failed: {error}"))?,
        )
        .await
    }
}

#[async_trait]
impl CellarBackend for CellarApi {
    async fn status(&self, instance: Option<String>) -> Result<Value, String> {
        self.get_external_scoped("/api/v1/status", instance).await
    }

    async fn logs(&self, query: LogQuery) -> Result<Value, String> {
        let mut url = self.scoped("/api/v1/logs", query.instance.clone())?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(value) = query.query.filter(|value| !value.trim().is_empty()) {
                pairs.append_pair("q", &value);
            }
            if let Some(value) = query.tag.filter(|value| !value.trim().is_empty()) {
                pairs.append_pair("tag", &value);
            }
            if let Some(value) = query.level.filter(|value| !value.trim().is_empty()) {
                pairs.append_pair("level", &value);
            }
            if let Some(value) = query.limit {
                pairs.append_pair("limit", &value.clamp(1, 5000).to_string());
            }
        }
        let token = self
            .external_token
            .as_deref()
            .ok_or_else(|| "CELLAR_API_TOKEN is required for MCP read tools".to_owned())?;
        let response = self
            .client
            .get(url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header("User-Agent", "cellar-mcp")
            .send()
            .await
            .map_err(|error| format!("Cellar request failed: {error}"))?;
        decode_json(response).await
    }

    async fn resources(&self, instance: Option<String>) -> Result<Value, String> {
        self.get_external_scoped("/api/v1/resources", instance)
            .await
    }

    async fn addresses(&self) -> Result<Value, String> {
        self.get_external("/api/v1/addresses").await
    }

    async fn versions(&self) -> Result<Value, String> {
        self.get_external("/api/v1/versions").await
    }

    async fn configs(&self) -> Result<Value, String> {
        self.get_external("/api/v1/configs").await
    }

    async fn instances(&self) -> Result<Value, String> {
        self.get_external("/api/v1/instances").await
    }

    async fn command(&self, command: String, instance: Option<String>) -> Result<Value, String> {
        self.command_request(command, instance).await
    }
}

async fn decode_json(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("reading Cellar response: {error}"))?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err("Cellar response exceeded the 2 MiB MCP limit".to_owned());
    }
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&body);
        return Err(format!("Cellar returned HTTP {status}: {detail}"));
    }
    serde_json::from_slice(&body).map_err(|error| format!("invalid JSON from Cellar: {error}"))
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn env_flag(name: &str) -> bool {
    matches!(
        env_value(name).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Spawn an external MCP server and return its advertised tools.
pub async fn list_child_tools(
    command: &str,
    args: &[String],
) -> Result<Vec<rmcp::model::Tool>, String> {
    let mut process = tokio::process::Command::new(command);
    process.args(args);
    let transport = rmcp::transport::TokioChildProcess::new(process)
        .map_err(|error| format!("starting MCP server: {error}"))?;
    let service = ().serve(transport).await.map_err(|error| error.to_string())?;
    let tools = service
        .peer()
        .list_all_tools()
        .await
        .map_err(|error| error.to_string())?;
    service.cancel().await.map_err(|error| error.to_string())?;
    Ok(tools)
}

/// Invoke one tool on an external MCP server over its stdio transport.
pub async fn call_child_tool(
    command: &str,
    args: &[String],
    tool: &str,
    arguments: Option<JsonObject>,
) -> Result<CallToolResult, String> {
    let mut process = tokio::process::Command::new(command);
    process.args(args);
    let transport = rmcp::transport::TokioChildProcess::new(process)
        .map_err(|error| format!("starting MCP server: {error}"))?;
    let service = ().serve(transport).await.map_err(|error| error.to_string())?;
    let params = rmcp::model::CallToolRequestParams::new(Cow::Owned(tool.to_owned()));
    let params = match arguments {
        Some(arguments) => params.with_arguments(arguments),
        None => params,
    };
    let result = service
        .peer()
        .call_tool(params)
        .await
        .map_err(|error| error.to_string())?;
    service.cancel().await.map_err(|error| error.to_string())?;
    Ok(result)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Records which instance each call was told to address, because the whole
    /// risk in this file is a tool that quietly means the primary.
    #[derive(Debug, Default)]
    struct FakeBackend {
        addressed: std::sync::Mutex<Vec<(&'static str, Option<String>)>>,
    }

    impl FakeBackend {
        fn note(&self, tool: &'static str, instance: Option<String>) {
            if let Ok(mut log) = self.addressed.lock() {
                log.push((tool, instance));
            }
        }
    }

    #[async_trait]
    impl CellarBackend for FakeBackend {
        async fn status(&self, instance: Option<String>) -> Result<Value, String> {
            self.note("status", instance);
            Ok(json!({"state": "running"}))
        }
        async fn logs(&self, query: LogQuery) -> Result<Value, String> {
            self.note("logs", query.instance);
            Ok(json!([]))
        }
        async fn resources(&self, instance: Option<String>) -> Result<Value, String> {
            self.note("resources", instance);
            Ok(json!({"cpu": 10}))
        }
        async fn addresses(&self) -> Result<Value, String> {
            Ok(json!([]))
        }
        async fn versions(&self) -> Result<Value, String> {
            Ok(json!({"build_drift": {"state": "synced"}}))
        }
        async fn configs(&self) -> Result<Value, String> {
            Ok(json!({"profiles": []}))
        }
        async fn instances(&self) -> Result<Value, String> {
            Ok(json!({"primary": "dev", "instances": []}))
        }
        async fn command(&self, _: String, instance: Option<String>) -> Result<Value, String> {
            self.note("command", instance);
            Ok(json!({"ok": true}))
        }
    }

    /// A wrong id is a 404 from Cellar; an id with a `&` in it would otherwise
    /// be a request that silently means something else.
    #[test]
    fn an_instance_id_is_a_query_pair_rather_than_concatenated_text() {
        let api = CellarApi {
            client: reqwest::Client::new(),
            base_url: "http://127.0.0.1:8081".to_owned(),
            external_token: None,
            web_password: None,
            command_enabled: false,
            session_cookie: Arc::new(tokio::sync::Mutex::new(None)),
        };

        let url = api
            .scoped("/api/v1/status", Some("dev&admin=1".to_owned()))
            .unwrap();
        assert_eq!(
            url.query(),
            Some("instance=dev%26admin%3D1"),
            "an id must not be able to add a parameter"
        );

        assert_eq!(api.scoped("/api/v1/status", None).unwrap().query(), None);
        assert_eq!(
            api.scoped("/api/v1/status", Some("  ".to_owned()))
                .unwrap()
                .query(),
            None,
            "blank is not an instance"
        );
    }

    #[test]
    fn a_cellar_url_must_be_http_or_https() {
        let error = CellarApi::from_env(Some("file:///tmp/cellar")).unwrap_err();
        assert!(error.contains("http or https"));
    }

    #[test]
    fn the_server_does_not_hold_credentials_in_debug_output() {
        let api = CellarApi {
            client: reqwest::Client::new(),
            base_url: "http://127.0.0.1:8081".to_owned(),
            external_token: Some("token-that-must-not-print".to_owned()),
            web_password: Some("password-that-must-not-print".to_owned()),
            command_enabled: true,
            session_cookie: Arc::new(tokio::sync::Mutex::new(None)),
        };
        let printed = format!("{api:?}");
        assert!(!printed.contains("token-that-must-not-print"));
        assert!(!printed.contains("password-that-must-not-print"));
    }

    #[tokio::test]
    async fn the_server_wraps_backend_json_as_structured_content() {
        let server = CellarMcpServer::new(Arc::new(FakeBackend::default()));
        let result = server
            .cellar_status(Parameters(InstanceSelector::default()))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.structured_content, Some(json!({"state": "running"})));
    }

    /// Every server-scoped tool has to pass the instance through. A tool that
    /// accepts the argument and drops it is worse than one that never had it:
    /// the caller is told which server it addressed and it addressed another.
    #[tokio::test]
    async fn every_server_scoped_tool_passes_the_instance_through() {
        let backend = Arc::new(FakeBackend::default());
        let server = CellarMcpServer::new(backend.clone());
        let wanted = Some("published".to_owned());

        server
            .cellar_status(Parameters(InstanceSelector {
                instance: wanted.clone(),
            }))
            .await
            .unwrap();
        server
            .cellar_resources(Parameters(InstanceSelector {
                instance: wanted.clone(),
            }))
            .await
            .unwrap();
        server
            .cellar_logs(Parameters(LogQuery {
                instance: wanted.clone(),
                ..LogQuery::default()
            }))
            .await
            .unwrap();
        server
            .cellar_command(Parameters(CommandRequest {
                command: "status".to_owned(),
                instance: wanted.clone(),
            }))
            .await
            .unwrap();

        let addressed = backend.addressed.lock().unwrap().clone();
        assert_eq!(
            addressed,
            vec![
                ("status", wanted.clone()),
                ("resources", wanted.clone()),
                ("logs", wanted.clone()),
                ("command", wanted),
            ]
        );
    }
}
