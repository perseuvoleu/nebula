use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::oneshot;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Point {
    ReapDecided,
    ExitSeen,
    ExitRetired,
}

/// Holds daemon code at a `Point` until the test lets it go, so a race can be
/// replayed in one exact order.
#[derive(Default)]
pub struct Pauses(Mutex<HashMap<Point, Armed>>);

struct Armed {
    reached: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

pub struct Held {
    reached: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
}

impl Pauses {
    /// The next pass through `point` stops there.
    pub fn arm(&self, point: Point) -> Held {
        let (reached_tx, reached) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        self.0.lock().unwrap().insert(
            point,
            Armed {
                reached: reached_tx,
                release: release_rx,
            },
        );
        Held { reached, release }
    }

    fn take(&self, point: Point) -> Option<oneshot::Receiver<()>> {
        let armed = self.0.lock().unwrap().remove(&point)?;
        let _ = armed.reached.send(());
        Some(armed.release)
    }

    pub async fn at(&self, point: Point) {
        if let Some(release) = self.take(point) {
            let _ = release.await;
        }
    }

    pub fn at_blocking(&self, point: Point) {
        if let Some(release) = self.take(point) {
            let _ = release.blocking_recv();
        }
    }
}

impl Held {
    pub async fn reached(&mut self) {
        tokio::time::timeout(std::time::Duration::from_secs(10), &mut self.reached)
            .await
            .expect("the paused point was never reached")
            .expect("the pause was dropped");
    }

    pub fn release(self) {
        let _ = self.release.send(());
    }
}
