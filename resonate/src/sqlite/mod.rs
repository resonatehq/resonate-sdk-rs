//! An alternative [`Network`](crate::network::Network) backed by the Resonate
//! server's SQLite engine, embedded in-process.
//!
//! Where [`LocalNetwork`](crate::network::LocalNetwork) reimplements the server
//! as an in-memory state machine, this module vendors the real server's request
//! handlers ([`server`]), SQLite persistence ([`persistence_sqlite`]) and
//! timeout processing ([`processing`]) verbatim and drives them directly —
//! giving durable, on-disk semantics identical to talking to a `resonate`
//! server over HTTP, but with no network hop.
//!
//! Two layers:
//! - [`SqliteServer`] owns the SQLite storage, the background timeout/message
//!   loops, and a registry of connected clients. Outgoing `execute`/`unblock`
//!   messages are routed to clients by their `poll://` address (group + pid),
//!   mirroring the server's poll registry.
//! - [`SqliteNetwork`] is a single client connection (one `pid`/`group`) that
//!   implements [`Network`]. Many connections can share one server, so
//!   multi-worker scenarios behave exactly as they do against a real server.
//!
//! ```no_run
//! use std::sync::Arc;
//! use resonate_sdk::prelude::*;
//! use resonate_sdk::sqlite::SqliteNetwork;
//!
//! # async fn example() -> resonate_sdk::error::Result<()> {
//! let net = Arc::new(SqliteNetwork::new("resonate.db", None, None).await?);
//! let resonate = Resonate::new(ResonateConfig {
//!     network: Some(net),
//!     ..Default::default()
//! });
//! # let _ = resonate;
//! # Ok(())
//! # }
//! ```
//!
//! Note: the libsql backend is natively async (queries run through the libsql
//! client, not [`tokio::task::block_in_place`]), so a `SqliteNetwork` runs on any
//! Tokio runtime — single- or multi-threaded.

mod address;
mod persistence;
mod persistence_sqlite;
mod processing;
mod server;
mod types;
mod util;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};

use crate::error::{Error, Result};
use crate::network::Network;

use address::{parse_address, Address, PollCast};
use persistence_sqlite::SqliteStorage;
use server::{dispatch, Server};
pub use server::{Config, MessagesConfig, ServerConfig, TasksConfig, TimeoutsConfig};
use types::RequestEnvelope;

type Subscribers = RwLock<Vec<Box<dyn Fn(String) + Send + Sync>>>;

/// Selects how the embedded server's libsql database is opened.
#[derive(Clone, Debug)]
pub enum SqliteBackend {
    /// A local database file, or `":memory:"` for an ephemeral in-memory DB.
    Local(String),
    /// A remote Turso/sqld database addressed by URL with an auth token.
    Remote { url: String, auth_token: String },
    /// An embedded replica: a local file kept in sync with a remote database.
    RemoteReplica {
        path: String,
        url: String,
        auth_token: String,
        /// Optional background sync interval. `None` syncs only on open.
        sync_interval: Option<Duration>,
    },
}

/// A single client connection registered with a [`SqliteServer`].
struct Connection {
    group: String,
    pid: String,
    subscribers: Subscribers,
}

/// Shared state owned by a [`SqliteServer`] and captured by its background loops.
struct Shared {
    server: Arc<Server>,
    connections: RwLock<Vec<Arc<Connection>>>,
}

/// An embedded, SQLite-backed Resonate server.
///
/// Holds the storage and background loops; hand out [`SqliteNetwork`] client
/// connections with [`SqliteServer::connect`]. Multiple connections share the
/// same database and message router.
pub struct SqliteServer {
    shared: Arc<Shared>,
    handles: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    started: AtomicBool,
}

impl Drop for SqliteServer {
    fn drop(&mut self) {
        for handle in self.handles.lock().drain(..) {
            handle.abort();
        }
    }
}

impl SqliteServer {
    /// Open (or create) a local libsql database at `path` (use `":memory:"` for
    /// an ephemeral DB) with default [`Config`].
    pub async fn open(path: &str) -> Result<Arc<Self>> {
        Self::open_backend(SqliteBackend::Local(path.to_string()), Config::default()).await
    }

    /// Like [`SqliteServer::open`], but with an explicit [`Config`].
    pub async fn open_with_config(path: &str, config: Config) -> Result<Arc<Self>> {
        Self::open_backend(SqliteBackend::Local(path.to_string()), config).await
    }

    /// Open against a remote Turso/sqld database (URL + auth token).
    pub async fn open_remote(
        url: impl Into<String>,
        auth_token: impl Into<String>,
        config: Config,
    ) -> Result<Arc<Self>> {
        Self::open_backend(
            SqliteBackend::Remote {
                url: url.into(),
                auth_token: auth_token.into(),
            },
            config,
        )
        .await
    }

    /// Open an embedded replica: a local file at `path` kept in sync with a
    /// remote database (URL + auth token), optionally syncing on `sync_interval`.
    pub async fn open_remote_replica(
        path: impl Into<String>,
        url: impl Into<String>,
        auth_token: impl Into<String>,
        sync_interval: Option<Duration>,
        config: Config,
    ) -> Result<Arc<Self>> {
        Self::open_backend(
            SqliteBackend::RemoteReplica {
                path: path.into(),
                url: url.into(),
                auth_token: auth_token.into(),
                sync_interval,
            },
            config,
        )
        .await
    }

    /// Open the server against an explicit [`SqliteBackend`] and [`Config`].
    pub async fn open_backend(backend: SqliteBackend, config: Config) -> Result<Arc<Self>> {
        let storage = SqliteStorage::open(&backend, config.tasks.retry_timeout)
            .await
            .map_err(|e| Error::ServerError {
                code: 500,
                message: format!("failed to open sqlite database: {}", e),
            })?;
        let server = Arc::new(Server::new(config, storage));
        Ok(Arc::new(Self {
            shared: Arc::new(Shared {
                server,
                connections: RwLock::new(Vec::new()),
            }),
            handles: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
        }))
    }

    /// Create a client connection (a [`Network`]) backed by this server.
    ///
    /// - `pid`: Process ID for this worker (or generated).
    /// - `group`: Group name for routing (default: `"default"`).
    pub fn connect(self: &Arc<Self>, pid: Option<String>, group: Option<String>) -> SqliteNetwork {
        let pid = pid.unwrap_or_else(crate::network::uuid_no_dashes);
        let group = group.unwrap_or_else(|| "default".to_string());
        let unicast = format!("poll://uni@{}/{}", group, pid);
        let anycast = format!("poll://any@{}/{}", group, pid);

        let conn = Arc::new(Connection {
            group,
            pid,
            subscribers: RwLock::new(Vec::new()),
        });
        self.shared.connections.write().push(conn.clone());

        SqliteNetwork {
            server: self.clone(),
            conn,
            unicast,
            anycast,
        }
    }

    /// Spawn the background timeout + message loops once.
    fn ensure_started(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        // Timeout processing loop (promise/task retry/lease + schedules).
        let timeout_shared = self.shared.clone();
        let timeout_interval = self.shared.server.config.timeouts.poll_interval.max(1);
        let timeout_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(timeout_interval));
            loop {
                interval.tick().await;
                if timeout_shared.server.debug_mode.load(Ordering::SeqCst) {
                    continue;
                }
                let now = util::system_time_ms();
                if let Err(e) = timeout_shared
                    .server
                    .storage
                    .transact(move |db| {
                        Box::pin(async move { processing::process_all_timeouts(db, now).await })
                    })
                    .await
                {
                    tracing::error!(error = %e, "sqlite_network: background timeout processing failed");
                }
            }
        });

        // Message delivery loop — drains messages enqueued by the timeout loop
        // (and any not already drained by `send`).
        let msg_shared = self.shared.clone();
        let msg_interval = self.shared.server.config.messages.poll_interval.max(1);
        let msg_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(msg_interval));
            loop {
                interval.tick().await;
                if msg_shared.server.debug_mode.load(Ordering::SeqCst) {
                    continue;
                }
                route_outgoing(&msg_shared).await;
            }
        });

        let mut handles = self.handles.lock();
        handles.push(timeout_handle);
        handles.push(msg_handle);
    }
}

/// A [`Network`] implementation: one client connection to a [`SqliteServer`].
pub struct SqliteNetwork {
    server: Arc<SqliteServer>,
    conn: Arc<Connection>,
    unicast: String,
    anycast: String,
}

impl SqliteNetwork {
    /// Open a private [`SqliteServer`] at `path` and return a single connection
    /// to it. Use [`SqliteServer::connect`] to share one server across workers.
    pub async fn new(path: &str, pid: Option<String>, group: Option<String>) -> Result<Self> {
        Self::with_config(path, pid, group, Config::default()).await
    }

    /// Like [`SqliteNetwork::new`], but with an explicit [`Config`].
    pub async fn with_config(
        path: &str,
        pid: Option<String>,
        group: Option<String>,
        config: Config,
    ) -> Result<Self> {
        let server = SqliteServer::open_with_config(path, config).await?;
        Ok(server.connect(pid, group))
    }

    /// Like [`SqliteNetwork::new`], but against an explicit [`SqliteBackend`].
    pub async fn with_backend(
        backend: SqliteBackend,
        pid: Option<String>,
        group: Option<String>,
        config: Config,
    ) -> Result<Self> {
        let server = SqliteServer::open_backend(backend, config).await?;
        Ok(server.connect(pid, group))
    }
}

/// Claim a batch of outgoing messages and route each to the connection(s) whose
/// `poll://` address matches.
///
/// Mirrors `processing::processing_messages::process_batch` from the server, but
/// instead of routing through a transport dispatcher it forwards to the matching
/// in-process `recv` subscribers (the poll registry, in miniature).
async fn route_outgoing(shared: &Arc<Shared>) {
    let batch_size = shared.server.config.messages.batch_size;
    let server_url = shared.server.config.server.url.clone().unwrap_or_default();

    let (execute_msgs, unblock_msgs) = match shared
        .server
        .storage
        .transact(move |db| Box::pin(async move { db.take_outgoing(batch_size).await }))
        .await
    {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::error!(error = %e, "sqlite_network: failed to take outgoing messages");
            return;
        }
    };

    if execute_msgs.is_empty() && unblock_msgs.is_empty() {
        return;
    }

    // Snapshot the connection registry (cheap Arc clones) so callbacks don't run
    // while the registry lock is held.
    let conns: Vec<Arc<Connection>> = shared.connections.read().clone();

    for msg in execute_msgs {
        let payload = serde_json::json!({
            "kind": "execute",
            "head": { "serverUrl": server_url },
            "data": { "task": { "id": msg.id, "version": msg.version } },
        });
        deliver(&conns, &msg.address, &payload.to_string());
    }
    for msg in unblock_msgs {
        let payload = serde_json::json!({
            "kind": "unblock",
            "head": {},
            "data": { "promise": msg.promise },
        });
        deliver(&conns, &msg.address, &payload.to_string());
    }
}

/// Deliver `msg` to every connection matching `address`.
///
/// `poll://uni@group/pid` → the connection with that exact group + pid.
/// `poll://any@group` → every connection in that group (acquire is
/// version-gated, so anycast fan-out is safe). Non-`poll` addresses are not
/// deliverable in-process and are dropped.
fn deliver(conns: &[Arc<Connection>], address: &str, msg: &str) {
    let Some(Address::Poll(addr)) = parse_address(address) else {
        return;
    };
    for conn in conns {
        if conn.group != addr.group {
            continue;
        }
        let matches = match addr.cast {
            PollCast::Any => true,
            PollCast::Uni => addr.id.as_deref() == Some(conn.pid.as_str()),
        };
        if matches {
            for cb in conn.subscribers.read().iter() {
                cb(msg.to_string());
            }
        }
    }
}

#[async_trait::async_trait]
impl Network for SqliteNetwork {
    fn pid(&self) -> &str {
        &self.conn.pid
    }
    fn group(&self) -> &str {
        &self.conn.group
    }
    fn unicast(&self) -> &str {
        &self.unicast
    }
    fn anycast(&self) -> &str {
        &self.anycast
    }

    async fn start(&self) -> Result<()> {
        self.server.ensure_started();
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        // Deregister this connection and drop its subscribers. Background loops
        // belong to the (possibly shared) server and keep running until the
        // server itself is dropped.
        let ptr = Arc::as_ptr(&self.conn);
        self.server
            .shared
            .connections
            .write()
            .retain(|c| !std::ptr::eq(Arc::as_ptr(c), ptr));
        self.conn.subscribers.write().clear();
        Ok(())
    }

    async fn send(&self, req: String) -> Result<String> {
        let req_env: RequestEnvelope = serde_json::from_str(&req)
            .map_err(|e| Error::DecodingError(format!("invalid JSON request: {}", e)))?;

        let now = util::system_time_ms();
        let response = dispatch(&self.server.shared.server, &req_env, now).await;
        let resp_str = serde_json::to_string(&response)?;

        // Promptly deliver any messages this request enqueued (execute/unblock),
        // matching `LocalNetwork`'s synchronous dispatch on send.
        route_outgoing(&self.server.shared).await;

        Ok(resp_str)
    }

    fn recv(&self, callback: Box<dyn Fn(String) + Send + Sync>) {
        self.conn.subscribers.write().push(callback);
    }

    fn target_resolver(&self, target: &str) -> String {
        format!("poll://any@{}", target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(resp: &serde_json::Value) -> i64 {
        resp.get("head")
            .and_then(|h| h.get("status"))
            .and_then(|s| s.as_i64())
            .unwrap_or(0)
    }

    fn data(resp: &serde_json::Value) -> &serde_json::Value {
        resp.get("data").unwrap_or(resp)
    }

    async fn net() -> SqliteNetwork {
        SqliteNetwork::new(":memory:", Some("test-pid".into()), Some("default".into()))
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn identity() {
        let net = net().await;
        assert_eq!(net.pid(), "test-pid");
        assert_eq!(net.group(), "default");
        assert_eq!(net.unicast(), "poll://uni@default/test-pid");
        assert_eq!(net.anycast(), "poll://any@default/test-pid");
        assert_eq!(net.target_resolver("hello"), "poll://any@hello");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn creates_and_gets_promise() {
        let net = net().await;
        let create = serde_json::json!({
            "kind": "promise.create",
            "head": { "corrId": "c1", "version": "2026-04-01" },
            "data": {
                "id": "p1", "timeoutAt": i64::MAX,
                "param": { "data": "test" }, "tags": { "resonate:scope": "global" },
            },
        });
        let resp: serde_json::Value =
            serde_json::from_str(&net.send(create.to_string()).await.unwrap()).unwrap();
        assert!(status(&resp) == 200 || status(&resp) == 201);
        assert_eq!(data(&resp)["promise"]["id"], "p1");
        assert_eq!(data(&resp)["promise"]["state"], "pending");

        let get = serde_json::json!({
            "kind": "promise.get",
            "head": { "corrId": "c2", "version": "2026-04-01" },
            "data": { "id": "p1" },
        });
        let get_resp: serde_json::Value =
            serde_json::from_str(&net.send(get.to_string()).await.unwrap()).unwrap();
        assert_eq!(status(&get_resp), 200);
        assert_eq!(data(&get_resp)["promise"]["id"], "p1");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn idempotent_promise_create() {
        let net = net().await;
        let create = serde_json::json!({
            "kind": "promise.create",
            "head": { "corrId": "c1", "version": "2026-04-01" },
            "data": { "id": "p1", "timeoutAt": i64::MAX, "param": {}, "tags": {} },
        });
        let r1: serde_json::Value =
            serde_json::from_str(&net.send(create.to_string()).await.unwrap()).unwrap();
        assert_eq!(status(&r1), 200);
        let r2: serde_json::Value =
            serde_json::from_str(&net.send(create.to_string()).await.unwrap()).unwrap();
        assert_eq!(status(&r2), 200);
        assert_eq!(data(&r2)["promise"]["id"], "p1");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn task_create_dispatches_execute_to_subscriber() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let net = net().await;
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        net.recv(Box::new(move |raw: String| {
            if raw.contains("\"execute\"") {
                count2.fetch_add(1, Ordering::SeqCst);
            }
        }));

        let create = serde_json::json!({
            "kind": "task.create",
            "head": { "corrId": "c1", "version": "2026-04-01" },
            "data": {
                "pid": "test-pid", "ttl": 60000,
                "action": {
                    "kind": "promise.create",
                    "head": { "corrId": "c1a", "version": "2026-04-01" },
                    "data": {
                        "id": "rpc-1", "timeoutAt": i64::MAX,
                        "param": { "data": "x" },
                        "tags": { "resonate:target": "poll://any@default" },
                    },
                },
            },
        });
        let resp: serde_json::Value =
            serde_json::from_str(&net.send(create.to_string()).await.unwrap()).unwrap();
        assert!(status(&resp) == 200 || status(&resp) == 201);
        assert_eq!(data(&resp)["task"]["state"], "acquired");

        // task.create on an acquired task does not enqueue an execute; release it
        // back to pending to enqueue one, then confirm it reaches the subscriber.
        let release = serde_json::json!({
            "kind": "task.release",
            "head": { "corrId": "c2", "version": "2026-04-01" },
            "data": { "id": "rpc-1", "version": 1 },
        });
        let rel: serde_json::Value =
            serde_json::from_str(&net.send(release.to_string()).await.unwrap()).unwrap();
        assert_eq!(status(&rel), 200);

        assert!(
            count.load(Ordering::SeqCst) >= 1,
            "expected at least one execute message delivered to subscriber"
        );
    }
}
