use std::borrow::Cow;

use askama::Template;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{logging::BadRequestReason, models::Account};

#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorTemplate {
    account: Option<Account>,
    error: anyhow::Error,
}

/// Inteprets an [`anyhow::Error`] as an internal server error.
pub struct InternalError(anyhow::Error);

impl IntoResponse for InternalError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorTemplate {
                account: None,
                error: self.0,
            },
        )
            .into_response()
    }
}

impl<E> From<E> for InternalError
where
    E: Into<anyhow::Error>,
{
    fn from(value: E) -> Self {
        Self(value.into())
    }
}

/// An error type that represents its errors as a JSON response.
///
/// The body is shaped after Discord's error model: a human-readable `message`,
/// a machine-readable numeric `code`, and an optional `errors` object carrying
/// field-level validation detail (`{ "field": { "_errors": ["…"] } }`). The
/// legacy `error` field is kept as an alias of `message` so pre-existing clients
/// and tools keep working unchanged.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ApiError {
    /// The error message for this error. Kept as a legacy alias of `message`.
    pub error: Cow<'static, str>,
    /// The human-readable error message (Discord-style). Mirrors `error`.
    #[serde(default)]
    pub message: Cow<'static, str>,
    /// The associated error code.
    #[schema(value_type = u8)]
    pub code: ApiErrorCode,
    /// Optional field-level validation detail, Discord-style:
    /// `{ "url": { "_errors": ["must be http(s)"] } }`. Omitted when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub errors: Option<serde_json::Value>,
}

/// An error code that the client can use to quickly check error conditions.
#[derive(Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash, ToSchema)]
#[repr(u8)]
pub enum ApiErrorCode {
    /// An internal server error happened. This should be rather rare.
    ServerError = 0,
    /// The client provided request was invalid in some way or another.
    BadRequest = 1,
    /// An internal error code that represents that the account is already registered.
    ///
    /// Do not rely on this or use it.
    #[schema(deprecated)]
    UsernameRegistered = 2,
    /// An internal error code that represents that the account has provided valid credentials.
    ///
    /// Do not rely on this or use it.
    #[schema(deprecated)]
    IncorrectLogin = 3,
    /// The client does not have permission to execute this action.
    NoPermissions = 4,
    /// The entry already exists.
    EntryAlreadyExists = 5,
    /// The entity being searched for does not exist.
    NotFound = 6,
    /// The client is not authorized, a proper authorization header must be provided.
    Unauthorized = 7,
    /// The client is being rate limited.
    RateLimited = 8,
    /// The request body failed validation; see the `errors` map for field detail.
    Validation = 9,
    /// The request payload exceeded the maximum accepted size.
    PayloadTooLarge = 10,
    /// The request's media type is not supported for this endpoint.
    UnsupportedMedia = 11,
}

impl ApiErrorCode {
    pub fn from_number(number: u8) -> Option<Self> {
        match number {
            1 => Some(Self::BadRequest),
            2 => Some(Self::UsernameRegistered),
            3 => Some(Self::IncorrectLogin),
            4 => Some(Self::NoPermissions),
            5 => Some(Self::EntryAlreadyExists),
            6 => Some(Self::NotFound),
            7 => Some(Self::Unauthorized),
            8 => Some(Self::RateLimited),
            9 => Some(Self::Validation),
            10 => Some(Self::PayloadTooLarge),
            11 => Some(Self::UnsupportedMedia),
            _ => None,
        }
    }
}

impl Serialize for ApiErrorCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u8(*self as u8)
    }
}

impl<'de> Deserialize<'de> for ApiErrorCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let num = u8::deserialize(deserializer)?;
        Self::from_number(num).ok_or_else(|| serde::de::Error::custom("unknown error code"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let incorrect_login = self.code == ApiErrorCode::IncorrectLogin;
        let mut response = (self.status_code(), Json(self)).into_response();
        if incorrect_login {
            response.extensions_mut().insert(BadRequestReason::IncorrectLogin);
        }
        response
    }
}

impl ApiError {
    /// The single constructor every other builder routes through, so `error`
    /// and its `message` alias never drift apart.
    fn build(message: Cow<'static, str>, code: ApiErrorCode) -> Self {
        Self {
            error: message.clone(),
            message,
            code,
            errors: None,
        }
    }

    /// Creates a new [`ApiError`] with [`ApiErrorCode::BadRequest`] as the code.
    pub fn new<S>(s: S) -> Self
    where
        S: Into<Cow<'static, str>>,
    {
        Self::build(s.into(), ApiErrorCode::BadRequest)
    }

    pub fn with_code(mut self, code: ApiErrorCode) -> Self {
        self.code = code;
        self
    }

    /// Replaces the human-readable error message, keeping the code.
    pub fn with_message(mut self, message: impl Into<Cow<'static, str>>) -> Self {
        let message = message.into();
        self.error = message.clone();
        self.message = message;
        self
    }

    /// Attaches Discord-style field-level validation detail, e.g.
    /// `serde_json::json!({ "url": { "_errors": ["must be http(s)"] } })`.
    pub fn with_field_errors(mut self, errors: serde_json::Value) -> Self {
        self.errors = Some(errors);
        self
    }

    /// A `BadRequest` carrying a single field's validation error, shaped like
    /// Discord's `errors` map. Convenience over [`with_field_errors`].
    pub fn validation(field: &str, message: impl Into<Cow<'static, str>>) -> Self {
        let message = message.into();
        Self::build(message.clone(), ApiErrorCode::Validation).with_field_errors(serde_json::json!({
            field: { "_errors": [message] }
        }))
    }

    pub fn incorrect_login() -> Self {
        Self::build("incorrect username or password".into(), ApiErrorCode::IncorrectLogin)
    }

    pub fn forbidden() -> Self {
        Self::build("no permissions to do this action".into(), ApiErrorCode::NoPermissions)
    }

    pub fn not_found(error: impl Into<Cow<'static, str>>) -> Self {
        Self::build(error.into(), ApiErrorCode::NotFound)
    }

    pub fn unauthorized() -> Self {
        Self::build("unauthorized".into(), ApiErrorCode::Unauthorized)
    }

    pub fn rate_limited() -> Self {
        Self::build("rate limit exceeded".into(), ApiErrorCode::RateLimited)
    }

    fn status_code(&self) -> StatusCode {
        match self.code {
            ApiErrorCode::ServerError => StatusCode::INTERNAL_SERVER_ERROR,
            ApiErrorCode::NoPermissions => StatusCode::FORBIDDEN,
            ApiErrorCode::NotFound => StatusCode::NOT_FOUND,
            ApiErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ApiErrorCode::EntryAlreadyExists => StatusCode::CONFLICT,
            ApiErrorCode::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ApiErrorCode::UnsupportedMedia => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            _ => StatusCode::BAD_REQUEST,
        }
    }
}

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(value: E) -> Self {
        Self::build(Cow::Owned(value.into().to_string()), ApiErrorCode::ServerError)
    }
}
