use std::borrow::Cow;

/// Validate a local username at account-creation boundaries.
pub(crate) fn username(username: &str) -> Result<(), Cow<'static, str>> {
    if username.len() < 2
        || username.len() > 30
        || !username
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err("must be 2-30 ASCII letters, numbers, or underscores".into());
    }
    Ok(())
}

/// Perform Roosty's intentionally small local email syntax check.
pub(crate) fn email(email: &str) -> Result<(), Cow<'static, str>> {
    if !email.contains('@') || email.trim() != email {
        return Err("must contain @ and must not contain surrounding whitespace".into());
    }
    Ok(())
}
