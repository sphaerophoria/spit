use std::time::{Duration, Instant};

pub(crate) struct Timer {
    start: Instant,
}

impl Timer {
    pub(crate) fn new() -> Timer {
        Timer {
            start: Instant::now(),
        }
    }

    pub(crate) fn elapsed(&self) -> Duration {
        Instant::now() - self.start
    }

    pub(crate) fn reset(&mut self) {
        self.start = Instant::now();
    }
}
