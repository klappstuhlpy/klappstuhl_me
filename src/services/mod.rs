//! Feature backends: the long-running subsystems and admin tools (Docker,
//! firewall, health checks, metrics, Postgres, proxy, secrets, SSH, backups,
//! audit log, and alerting).

pub mod alerts;
pub mod audit;
pub mod backup;
pub mod docker;
pub mod firewall;
pub mod health;
pub mod metrics;
pub mod postgres;
pub mod proxy;
pub mod secrets;
pub mod ssh;
