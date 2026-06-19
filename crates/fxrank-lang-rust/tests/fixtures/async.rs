// Fixture for Task 15: async_boundary, await_count, unresolved-await confidence.

/// Two awaits: async_boundary == true, await_count == 2.
pub async fn two_awaits(a: impl Future<Output = u32>, b: impl Future<Output = u32>) -> u32 {
    let x = a.await;
    let y = b.await;
    x + y
}

/// Async fn, no awaits: async_boundary == true, await_count == 0.
pub async fn async_no_await() -> u32 {
    42
}

/// Sync fn, no awaits: async_boundary == false, await_count == 0.
pub fn sync_no_await() -> u32 {
    1 + 1
}

/// Async fn with one await and no other effects → confidence must be 0.8.
pub async fn async_with_await(fut: impl Future<Output = ()>) {
    fut.await;
}

/// Async fn with an heuristic effect (heuristic confidence 0.6) AND one await.
/// Min(0.6, 0.8) == 0.6; this is the "both" scenario — confidence stays 0.6.
pub async fn async_with_heuristic_and_await(c: std::sync::mpsc::Sender<u32>, fut: impl Future<Output = ()>) {
    c.send(42).unwrap();
    fut.await;
}

/// Sync fn with one heuristic effect → confidence == 0.6 (no await penalty).
pub fn sync_heuristic_only(c: std::sync::mpsc::Sender<u32>) {
    c.send(42).unwrap();
}
