-- Raven's Eye (docs/ADMIN.md): each finished UTC day's platform numbers, kept once.
CREATE TABLE ravens_eye_days (
    day date PRIMARY KEY,
    stats jsonb NOT NULL,
    computed_at timestamptz NOT NULL DEFAULT now()
);
