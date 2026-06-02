use crate::internal_prelude::*;

type Sender<T> = tokio::sync::oneshot::Sender<T>;

struct PendingRequest {
    request: SerializedRequest,
    tx: Sender<TransportResult<Response>>,
}

struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BatchingLayer {
    max_batch_size: usize,
    batching_duration: Duration,
}

impl BatchingLayer {
    pub fn new() -> Self {
        Self {
            max_batch_size: 100,
            batching_duration: Duration::from_millis(200),
        }
    }

    pub fn with_max_batch_size(mut self, max_batch_size: impl Into<usize>) -> Self {
        self.max_batch_size = max_batch_size.into();
        self
    }

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

#[derive(Clone)]
pub struct BatchingService<S> {
    service: S,
    max_batch_size: usize,
    batching_duration: Duration,
    pending_requests: Arc<StdMutex<Vec<PendingRequest>>>,
    abort_on_drop: Arc<OnceLock<AbortOnDrop>>,
}

impl<S> BatchingService<S>
where
    S: Service<RequestPacket, Future = TransportFut<'static>, Error = TransportError>
        + Send
        + Clone
        + 'static,
{
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

                    let mut chunk_futures = Vec::new();
                    while !remaining.is_empty() {
                        let chunk_end = remaining.len().min(max_batch_size);
                        let chunk = remaining.drain(..chunk_end).collect::<Vec<_>>();
                        let mut service = service.clone();

                        chunk_futures.push(async move {
                            send_chunk(&mut service, chunk).await;
                        });
                    }

                    futures::future::join_all(chunk_futures).await;
                }
            });

            AbortOnDrop(handle.abort_handle())
        });
    }
}

async fn send_chunk<S>(service: &mut S, chunk: Vec<PendingRequest>)
where
    S: Service<RequestPacket, Future = TransportFut<'static>, Error = TransportError>,
{
    let chunk_size = chunk.len();
    let mut senders = HashMap::<Id, Sender<TransportResult<Response>>>::with_capacity(chunk_size);
    let mut serialized_requests = Vec::<SerializedRequest>::with_capacity(chunk_size);

    for pending in chunk {
        let id = pending.request.id().clone();
        serialized_requests.push(pending.request);
        senders.insert(id, pending.tx);
    }

    let packet = serialized_requests.into_iter().collect::<RequestPacket>();

    debug!(chunk_size, "Sending batch request");

    match service.call(packet).await {
        Ok(response_packet) => {
            let responses = match response_packet {
                ResponsePacket::Single(response) => vec![response],
                ResponsePacket::Batch(responses) => responses,
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
        Err(err) => {
            debug!(chunk_size, error = %err, "Batch request failed, notifying all senders");
            for (_, tx) in senders {
                let _ = tx.send(Err(TransportErrorKind::custom_str(&format!(
                    "Batch request failed: {err}"
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
