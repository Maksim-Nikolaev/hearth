#[derive(Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub jwt_secret: String,
    pub access_ttl_secs: i64,
    pub refresh_ttl_secs: i64,
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self {
            database_url: std::env::var("DATABASE_URL").expect("DATABASE_URL"),
            jwt_secret: std::env::var("JWT_SECRET").expect("JWT_SECRET"),
            access_ttl_secs: std::env::var("ACCESS_TTL_SECS").unwrap_or("900".into()).parse().unwrap(),
            refresh_ttl_secs: std::env::var("REFRESH_TTL_SECS").unwrap_or("2592000".into()).parse().unwrap(),
        }
    }
}
