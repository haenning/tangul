//! The MCP server over real HTTP: a client's handshake, listing and calling tools
//! against a SQLite file with the real engine, and what keeps others out (token,
//! browser origins).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tangul_ai::mcp::{CtxSource, McpServer};
use tangul_ai::{Ctx, Exposed, ToolCall};
use tangul_assist::dialect::dialect_for;
use tangul_assist::schema::SchemaIndex;
use tangul_core::{ConnId, ConnectConfig, DriverKind, TlsMode, ValueAccess};
use tangul_engine::{Command, EngineConfig, Event};
use tangul_service::knowledge::Knowledge;

const TOKEN: &str = "test-token";

/// A shop in a SQLite file, connected; a server exposing it, recording calls.
async fn server(name: &str) -> (std::path::PathBuf, McpServer, Arc<Mutex<Vec<ToolCall>>>) {
    server_on(name, 0).await
}

/// [`server`] on `port`.
async fn server_on(
    name: &str,
    port: u16,
) -> (std::path::PathBuf, McpServer, Arc<Mutex<Vec<ToolCall>>>) {
    let dir = std::env::temp_dir().join(format!("tangul-mcp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("shop.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL);
             INSERT INTO users (email) VALUES ('ann@example.com'), ('bob@example.com');",
        )
        .unwrap();
    let (engine, events) = tangul_engine::start_with(EngineConfig::isolated());
    let conn = ConnId(1);
    engine.send(Command::Connect {
        conn,
        driver: DriverKind::Sqlite,
        cfg: ConnectConfig {
            host: String::new(),
            port: 0,
            user: String::new(),
            password: None,
            database: path.to_string_lossy().into_owned(),
            tls: TlsMode::Disable,
        },
        ssh: None,
    });
    let info = loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("event in time")
            .expect("engine running");
        if let Event::Schema {
            result,
            complete: true,
            ..
        } = event
        {
            break result.unwrap();
        }
    };
    let knowledge = Knowledge::default();
    let dialect = dialect_for(DriverKind::Sqlite);
    let schema = SchemaIndex::new(info, dialect, dialect.default_schema(""));
    knowledge.set_schema("u1", "db1", conn, Arc::new(schema));
    let exposed = vec![Exposed {
        name: "shop".into(),
        user: "u1".into(),
        conn,
        driver: DriverKind::Sqlite,
        values: ValueAccess::None,
        bookmarks: Vec::new(),
    }];
    let calls: Arc<Mutex<Vec<ToolCall>>> = Arc::default();
    let sink = {
        let calls = calls.clone();
        Arc::new(move |c: ToolCall| calls.lock().unwrap().push(c))
    };
    let source: CtxSource = Arc::new(move || {
        Ctx::new(knowledge.clone(), Some(engine.clone()), exposed.clone())
            .with_audit("mcp", sink.clone())
    });
    let server = McpServer::start(port, TOKEN.into(), source).unwrap();
    (dir, server, calls)
}

/// An HTTP response: status, headers (lower case names), body.
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The JSON-RPC message: the body, or the event carrying it in an SSE stream.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .ok()
            .or_else(|| {
                self.body
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .find_map(|d| serde_json::from_str(d.trim()).ok())
            })
            .unwrap_or_else(|| panic!("no JSON-RPC message: {}", self.body))
    }
}

/// POSTs `body` to `/mcp` with `headers`, as a plain HTTP/1.1 client.
fn post(addr: SocketAddr, headers: &[(&str, &str)], body: &Value) -> Reply {
    let body = body.to_string();
    let mut request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
        addr.port(),
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&body);
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8_lossy(&raw).to_string();
    let (head, rest) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    let chunked = headers
        .iter()
        .any(|(n, v)| n == "transfer-encoding" && v.contains("chunked"));
    let body = if chunked {
        unchunk(rest)
    } else {
        rest.to_string()
    };
    Reply {
        status,
        headers,
        body,
    }
}

/// A chunked body joined.
fn unchunk(mut rest: &str) -> String {
    let mut out = String::new();
    while let Some((size, after)) = rest.split_once("\r\n") {
        let Ok(n) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if n == 0 {
            break;
        }
        out.push_str(&after[..n]);
        rest = &after[n + 2..];
    }
    out
}

fn rpc(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// A client's handshake in `version`; the headers to send along afterwards.
fn initialize(addr: SocketAddr, version: &str) -> Vec<(String, String)> {
    let auth = format!("Bearer {TOKEN}");
    let reply = post(
        addr,
        &[("Authorization", &auth)],
        &rpc(
            1,
            "initialize",
            json!({
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1"}
            }),
        ),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let result = &reply.json()["result"];
    assert_eq!(result["serverInfo"]["name"], "tangul");
    assert!(result["capabilities"]["tools"].is_object());
    assert!(
        result["instructions"]
            .as_str()
            .unwrap()
            .contains("list_connections")
    );
    let mut headers = vec![("Authorization".to_string(), auth)];
    if let Some(session) = reply.header("mcp-session-id") {
        headers.push(("Mcp-Session-Id".into(), session.to_string()));
    }
    headers.push((
        "MCP-Protocol-Version".into(),
        result["protocolVersion"].as_str().unwrap().to_string(),
    ));
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let _ = post(addr, &as_refs(&headers), &initialized);
    headers
}

fn as_refs(headers: &[(String, String)]) -> Vec<(&str, &str)> {
    headers
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect()
}

/// Calls tool `name`: its text and whether it is an error.
fn call(addr: SocketAddr, headers: &[(String, String)], name: &str, args: Value) -> (String, bool) {
    let reply = post(
        addr,
        &as_refs(headers),
        &rpc(3, "tools/call", json!({"name": name, "arguments": args})),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let result = &reply.json()["result"];
    (
        result["content"][0]["text"].as_str().unwrap().to_string(),
        result["isError"].as_bool().unwrap_or(false),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_lists_and_calls_the_tools() {
    let (dir, server, calls) = server("calls").await;
    let addr = server.addr();
    let headers = tokio::task::spawn_blocking(move || {
        let headers = initialize(addr, "2025-06-18");
        let reply = post(addr, &as_refs(&headers), &rpc(2, "tools/list", json!({})));
        assert_eq!(reply.status, 200, "{}", reply.body);
        let tools = reply.json()["result"]["tools"].as_array().unwrap().clone();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "list_connections",
                "tangul_features",
                "search_schema",
                "describe_table",
                "find_join_path",
                "schema_slice",
                "column_profile",
                "list_enums",
                "view_definition",
                "run_readonly",
                "get_cell",
                "list_bookmarks",
                "propose_sql"
            ]
        );
        let describe = tools
            .iter()
            .find(|t| t["name"] == "describe_table")
            .unwrap();
        assert_eq!(describe["inputSchema"]["required"], json!(["table"]));

        assert_eq!(
            call(addr, &headers, "describe_table", json!({"table": "users"})),
            (
                "users (table)\ncolumns:\n  id integer pk identity\n  email text not null".into(),
                false
            )
        );
        assert_eq!(
            call(
                addr,
                &headers,
                "run_readonly",
                json!({"sql": "SELECT count(*) AS n FROM users"})
            ),
            ("n\n2\n(1 row)".into(), false)
        );
        // Writes are never run; the model is told why.
        let (text, error) = call(
            addr,
            &headers,
            "run_readonly",
            json!({"sql": "DELETE FROM users"}),
        );
        assert!(error && text.contains("was not run"), "{text}");
        headers
    })
    .await
    .unwrap();
    assert!(!headers.is_empty());
    let calls = calls.lock().unwrap();
    let seen: Vec<_> = calls
        .iter()
        .map(|c| (c.caller, c.tool.as_str(), c.outcome))
        .collect();
    assert_eq!(
        seen,
        [
            ("mcp", "describe_table", "ran"),
            ("mcp", "run_readonly", "ran"),
            ("mcp", "run_readonly", "refused")
        ]
    );
    // The statement a call ran, whole, and its rows: for the database's history.
    let ran: Vec<_> = calls.iter().map(|c| (c.sql.as_deref(), c.rows)).collect();
    assert_eq!(
        ran,
        [
            (None, None),
            (Some("SELECT count(*) AS n FROM users"), Some(1)),
            (Some("DELETE FROM users"), None)
        ]
    );
    drop(calls);
    server.stop();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// What Claude Code speaks: no handshake, the version in every request, and a list
/// that says how long it may be cached and for whom (else the client drops it).
#[tokio::test(flavor = "multi_thread")]
async fn the_latest_protocol_gets_a_cacheable_list() {
    let (dir, server, _) = server("latest").await;
    let addr = server.addr();
    tokio::task::spawn_blocking(move || {
        let auth = format!("Bearer {TOKEN}");
        let headers = [
            ("Authorization", auth.as_str()),
            ("MCP-Protocol-Version", "2026-07-28"),
        ];
        let meta = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"}
        });
        let reply = post(
            addr,
            &[headers[0], headers[1], ("Mcp-Method", "tools/list")],
            &rpc(1, "tools/list", json!({"_meta": meta})),
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        let result = &reply.json()["result"];
        assert_eq!(result["ttlMs"], 0, "{result}");
        assert_eq!(result["cacheScope"], "private");
        assert_eq!(result["tools"].as_array().unwrap().len(), 13);
        let reply = post(
            addr,
            &[
                headers[0],
                headers[1],
                ("Mcp-Method", "tools/call"),
                ("Mcp-Name", "describe_table"),
            ],
            &rpc(
                2,
                "tools/call",
                json!({"name": "describe_table", "arguments": {"table": "users"}, "_meta": meta}),
            ),
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        let result = &reply.json()["result"];
        assert!(!result["isError"].as_bool().unwrap_or(false), "{result}");
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("users (table)")
        );
        // A cut cell, read whole in a later call (tangul kept the result).
        let call = |id: u64, name: &str, arguments: Value| {
            let reply = post(
                addr,
                &[
                    headers[0],
                    headers[1],
                    ("Mcp-Method", "tools/call"),
                    ("Mcp-Name", name),
                ],
                &rpc(
                    id,
                    "tools/call",
                    json!({"name": name, "arguments": arguments, "_meta": meta}),
                ),
            );
            reply.json()["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let cut = call(
            3,
            "run_readonly",
            json!({"sql": "SELECT substr(hex(zeroblob(150)), 1, 300) AS v"}),
        );
        assert!(
            cut.contains("…[300 chars; get_cell r1 row 1 col v]"),
            "{cut}"
        );
        assert_eq!(
            call(
                4,
                "get_cell",
                json!({"result": "r1", "row": 1, "column": "v"})
            ),
            "0".repeat(300)
        );
    })
    .await
    .unwrap();
    server.stop();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn only_with_the_token_and_not_from_browsers() {
    let (dir, server, calls) = server("guard").await;
    let addr = server.addr();
    tokio::task::spawn_blocking(move || {
        let hello = rpc(
            1,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "test", "version": "1"}}),
        );
        assert_eq!(post(addr, &[], &hello).status, 401);
        assert_eq!(
            post(addr, &[("Authorization", "Bearer wrong")], &hello).status,
            401
        );
        assert_eq!(post(addr, &[("Authorization", TOKEN)], &hello).status, 401);
        // A web page's request (it has an Origin) is refused even with the token.
        let auth = format!("Bearer {TOKEN}");
        let from_page = post(
            addr,
            &[("Authorization", &auth), ("Origin", "https://evil.example")],
            &hello,
        );
        assert_eq!(from_page.status, 403, "{}", from_page.body);
        // The right token, no Origin: fine.
        assert_eq!(post(addr, &[("Authorization", &auth)], &hello).status, 200);
    })
    .await
    .unwrap();
    assert!(calls.lock().unwrap().is_empty());
    server.stop();
    std::fs::remove_dir_all(&dir).unwrap();
}
