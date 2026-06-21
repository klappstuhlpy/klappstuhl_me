//! Account authentication: password hashing plus the TOTP, key, and token
//! submodules that back two-factor auth and API/session credentials.

pub mod key;
pub mod token;
pub mod totp;

use argon2::{
    password_hash::{rand_core::OsRng, SaltString},
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
};

/// Sentinel stored in `account.password` for accounts created via Discord OAuth
/// that have never set a password. It is intentionally *not* a valid Argon2 PHC
/// string, so [`validate_password`] always fails for it — such accounts can only
/// sign in through Discord until they optionally set a password of their own.
pub const NO_PASSWORD_SENTINEL: &str = "!";

/// Hashes a plaintext password using Argon2 with a random salt.
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let argon2 = Argon2::default();
    let salt = SaltString::generate(&mut OsRng);
    Ok(argon2.hash_password(password.as_bytes(), &salt)?.to_string())
}

/// Verifies a plaintext password against an Argon2 hash. Returns `Ok(())` if the password matches.
pub fn validate_password(password: &str, password_hash: &str) -> anyhow::Result<()> {
    let argon2 = Argon2::default();
    let hash = PasswordHash::new(password_hash)?;
    argon2.verify_password(password.as_bytes(), &hash)?;
    Ok(())
}
