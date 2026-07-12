use argon2::{
    Argon2, PasswordHasher,
    password_hash::{SaltString, rand_core::OsRng},
};
use roost_core::{Result, RoostError};
use uuid::Uuid;

pub fn generate_temporary_password() -> String {
    format!("roost-{}", Uuid::now_v7().simple())
}

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| RoostError::InvalidInput(error.to_string()))
}
