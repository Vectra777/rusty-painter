//! A small mutation fuzzer for the file readers (tests only): damages a
//! good file in many ways and reports any input that makes the reader
//! panic, or take far too long. The readers must turn every damaged file
//! into an error (or a document), never a crash.
//!
//! The `fuzz_*` tests are ignored (they take a while); run them with
//! `cargo test --release --lib fuzz_ -- --ignored --nocapture`, and
//! `RP_FUZZ_ROUNDS` for more rounds (default 3000 per format).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

/// A fixed-seed xorshift, so a failure can be found again.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// `seed` damaged a few ways at once.
fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut b = seed.to_vec();
    for _ in 0..1 + rng.below(4) {
        if b.is_empty() {
            b.push(0);
        }
        let at = rng.below(b.len());
        match rng.below(8) {
            // Flip a bit.
            0 => b[at] ^= 1 << rng.below(8),
            // A byte at an edge value.
            1 => b[at] = [0, 1, 0x7f, 0x80, 0xff][rng.below(5)],
            // A 32-bit number (either byte order) at an extreme.
            2 | 3 => {
                let v: u32 = [
                    0,
                    1,
                    0xffff_ffff,
                    0x7fff_ffff,
                    0x8000_0000,
                    1 << 20,
                    1 << 16,
                    0xffff,
                ][rng.below(8)];
                let bytes = if rng.below(2) == 0 {
                    v.to_be_bytes()
                } else {
                    v.to_le_bytes()
                };
                for (i, x) in bytes.iter().enumerate() {
                    if let Some(slot) = b.get_mut(at + i) {
                        *slot = *x;
                    }
                }
            }
            // Cut it short.
            4 => b.truncate(at),
            // Repeat a piece.
            5 => {
                let len = 1 + rng.below(64.min(b.len() - at));
                let piece = b[at..at + len].to_vec();
                let to = rng.below(b.len());
                b.splice(to..to, piece);
            }
            // Take a piece out.
            6 => {
                let len = 1 + rng.below(64.min(b.len() - at));
                b.drain(at..at + len);
            }
            // Random bytes.
            _ => {
                for i in 0..1 + rng.below(8) {
                    if let Some(slot) = b.get_mut(at + i) {
                        *slot = rng.next() as u8;
                    }
                }
            }
        }
    }
    b
}

/// Feed `read` `RP_FUZZ_ROUNDS` damaged versions of `seed`; panics (after
/// trying them all) listing the inputs that panicked or took over
/// `too_long`, saving the first of each under the temp dir.
pub(crate) fn fuzz(name: &str, seed: &[u8], too_long: Duration, read: impl Fn(&[u8])) {
    let rounds: usize = std::env::var("RP_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    // The reader must accept the good file quietly.
    read(seed);
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ name.len() as u64);
    let mut failures: Vec<String> = Vec::new();
    let mut slowest = Duration::ZERO;
    for round in 0..rounds {
        let input = mutate(seed, &mut rng);
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| read(&input)));
        let took = started.elapsed();
        slowest = slowest.max(took);
        let problem = match result {
            Err(e) => Some(
                e.downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "panic".into()),
            ),
            Ok(()) if took > too_long => Some(format!("took {took:?}")),
            Ok(()) => None,
        };
        if let Some(problem) = problem {
            let path = std::env::temp_dir().join(format!("rp-fuzz-{name}-{round}.bin"));
            let _ = std::fs::write(&path, &input);
            failures.push(format!("round {round}: {problem} ({})", path.display()));
        }
    }
    std::panic::set_hook(hook);
    eprintln!(
        "fuzz {name}: {rounds} rounds, slowest {slowest:?}, {} failures",
        failures.len()
    );
    // Distinct problems only.
    failures.sort_by_key(|f| f.split(": ").nth(1).map(str::to_owned));
    failures.dedup_by_key(|f| f.split(": ").nth(1).map(str::to_owned));
    assert!(failures.is_empty(), "{name}:\n{}", failures.join("\n"));
}
