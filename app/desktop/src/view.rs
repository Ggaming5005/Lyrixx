//! The `View` the window draws: the engine's view plus whether the engine
//! runs, and the `lyrix://view` event that carries it.

use lyrix::view::EngineView;
use serde::Serialize;
use std::time::Duration;
use tokio::sync::watch;

/// The event the window listens to.
pub const VIEW_EVENT: &str = "lyrix://view";

/// The event is sent at most this often.
pub const VIEW_EVENT_EVERY: Duration = Duration::from_millis(100);

/// `View` in `CONTRACT.md`: [`EngineView`]'s fields with `running` and `error`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    /// False while the engine restarts, after it stopped, or when it could not start.
    pub running: bool,
    /// Why the engine is not running, in plain words. `None` while it runs
    /// and while it restarts.
    pub error: Option<String>,
    #[serde(flatten)]
    pub engine: EngineView,
}

impl View {
    /// The view while no engine runs: nothing playing, no targets, and the
    /// pause state of the marker file.
    pub fn stopped(error: Option<String>, paused: bool) -> Self {
        Self {
            running: false,
            error,
            engine: EngineView {
                paused,
                ..EngineView::default()
            },
        }
    }
}

/// Calls `emit` with the newest value each time `views` changes, at most once
/// per `every`: the first change goes out at once, and the changes during the
/// pause that follows go out together, as the newest value, when it ends.
/// Nothing queues up. Returns once the sender is gone and the last change
/// went out.
pub async fn forward_throttled<T, F>(mut views: watch::Receiver<T>, every: Duration, mut emit: F)
where
    F: FnMut(&T),
{
    while views.changed().await.is_ok() {
        {
            let view = views.borrow_and_update();
            emit(&view);
        }
        tokio::time::sleep(every).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lyrix::view::{TargetState, TargetView};
    use std::sync::{Arc, Mutex};
    use tokio::time::Instant;

    #[test]
    fn serializes_running_and_error_next_to_the_engine_fields() {
        let view = View {
            running: true,
            error: None,
            engine: EngineView {
                source: "mpris".into(),
                paused: true,
                now: None,
                status: None,
                targets: vec![TargetView {
                    id: "discord".into(),
                    state: TargetState::Waiting,
                    detail: Some("Discord is not running".into()),
                }],
            },
        };
        assert_eq!(
            serde_json::to_value(&view).unwrap(),
            serde_json::json!({
                "running": true,
                "error": null,
                "source": "mpris",
                "paused": true,
                "now": null,
                "status": null,
                "targets": [{"id": "discord", "state": "waiting", "detail": "Discord is not running"}],
            })
        );
    }

    #[test]
    fn a_stopped_view_has_nothing_but_the_pause_state_and_the_error() {
        let view = View::stopped(Some("broken".into()), true);
        assert!(!view.running);
        assert_eq!(view.error.as_deref(), Some("broken"));
        assert!(view.engine.paused);
        assert_eq!(view.engine.source, "");
        assert!(view.engine.now.is_none());
        assert!(view.engine.targets.is_empty());
    }

    /// What was emitted: (ms since the start, value).
    type Seen = Arc<Mutex<Vec<(u64, u32)>>>;

    /// Records what was emitted and when (ms since `start`).
    fn recorder(start: Instant) -> (Seen, impl FnMut(&u32)) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let emit = move |value: &u32| {
            let at = Instant::now().duration_since(start).as_millis() as u64;
            sink.lock().unwrap().push((at, *value));
        };
        (seen, emit)
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_change_goes_out_at_once_and_a_burst_is_merged() {
        let start = Instant::now();
        let (tx, rx) = watch::channel(0u32);
        let (seen, emit) = recorder(start);
        let task = tokio::spawn(forward_throttled(rx, Duration::from_millis(100), emit));

        tx.send(1).unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        for value in 2..=20 {
            tx.send(value).unwrap();
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
        // 10 + 19 * 3 = 67 ms: the burst after the first change waits for 100 ms.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(*seen.lock().unwrap(), vec![(0, 1), (100, 20)]);

        drop(tx);
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_goes_out_without_a_change() {
        let start = Instant::now();
        let (tx, rx) = watch::channel(0u32);
        let (seen, emit) = recorder(start);
        let task = tokio::spawn(forward_throttled(rx, Duration::from_millis(100), emit));

        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(seen.lock().unwrap().is_empty());
        tx.send(7).unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(*seen.lock().unwrap(), vec![(500, 7)]);

        drop(tx);
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn changes_spread_out_go_out_one_by_one_at_most_every_interval() {
        let start = Instant::now();
        let (tx, rx) = watch::channel(0u32);
        let (seen, emit) = recorder(start);
        let task = tokio::spawn(forward_throttled(rx, Duration::from_millis(100), emit));

        for value in 1..=3 {
            tx.send(value).unwrap();
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        assert_eq!(*seen.lock().unwrap(), vec![(0, 1), (150, 2), (300, 3)]);

        // Faster than the interval: one every 100 ms, never a queue.
        for value in 4..=9 {
            tx.send(value).unwrap();
            tokio::time::sleep(Duration::from_millis(35)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let late: Vec<(u64, u32)> = seen.lock().unwrap()[3..].to_vec();
        // Sent at 450 (4), 485 (5), 520 (6), 555 (7), 590 (8) and 625 (9).
        assert_eq!(late, vec![(450, 4), (550, 6), (650, 9)]);

        drop(tx);
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn the_last_change_still_goes_out_when_the_sender_is_gone() {
        let start = Instant::now();
        let (tx, rx) = watch::channel(0u32);
        let (seen, emit) = recorder(start);
        let task = tokio::spawn(forward_throttled(rx, Duration::from_millis(100), emit));

        tx.send(1).unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        tx.send(2).unwrap();
        drop(tx);
        task.await.unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![(0, 1), (100, 2)]);
    }
}
