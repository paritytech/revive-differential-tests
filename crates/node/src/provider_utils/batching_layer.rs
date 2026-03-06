use std::collections::HashMap;
use std::sync::OnceLock;

use crate::internal_prelude::*;

type Sender<T> = tokio::sync::oneshot::Sender<T>;

struct PendingRequest {
    request: SerializedRequest,
    tx: Sender<TransportResult<Response>>,
}

/// A handle that aborts the associated task when dropped.
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A [`Layer`] that batches individual JSON-RPC requests into batch calls.
///
/// Instead of sending each RPC request individually, incoming [`RequestPacket::Single`] requests
/// are collected in a shared queue. A background task periodically drains the queue, groups the
/// requests into chunks of at most `max_batch_size`, and sends each chunk as a single
/// [`RequestPacket::Batch`] to the inner service. Responses are matched back to their callers by
/// JSON-RPC [`Id`].
///
/// [`RequestPacket::Batch`] requests bypass the batching logic and are forwarded directly to the
/// inner service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BatchingLayer {
    max_batch_size: usize,
    batching_duration: Duration,
}

impl BatchingLayer {
    /// Creates a new [`BatchingLayer`] with default settings (batch size 100, flush interval 200ms).
    pub fn new() -> Self {
        Self {
            max_batch_size: 100,
            batching_duration: Duration::from_millis(200),
        }
    }

    /// Sets the maximum number of requests to include in a single batch call.
    pub fn with_max_batch_size(mut self, max_batch_size: impl Into<usize>) -> Self {
        self.max_batch_size = max_batch_size.into();
        self
    }

    /// Sets the interval at which the background task flushes pending requests.
    pub fn with_batching_duration(mut self, batching_duration: impl Into<Duration>) -> Self {
        self.batching_duration = batching_duration.into();
        self
    }
}

impl Default for BatchingLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for BatchingLayer {
    type Service = BatchingService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BatchingService {
            service: inner,
            max_batch_size: self.max_batch_size,
            batching_duration: self.batching_duration,
            pending_requests: Arc::new(StdMutex::new(Vec::new())),
            abort_on_drop: Arc::new(OnceLock::new()),
        }
    }
}

/// The [`Service`] created by [`BatchingLayer`].
///
/// Queues individual RPC requests and dispatches them in batches via a background task. See
/// [`BatchingLayer`] for details.
#[derive(Clone)]
pub struct BatchingService<S> {
    /// The inner transport service used to send batched requests.
    service: S,
    /// Maximum number of requests per batch call.
    max_batch_size: usize,
    /// How often the background task flushes the queue.
    batching_duration: Duration,
    /// Shared queue of requests waiting to be batched and sent.
    pending_requests: Arc<StdMutex<Vec<PendingRequest>>>,
    /// Lazily-initialized handle to the background flush task. When all [`BatchingService`] clones
    /// are dropped, the [`AbortOnDrop`] is dropped and the background task is cancelled.
    abort_on_drop: Arc<OnceLock<AbortOnDrop>>,
}

impl<S> BatchingService<S>
where
    S: Service<RequestPacket, Future = TransportFut<'static>, Error = TransportError>
        + Send
        + Clone
        + 'static,
{
    /// Spawns the background flush task if it has not been spawned yet.
    fn ensure_background_task_spawned(&self) {
        self.abort_on_drop.get_or_init(|| {
            let pending_requests = self.pending_requests.clone();
            let service = self.service.clone();
            let max_batch_size = self.max_batch_size;
            let batching_duration = self.batching_duration;

            debug!(
                max_batch_size,
                ?batching_duration,
                "Spawning batching background task"
            );

            let handle = tokio::spawn(async move {
                let mut interval = interval(batching_duration);

                loop {
                    interval.tick().await;

                    let mut remaining: Vec<PendingRequest> = {
                        let mut lock = pending_requests.lock().expect("Mutex poisoned");
                        lock.drain(..).collect()
                    };

                    if remaining.is_empty() {
                        continue;
                    }

                    let total_requests = remaining.len();
                    debug!(total_requests, max_batch_size, "Flushing pending requests");

                    // Split into chunks and send them all concurrently.
                    let mut chunk_futures = Vec::new();
                    while !remaining.is_empty() {
                        let chunk_end = remaining.len().min(max_batch_size);
                        let chunk: Vec<PendingRequest> = remaining.drain(..chunk_end).collect();
                        let mut svc = service.clone();

                        chunk_futures.push(async move {
                            send_chunk(&mut svc, chunk).await;
                        });
                    }

                    futures::future::join_all(chunk_futures).await;
                }
            });

            AbortOnDrop(handle.abort_handle())
        });
    }
}

/// Sends a single chunk of pending requests as a batch call, then dispatches responses back to
/// their callers via oneshot channels.
async fn send_chunk<S>(service: &mut S, chunk: Vec<PendingRequest>)
where
    S: Service<RequestPacket, Future = TransportFut<'static>, Error = TransportError>,
{
    let chunk_size = chunk.len();

    let mut senders: HashMap<Id, Sender<TransportResult<Response>>> =
        HashMap::with_capacity(chunk_size);
    let mut serialized_requests: Vec<SerializedRequest> = Vec::with_capacity(chunk_size);

    for pending in chunk {
        let id = pending.request.id().clone();
        serialized_requests.push(pending.request);
        senders.insert(id, pending.tx);
    }

    let packet: RequestPacket = serialized_requests.into_iter().collect();

    debug!(chunk_size, "Sending batch request");

    match service.call(packet).await {
        Ok(response_packet) => {
            let responses = match response_packet {
                ResponsePacket::Single(r) => vec![r],
                ResponsePacket::Batch(rs) => rs,
            };

            let response_count = responses.len();
            for response in responses {
                if let Some(tx) = senders.remove(&response.id) {
                    let _ = tx.send(Ok(response));
                }
            }

            let unmatched = senders.len();
            if unmatched > 0 {
                debug!(
                    unmatched,
                    response_count, chunk_size, "Some requests had no matching response in batch"
                );
            }

            for (id, tx) in senders {
                debug!(%id, "No matching response for request");
                let _ = tx.send(Err(TransportErrorKind::custom_str(
                    "No matching response in batch for request ID",
                )));
            }
        }
        Err(e) => {
            debug!(chunk_size, error = %e, "Batch request failed, notifying all senders");
            for (_, tx) in senders {
                let _ = tx.send(Err(TransportErrorKind::custom_str(&format!(
                    "Batch request failed: {e}"
                ))));
            }
        }
    }
}

impl<S> Service<RequestPacket> for BatchingService<S>
where
    S: Service<
            RequestPacket,
            Response = ResponsePacket,
            Future = TransportFut<'static>,
            Error = TransportError,
        > + Clone
        + Send
        + 'static,
{
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        match req {
            RequestPacket::Batch(_) => {
                debug!(
                    batch_size = req.len(),
                    "Forwarding batch request directly to inner service"
                );
                self.service.clone().call(req)
            }
            RequestPacket::Single(request) => {
                let id = request.id().clone();
                let method = request.method().to_string();
                let (tx, rx) = tokio::sync::oneshot::channel();

                {
                    let mut lock = self.pending_requests.lock().expect("Mutex poisoned");
                    lock.push(PendingRequest { request, tx });
                    debug!(
                        %id,
                        method,
                        queue_size = lock.len(),
                        "Queued request for batching"
                    );
                }

                self.ensure_background_task_spawned();

                Box::pin(async move {
                    let response = rx.await.map_err(|_| {
                        TransportErrorKind::custom_str(
                            "Batching layer: sender dropped without response",
                        )
                    })??;
                    Ok(ResponsePacket::Single(response))
                })
            }
        }
    }
}
