use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

pub struct ScopeTimer {
    name: &'static str,
    start: Instant,
    enabled: bool,
}

impl ScopeTimer {
    pub fn new(name: &'static str) -> Self {
        // Toggle profiling here; when disabled this becomes a cheap no-op.
        const ENABLE: bool = false;
        Self {
            name,
            start: Instant::now(),
            enabled: ENABLE,
        }
    }
}

impl Drop for ScopeTimer {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        let elapsed = self.start.elapsed();
        let stats = STATS.get_or_init(Default::default);
        let mut stats = stats.lock().unwrap();
        let entry = stats.entry(self.name).or_insert_with(Stats::new);
        entry.update(elapsed);
        let _ = entry.avg();

        // Logging disabled to keep hot paths free of stdout overhead.
        let _ = (elapsed, entry); // keep stats updated without printing
    }
}

#[derive(Default)]
struct Stats {
    total: u128,
    count: u128,
    max: std::time::Duration,
    last: std::time::Duration,
}

impl Stats {
    fn new() -> Self {
        Self::default()
    }

    fn update(&mut self, elapsed: std::time::Duration) {
        self.total += elapsed.as_nanos();
        self.count += 1;
        if elapsed > self.max {
            self.max = elapsed;
        }
        self.last = elapsed;
    }

    fn avg(&self) -> std::time::Duration {
        if self.count == 0 {
            return std::time::Duration::ZERO;
        }
        std::time::Duration::from_nanos((self.total / self.count) as u64)
    }
}

static STATS: OnceLock<Mutex<HashMap<&'static str, Stats>>> = OnceLock::new();
