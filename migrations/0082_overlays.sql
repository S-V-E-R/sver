-- Native alerts and overlays (docs/OVERLAYS.md): the browser source's private link and settings.
CREATE TABLE overlay_settings (
    channel_id text PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    token_hash text UNIQUE,
    settings jsonb,
    seen_at timestamptz
);
