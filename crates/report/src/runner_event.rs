//! The types associated with the events sent by the runner to the reporter.

use std::path::PathBuf;

use revive_dt_format::corpus::Corpus;
use revive_dt_format::metadata::Metadata;
use tokio::sync::{broadcast, oneshot};

use crate::ReporterEvent;

macro_rules! keep_if_doc {
    (#[doc = $doc:expr]) => {
        #[doc = $doc]
    };
    ( $($_:tt)* ) => {};
}

/// Defines the runner-event which is sent from the test runners to the report aggregator.
///
/// This macro defines a number of things related to the reporting infrastructure and the interface
/// used. First of all, it defines the enum of all of the possible events that the runners can send
/// to the aggregator. For each one of the variants it defines a separate struct for it to allow the
/// variant field in the enum to be put in a [`Box`].
///
/// In addition to the above, it defines [`From`] implementations for the various event types for
/// the [`RunnerEvent`] enum essentially allowing for events such as [`CorpusFileDiscoveryEvent`] to
/// be converted into a [`RunnerEvent`].
///
/// In addition to the above, it also defines the [`RunnerEventReporter`] which is a wrapper around
/// an [`UnboundedSender`] allowing for events to be sent to the report aggregator.
///
/// With the above description, we can see that this macro defines almost all of the interface of
/// the reporting infrastructure, from the enum itself, to its associated types, and also to the
/// reporter that's used to report events to the aggregator.
///
/// [`UnboundedSender`]: tokio::sync::mpsc::UnboundedSender
macro_rules! define_event {
    (
        $(#[$enum_meta: meta])*
        $vis: vis enum $ident: ident {
            $(
                $(#[$variant_meta: meta])*
                $variant_ident: ident {
                    $(
                        $(#[$field_meta: meta])*
                        $field_ident: ident: $field_ty: ty
                    ),* $(,)?
                }
            ),* $(,)?
        }
    ) => {
        paste::paste! {
            $(#[$enum_meta])*
            $vis enum $ident {
                $(
                    $(#[$variant_meta])*
                    $variant_ident(Box<[<$variant_ident Event>]>)
                ),*
            }

            $(
                $(#[$variant_meta])*
                $vis struct [<$variant_ident Event>] {
                    $(
                        $(#[$field_meta])*
                        $vis $field_ident: $field_ty
                    ),*
                }
            )*

            $(
                impl From<[<$variant_ident Event>]> for $ident {
                    fn from(value: [<$variant_ident Event>]) -> Self {
                        Self::$variant_ident(Box::new(value))
                    }
                }
            )*

            /// Provides a way to report events to the aggregator.
            ///
            /// Under the hood, this is a wrapper around an [`UnboundedSender`] which abstracts away
            /// the fact that channels are used and that implements high-level methods for reporting
            /// various events to the aggregator.
            #[derive(Clone, Debug)]
            pub struct [< $ident Reporter >]($vis tokio::sync::mpsc::UnboundedSender<$ident>);

            impl From<tokio::sync::mpsc::UnboundedSender<$ident>> for [< $ident Reporter >] {
                fn from(value: tokio::sync::mpsc::UnboundedSender<$ident>) -> Self {
                    Self(value)
                }
            }

            impl [< $ident Reporter >] {
                fn report(&self, event: impl Into<$ident>) -> anyhow::Result<()> {
                    self.0.send(event.into()).map_err(Into::into)
                }

                $(
                    keep_if_doc!($(#[$variant_meta])*);
                    pub fn [< report_ $variant_ident:snake _event >](&self, $($field_ident: impl Into<$field_ty>),*) -> anyhow::Result<()> {
                        self.report([< $variant_ident Event >] {
                            $($field_ident: $field_ident.into()),*
                        })
                    }
                )*
            }
        }
    };
}

define_event! {
    /// An event type that's sent by the test runners/drivers to the report aggregator.
    pub(crate) enum RunnerEvent {
        /// An event emitted by the reporter when it wishes to listen to events emitted by the
        /// aggregator.
        SubscribeToEvents {
            /// The channel that the aggregator is to send the receive side of the channel on.
            tx: oneshot::Sender<broadcast::Receiver<ReporterEvent>>
        },
        /// An event emitted by runners when they've discovered a corpus file.
        CorpusFileDiscovery {
            /// The contents of the corpus file.
            corpus: Corpus
        },
        /// An event emitted by runners when they've discovered a metadata file.
        MetadataFileDiscovery {
            /// The path of the metadata file discovered.
            path: PathBuf,
            /// The content of the metadata file.
            metadata: Metadata
        },
        /// An event emitted by the runners when the execution is completed and the aggregator can
        /// stop.
        ExecutionCompleted {}
    }
}

/// An extension to the [`Reporter`] implemented by the macro.
impl RunnerEventReporter {
    pub async fn subscribe(&self) -> anyhow::Result<broadcast::Receiver<ReporterEvent>> {
        let (tx, rx) = oneshot::channel::<broadcast::Receiver<ReporterEvent>>();
        self.report_subscribe_to_events_event(tx)?;
        rx.await.map_err(Into::into)
    }
}

pub type Reporter = RunnerEventReporter;
