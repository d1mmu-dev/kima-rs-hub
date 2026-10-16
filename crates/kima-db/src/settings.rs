use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq)]
pub struct SystemSettings {
    pub registration_open: bool,
    pub library_root: Option<String>,
}

/// Fields to change; `None` leaves the stored value alone.
#[derive(Debug, Default, Clone)]
pub struct SettingsPatch {
    pub registration_open: Option<bool>,
    pub library_root: Option<String>,
}

pub async fn get(pool: &PgPool) -> sqlx::Result<SystemSettings> {
    sqlx::query_as!(
        SystemSettings,
        "SELECT registration_open, library_root FROM system_settings"
    )
    .fetch_one(pool)
    .await
}

pub async fn update(pool: &PgPool, patch: SettingsPatch) -> sqlx::Result<SystemSettings> {
    sqlx::query_as!(
        SystemSettings,
        r#"UPDATE system_settings
           SET registration_open = COALESCE($1, registration_open),
               library_root = COALESCE($2, library_root)
           RETURNING registration_open, library_root"#,
        patch.registration_open,
        patch.library_root
    )
    .fetch_one(pool)
    .await
}
