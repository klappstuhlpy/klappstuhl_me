//! In-process login soft-lockout — the site's own auth hardening, independent
//! of the admin surface.
//!
//! After [`THRESHOLD`] failed authentication attempts from one IP within
//! [`WINDOW`], further attempts from that IP are refused at the application
//! layer until the window elapses. It is a decaying per-IP counter held in
//! process memory (bounded and LRU-evicted) — no firewall backend and no admin
//! app involved, so the site stays hardened whether or not `kls-admin` is
//! running. State is lost on restart, which is fine for a short-window throttle.
//!
//! This replaces the old cross-surface coupling in which a failed site login
//! reached into the admin `firewall::lockout` counter (severed in the
//! admin-separation Phase 1). A *firewall-level* ban driven by these same
//! events can return later as an additive, admin-app-owned feature; the site's
//! soft-lockout does not depend on it.

use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use quick_cache::sync::Cache;

/// Failed attempts from one IP within [`WINDOW`] that trip the lockout.
pub const THRESHOLD: u32 = 8;
/// Sliding window over which failures accumulate. Once the threshold is reached
/// the IP stays locked until `WINDOW` has passed since its *first* failure.
pub const WINDOW: Duration = Duration::from_secs(600);
/// Maximum distinct IPs tracked at once (bounds memory; LRU-evicted).
const CAPACITY: usize = 10_000;

#[derive(Clone, Copy)]
struct AttemptWindow {
    count: u32,
    /// When the current window started (the first failure in it).
    started: Instant,
}

fn table() -> &'static Cache<IpAddr, AttemptWindow> {
    static TABLE: OnceLock<Cache<IpAddr, AttemptWindow>> = OnceLock::new();
    TABLE.get_or_init(|| Cache::new(CAPACITY))
}

/// Record one failed authentication attempt from `ip`. Called only on genuine
/// credential failures (bad password / bad 2FA code / re-auth failures), never
/// on validation refusals — the caller decides what counts.
pub fn register_failure(ip: IpAddr) {
    let now = Instant::now();
    let next = match table().get(&ip) {
        // Still inside the live window → accumulate, keep the window's start.
        Some(w) if now.duration_since(w.started) < WINDOW => AttemptWindow {
            count: w.count.saturating_add(1),
            started: w.started,
        },
        // No entry, or the previous window has fully elapsed → start fresh.
        _ => AttemptWindow { count: 1, started: now },
    };
    table().insert(ip, next);
}

/// True when `ip` is currently locked out: at or over [`THRESHOLD`] within a
/// still-live [`WINDOW`]. An elapsed window reports unlocked (and is treated as
/// reset by the next [`register_failure`]).
pub fn is_locked(ip: IpAddr) -> bool {
    match table().get(&ip) {
        Some(w) => Instant::now().duration_since(w.started) < WINDOW && w.count >= THRESHOLD,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, n))
    }

    #[test]
    fn locks_only_after_threshold() {
        let addr = ip(1);
        for _ in 0..THRESHOLD - 1 {
            register_failure(addr);
        }
        assert!(!is_locked(addr), "below threshold must not lock");
        register_failure(addr);
        assert!(is_locked(addr), "reaching threshold must lock");
    }

    #[test]
    fn distinct_ips_are_independent() {
        let a = ip(2);
        let b = ip(3);
        for _ in 0..THRESHOLD {
            register_failure(a);
        }
        assert!(is_locked(a));
        assert!(!is_locked(b), "one IP's failures must not lock another");
    }

    #[test]
    fn unknown_ip_is_not_locked() {
        assert!(!is_locked(ip(4)));
    }
}
