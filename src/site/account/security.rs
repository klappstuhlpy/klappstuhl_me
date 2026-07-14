//! Security mutations: setting / changing the password, enrolling and disabling
//! TOTP, and regenerating recovery codes. All of these bail back to
//! `/account/security`, the page that hosts their controls.

use crate::{
    auth::{hash_password, validate_password},
    flash::{FlashMessage, Flasher},
    headers::ClientIp,
    models::Account,
    token::{Token, TokenRejection},
    AppState,
};
use askama::Template;
use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Form,
};
use serde::Deserialize;

use super::{auth::TotpCodeForm, cookie_redirect, load_full_account};

/// The page every control on the security card returns to.
const SECURITY_PAGE: &str = "/account/security";

#[derive(Deserialize)]
pub struct ChangePasswordForm {
    /// Absent / empty for accounts that have no password yet (Discord signups).
    #[serde(default)]
    old_password: String,
    new_password: String,
    /// Must match `new_password`; guards against typos in the new password.
    #[serde(default)]
    confirm_password: String,
    #[serde(deserialize_with = "crate::utils::empty_string_is_none")]
    session_description: Option<String>,
}

pub async fn change_password(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    token: Token,
    flasher: Flasher,
    Form(form): Form<ChangePasswordForm>,
) -> Response {
    if !((8..=128).contains(&form.new_password.len())) {
        return flasher
            .add("Password length must be 8 to 128 characters")
            .bail(SECURITY_PAGE);
    }
    if form.new_password != form.confirm_password {
        return flasher.add("The two passwords do not match").bail(SECURITY_PAGE);
    }

    let account = match state
        .database()
        .get::<Account, _, _>("SELECT * FROM account WHERE id = ?", [token.id])
        .await
    {
        Ok(Some(account)) => account,
        Ok(None) => {
            flasher.add("Somehow, this account does not exist.");
            return TokenRejection(state.config().cookie_domain()).into_response();
        }
        Err(e) => {
            return flasher.add(format!("SQL error: {e}")).bail(SECURITY_PAGE);
        }
    };

    // Accounts created via Discord have no password to verify — for them this is
    // setting a first password, so only require the current one when it exists.
    if account.has_password() && validate_password(&form.old_password, &account.password).is_err() {
        return flasher.add("Invalid password").bail(SECURITY_PAGE);
    }

    let Ok(changed_hash) = hash_password(&form.new_password) else {
        return flasher
            .add("Failed to hash password somehow. Try again later?")
            .bail(SECURITY_PAGE);
    };

    match state
        .database()
        .execute(
            "UPDATE account SET password = ? WHERE id = ?",
            (changed_hash, account.id),
        )
        .await
    {
        Ok(_) => {
            let Ok(token) = Token::new(account.id) else {
                return flasher.add("Failed to obtain new token cookie").bail(SECURITY_PAGE);
            };
            let cfg = state.config();
            let cookie = token.to_cookie(&cfg.secret_key, cfg.cookie_domain().as_deref());
            state.invalidate_account_cache(account.id);
            state.invalidate_account_sessions(account.id).await;
            state.save_session(&token, form.session_description).await;
            state
                .audit("auth.password.change")
                .actor(&account)
                .ip_opt(client_ip)
                .fire();
            flasher.add(FlashMessage::success("Successfully changed password."));
            cookie_redirect(cookie, SECURITY_PAGE)
        }
        Err(e) => flasher.add(format!("SQL error: {e}")).bail(SECURITY_PAGE),
    }
}

// ─── TOTP enrollment ────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "auth/account_2fa.html")]
struct Totp2faSetupTemplate {
    account: Option<Account>,
    qr_svg: String,
    secret_base32: String,
    recovery_codes: Vec<String>,
}

/// Replaces this account's recovery codes with a fresh set and returns them in
/// plaintext (the only time they are ever visible). Codes are stored hashed.
async fn reset_recovery_codes(state: &AppState, account_id: i64) -> Vec<String> {
    let codes = crate::totp::generate_recovery_codes();
    let _ = state
        .database()
        .execute("DELETE FROM totp_recovery_code WHERE account_id = ?", [account_id])
        .await;
    for code in &codes {
        let _ = state
            .database()
            .execute(
                "INSERT INTO totp_recovery_code (account_id, code_hash) VALUES (?, ?)",
                (account_id, crate::totp::hash_recovery_code(code)),
            )
            .await;
    }
    codes
}

/// Begins enrollment: generates and stores a fresh (still-disabled) secret and
/// recovery codes, then shows the QR + codes. Enabling requires verifying a
/// code via [`totp_enable`], so a misconfigured authenticator can never lock
/// the user out.
pub async fn totp_setup(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    flasher: Flasher,
) -> Response {
    // 2FA only protects the password login — Discord sign-in deliberately skips
    // the TOTP challenge. With no password set, enabling it would guard nothing,
    // so require a password first.
    if !account.has_password() {
        return flasher
            .add("Set a password before enabling two-factor authentication.")
            .bail(SECURITY_PAGE);
    }

    let key = state.config().secret_key;
    let secret = crate::totp::generate_secret();
    let Ok(encrypted) = crate::totp::encrypt_secret(&key, &secret) else {
        return flasher.add("Could not generate a 2FA secret.").bail(SECURITY_PAGE);
    };

    if state
        .database()
        .execute(
            "UPDATE account SET totp_secret = ?, totp_enabled = 0 WHERE id = ?",
            (encrypted, account.id),
        )
        .await
        .is_err()
    {
        return flasher.add("Could not store the 2FA secret.").bail(SECURITY_PAGE);
    }

    let codes = reset_recovery_codes(&state, account.id).await;
    state.invalidate_account_cache(account.id);
    state.audit("auth.2fa.setup").actor(&account).ip_opt(client_ip).fire();

    let uri = crate::totp::otpauth_uri(&secret, &account.name);
    let qr_svg = crate::totp::qr_svg(&uri).unwrap_or_default();
    Totp2faSetupTemplate {
        account: Some(account),
        qr_svg,
        secret_base32: crate::totp::base32_secret(&secret),
        recovery_codes: codes,
    }
    .into_response()
}

pub async fn totp_enable(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    flasher: Flasher,
    Form(form): Form<TotpCodeForm>,
) -> Response {
    let key = state.config().secret_key;
    let Some(acc) = load_full_account(&state, account.id).await else {
        return flasher.add("Account not found.").bail(SECURITY_PAGE);
    };
    let secret = acc
        .totp_secret
        .as_deref()
        .and_then(|enc| crate::totp::decrypt_secret(&key, enc));
    let Some(secret) = secret else {
        return flasher.add("Start 2FA setup first.").bail(SECURITY_PAGE);
    };
    if !crate::totp::verify(&secret, form.code.trim()) {
        return flasher.add("That code didn't match. Try again.").bail(SECURITY_PAGE);
    }
    if state
        .database()
        .execute("UPDATE account SET totp_enabled = 1 WHERE id = ?", [account.id])
        .await
        .is_err()
    {
        return flasher.add("Could not enable 2FA.").bail(SECURITY_PAGE);
    }
    state.invalidate_account_cache(account.id);
    state.audit("auth.2fa.enable").actor(&account).ip_opt(client_ip).fire();
    flasher
        .add(FlashMessage::success("Two-factor authentication is now enabled."))
        .bail(SECURITY_PAGE)
}

#[derive(Deserialize)]
pub struct PasswordConfirmForm {
    password: String,
}

pub async fn totp_disable(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    flasher: Flasher,
    Form(form): Form<PasswordConfirmForm>,
) -> Response {
    let Some(acc) = load_full_account(&state, account.id).await else {
        return flasher.add("Account not found.").bail(SECURITY_PAGE);
    };
    if validate_password(&form.password, &acc.password).is_err() {
        return flasher.add("Invalid password.").bail(SECURITY_PAGE);
    }
    let _ = state
        .database()
        .execute(
            "UPDATE account SET totp_secret = NULL, totp_enabled = 0 WHERE id = ?",
            [account.id],
        )
        .await;
    let _ = state
        .database()
        .execute("DELETE FROM totp_recovery_code WHERE account_id = ?", [account.id])
        .await;
    state.invalidate_account_cache(account.id);
    state.audit("auth.2fa.disable").actor(&account).ip_opt(client_ip).fire();
    flasher
        .add(FlashMessage::success("Two-factor authentication disabled."))
        .bail(SECURITY_PAGE)
}

#[derive(Template)]
#[template(path = "auth/account_recovery_codes.html")]
struct RecoveryCodesTemplate {
    account: Option<Account>,
    recovery_codes: Vec<String>,
}

/// Issues a fresh set of recovery codes, invalidating the old ones. Password
/// confirmation is required — the codes are a second factor's escape hatch, so
/// minting new ones must be as guarded as disabling 2FA.
pub async fn totp_recovery_codes(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    flasher: Flasher,
    Form(form): Form<PasswordConfirmForm>,
) -> Response {
    let Some(acc) = load_full_account(&state, account.id).await else {
        return flasher.add("Account not found.").bail(SECURITY_PAGE);
    };
    if !acc.has_totp() {
        return flasher
            .add("Two-factor authentication is not enabled.")
            .bail(SECURITY_PAGE);
    }
    if validate_password(&form.password, &acc.password).is_err() {
        return flasher.add("Invalid password.").bail(SECURITY_PAGE);
    }

    let codes = reset_recovery_codes(&state, account.id).await;
    state
        .audit("auth.2fa.recovery_codes")
        .actor(&account)
        .ip_opt(client_ip)
        .fire();

    RecoveryCodesTemplate {
        account: Some(account),
        recovery_codes: codes,
    }
    .into_response()
}
