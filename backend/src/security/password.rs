use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

#[derive(Debug)]
pub struct PasswordError(pub String);

pub fn hash(plain: &str) -> Result<String, PasswordError> {
    let salt = SaltString::generate(&mut OsRng);

    Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| PasswordError(e.to_string()))
}

pub fn verify(plain: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(plain.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_roundtrips() {
        let phc = hash("correct horse").unwrap();

        assert!(verify("correct horse", &phc));
        assert!(!verify("wrong horse", &phc));
        assert_ne!(phc, "correct horse"); // never store plaintext
    }
}
