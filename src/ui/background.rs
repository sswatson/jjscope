use std::sync::mpsc::{self, Receiver, TryRecvError};

/// One read in flight. Callers keep the desired request in their own state,
/// so rapid navigation never queues work for every intermediate selection.
/// Dropping this task detaches the read; it never waits on the UI thread.
pub struct BackgroundRead<K, V> {
    running: Option<(K, Receiver<V>)>,
}

impl<K: PartialEq, V: Send + 'static> BackgroundRead<K, V> {
    pub fn new() -> Self {
        Self { running: None }
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn start(&mut self, key: K, read: impl FnOnce() -> V + Send + 'static) {
        if self.is_running() {
            return;
        }
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = tx.send(read());
        });
        self.running = Some((key, rx));
    }

    /// Discard results for an obsolete selection or cache generation.
    pub fn poll(&mut self, wanted: &K) -> Option<V> {
        let (key, rx) = self.running.as_ref()?;
        match rx.try_recv() {
            Ok(value) => {
                let current = key == wanted;
                self.running = None;
                current.then_some(value)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.running = None;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_reads_do_not_block_and_intermediate_requests_are_skipped() {
        let mut task = BackgroundRead::new();
        let (release, wait) = mpsc::channel();
        task.start(1, move || wait.recv().unwrap());
        assert_eq!(task.poll(&1), None);
        task.start(2, || panic!("must not queue an intermediate read"));
        assert_eq!(task.poll(&3), None);
        release.send(42).unwrap();
        // Even while the old request finishes, the newer selection rejects it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while task.is_running() {
            assert_eq!(task.poll(&3), None);
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!task.is_running());
        let (tx, rx) = mpsc::channel();
        tx.send(99).unwrap();
        task.running = Some((3, rx));
        assert_eq!(task.poll(&3), Some(99));
    }

    #[test]
    fn invalidation_rejects_a_result_for_the_same_selection() {
        let mut task = BackgroundRead::new();
        let (tx, rx) = mpsc::channel();
        tx.send("old config").unwrap();
        task.running = Some((("commit", 0), rx));
        assert_eq!(task.poll(&("commit", 1)), None);
        assert!(!task.is_running());
    }
}
