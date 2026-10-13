use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Token bucket shared by every connection it throttles.
///
/// Callers take tokens before writing a chunk; when the bucket goes negative they
/// sleep off the debt, so several connections share the rate fairly without a
/// central scheduler.
pub struct RateLimiter {
    /// Bytes per second; 0 disables limiting.
    rate: AtomicU64,
    bucket: Mutex<Bucket>,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    pub fn new(rate: u64) -> Self {
        Self {
            rate: AtomicU64::new(rate),
            bucket: Mutex::new(Bucket {
                tokens: 0.0,
                last: Instant::now(),
            }),
        }
    }

    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    pub fn set_rate(&self, rate: u64) {
        self.rate.store(rate, Ordering::Relaxed);
        let mut bucket = self.bucket.lock();
        bucket.tokens = 0.0;
        bucket.last = Instant::now();
    }

    /// How long the caller must wait before using `bytes`.
    fn reserve(&self, bytes: usize) -> Option<Duration> {
        let rate = self.rate();
        if rate == 0 {
            return None;
        }
        let rate = rate as f64;
        let mut bucket = self.bucket.lock();
        let now = Instant::now();
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.last = now;
        // Allow at most a quarter second of burst so short pauses don't bank speed.
        bucket.tokens = (bucket.tokens + elapsed * rate).min(rate / 4.0);
        bucket.tokens -= bytes as f64;
        (bucket.tokens < 0.0).then(|| Duration::from_secs_f64(-bucket.tokens / rate))
    }

    pub async fn acquire(&self, bytes: usize) {
        if let Some(wait) = self.reserve(bytes) {
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn limits_throughput() {
        let limiter = RateLimiter::new(400_000);
        let start = Instant::now();
        for _ in 0..20 {
            limiter.acquire(20_000).await;
        }
        // 400 KB at 400 KB/s should take about a second.
        let elapsed = start.elapsed().as_secs_f64();
        assert!(elapsed > 0.8 && elapsed < 1.5, "took {elapsed}s");
    }

    #[tokio::test]
    async fn unlimited_is_free() {
        let limiter = RateLimiter::new(0);
        let start = Instant::now();
        for _ in 0..1000 {
            limiter.acquire(1 << 20).await;
        }
        assert!(start.elapsed() < Duration::from_millis(50));
    }
}
