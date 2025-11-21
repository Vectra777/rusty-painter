use std::time::Instant;

pub struct ScopeTimer {
    name: &'static str,
    start: Instant,
}

impl ScopeTimer {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            start: Instant::now(),
        }
    }
}

impl Drop for ScopeTimer {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        println!("[TIMER] {} took {:?}", self.name, elapsed);
    }
}
