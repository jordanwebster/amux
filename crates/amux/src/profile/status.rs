//! Observed runtime state and adapters for legacy status reporters.

use std::sync::Arc;

use tokio::sync::watch;

use crate::subscription::SubscriptionReporter;
use crate::update::{UpdateReporter, UpdateStatus};

/// Connectivity observed by startup and the cloud connector, apart from intent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Observed {
    Local,
    Connecting,
    Connected,
    Retrying,
    AuthenticationRequired,
    SubscriptionRequired,
    UpdateRequired { minimum_version: Option<String> },
    StartupFailed,
}

/// The publisher can outlive a failed start so its owner can retain the error
/// state even when there is no runtime to query.
#[derive(Clone)]
pub(crate) struct RuntimeStatus {
    tx: watch::Sender<Observed>,
    update_reporter: Option<Arc<dyn UpdateReporter>>,
    subscription_reporter: Option<Arc<dyn SubscriptionReporter>>,
}

impl RuntimeStatus {
    pub(crate) fn new(
        update_reporter: Option<Arc<dyn UpdateReporter>>,
        subscription_reporter: Option<Arc<dyn SubscriptionReporter>>,
    ) -> Self {
        let (tx, _) = watch::channel(Observed::Local);
        Self {
            tx,
            update_reporter,
            subscription_reporter,
        }
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<Observed> {
        self.tx.subscribe()
    }

    pub(crate) fn report(&self, observed: Observed) {
        // Retain state even when no screen or supervisor is currently watching.
        self.tx.send_replace(observed.clone());
        // Adapt synchronously: a terminal connector can finish immediately
        // after publishing, and teardown must not discard its marker update.
        match observed {
            Observed::Local | Observed::Connected => {
                // Local operation says nothing about whether the cloud still
                // requires an update. Clear that marker only after connecting.
                if observed == Observed::Connected
                    && let Some(reporter) = &self.update_reporter
                {
                    reporter.report(UpdateStatus::Required(None));
                }
                if let Some(reporter) = &self.subscription_reporter {
                    reporter.report_subscription_required(false);
                }
            }
            Observed::SubscriptionRequired => {
                if let Some(reporter) = &self.subscription_reporter {
                    reporter.report_subscription_required(true);
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
        subscriptions: Mutex<Vec<bool>>,
    }

    impl UpdateReporter for CapturingReporter {
        fn report(&self, status: UpdateStatus) {
            self.updates.lock().unwrap().push(status);
        }
    }

    impl SubscriptionReporter for CapturingReporter {
        fn report_subscription_required(&self, required: bool) {
            self.subscriptions.lock().unwrap().push(required);
        }
    }

    #[test]
    fn profile_runtime_local_preserves_update_required_until_connected() {
        let reporter = Arc::new(CapturingReporter::default());
        let status = RuntimeStatus::new(Some(reporter.clone()), Some(reporter.clone()));
        status.report(Observed::UpdateRequired {
            minimum_version: Some("99.0.0".into()),
        });
        status.report(Observed::SubscriptionRequired);
        status.report(Observed::Local);

        assert!(matches!(
            reporter.updates.lock().unwrap().as_slice(),
            [UpdateStatus::Required(Some(version))] if version == "99.0.0"
        ));
        assert_eq!(*reporter.subscriptions.lock().unwrap(), [true, false]);
        assert_eq!(*status.subscribe().borrow(), Observed::Local);
        println!("Local: update-required remains; subscription-required clears");

        status.report(Observed::SubscriptionRequired);
        status.report(Observed::Connected);

        assert!(matches!(
            reporter.updates.lock().unwrap().as_slice(),
            [UpdateStatus::Required(Some(version)), UpdateStatus::Required(None)]
                if version == "99.0.0"
        ));
        assert_eq!(
            *reporter.subscriptions.lock().unwrap(),
            [true, false, true, false]
        );
        assert_eq!(*status.subscribe().borrow(), Observed::Connected);
        println!("Connected: update-required and subscription-required clear");
    }
}
