-- Key-value system settings; secrets encrypted with kima_core::cipher (AAD "settings_kv:" || key).
CREATE TABLE settings_kv (
    key        text PRIMARY KEY CHECK (key ~ '^[a-zA-Z][a-zA-Z0-9]{0,63}$'),
    value      jsonb,          -- plain settings
    secret_enc bytea,          -- encrypted settings
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(value, secret_enc) = 1)
);
