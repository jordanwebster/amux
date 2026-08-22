pub trait SubscriptionReporter: Send + Sync + 'static {
    fn report_subscription_required(&self, required: bool);
}
