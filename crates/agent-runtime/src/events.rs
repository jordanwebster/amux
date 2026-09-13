use tokio::sync::mpsc;

const DEFAULT_EVENT_BUFFER: usize = 256;

pub(crate) struct EventSource<E> {
    capacity: usize,
    subscribers: Vec<EventSubscriber<E>>,
}

struct EventSubscriber<E> {
    tx: mpsc::Sender<E>,
    drop_on_overflow: bool,
}

impl<E> Default for EventSource<E> {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_EVENT_BUFFER,
            subscribers: Vec::new(),
        }
    }
}

impl<E> EventSource<E> {
    pub(crate) fn subscribe(&mut self) -> mpsc::Receiver<E> {
        self.subscribe_with_policy(false)
    }

    pub(crate) fn subscribe_drop_on_overflow(&mut self) -> mpsc::Receiver<E> {
        self.subscribe_with_policy(true)
    }

    fn subscribe_with_policy(&mut self, drop_on_overflow: bool) -> mpsc::Receiver<E> {
        let (tx, rx) = mpsc::channel(self.capacity);
        self.subscribers.push(EventSubscriber {
            tx,
            drop_on_overflow,
        });
        rx
    }
}

impl<E: Clone> EventSource<E> {
    pub(crate) fn emit(&mut self, event: E) {
        self.subscribers
            .retain(|subscriber| match subscriber.tx.try_send(event.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) if !subscriber.drop_on_overflow => {
                    panic!("critical event subscriber queue full")
                }
                Err(_) => false,
            });
    }
}
