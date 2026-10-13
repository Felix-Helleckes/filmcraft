//! Mutation fuzzing for parsers of untrusted files (AGENTS.md §0.8): deterministic mutations of
//! valid seed documents, each run under `catch_unwind`, with distinct panics collected and their
//! inputs saved so they can become regression tests.
//!
//! [`mutate`] knows bytes and text: besides flips, overwrites, truncation and splices it replaces
//! numbers with extremes (overflow, zero divisors, NaN), drops multi-byte characters into the middle
//! of tokens (byte-index string slicing) and duplicates or drops whole lines.
//!
//! ```ignore
//! let found = filmcraft_testkit::fuzz::run("srt", &seeds, 2000, |bytes| { let _ = parse(bytes); });
//! filmcraft_testkit::fuzz::assert_clean("srt", &found);
//! ```
//! `FILMCRAFT_FUZZ_ITERS` overrides the iteration count of every [`run`]. A single input that runs
//! longer than `FILMCRAFT_FUZZ_CASE_SECS` (default 10) is a hang: a watchdog saves it to
//! `target/fuzz-findings/<name>-hang.bin` and aborts, rather than letting an endless loop or an
//! unbounded allocation take the machine down.

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, PoisonError};
use std::time::{Duration, Instant};

/// Xorshift64: deterministic, so a finding reproduces from the seed alone.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        if self.0 == 0 {
            self.0 = 0x9e37_79b9_7f4a_7c15;
        }
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform-ish in `0..n` (0 when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}

/// Numbers that break arithmetic: overflow, zero, negative, non-finite, out-of-range timecodes.
const EXTREMES: &[&str] = &[
    "0",
    "-1",
    "-0",
    "99999999999999999999",
    "18446744073709551615",
    "9223372036854775807",
    "-9223372036854775808",
    "4294967296",
    "1e308",
    "-1e308",
    "NaN",
    "inf",
    "0.0000001",
    "00:00:00:00",
    "99:99:99:99",
    "23:59:59:29",
];

/// Characters that break byte-indexed string slicing, escaping and line handling.
const ODD: &[&str] =
    &["é", "日本語", "🎬", "\u{feff}", "\u{0}", "\u{301}", "\\", "\"", "'", "<", ">", "&", "/", ":", ";", ",", "\n", "\r\n", "\t", "%", "#", "*"];

/// One to three random mutations of `seed`.
pub fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut d = seed.to_vec();
    for _ in 0..1 + rng.below(3) {
        if d.is_empty() {
            d.extend_from_slice(seed);
            if d.is_empty() {
                d.push(b'0');
            }
        }
        match rng.below(10) {
            0 => {
                for _ in 0..1 + rng.below(8) {
                    let p = rng.below(d.len());
                    d[p] ^= 1 << rng.below(8);
                }
            }
            1 => {
                for _ in 0..1 + rng.below(4) {
                    let p = rng.below(d.len());
                    d[p] = rng.next_u64() as u8;
                }
            }
            2 => {
                let n = rng.below(d.len());
                d.truncate(n);
            }
            3 if !seed.is_empty() => {
                // a chunk of the seed again, somewhere else: repeated or misplaced structure
                let a = rng.below(seed.len());
                let b = (a + 1 + rng.below(200)).min(seed.len());
                let at = rng.below(d.len() + 1);
                d.splice(at..at, seed[a..b].iter().copied());
            }
            4 => {
                let a = rng.below(d.len());
                let b = (a + 1 + rng.below(64)).min(d.len());
                d.drain(a..b);
            }
            5 | 6 => {
                // a number replaced by an extreme one
                let starts: Vec<usize> = (0..d.len()).filter(|&i| d[i].is_ascii_digit() && (i == 0 || !d[i - 1].is_ascii_digit())).collect();
                if starts.is_empty() {
                    continue;
                }
                let s = starts[rng.below(starts.len())];
                let e = (s..d.len()).find(|&i| !d[i].is_ascii_digit()).unwrap_or(d.len());
                d.splice(s..e, EXTREMES[rng.below(EXTREMES.len())].bytes());
            }
            7 => {
                let at = rng.below(d.len() + 1);
                d.splice(at..at, ODD[rng.below(ODD.len())].bytes());
            }
            8 => {
                // a whole line duplicated or dropped
                let starts: Vec<usize> = std::iter::once(0).chain((0..d.len()).filter(|&i| d[i] == b'\n').map(|i| i + 1)).filter(|&i| i < d.len()).collect();
                if starts.is_empty() {
                    continue;
                }
                let a = starts[rng.below(starts.len())];
                let b = (a..d.len()).find(|&i| d[i] == b'\n').map_or(d.len(), |i| i + 1);
                if rng.below(2) == 0 {
                    let line = d[a..b].to_vec();
                    d.splice(b..b, line);
                } else {
                    d.drain(a..b);
                }
            }
            _ => {
                let n = rng.below(64);
                d.extend((0..n).map(|_| rng.next_u64() as u8));
            }
        }
    }
    d
}

/// A distinct panic: where and what, and an input that triggers it.
#[derive(Clone, Debug)]
pub struct Finding {
    pub message: String,
    pub input: Vec<u8>,
}

thread_local! {
    static CAPTURE: Cell<bool> = const { Cell::new(false) };
    static LAST: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Install (once per process) a panic hook that records panics on fuzzing threads and passes every
/// other panic to the previous hook, so tests running in parallel keep their normal output.
fn install_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if CAPTURE.with(Cell::get) {
                let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
                let payload = info.payload();
                let msg = payload.downcast_ref::<&str>().map(|s| (*s).to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_default();
                LAST.with(|l| *l.borrow_mut() = format!("{at}: {msg}"));
            } else {
                previous(info);
            }
        }));
    });
}

/// Run `check` on each seed and on `iters` mutations per seed (or `FILMCRAFT_FUZZ_ITERS`), returning
/// the distinct panics (by location and message). `name` labels hang reports and varies the
/// mutation stream. The seeds themselves must not panic: a seed that does is reported like any
/// finding.
pub fn run(name: &str, seeds: &[Vec<u8>], iters: usize, mut check: impl FnMut(&[u8])) -> Vec<Finding> {
    install_hook();
    let iters = std::env::var("FILMCRAFT_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(iters);
    let limit = std::env::var("FILMCRAFT_FUZZ_CASE_SECS").ok().and_then(|v| v.parse().ok()).map_or(Duration::from_secs(10), Duration::from_secs_f64);
    let salt = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    let current: Arc<Mutex<(Instant, Vec<u8>)>> = Arc::new(Mutex::new((Instant::now(), Vec::new())));
    let done = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let (current, done, hang) = (Arc::clone(&current), Arc::clone(&done), format!("{name}-hang.bin"));
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
                let (since, input) = {
                    let c = current.lock().unwrap_or_else(PoisonError::into_inner);
                    (c.0, if c.0.elapsed() > limit { c.1.clone() } else { Vec::new() })
                };
                if since.elapsed() > limit && !done.load(Ordering::Relaxed) {
                    let path = findings_dir().join(hang);
                    let _ = std::fs::write(&path, &input);
                    eprintln!("fuzz: one input ran longer than {limit:?} (a hang or an unbounded allocation); saved to {}", path.display());
                    std::process::abort();
                }
            }
        })
    };
    let mut found: Vec<Finding> = Vec::new();
    CAPTURE.with(|c| c.set(true));
    let mut one = |input: Vec<u8>, found: &mut Vec<Finding>| {
        *current.lock().unwrap_or_else(PoisonError::into_inner) = (Instant::now(), input.clone());
        if catch_unwind(AssertUnwindSafe(|| check(&input))).is_err() {
            let message = LAST.with(|l| l.borrow().clone());
            if !found.iter().any(|f| f.message == message) {
                found.push(Finding { message, input });
            }
        }
    };
    for (si, seed) in seeds.iter().enumerate() {
        one(seed.clone(), &mut found);
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ salt ^ (si as u64 + 1).wrapping_mul(0xbf58_476d_1ce4_e5b9));
        for _ in 0..iters {
            let input = mutate(&mut rng, seed);
            one(input, &mut found);
        }
    }
    CAPTURE.with(|c| c.set(false));
    done.store(true, Ordering::Relaxed);
    let _ = watchdog.join();
    found
}

fn findings_dir() -> PathBuf {
    let dir = crate::workspace_root().join("target").join("fuzz-findings");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Fail with every finding listed; each input is written to `<target>/fuzz-findings/<name>-<n>.bin`
/// so it can be turned into a regression test.
pub fn assert_clean(name: &str, found: &[Finding]) {
    if found.is_empty() {
        return;
    }
    let dir = findings_dir();
    let mut report = String::new();
    for (i, f) in found.iter().enumerate() {
        let path: PathBuf = dir.join(format!("{name}-{i}.bin"));
        let saved = std::fs::write(&path, &f.input).is_ok();
        report.push_str(&format!(
            "\n  {}\n    input: {}",
            f.message,
            if saved { path.display().to_string() } else { format!("{:?}", String::from_utf8_lossy(&f.input)) }
        ));
    }
    panic!("{name}: {} distinct panic(s) on mutated input:{report}", found.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutations_are_deterministic_and_vary() {
        let seed = b"001  AX  V  C  01:00:00:00 01:00:01:00\n* FROM CLIP NAME: a.mov\n".to_vec();
        let (mut a, mut b) = (Rng(7), Rng(7));
        let xs: Vec<Vec<u8>> = (0..50).map(|_| mutate(&mut a, &seed)).collect();
        let ys: Vec<Vec<u8>> = (0..50).map(|_| mutate(&mut b, &seed)).collect();
        assert_eq!(xs, ys);
        assert!(xs.iter().filter(|x| **x != seed).count() > 40);
        assert!(mutate(&mut Rng(1), b"").len() < 100, "an empty seed is fine");
    }

    #[test]
    fn run_reports_distinct_panics_and_keeps_inputs() {
        let seeds = vec![b"10 20 30".to_vec()];
        let found = run("unit", &seeds, 300, |b| {
            let s = String::from_utf8_lossy(b);
            if s.contains("99999999999999999999") {
                panic!("too big");
            }
        });
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.ends_with("too big"));
        assert!(String::from_utf8_lossy(&found[0].input).contains("99999999999999999999"));
        assert!(run("unit", &seeds, 50, |_| {}).is_empty());
    }
}
