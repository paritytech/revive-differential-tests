/// The [`Future`] type used by the framework for all functions which return a
/// future.
#[macro_export]
macro_rules! framework_future {
    ($ty: ty) => {
        std::pin::Pin<std::boxed::Box<dyn std::future::Future<Output = $ty> + Send + 'static>>
    };
}

/// The [`Stream`] type used by the framework for all functions which return a
/// stream.
///
/// [`Stream`]: futures::Stream
#[macro_export]
macro_rules! framework_stream {
    ($ty: ty) => {
        std::pin::Pin<std::boxed::Box<dyn ::futures::Stream<Item = $ty> + Send + 'static>>
    };
}

pub use {framework_future, framework_stream};
