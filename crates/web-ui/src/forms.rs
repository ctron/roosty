use strum::{EnumString, IntoStaticStr};

/// Non-sensitive login failure exposed through the form redirect query.
#[derive(Clone, Copy, Debug, EnumString, Eq, IntoStaticStr, PartialEq)]
#[strum(serialize_all = "snake_case")]
pub enum LoginError {
    InvalidCredentials,
}

/// Result of a password-change submission exposed through the form redirect query.
#[derive(Clone, Copy, Debug, EnumString, Eq, IntoStaticStr, PartialEq)]
#[strum(serialize_all = "snake_case")]
pub enum PasswordChangeResult {
    PasswordChanged,
    ConfirmationMismatch,
    TooShort,
    CurrentPasswordIncorrect,
    ChangeFailed,
    VerificationFailed,
}
