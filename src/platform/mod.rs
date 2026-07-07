//! Cross-cutting HTTP plumbing shared by every surface: response caching, flash
//! messages, header/client-IP helpers, rate limiting, and permission scopes.
//! These wrap the route handlers in `site` and `admin` but belong to neither.

pub mod cached;
pub mod flash;
pub mod headers;
pub mod ratelimit;
pub mod scope;
