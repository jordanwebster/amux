//! Observed runtime state and its update-status adapter.

use std::sync::Arc;

pub use model::RelayCarrier;
use tokio::sync::watch;

use crate::update::{UpdateReporter, UpdateStatus};

/// Connectivity observed by startup and the cloud connector, apart from intent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Observed {
    Local,
    Connecting,
    Connected {
        tier: crate::Tier,
        carrier: RelayCarrier,
    },
    Retrying,
    AuthenticationRequired,
    UpdateRequired {
        minimum_version: Option<String>,
    },
    StartupFailed,
}

/// The publisher can outlive a failed start so its owner can retain the error
/// state even when there is no runtime to query.
#[derive(Clone)]
pub(crate) struct RuntimeStatus {
    tx: watch::Sender<Observed>,
    observer: Option<Arc<dyn Fn(Observed) + Send + Sync>>,
    update_reporter: Option<Arc<dyn UpdateReporter>>,
}

impl RuntimeStatus {
    pub(crate) fn new(update_reporter: Option<Arc<dyn UpdateReporter>>) -> Self {
        let (tx, _) = watch::channel(Observed::Local);
        Self {
            tx,
            observer: None,
            update_reporter,
        }
    }

    pub(crate) fn with_observer(
        mut self,
        observer: impl Fn(Observed) + Send + Sync + 'static,
    ) -> Self {
        self.observer = Some(Arc::new(observer));
        self
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<Observed> {
        self.tx.subscribe()
    }

    pub(crate) fn report(&self, observed: Observed) {
        // Retain state even when no screen or supervisor is currently watching.
        self.tx.send_replace(observed.clone());
        if let Some(observer) = &self.observer {
            observer(observed.clone());
        }
        // Adapt synchronously: a terminal connector can finish immediately
        // after publishing, and teardown must not discard its marker update.
        match observed {
            Observed::Local | Observed::Connected { .. } => {
                // Local operation says nothing about whether the cloud still
                // requires an update. Clear that marker only after connecting.
                if matches!(observed, Observed::Connected { .. })
                    && let Some(reporter) = &self.update_reporter
                {
                    reporter.report(UpdateStatus::Required(None));
                }
            }
            Observed::UpdateRequired {
                minimum_version: Some(version),
            } => {
                if let Some(reporter) = &self.update_reporter {
                    reporter.report(UpdateStatus::Required(Some(version)));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct CapturingReporter {
        updates: Mutex<Vec<UpdateStatus>>,
    }

    impl UpdateReporter for CapturingReporter {
        fn report(&self, status: UpdateStatus) {
            self.updates.lock().unwrap().push(status);
        }
    }

    #[test]
    fn profile_runtime_local_preserves_update_required_until_connected() {
        let reporter = Arc::new(CapturingReporter::default());
        let status = RuntimeStatus::new(Some(reporter.clone()));
        status.report(Observed::UpdateRequired {
            minimum_version: Some("99.0.0".into()),
        });
        status.report(Observed::Local);

        assert!(matches!(
            reporter.updates.lock().unwrap().as_slice(),
            [UpdateStatus::Required(Some(version))] if version == "99.0.0"
        ));
        assert_eq!(*status.subscribe().borrow(), Observed::Local);
        println!("Local: update-required remains");

        status.report(Observed::Connected {
            tier: crate::Tier::Pro,
            carrier: RelayCarrier::Tcp,
        });

        assert!(matches!(
            reporter.updates.lock().unwrap().as_slice(),
            [UpdateStatus::Required(Some(version)), UpdateStatus::Required(None)]
                if version == "99.0.0"
        ));
        assert!(matches!(
            *status.subscribe().borrow(),
            Observed::Connected {
                tier: crate::Tier::Pro,
                carrier: RelayCarrier::Tcp
            }
        ));
        println!("Connected: update-required clears");
    }
}
