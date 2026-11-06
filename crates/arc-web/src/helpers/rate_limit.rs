/// Rate limits identify the connected socket peer. Forwarding headers are
/// client-controlled unless a separately configured trusted proxy verifies them.
pub fn client_ip(req: &actix_web::HttpRequest) -> String {
    req.peer_addr()
        .map(|peer| peer.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

use std::collections::HashMap;
use std::env;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::info;

#[derive(Clone)]
pub struct RateLimiter {
    max_requests: usize,
    period: Duration,
    requests: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
}

/// Wrapper type for login-specific rate limiter to distinguish from global rate limiter
#[derive(Clone)]
pub struct LoginRateLimiter(pub RateLimiter);

impl std::ops::Deref for LoginRateLimiter {
    type Target = RateLimiter;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl RateLimiter {
    pub fn new(max_requests: usize, period: Duration) -> Self {
        Self {
            max_requests,
            period,
            requests: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check if a key is allowed to make a request.
    /// Returns Ok(()) if allowed, Err(Duration) with retry_after time if rate limited.
    pub fn check(&self, key: String) -> Result<(), Duration> {
        let now = Instant::now();
        let mut requests = self.requests.lock().unwrap();

        // Get or create entry for this key
        let key_requests = requests.entry(key).or_default();

        // Remove old requests outside the time window
        key_requests.retain(|&time| now.duration_since(time) < self.period);

        // Check if we've exceeded the limit
        if key_requests.len() >= self.max_requests {
            // Calculate retry_after duration
            if let Some(&oldest) = key_requests.first() {
                let retry_after = self.period.saturating_sub(now.duration_since(oldest));
                return Err(retry_after);
            }
            return Err(self.period);
        }

        // Add this request
        key_requests.push(now);
        Ok(())
    }
}

/// Create a rate limiter for login endpoints from environment configuration.
pub fn create_rate_limiter() -> LoginRateLimiter {
    let max_requests = env::var("RATE_LIMIT_MAX_REQUESTS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(5);

    let period_secs = env::var("RATE_LIMIT_PERIOD_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);

    info!(
        max_requests = max_requests,
        period_secs = period_secs,
        "Configuring login rate limiter"
    );

    LoginRateLimiter(RateLimiter::new(
        max_requests,
        Duration::from_secs(period_secs),
    ))
}

/// Create a global rate limiter for all endpoints.
pub fn create_global_rate_limiter() -> RateLimiter {
    let max_requests = env::var("GLOBAL_RATE_LIMIT_MAX_REQUESTS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100);

    let period_secs = env::var("GLOBAL_RATE_LIMIT_PERIOD_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);

    info!(
        max_requests = max_requests,
        period_secs = period_secs,
        "Configuring global rate limiter"
    );

    RateLimiter::new(max_requests, Duration::from_secs(period_secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn test_rate_limiter_allows_within_limit() {
        let limiter = RateLimiter::new(3, Duration::from_secs(1));

        assert!(limiter.check("test_key".to_string()).is_ok());
        assert!(limiter.check("test_key".to_string()).is_ok());
        assert!(limiter.check("test_key".to_string()).is_ok());
    }

    #[test]
    fn test_rate_limiter_blocks_over_limit() {
        let limiter = RateLimiter::new(2, Duration::from_secs(1));

        assert!(limiter.check("test_key".to_string()).is_ok());
        assert!(limiter.check("test_key".to_string()).is_ok());
        assert!(limiter.check("test_key".to_string()).is_err());
    }

    #[test]
    fn test_rate_limiter_resets_after_period() {
        let limiter = RateLimiter::new(2, Duration::from_millis(100));

        assert!(limiter.check("test_key".to_string()).is_ok());
        assert!(limiter.check("test_key".to_string()).is_ok());
        assert!(limiter.check("test_key".to_string()).is_err());

        sleep(Duration::from_millis(150));

        // Should be allowed again after period expires
        assert!(limiter.check("test_key".to_string()).is_ok());
    }

    #[test]
    fn test_rate_limiter_separate_keys() {
        let limiter = RateLimiter::new(1, Duration::from_secs(1));

        assert!(limiter.check("key1".to_string()).is_ok());
        assert!(limiter.check("key2".to_string()).is_ok());

        // Each key should be rate limited independently
        assert!(limiter.check("key1".to_string()).is_err());
        assert!(limiter.check("key2".to_string()).is_err());
    }
}
