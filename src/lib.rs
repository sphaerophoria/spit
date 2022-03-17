use std::time::{Duration, Instant};

pub mod app;
pub mod git;
pub mod gui;

struct Timer {
    start: Instant,
}

impl Timer {
    fn new() -> Timer {
        Timer {
            start: Instant::now(),
        }
    }

    fn elapsed(&self) -> Duration {
        Instant::now() - self.start
    }

    fn reset(&mut self) {
        self.start = Instant::now();
    }
}
