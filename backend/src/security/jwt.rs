use crate::users::entity::Role;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug)]
pub struct JwtError(pub String);

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub username: String,
    pub roles: Vec<String>,
    pub exp: i64,
}

fn role_strings(role: Role) -> Vec<String> {
    match role {
        Role::User => vec!["USER".into()],
        Role::Admin => vec!["USER".into(), "ADMIN".into()],
    }
}

pub fn encode_access(secret: &str, user_id: Uuid, username: &str, role: Role, ttl_secs: i64) -> Result<String, JwtError> {
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + ttl_secs;

    let claims = Claims { sub: user_id, username: username.to_string(), roles: role_strings(role), exp };

    encode(&Header::default(), &claims, &EncodingKey::from_secret(secret.as_bytes()))
        .map_err(|e| JwtError(e.to_string()))
}

pub fn decode_access(secret: &str, token: &str) -> Result<Claims, JwtError> {
    decode::<Claims>(token, &DecodingKey::from_secret(secret.as_bytes()), &Validation::default())
        .map(|data| data.claims)
        .map_err(|e| JwtError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::users::entity::Role;
    use uuid::Uuid;

    #[test]
    fn encode_then_decode_roundtrips() {
        let id = Uuid::now_v7();

        let token = encode_access("secret-at-least-32-bytes-xxxxxxxxxx", id, "alice", Role::Admin, 900).unwrap();
        let claims = decode_access("secret-at-least-32-bytes-xxxxxxxxxx", &token).unwrap();

        assert_eq!(claims.sub, id);
        assert_eq!(claims.username, "alice");
        assert!(claims.roles.contains(&"ADMIN".to_string()));
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let token = encode_access("secret-at-least-32-bytes-xxxxxxxxxx", Uuid::now_v7(), "bob", Role::User, 900).unwrap();

        assert!(decode_access("different-secret-also-32-bytes-yyyy", &token).is_err());
    }
}
