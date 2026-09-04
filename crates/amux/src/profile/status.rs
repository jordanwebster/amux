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
                if let Some(reporter) = &self.update_reporter {
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
