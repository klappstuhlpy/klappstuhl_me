//! Changing the username.
//!
//! A username is not just a label here: it is the address of the public page
//! (`/user/:name`) and the label every audit row is written under
//! (`audit_log.actor_label`). Handing a name from one account to another
//! therefore hands over identity, so a rename has to be more careful than a
//! `SET name = ?`.
//!
//! Three rules make that safe, and [`claim`] enforces all three inside a single
//! `IMMEDIATE` transaction alongside the UPDATE — so two renames racing for the
//! same name cannot both win, and the `username_change` history can never
//! disagree with the `account` row:
//!
//! 1. **Uniqueness.** The `account.name` UNIQUE index is the authority; a lost
//!    race surfaces as [`Rejection::Taken`], not a 500.
//! 2. **A hold on released names.** For [`RELEASE_HOLD_DAYS`] after you give a
//!    name up, only *you* can take it back ([`Rejection::Held`] for anyone
//!    else). Without this, someone could take a name the moment its owner
//!    renamed and inherit their public page and audit trail.
//! 3. **A cooldown.** One change per [`COOLDOWN_DAYS`] per account
//!    ([`Rejection::Cooldown`]) — otherwise the hold could be churned around.
//!
//! [`check`] answers the same question without mutating anything: it backs the
//! live "is this name free?" hint on the signup page and the rename dialog, and
//! is deliberately the *same* rule set, so the hint never promises a name the
//! POST would refuse.

use crate::{
    auth::validate_password,
    flash::{FlashMessage, Flasher},
    headers::ClientIp,
    models::{is_valid_username, Account},
    AppState,
};
use axum::{
    extract::{Query, State},
    response::Response,
    Form, Json,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use super::load_full_account;

/// The page the rename dialog lives on, and where every outcome bounces back to.
const PROFILE_PAGE: &str = "/account/profile";

/// How long an account must wait between renames.
pub const COOLDOWN_DAYS: i64 = 30;

/// How long a released username stays claimable only by the account that
/// released it. Matching the cooldown is deliberate: an account is free to undo
/// its own rename the moment it is allowed to rename again, and never sooner
/// than a stranger could take the name.
pub const RELEASE_HOLD_DAYS: i64 = 30;

/// Names no account may hold, for two different reasons.
///
/// The first four would collide with something real. `percy-service` is the
/// service account that owns every per-guild gallery key (see
/// `AppState::ensure_gallery_service_account`); `system`, `scheduler` and
/// `anonymous` are `actor_label` values the audit log writes for actors that are
/// *not* accounts (the firewall lockout reaper, the cron scheduler, and
/// unauthenticated requests). The admin audit page filters by that label, so an
/// account holding one of them would be indistinguishable from the machine in
/// its own audit trail.
///
/// The rest are impersonation guards: a `/user/admin` page carries an authority
/// this site never granted it.
const RESERVED: &[&str] = &[
    "percy-service",
    "system",
    "scheduler",
    "anonymous",
    "percy",
    "admin",
    "moderator",
    "owner",
];

/// Why a name cannot be taken. Everything user-facing goes through
/// [`Rejection::message`], so the dialog, the flash and the JSON hint always say
/// the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Fails [`is_valid_username`] (length or character set).
    Invalid,
    /// On the [`RESERVED`] list.
    Reserved,
    /// Already the name of this very account — a no-op, not a failure.
    Unchanged,
    /// Held by another account right now.
    Taken,
    /// Released by *another* account within the hold window.
    Held,
    /// This account renamed too recently; the value is when it may rename again.
    Cooldown(OffsetDateTime),
}

impl Rejection {
    /// A stable machine-readable key. The JSON hint sends this so the page can
    /// style the outcome without parsing prose.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Reserved => "reserved",
            Self::Unchanged => "unchanged",
            Self::Taken => "taken",
            Self::Held => "held",
            Self::Cooldown(_) => "cooldown",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Invalid => {
                "Username must be 3-32 characters, lowercase letters, digits, dot, dash, or underscore.".to_string()
            }
            Self::Reserved => "That username is reserved.".to_string(),
            Self::Unchanged => "That is already your username.".to_string(),
            Self::Taken => "That username is already taken.".to_string(),
            Self::Held => format!(
                "That username was recently released by someone else. It is reserved for them for {RELEASE_HOLD_DAYS} days."
            ),
            Self::Cooldown(until) => format!(
                "You can only change your username once every {COOLDOWN_DAYS} days. Try again after {}.",
                until.date()
            ),
        }
    }
}

/// The rules that need no database: shape, and the reserved list.
fn static_check(name: &str) -> Result<(), Rejection> {
    if !is_valid_username(name) {
        return Err(Rejection::Invalid);
    }
    if RESERVED.contains(&name) {
        return Err(Rejection::Reserved);
    }
    Ok(())
}

/// SQLite's own clock, `days` in the past, in the same `...Z` format the
/// `changed_at` default writes — so the comparison is a plain string compare.
fn ago(days: i64) -> String {
    format!("-{days} days")
}

/// Is `name` free for `account` (or for a brand-new signup, when `account` is
/// `None`)? Read-only, and never the authority: [`claim`] re-checks everything
/// under a transaction. This exists so the UI can say *why* ahead of the submit.
pub async fn availability(state: &AppState, name: &str, account: Option<&Account>) -> Result<(), Rejection> {
    static_check(name)?;

    if account.is_some_and(|a| a.name == name) {
        return Err(Rejection::Unchanged);
    }

    let self_id = account.map(|a| a.id);
    let wanted = name.to_owned();
    state
        .database()
        .call(move |conn| -> rusqlite::Result<Result<(), Rejection>> {
            let owner: Option<i64> = conn
                .query_row("SELECT id FROM account WHERE name = ?", [&wanted], |row| row.get(0))
                .optional()?;
            if owner.is_some() {
                return Ok(Err(Rejection::Taken));
            }

            let holder: Option<i64> = conn
                .query_row(
                    "SELECT account_id FROM username_change
                      WHERE old_name = ? AND changed_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)
                      ORDER BY changed_at DESC LIMIT 1",
                    rusqlite::params![&wanted, ago(RELEASE_HOLD_DAYS)],
                    |row| row.get(0),
                )
                .optional()?;
            // Your own released name is yours to take back during the hold.
            if holder.is_some() && holder != self_id {
                return Ok(Err(Rejection::Held));
            }

            Ok(Ok(()))
        })
        .await
        // A database failure is not an "available" answer. Say nothing rather
        // than promise a name we could not verify.
        .unwrap_or(Err(Rejection::Taken))
}

/// When this account may next rename, or `None` if it may right now.
pub async fn cooldown_until(state: &AppState, account_id: i64) -> Option<OffsetDateTime> {
    let last: Option<OffsetDateTime> = state
        .database()
        .call(move |conn| {
            conn.query_row(
                "SELECT changed_at FROM username_change
                  WHERE account_id = ? ORDER BY changed_at DESC LIMIT 1",
                [account_id],
                |row| row.get(0),
            )
            .optional()
        })
        .await
        .ok()
        .flatten();

    last.map(|at| at + Duration::days(COOLDOWN_DAYS))
        .filter(|until| *until > OffsetDateTime::now_utc())
}

/// The names this account used to hold, newest first.
pub async fn previous_names(state: &AppState, account_id: i64) -> Vec<String> {
    state
        .database()
        .call(move |conn| -> rusqlite::Result<Vec<String>> {
            let mut stmt = conn.prepare_cached(
                "SELECT old_name FROM username_change
                  WHERE account_id = ? ORDER BY changed_at DESC LIMIT 5",
            )?;
            let rows: rusqlite::Result<Vec<String>> = stmt.query_map([account_id], |row| row.get(0))?.collect();
            rows
        })
        .await
        .unwrap_or_default()
}

/// Takes `new_name` for `account_id`, or explains why not.
///
/// Every rule is re-checked here, inside one `IMMEDIATE` transaction with the
/// UPDATE and the history row — [`availability`] runs unlocked and can be stale
/// by the time the form is submitted. Two requests racing for the same free name
/// serialise on the transaction; the loser's UPDATE trips the `account.name`
/// UNIQUE index and comes back as [`Rejection::Taken`].
///
/// The outer `rusqlite::Result` is a database failure; the inner one is a
/// refusal, which is a normal outcome.
fn claim(conn: &mut Connection, account_id: i64, new_name: &str) -> rusqlite::Result<Result<String, Rejection>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // Re-read the current name under the lock: the caller's `Account` may be a
    // cached copy from before a rename that landed a moment ago.
    let old_name: String = tx.query_row("SELECT name FROM account WHERE id = ?", [account_id], |row| row.get(0))?;
    if old_name == new_name {
        return Ok(Err(Rejection::Unchanged));
    }

    let last_change: Option<OffsetDateTime> = tx
        .query_row(
            "SELECT changed_at FROM username_change WHERE account_id = ? ORDER BY changed_at DESC LIMIT 1",
            [account_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(last) = last_change {
        let until = last + Duration::days(COOLDOWN_DAYS);
        if until > OffsetDateTime::now_utc() {
            return Ok(Err(Rejection::Cooldown(until)));
        }
    }

    let holder: Option<i64> = tx
        .query_row(
            "SELECT account_id FROM username_change
              WHERE old_name = ? AND changed_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)
              ORDER BY changed_at DESC LIMIT 1",
            rusqlite::params![new_name, ago(RELEASE_HOLD_DAYS)],
            |row| row.get(0),
        )
        .optional()?;
    if holder.is_some() && holder != Some(account_id) {
        return Ok(Err(Rejection::Held));
    }

    match tx.execute(
        "UPDATE account SET name = ? WHERE id = ?",
        rusqlite::params![new_name, account_id],
    ) {
        Ok(_) => {}
        // The only UNIQUE index on `account` is the name — someone holds it.
        Err(rusqlite::Error::SqliteFailure(err, _)) if err.code == rusqlite::ErrorCode::ConstraintViolation => {
            return Ok(Err(Rejection::Taken));
        }
        Err(e) => return Err(e),
    }

    tx.execute(
        "INSERT INTO username_change(account_id, old_name, new_name) VALUES (?, ?, ?)",
        rusqlite::params![account_id, &old_name, new_name],
    )?;
    tx.commit()?;
    Ok(Ok(old_name))
}

// ─── The live availability hint ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CheckQuery {
    name: String,
}

#[derive(Serialize)]
pub struct CheckResponse {
    available: bool,
    /// `null` when available; otherwise the stable key from [`Rejection::reason`].
    reason: Option<&'static str>,
    message: String,
}

/// `GET /account/username/check?name=…` — the same rules [`claim`] enforces, as
/// JSON, so signup and the rename dialog can say "taken" before the submit.
///
/// Reachable logged-out (signup needs it) and rate-limited at the route. It
/// leaks nothing the signup form's own error message doesn't already: whether a
/// name exists.
pub async fn check(
    State(state): State<AppState>,
    account: Option<Account>,
    Query(query): Query<CheckQuery>,
) -> Json<CheckResponse> {
    let name = query.name.trim();
    Json(match availability(&state, name, account.as_ref()).await {
        Ok(()) => CheckResponse {
            available: true,
            reason: None,
            message: format!("{name} is available."),
        },
        Err(rejection) => CheckResponse {
            available: false,
            reason: Some(rejection.reason()),
            message: rejection.message(),
        },
    })
}

// ─── The rename ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ChangeUsernameForm {
    username: String,
    /// Current password. Empty for accounts that have none (Discord signups),
    /// which is the same concession [`super::security::change_password`] makes.
    #[serde(default)]
    password: String,
}

/// `POST /account/username` — renames the account.
///
/// Re-authenticates first (a hijacked session should not be able to rename the
/// account out from under its owner), then hands the decision to [`claim`].
pub async fn change_username(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    flasher: Flasher,
    Form(form): Form<ChangeUsernameForm>,
) -> Response {
    let new_name = form.username.trim().to_owned();

    // Cheap, unlocked pre-checks purely for a better message; `claim` is the
    // authority and repeats them.
    if let Err(rejection) = static_check(&new_name) {
        return flasher.add(rejection.message()).bail(PROFILE_PAGE);
    }

    let Some(full) = load_full_account(&state, account.id).await else {
        return flasher
            .add("This account could not be loaded. Try again.")
            .bail(PROFILE_PAGE);
    };
    if full.has_password() && validate_password(&form.password, &full.password).is_err() {
        state
            .audit("auth.username.change_fail")
            .actor(&account)
            .ip_opt(client_ip)
            .meta(serde_json::json!({ "reason": "bad_password" }))
            .fire();
        super::register_failure(client_ip);
        return flasher.add("Invalid password.").bail(PROFILE_PAGE);
    }

    let account_id = account.id;
    let wanted = new_name.clone();
    let claimed: rusqlite::Result<Result<String, Rejection>> = state
        .database()
        .call(move |conn| claim(conn, account_id, &wanted))
        .await;

    let old_name = match claimed {
        Ok(Ok(old_name)) => old_name,
        Ok(Err(rejection)) => {
            state
                .audit("auth.username.change_fail")
                .actor(&account)
                .ip_opt(client_ip)
                .meta(serde_json::json!({ "reason": rejection.reason(), "wanted": new_name }))
                .fire();
            return flasher.add(rejection.message()).bail(PROFILE_PAGE);
        }
        Err(e) => {
            tracing::error!(error = %e, account_id = account.id, "username change failed");
            return flasher
                .add("Something went wrong changing your username. Nothing was changed.")
                .bail(PROFILE_PAGE);
        }
    };

    // The cached `Account` (and every session-resolved copy of it) still carries
    // the old name — the site header would keep showing it otherwise.
    state.invalidate_account_cache(account.id);

    // `actor(&account)` labels the row with the name the account had when it
    // acted (the old one) and keys it by id, so the feed and the admin audit
    // filter still find it after the rename. Both names are in the meta.
    state
        .audit("auth.username.change")
        .actor(&account)
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "from": old_name, "to": new_name }))
        .fire();

    flasher
        .add(FlashMessage::success(format!(
            "You are now {new_name}. Your old username is reserved for you for {RELEASE_HOLD_DAYS} days."
        )))
        .bail(PROFILE_PAGE)
}

/// Drives the two handlers over HTTP, on the same layer stack `main.rs` builds.
///
/// [`claim`]'s rules are pinned by [`tests`] below, against the database
/// directly. What this module is for is everything *around* them: that the form
/// reaches the handler, that re-authentication really gates the rename, and that
/// the JSON hint tells the page the same thing the POST would.
#[cfg(test)]
mod http {
    use crate::{auth::hash_password, models::Account, token::Token, AppState, Database};
    use axum::{
        body::Body,
        http::{header::LOCATION, Request, StatusCode},
        routing::{get, post},
        Extension, Router,
    };
    use tower::ServiceExt;

    const PASSWORD: &str = "correct-horse-battery";

    async fn test_state() -> AppState {
        // One connection: every `:memory:` connection is a separate database.
        let database = Database::file(":memory:")
            .connections(1)
            .with_init(crate::migrations::migrate)
            .open()
            .await
            .expect("open in-memory db");
        AppState::for_tests(database).await
    }

    /// The two routes, minus their rate-limit layers (not what these test).
    fn router(state: AppState) -> Router {
        let key = state.config().secret_key;
        Router::new()
            .route("/account/username", post(super::change_username))
            .route("/account/username/check", get(super::check))
            .with_state(state)
            .layer(axum::middleware::from_fn(crate::flash::process_flash_messages))
            .layer(axum::middleware::from_fn(crate::parse_cookies))
            .layer(Extension(key))
    }

    async fn seed_account(state: &AppState, name: &str) -> Account {
        let password = hash_password(PASSWORD).expect("hash");
        state
            .database()
            .execute(
                "INSERT INTO account(name, password) VALUES (?, ?)",
                (name.to_owned(), password),
            )
            .await
            .expect("insert account");
        state
            .database()
            .get::<Account, _, _>("SELECT * FROM account WHERE name = ?", [name.to_owned()])
            .await
            .expect("load account")
            .expect("account exists")
    }

    /// A browser login for `account`, as the signed `token` cookie value.
    async fn login(state: &AppState, account: &Account) -> String {
        let token = Token::new(account.id).expect("token");
        state.save_session(&token, Some("laptop".to_owned())).await;
        token.to_cookie(&state.config().secret_key, None).value().to_owned()
    }

    /// `POST /account/username`, returning the `Location` it bounces to.
    async fn post_rename(state: &AppState, cookie: &str, name: &str, password: &str) -> String {
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/account/username")
                    .header("Cookie", format!("token={cookie}"))
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("username={name}&password={password}")))
                    .unwrap(),
            )
            .await
            .expect("handler ran");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        response
            .headers()
            .get(LOCATION)
            .expect("a Location header")
            .to_str()
            .expect("ascii location")
            .to_owned()
    }

    /// `GET /account/username/check`, as the parsed JSON the page receives.
    async fn get_check(state: &AppState, name: &str) -> serde_json::Value {
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/account/username/check?name={name}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("handler ran");

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("read body");
        serde_json::from_slice(&body).expect("json body")
    }

    async fn name_of(state: &AppState, id: i64) -> String {
        state
            .database()
            .get_row("SELECT name FROM account WHERE id = ?", [id], |row| row.get(0))
            .await
            .expect("load name")
    }

    #[tokio::test]
    async fn the_form_renames_the_account() {
        let state = test_state().await;
        let account = seed_account(&state, "before").await;
        let cookie = login(&state, &account).await;

        let location = post_rename(&state, &cookie, "after", PASSWORD).await;

        assert_eq!(location, super::PROFILE_PAGE);
        assert_eq!(name_of(&state, account.id).await, "after");
    }

    /// Re-authentication is the point of the password field: a stolen session
    /// cookie must not be able to rename the account out from under its owner.
    #[tokio::test]
    async fn a_wrong_password_refuses_the_rename() {
        let state = test_state().await;
        let account = seed_account(&state, "before").await;
        let cookie = login(&state, &account).await;

        let location = post_rename(&state, &cookie, "after", "hunter2").await;

        assert_eq!(location, super::PROFILE_PAGE);
        assert_eq!(
            name_of(&state, account.id).await,
            "before",
            "the rename went through on a bad password"
        );
    }

    /// The hint the signup page shows is the answer the POST would give — that
    /// equivalence is the only reason it is safe to show it.
    #[tokio::test]
    async fn the_check_endpoint_answers_for_a_logged_out_signup() {
        let state = test_state().await;
        seed_account(&state, "occupied").await;

        let free = get_check(&state, "vacant").await;
        assert_eq!(free["available"], serde_json::json!(true));

        let taken = get_check(&state, "occupied").await;
        assert_eq!(taken["available"], serde_json::json!(false));
        assert_eq!(taken["reason"], serde_json::json!("taken"));

        let reserved = get_check(&state, "percy-service").await;
        assert_eq!(reserved["reason"], serde_json::json!("reserved"));

        let malformed = get_check(&state, "ab").await;
        assert_eq!(malformed["reason"], serde_json::json!("invalid"));
    }

    /// A name under hold reads as unavailable to a stranger *before* they submit,
    /// so the hint and [`super::claim`] cannot disagree.
    #[tokio::test]
    async fn the_check_endpoint_reports_a_held_name() {
        let state = test_state().await;
        let account = seed_account(&state, "wanted").await;
        let cookie = login(&state, &account).await;

        post_rename(&state, &cookie, "renamed", PASSWORD).await;

        // Logged out — i.e. as anyone but the account that released it.
        let held = get_check(&state, "wanted").await;
        assert_eq!(held["available"], serde_json::json!(false));
        assert_eq!(held["reason"], serde_json::json!("held"));
    }
}

/// Drives the rules against a real, migrated database.
///
/// [`claim`] is the whole safety story of a rename, so it is tested directly
/// rather than through the handler: every refusal it can return is reachable
/// from a two-account setup, and the handler adds only re-authentication on top.
#[cfg(test)]
mod tests {
    use super::*;

    /// The pool sets `foreign_keys` on every connection; an in-memory one starts
    /// without it, and the `username_change` cascade would silently do nothing.
    fn migrated_db() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        crate::migrations::migrate(&mut conn).expect("migrate");
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        conn
    }

    fn seed_account(conn: &Connection, name: &str) -> i64 {
        conn.execute("INSERT INTO account(name, password) VALUES (?, 'hash')", [name])
            .unwrap();
        conn.last_insert_rowid()
    }

    fn name_of(conn: &Connection, id: i64) -> String {
        conn.query_row("SELECT name FROM account WHERE id = ?", [id], |row| row.get(0))
            .unwrap()
    }

    /// Backdates this account's last rename so the cooldown has expired, without
    /// making the tests wait 30 days.
    fn backdate_last_change(conn: &Connection, account_id: i64, days: i64) {
        conn.execute(
            "UPDATE username_change
                SET changed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)
              WHERE account_id = ?",
            rusqlite::params![super::ago(days), account_id],
        )
        .unwrap();
    }

    #[test]
    fn renaming_moves_the_name_and_records_the_history() {
        let mut conn = migrated_db();
        let id = seed_account(&conn, "old");

        assert_eq!(claim(&mut conn, id, "new").unwrap(), Ok("old".to_string()));

        assert_eq!(name_of(&conn, id), "new");
        let (old, new): (String, String) = conn
            .query_row(
                "SELECT old_name, new_name FROM username_change WHERE account_id = ?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((old.as_str(), new.as_str()), ("old", "new"));
    }

    #[test]
    fn a_name_another_account_holds_is_refused() {
        let mut conn = migrated_db();
        let me = seed_account(&conn, "me");
        seed_account(&conn, "taken");

        assert_eq!(claim(&mut conn, me, "taken").unwrap(), Err(Rejection::Taken));

        assert_eq!(name_of(&conn, me), "me", "the failed claim renamed the account anyway");
    }

    #[test]
    fn renaming_to_your_own_name_is_a_no_op() {
        let mut conn = migrated_db();
        let id = seed_account(&conn, "same");

        assert_eq!(claim(&mut conn, id, "same").unwrap(), Err(Rejection::Unchanged));

        let changes: i64 = conn
            .query_row("SELECT COUNT(*) FROM username_change", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes, 0, "a no-op rename wrote a history row");
    }

    #[test]
    fn a_second_rename_inside_the_cooldown_is_refused() {
        let mut conn = migrated_db();
        let id = seed_account(&conn, "first");

        claim(&mut conn, id, "second").unwrap().expect("the first rename");
        let outcome = claim(&mut conn, id, "third").unwrap();

        assert!(
            matches!(outcome, Err(Rejection::Cooldown(_))),
            "expected a cooldown, got {outcome:?}"
        );
        assert_eq!(name_of(&conn, id), "second");
    }

    #[test]
    fn once_the_cooldown_lapses_the_account_can_rename_again() {
        let mut conn = migrated_db();
        let id = seed_account(&conn, "first");

        claim(&mut conn, id, "second").unwrap().expect("the first rename");
        backdate_last_change(&conn, id, COOLDOWN_DAYS + 1);

        claim(&mut conn, id, "third").unwrap().expect("the second rename");
        assert_eq!(name_of(&conn, id), "third");
    }

    /// The point of the whole table: the name you just gave up is not up for
    /// grabs. Without the hold, `stranger` would inherit `/user/renamer` and
    /// every audit row written under that label.
    #[test]
    fn a_name_released_by_someone_else_is_held_against_strangers() {
        let mut conn = migrated_db();
        let renamer = seed_account(&conn, "wanted");
        let stranger = seed_account(&conn, "stranger");

        claim(&mut conn, renamer, "renamed").unwrap().expect("the rename");
        // `wanted` is now free as far as the UNIQUE index is concerned.
        let outcome = claim(&mut conn, stranger, "wanted").unwrap();

        assert_eq!(outcome, Err(Rejection::Held));
        assert_eq!(name_of(&conn, stranger), "stranger");
    }

    /// …but the account that released it may take it back — that is what makes a
    /// rename undoable. (Its own cooldown still has to lapse first.)
    #[test]
    fn the_releasing_account_can_take_its_old_name_back() {
        let mut conn = migrated_db();
        let id = seed_account(&conn, "wanted");

        claim(&mut conn, id, "renamed").unwrap().expect("the rename");
        backdate_last_change(&conn, id, COOLDOWN_DAYS + 1);

        claim(&mut conn, id, "wanted").unwrap().expect("the undo");
        assert_eq!(name_of(&conn, id), "wanted");
    }

    /// Once the hold lapses the name is genuinely free for anyone.
    #[test]
    fn a_released_name_is_free_again_after_the_hold() {
        let mut conn = migrated_db();
        let renamer = seed_account(&conn, "wanted");
        let stranger = seed_account(&conn, "stranger");

        claim(&mut conn, renamer, "renamed").unwrap().expect("the rename");
        backdate_last_change(&conn, renamer, RELEASE_HOLD_DAYS + 1);

        claim(&mut conn, stranger, "wanted").unwrap().expect("the claim");
        assert_eq!(name_of(&conn, stranger), "wanted");
    }

    #[test]
    fn reserved_and_malformed_names_never_reach_the_database() {
        assert_eq!(static_check("percy-service"), Err(Rejection::Reserved));
        assert_eq!(static_check("system"), Err(Rejection::Reserved));
        assert_eq!(static_check("ab"), Err(Rejection::Invalid));
        assert_eq!(static_check("Uppercase"), Err(Rejection::Invalid));
        assert_eq!(static_check("has spaces"), Err(Rejection::Invalid));
        assert_eq!(static_check("fine.name_1-2"), Ok(()));
    }

    /// Deleting an account must not leave its history behind holding names
    /// hostage — the row is `ON DELETE CASCADE`, so the name frees up with it.
    #[test]
    fn deleting_an_account_drops_its_username_history() {
        let mut conn = migrated_db();
        let id = seed_account(&conn, "leaver");
        claim(&mut conn, id, "left").unwrap().expect("the rename");

        conn.execute("DELETE FROM account WHERE id = ?", [id]).unwrap();

        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM username_change", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }
}
