use std::future::IntoFuture;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::{Error, Result};
#[cfg(test)]
use crate::sequencing::creation_channel;
use crate::sequencing::{await_created_id, CreationState};

/// Message delivered from a spawned background task to its handle.
///
/// A spawned task whose promise is still pending reports `Suspended` rather than
/// a value; awaiting the handle then signals the parent driver and parks. Being
/// internal, suspension never reaches user code as a `Result`.
#[derive(Debug)]
pub(crate) enum HandleMsg<T> {
    Done(Result<T>),
    Suspended,
}

/// Shared state of a spawned-task handle: the promise ID, the creation gate,
/// the typed result channel, and the parent context's suspend signal (notified
/// when the awaited task turns out to have suspended). `DurableFuture` and
/// `RemoteFuture` are thin wrappers over this.
///
/// The `'ctx` lifetime brands the handle to the `Context` that created it. This
/// is load-bearing: awaiting a handle parks on the assumption that a driver is
/// racing that context's suspend signal, which only holds inside the workflow
/// body. The brand makes the handle non-`'static` so it can't be moved into a
/// `tokio::spawn` — an orphaned await that would park forever is a compile error.
struct Handle<'ctx, T> {
    id: String,
    created: tokio::sync::watch::Receiver<CreationState>,
    receiver: tokio::sync::oneshot::Receiver<HandleMsg<T>>,
    suspend: Arc<tokio::sync::Notify>,
    /// Brands the handle to the creating context's lifetime; see the type doc.
    _ctx: PhantomData<&'ctx ()>,
}

impl<'ctx, T> Handle<'ctx, T> {
    /// Wait until the creation state leaves `InFlight`, then map it to the ID.
    /// The ID is only returned on confirmed server-side creation.
    async fn id(&self) -> Result<String> {
        await_created_id(&self.id, &self.created).await
    }

    /// Await the result delivered by the background task.
    ///
    /// On `Suspended`, signal the parent context's driver and park forever: the
    /// parent's `select!` wins, drops this future, and reads its `remote_todos`
    /// (the spawned task's todo was already recorded via its `Outcome`). Parking
    /// is sound only because the `'ctx` brand guarantees a driver is racing this
    /// context's suspend signal (see the `Handle` type doc).
    async fn recv(self) -> Result<T> {
        match self.receiver.await {
            Ok(HandleMsg::Done(result)) => result,
            Ok(HandleMsg::Suspended) => {
                self.suspend.notify_one();
                std::future::pending().await
            }
            Err(_) => Err(Error::JoinError(format!("task {} was dropped", self.id))),
        }
    }
}

/// A handle to an eagerly spawned local durable task.
///
/// Created by `ctx.run(F, args).spawn()`. Awaiting this future returns the
/// result once the spawned task completes. `id()` returns the durable promise
/// ID once the promise has been successfully created on the server.
///
/// The `'ctx` lifetime ties the handle to its context so it can't be awaited
/// outside the workflow body (e.g. moved into a `tokio::spawn`); see the
/// `Handle` type doc for why.
pub struct DurableFuture<'ctx, T>(Handle<'ctx, T>);

impl<'ctx, T> DurableFuture<'ctx, T> {
    pub(crate) fn pending(
        id: String,
        receiver: tokio::sync::oneshot::Receiver<HandleMsg<T>>,
        created: tokio::sync::watch::Receiver<CreationState>,
        suspend: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self(Handle {
            id,
            created,
            receiver,
            suspend,
            _ctx: PhantomData,
        })
    }

    /// Returns the durable promise ID once the promise has been successfully
    /// created on the server. Fails if creation failed (or was aborted because
    /// an earlier promise creation in the same workflow failed).
    pub async fn id(&self) -> Result<String> {
        self.0.id().await
    }
}

impl<'ctx, T: Send + 'static> IntoFuture for DurableFuture<'ctx, T> {
    type Output = Result<T>;
    type IntoFuture = Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'ctx>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            tracing::info!(
                target: "resonate::validation",
                promise_id = %self.0.id,
                "promise_execution_await"
            );
            self.0.recv().await
        })
    }
}

/// A handle to a remote durable task.
///
/// Created by `ctx.rpc("func", &args).spawn()`. Awaiting this future returns
/// the result once the remote promise is settled. If the promise is still
/// pending, awaiting parks and signals suspension (it never returns to user
/// code), suspending the workflow until the remote settles. `id()` returns the
/// durable promise ID once the promise has been successfully created on the
/// server.
///
/// The `'ctx` lifetime ties the handle to its context so it can't be awaited
/// outside the workflow body (e.g. moved into a `tokio::spawn`); see the
/// `Handle` type doc for why.
pub struct RemoteFuture<'ctx, T>(Handle<'ctx, T>);

impl<'ctx, T> RemoteFuture<'ctx, T> {
    pub(crate) fn pending(
        id: String,
        receiver: tokio::sync::oneshot::Receiver<HandleMsg<T>>,
        created: tokio::sync::watch::Receiver<CreationState>,
        suspend: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self(Handle {
            id,
            created,
            receiver,
            suspend,
            _ctx: PhantomData,
        })
    }

    /// Returns the durable promise ID once the promise has been successfully
    /// created on the server. Fails if creation failed (or was aborted because
    /// an earlier promise creation in the same workflow failed).
    pub async fn id(&self) -> Result<String> {
        self.0.id().await
    }
}

impl<'ctx, T: Send + 'static> IntoFuture for RemoteFuture<'ctx, T> {
    type Output = Result<T>;
    type IntoFuture = Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'ctx>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { self.0.recv().await })
    }
}

/// A handle to a detached, fire-and-forget remote execution.
///
/// Created by `ctx.detached("func", &args).spawn()`. Unlike [`RemoteFuture`],
/// a `DetachedHandle` is **not** a future — it deliberately does not implement
/// `IntoFuture`, because a detached call's result is never delivered back to
/// the parent. The only thing the parent can observe is the promise ID, via
/// [`id()`](DetachedHandle::id), once the promise exists on the server.
///
/// Dropping the handle is fine and common: the detached promise is still
/// created on the server (and a creation failure still fails the task at
/// flush). Hold the handle only when you need the ID to hand to an external
/// system.
pub struct DetachedHandle {
    id: String,
    created: tokio::sync::watch::Receiver<CreationState>,
}

impl DetachedHandle {
    pub(crate) fn pending(
        id: String,
        created: tokio::sync::watch::Receiver<CreationState>,
    ) -> Self {
        Self { id, created }
    }

    /// Returns the durable promise ID once the promise has been successfully
    /// created on the server. Fails if creation failed (or was aborted because
    /// an earlier promise creation in the same workflow failed).
    pub async fn id(&self) -> Result<String> {
        await_created_id(&self.id, &self.created).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn settled_creation() -> tokio::sync::watch::Receiver<CreationState> {
        let (tx, rx) = creation_channel();
        let _ = tx.send(CreationState::Created);
        rx
    }

    fn notify() -> Arc<tokio::sync::Notify> {
        Arc::new(tokio::sync::Notify::new())
    }

    // ── DurableFuture ──────────────────────────────────────────────

    #[tokio::test]
    async fn durable_future_pending_resolves_via_await() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let future: DurableFuture<'_, String> =
            DurableFuture::pending("test-id".into(), rx, settled_creation(), notify());

        tx.send(HandleMsg::Done(Ok("hello".to_string()))).unwrap();
        let result: String = future.await.unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn durable_future_pending_error_via_await() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let future: DurableFuture<'_, i32> =
            DurableFuture::pending("test-id".into(), rx, settled_creation(), notify());

        tx.send(HandleMsg::Done(Err(Error::Application {
            message: "task failed".into(),
        })))
        .unwrap();
        let err = future.await.unwrap_err();
        assert!(matches!(err, Error::Application { .. }));
    }

    #[tokio::test]
    async fn durable_future_suspended_signals_and_parks() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let suspend = notify();
        let future: DurableFuture<'_, i32> =
            DurableFuture::pending("test-id".into(), rx, settled_creation(), Arc::clone(&suspend));

        tx.send(HandleMsg::Suspended).unwrap();
        // Awaiting a suspended handle parks forever — it never hands a value
        // (or a sentinel error) back to user code.
        let parked = tokio::time::timeout(Duration::from_millis(50), future).await;
        assert!(parked.is_err(), "suspended handle await should park, not resolve");
        // ...and it signalled the parent's suspend Notify so the driver can wake.
        tokio::time::timeout(Duration::from_millis(50), suspend.notified())
            .await
            .expect("suspend signal should have fired");
    }

    // ── RemoteFuture ───────────────────────────────────────────────

    #[tokio::test]
    async fn remote_future_completed_via_await() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let future: RemoteFuture<'_, String> =
            RemoteFuture::pending("test-id".into(), rx, settled_creation(), notify());

        tx.send(HandleMsg::Done(Ok("remote-value".to_string())))
            .unwrap();
        let result: String = future.await.unwrap();
        assert_eq!(result, "remote-value");
    }

    #[tokio::test]
    async fn remote_future_failed_via_await() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let future: RemoteFuture<'_, i32> =
            RemoteFuture::pending("test-id".into(), rx, settled_creation(), notify());

        tx.send(HandleMsg::Done(Err(Error::Application {
            message: "remote error".into(),
        })))
        .unwrap();
        let err = future.await.unwrap_err();
        assert!(matches!(err, Error::Application { .. }));
    }

    #[tokio::test]
    async fn remote_future_suspended_signals_and_parks() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let suspend = notify();
        let future: RemoteFuture<'_, i32> =
            RemoteFuture::pending("test-id".into(), rx, settled_creation(), Arc::clone(&suspend));

        tx.send(HandleMsg::Suspended).unwrap();
        let parked = tokio::time::timeout(Duration::from_millis(50), future).await;
        assert!(parked.is_err(), "suspended handle await should park, not resolve");
        tokio::time::timeout(Duration::from_millis(50), suspend.notified())
            .await
            .expect("suspend signal should have fired");
    }

    // ── id() gating ────────────────────────────────────────────────

    #[tokio::test]
    async fn id_returns_after_creation_succeeds() {
        let (created_tx, created_rx) = creation_channel();
        let (_result_tx, result_rx) = tokio::sync::oneshot::channel::<HandleMsg<i32>>();
        let future: RemoteFuture<'_, i32> =
            RemoteFuture::pending("p.1".into(), result_rx, created_rx, notify());

        created_tx.send(CreationState::Created).unwrap();
        assert_eq!(future.id().await.unwrap(), "p.1");
        // id() can be called more than once
        assert_eq!(future.id().await.unwrap(), "p.1");
    }

    #[tokio::test]
    async fn id_fails_when_creation_failed() {
        let (created_tx, created_rx) = creation_channel();
        let (_result_tx, result_rx) = tokio::sync::oneshot::channel::<HandleMsg<i32>>();
        let future: RemoteFuture<'_, i32> =
            RemoteFuture::pending("p.1".into(), result_rx, created_rx, notify());

        created_tx
            .send(CreationState::Failed("boom".into()))
            .unwrap();
        let err = future.id().await.unwrap_err();
        assert!(matches!(err, Error::PromiseCreation(_)), "got: {err}");
    }
}
