## async-event-dispatch

---
A simple event dispatcher for Rust that is not susceptible to the slow-receiver problem.

```rust
#[derive(Clone)]
struct MyEvent(String);

let dispatcher = Dispatcher::<MyEvent>::new();
let subscriber = dispatcher.subscribe().await;

tokio::spawn(async move {
  while let Some(event) = subscriber.next().await {
    println!("Event: {}", event.0);
  }
})

dispatcher.dispatch(MyEvent("Hello World!".into())).await;
```

### Features

---
