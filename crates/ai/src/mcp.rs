//! The MCP server: MCP clients (Claude Code, Claude Desktop, …) use the tools of the
//! [`Registry`] over Streamable HTTP while the app runs. A generic adapter: tools are
//! listed and called as the registry has them, so every tool is written once.
//!
//! What it takes to call it:
//! - It listens on `127.0.0.1` only, and accepts only loopback `Host` headers.
//! - Every request needs `Authorization: Bearer <token>`.
//! - Requests with an `Origin` header are refused: browsers send one, CLI clients
//!   don't, so a web page can't reach it (DNS rebinding).
//!
//! What a call may do is the [`Ctx`]'s business, the one security layer below the
//! registry: exposed databases only, reads through the engine's gate.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::thread::JoinHandle;

use axum::Router;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::ctx::Ctx;
use crate::results::Results;
use crate::tool::Registry;

/// What the server tells clients when they connect.
const INSTRUCTIONS: &str = "tangul's databases, read-only. Start with list_connections, then \
    schema_slice for a question, describe_table and find_join_path for details, and \
    run_readonly to query (keep results small); get_cell gives a cell it cut, whole. \
    Nothing that changes data is run: propose_sql opens such statements in tangul for the \
    user to review and run (only changes the user asked for themselves). It also shows the \
    user a query, a bookmark (list_bookmarks) or an earlier result, as the grid or a chart. \
    tangul_features says what else the app can do for the user.";

/// Builds the [`Ctx`] for a call: the databases exposed right now, the knowledge
/// pool, the engine and the audit sink.
pub type CtxSource = Arc<dyn Fn() -> Ctx + Send + Sync>;

/// The registry's tools as MCP tools.
#[derive(Clone)]
struct Tangul {
    registry: Arc<Registry>,
    ctx: CtxSource,
    /// Results whose cells were cut, across calls (`get_cell`).
    results: Results,
}

impl ServerHandler for Tangul {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info = Implementation::new("tangul", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(INSTRUCTIONS.into());
        info
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = self
            .registry
            .specs()
            .into_iter()
            .map(|spec| {
                let schema = match spec.input_schema {
                    Value::Object(map) => map,
                    _ => Default::default(),
                };
                Tool::new(spec.name, spec.description, Arc::new(schema))
            })
            .collect();
        // Required by protocol 2026-07-28 (Claude Code rejects the list without
        // them): never cached, and for this client only (it is bound to the token).
        Ok(ListToolsResult::with_all_items(tools)
            .with_ttl_ms(0)
            .with_cache_scope(CacheScope::Private))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let ctx = (self.ctx)().with_results(self.results.clone());
        let input = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        // A tool's error goes to the model as the result (it says what to do instead),
        // not as a protocol error.
        let result = match self.registry.call(&ctx, &request.name, input).await {
            Ok(out) => CallToolResult::success(vec![ContentBlock::text(out.text)]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
        };
        Ok(result.into())
    }
}

/// A running server; dropped or [`McpServer::stop`]ped, it stops.
pub struct McpServer {
    addr: SocketAddr,
    cancel: CancellationToken,
    thread: Option<JoinHandle<()>>,
}

impl McpServer {
    /// Starts it on `127.0.0.1:port` (`0`: any free port) with its own runtime.
    /// Fails if the port is taken.
    pub fn start(port: u16, token: String, ctx: CtxSource) -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("tangul-mcp".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                runtime.block_on(serve(listener, token, ctx, stop));
            })?;
        Ok(Self {
            addr,
            cancel,
            thread: Some(thread),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL clients connect to.
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    pub fn stop(mut self) {
        self.shut_down();
    }

    fn shut_down(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.shut_down();
    }
}

async fn serve(
    listener: std::net::TcpListener,
    token: String,
    ctx: CtxSource,
    cancel: CancellationToken,
) {
    let handler = Tangul {
        registry: Arc::new(Registry::standard()),
        ctx,
        results: Results::default(),
    };
    let mut config = StreamableHttpServerConfig::default();
    config.cancellation_token = cancel.child_token();
    config.json_response = true;
    // Refuse every request that has an Origin: only browsers send one.
    let config = config.enforce_origin_validation();
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let token: Arc<str> = token.into();
    let app = Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(move |req: Request, next: Next| {
            let token = token.clone();
            async move { authorize(&token, req, next).await }
        }));
    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
        return;
    };
    let _ = axum::serve(listener, app)
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await;
}

/// Lets a request through only with `Authorization: Bearer <token>`.
async fn authorize(token: &str, req: Request, next: Next) -> Result<Response, StatusCode> {
    let given = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match given {
        Some(given) if same(given.as_bytes(), token.as_bytes()) => Ok(next.run(req).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Compares without stopping at the first difference (no timing hint).
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A new random token: 32 bytes, hex.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("system randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
