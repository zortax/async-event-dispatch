use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use deadqueue::{limited, unlimited};
use tokio::sync::oneshot::Sender;
use tokio::sync::{oneshot, RwLock};

enum Queue<T> {
  Bounded(limited::Queue<T>),
  Unbounded(unlimited::Queue<T>),
}

enum DispatcherType {
  Bounded(usize),
  Unbounded,
}

enum Event<T> {
  Item(T),
  ItemWait(T, Sender<()>),
  Close,
}

impl<T> Queue<T> {
  pub fn push(&self, item: T) {
    match self {
      Queue::Bounded(queue) => {
        let _ignored = queue.try_push(item);
      }
      Queue::Unbounded(queue) => {
        queue.push(item);
      }
    }
  }

  pub async fn pop(&self) -> T {
    match self {
      Queue::Bounded(queue) => queue.pop().await,
      Queue::Unbounded(queue) => queue.pop().await,
    }
  }
}

pub struct Subscriber<T> {
  queue: Arc<Queue<Event<T>>>,
  ref_count: Arc<AtomicUsize>,
}

impl<T> Subscriber<T> {
  pub async fn next(&self) -> Option<T> {
    match self.queue.pop().await {
      Event::Item(item) => Some(item),
      Event::ItemWait(item, responder) => {
        let _ignored = responder.send(());
        Some(item)
      }
      Event::Close => None,
    }
  }
}

impl<T> Clone for Subscriber<T> {
  fn clone(&self) -> Self {
    let _ignored = self.ref_count.fetch_add(1, Ordering::Relaxed);
    Self {
      queue: self.queue.clone(),
      ref_count: self.ref_count.clone(),
    }
  }
}

impl<T> Drop for Subscriber<T> {
  fn drop(&mut self) {
    let _ignored = self.ref_count.fetch_sub(1, Ordering::Relaxed);
  }
}

pub struct Dispatcher<T: Send + 'static> {
  dispatcher_type: DispatcherType,
  subscribers: Arc<RwLock<Vec<Subscriber<T>>>>,
}

impl<T: Clone + Send + 'static> Dispatcher<T> {
  pub fn new() -> Self {
    Self {
      dispatcher_type: DispatcherType::Unbounded,
      subscribers: Arc::new(RwLock::new(Vec::new())),
    }
  }

  pub fn new_bounded(limit: usize) -> Self {
    Self {
      dispatcher_type: DispatcherType::Bounded(limit),
      subscribers: Arc::new(RwLock::new(Vec::new())),
    }
  }

  pub async fn cleanup(&self) {
    let mut subscribers = self.subscribers.write().await;
    subscribers
      .retain(|subscriber| subscriber.ref_count.load(Ordering::Relaxed) > 1)
  }

  pub async fn dispatch(&self, event: T) {
    let subscribers = self.subscribers.read().await;
    let mut cleanup = false;
    for subscriber in subscribers.iter() {
      if subscriber.ref_count.load(Ordering::Relaxed) > 1 {
        subscriber.queue.push(Event::Item(event.clone()));
      } else {
        cleanup = true;
      }
    }
    if cleanup {
      drop(subscribers);
      self.cleanup().await;
    }
  }

  pub async fn dispatch_wait(&self, event: T) {
    let subscribers = self.subscribers.read().await;
    let mut receivers = Vec::new();
    let mut cleanup = false;
    for subscriber in subscribers.iter() {
      if subscriber.ref_count.load(Ordering::Relaxed) > 1 {
        let (responder, receiver) = oneshot::channel();
        receivers.push(receiver);
        subscriber
          .queue
          .push(Event::ItemWait(event.clone(), responder));
      } else {
        cleanup = true;
      }
    }

    if cleanup {
      drop(subscribers);
      self.cleanup().await;
    }

    for fut in receivers.into_iter() {
      let _ignored = fut.await;
    }
  }

  pub async fn subscriber_count(&self) -> usize {
    self.subscribers.read().await.len()
  }

  pub async fn subscribe(&self) -> Subscriber<T> {
    let subscriber = match self.dispatcher_type {
      DispatcherType::Bounded(limit) => Subscriber {
        queue: Arc::new(Queue::Bounded(limited::Queue::new(limit))),
        ref_count: Arc::new(AtomicUsize::new(1)),
      },
      DispatcherType::Unbounded => Subscriber {
        queue: Arc::new(Queue::Unbounded(unlimited::Queue::new())),
        ref_count: Arc::new(AtomicUsize::new(1)),
      },
    };
    let mut subscribers = self.subscribers.write().await;
    subscribers.push(subscriber.clone());
    subscriber
  }
}

impl<T: Clone + Send + 'static> Default for Dispatcher<T> {
  fn default() -> Self {
    Dispatcher::new()
  }
}

impl<T: Send + 'static> Drop for Dispatcher<T> {
  fn drop(&mut self) {
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
      let subscribers = self.subscribers.clone();
      rt.spawn(async move {
        for subscriber in subscribers.read().await.iter() {
          subscriber.queue.push(Event::Close);
        }
      });
    } else {
      // blocking_read() is deadlock-safe as we do not hand out
      // any guards and there aren't any references anymore
      for subscriber in self.subscribers.blocking_read().iter() {
        subscriber.queue.push(Event::Close);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use tokio::sync::Mutex;

  use crate::Dispatcher;

  #[tokio::test]
  async fn it_works() {
    let dispatcher = Dispatcher::<i32>::new();
    let subscriber = dispatcher.subscribe().await;
    let subscriber2 = dispatcher.subscribe().await;
    let read = Arc::new(Mutex::new(false));
    let handle = tokio::spawn({
      let read = read.clone();
      async move {
        assert_eq!(subscriber.next().await, Some(42));
        assert_eq!(subscriber.next().await, Some(69));
        *read.lock().await = true;
      }
    });
    dispatcher.dispatch(42).await;
    dispatcher.dispatch(69).await;
    assert!(handle.await.is_ok());
    assert!(*read.lock().await);
    // no slow receiver issues
    assert_eq!(subscriber2.next().await, Some(42));
    assert_eq!(subscriber2.next().await, Some(69));

    drop(dispatcher);
    assert_eq!(subscriber2.next().await, None);
  }
}
