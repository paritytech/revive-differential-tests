/// The [`Future`] type used by the framework for all functions which return a
/// future.
pub type FrameworkFuture<T> =
    std::pin::Pin<std::boxed::Box<dyn std::future::Future<Output = T> + Send + 'static>>;

/// The [`Stream`] type used by the framework for all functions which return a
/// stream.
///
/// [`Stream`]: futures::Stream
pub type FrameworkStream<T> =
    std::pin::Pin<std::boxed::Box<dyn futures::Stream<Item = T> + Send + 'static>>;
