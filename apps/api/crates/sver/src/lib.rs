use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{sync::Arc, time::Duration};
pub mod account;
pub mod activity;
pub mod alerts;
pub mod auth;
pub mod bans;
pub mod beacons;
pub mod boards;
pub mod bot;
pub mod chat;
pub mod commands;
pub mod crowd;
pub mod devapps;
pub mod discord;
pub mod discovery;
pub mod dms;
pub mod emotes;
pub mod engagement;
pub mod events;
pub mod factions;
pub mod gateway;
pub mod guilds;
pub mod integrity;
pub mod ipinfo;
pub mod jobs;
pub mod ledger;
pub mod linked_chat;
pub mod magnet;
pub mod media;
pub mod moderation;
pub mod money;
pub mod oauth;
pub mod open_data;
pub mod outside_emotes;
pub mod overlays;
pub mod parts;
pub mod payouts;
pub mod playback;
pub mod plays;
pub mod probe;
pub mod profile_import;
pub mod profile_jobs;
pub mod profiles;
pub mod progression;
pub mod raids;
pub mod rename;
pub mod reserved;
pub mod restream;
pub mod roadmap;
pub mod safety;
pub mod security;
pub mod shine;
pub mod skills;
pub mod social;
pub mod squads;
pub mod staff_console;
pub mod staff_push;
pub mod staff_streams;
pub mod streams;
pub mod stripe;
pub mod studio;
pub mod subs;
pub mod support;
pub mod surge;
pub mod switches;
pub mod take_down;
pub mod text;
pub mod tiers;
pub mod videos;
pub mod wall;

/// Endpoints an app may call with a person's bearer token (docs/DEVELOPER_PLATFORM.md).
fn app_callable(path: &str) -> bool {
    path.starts_with("/api/hooks")
        || matches!(path, "/api/me/raids" | "/api/me/stream")
        || (path.starts_with("/api/channels/")
            && ["/polls", "/close", "/marker", "/board/disabled"]
                .iter()
                .any(|end| path.ends_with(end)))
}

#[derive(Clone)]
pub struct Config {
    pub origin: String,
    pub production: bool,
    pub key: [u8; 32],
    pub turnstile_site_key: String,
    pub turnstile_secret: String,
    pub resend_key: String,
    pub mail_from: String,
    /// The postal address printed at the foot of opt-in alert emails (CAN-SPAM).
    pub mail_postal_address: String,
    pub providers: Vec<oauth::Provider>,
    pub turnstile_url: String,
    pub breach_url: String,
    pub resend_url: String,
    pub trusted_proxy: Option<std::net::IpAddr>,
    pub media: media::MediaConfig,
    pub youtube_oembed_url: String,
    pub soundcloud_oembed_url: String,
    pub thumbnail_hosts: Vec<String>,
    pub streaming: Option<streams::Config>,
    pub playback: playback::Config,
    pub integrity: integrity::Tuning,
    pub networks: ipinfo::Networks,
    pub magnet: magnet::Tuning,
    pub engagement: engagement::Tuning,
    pub factions: factions::Tuning,
    pub take_down: take_down::Config,
    pub staff_push: staff_push::Config,
    pub stripe: stripe::Config,
    pub videos: videos::Config,
    pub beacons: beacons::Config,
}
impl Config {
    pub fn from_env() -> std::result::Result<Self, String> {
        let env = |key: &str| std::env::var(key).unwrap_or_default();
        let production = match env("APP_ENV").as_str() {
            "development" => false,
            "production" => true,
            _ => return Err("APP_ENV must be development or production".into()),
        };
        let origin = env("APP_ORIGIN").trim_end_matches('/').to_string();
        let parsed = url::Url::parse(&origin).map_err(|_| "Invalid APP_ORIGIN")?;
        if parsed.origin().ascii_serialization() != origin
            || (production && parsed.scheme() != "https")
            || (!production && !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1")))
        {
            return Err("APP_ORIGIN must be an HTTPS origin (loopback only in development)".into());
        }
        let key: [u8; 32] = STANDARD
            .decode(env("ENCRYPTION_KEY"))
            .map_err(|_| "ENCRYPTION_KEY must be base64")?
            .try_into()
            .map_err(|_| "ENCRYPTION_KEY must decode to 32 bytes")?;
        let turnstile_secret = env("TURNSTILE_SECRET");
        let turnstile_site_key = env("TURNSTILE_SITE_KEY");
        let resend_key = env("RESEND_API_KEY");
        let mail_from = env("MAIL_FROM");
        // The registered DMCA agent's address on /dmca, unless the server sets another.
        let mail_postal_address = match env("MAIL_POSTAL_ADDRESS") {
            address if address.is_empty() => {
                "SVER LLC, 4030 Wake Forest Road, Suite 349, Raleigh, NC 27609".to_string()
            }
            address => address,
        };
        if turnstile_secret.is_empty() || turnstile_site_key.is_empty() {
            return Err("Turnstile keys are required".into());
        }
        if production
            && (turnstile_secret.starts_with("1x000")
                || turnstile_secret.starts_with("2x000")
                || turnstile_secret.starts_with("3x000")
                || resend_key.is_empty()
                || mail_from.is_empty())
        {
            return Err("Production requires real Turnstile and Resend configuration".into());
        }
        let trusted_proxy = if env("TRUSTED_PROXY_IP").is_empty() {
            None
        } else {
            Some(
                env("TRUSTED_PROXY_IP")
                    .parse()
                    .map_err(|_| "Invalid TRUSTED_PROXY_IP")?,
            )
        };
        let media = media::MediaConfig::from_env(production, &origin)?;
        Ok(Self {
            origin,
            production,
            key,
            turnstile_site_key,
            turnstile_secret,
            resend_key,
            mail_from,
            mail_postal_address,
            providers: oauth::providers_from_env()?,
            trusted_proxy,
            media,
            streaming: streams::Config::from_env()?,
            playback: playback::Config::from_env(production)?,
            integrity: integrity::Tuning::from_env()?,
            networks: ipinfo::Networks::from_env()?,
            magnet: magnet::Tuning::from_env()?,
            engagement: engagement::Tuning::from_env()?,
            factions: factions::Tuning::from_env(production)?,
            take_down: take_down::Config::from_env(),
            staff_push: staff_push::Config::from_env(),
            stripe: stripe::Config::from_env(production)?,
            videos: videos::Config::from_env(production)?,
            beacons: beacons::Config::from_env(production)?,
            youtube_oembed_url: "https://www.youtube.com/oembed".into(),
            soundcloud_oembed_url: "https://soundcloud.com/oembed".into(),
            thumbnail_hosts: vec!["ytimg.com".into(), "sndcdn.com".into()],
            turnstile_url: "https://challenges.cloudflare.com/turnstile/v0/siteverify".into(),
            breach_url: "https://api.pwnedpasswords.com/range/".into(),
            resend_url: "https://api.resend.com/emails".into(),
        })
    }
    pub fn cookie_name(&self) -> &'static str {
        if self.production {
            "__Host-sver"
        } else {
            "sver_dev"
        }
    }
}
#[derive(Clone)]
pub struct App {
    pub db: PgPool,
    pub config: Arc<Config>,
    pub http: reqwest::Client,
    pub hashing: Arc<tokio::sync::Semaphore>,
    pub dummy_hash: Arc<String>,
    pub chat: chat::Hub,
}
impl App {
    pub async fn new(db: PgPool, config: Config) -> Result<Self> {
        Ok(Self {
            db,
            config: Arc::new(config),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("SVER/2.0")
                .build()
                .map_err(|_| Error::internal())?,
            hashing: Arc::new(tokio::sync::Semaphore::new(4)),
            dummy_hash: Arc::new(security::hash_sync(&security::token())?),
            chat: chat::Hub::default(),
        })
    }
}
pub async fn connect(database_url: &str) -> std::result::Result<PgPool, String> {
    let db = PgPoolOptions::new()
        .max_connections(12)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
        .map_err(|_| "Could not connect to the isolated database")?;
    let legacy: bool = sqlx::query_scalar("SELECT to_regclass('public.\"User\"') IS NOT NULL OR to_regclass('public._prisma_migrations') IS NOT NULL").fetch_one(&db).await.map_err(|_| "Database safety check failed")?;
    if legacy {
        return Err("Refusing to migrate a legacy database".into());
    }
    sqlx::migrate!("../../../../migrations")
        .run(&db)
        .await
        .map_err(|e| format!("Login migration failed: {e}"))?;
    Ok(db)
}
#[derive(Debug)]
pub struct Error(pub StatusCode, pub &'static str, pub Option<i64>);
impl Error {
    pub fn bad(message: &'static str) -> Self {
        Self(StatusCode::BAD_REQUEST, message, None)
    }
    pub fn auth() -> Self {
        Self(StatusCode::UNAUTHORIZED, "Sign in to continue.", None)
    }
    pub fn denied(message: &'static str) -> Self {
        Self(StatusCode::FORBIDDEN, message, None)
    }
    pub fn unavailable() -> Self {
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "This service is temporarily unavailable. Please try again.",
            None,
        )
    }
    pub fn internal() -> Self {
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The request could not be completed.",
            None,
        )
    }
}
impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        if e.as_database_error()
            .is_some_and(|e| e.is_unique_violation())
        {
            return Self::bad("That email, username or linked identity is already in use.");
        }
        eprintln!(
            "Database request failed ({})",
            e.as_database_error()
                .and_then(|e| e.code())
                .map(|s| s.into_owned())
                .unwrap_or_else(|| "connection or query".into())
        );
        Self::internal()
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut response = (self.0, Json(json!({"error": self.1}))).into_response();
        if let Some(seconds) = self.2 {
            response
                .headers_mut()
                .insert("retry-after", seconds.to_string().parse().unwrap());
        }
        response
    }
}
pub type Result<T> = std::result::Result<T, Error>;
async fn boundaries(State(app): State<App>, req: Request, next: Next) -> Response {
    let media_hook = matches!(
        req.uri().path(),
        "/api/internal/srs/publish"
            | "/api/internal/srs/unpublish"
            | "/api/internal/srs/segment"
            | "/api/internal/streams/playback"
    );
    if media_hook {
        let Some(peer) = req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        else {
            return Error::denied("Invalid media callback.").into_response();
        };
        if let Err(error) = streams::authorize_hook(&app, peer.0, req.headers()) {
            return error.into_response();
        }
    } else if !matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    )
        // The one-click email unsubscribe (RFC 8058) is posted by mail providers without an
        // Origin; its encrypted token is the only authority and it only turns go-live email off.
        && req.uri().path() != "/api/notifications/unsubscribe"
        // Stripe posts webhooks without an Origin; the signature is checked in support::webhook.
        && req.uri().path() != "/api/stripe/webhook"
        // Twitch EventSub posts without an Origin; its HMAC signature is checked in linked_chat.
        && req.uri().path() != "/api/integrations/twitch/eventsub"
        // OAuth clients post tokens server to server; the client and PKCE checks are in devapps.
        && !matches!(req.uri().path(), "/api/oauth/token" | "/api/oauth/revoke" | "/api/oauth/device")
        // Apps call these with a bearer token (never a cookie); devapps::actor and
        // events::hook_owner then use only the token.
        && !(app_callable(req.uri().path())
            && req.headers().get("authorization").and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("Bearer ")))
        && req.headers().get("origin").and_then(|v| v.to_str().ok())
        != Some(&app.config.origin)
    {
        return Error::denied("Invalid request origin.").into_response();
    }
    // A banned account keeps only security, standing and appeal writes (bans::blocked_write).
    if !media_hook
        && !matches!(
            *req.method(),
            axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
        )
        && bans::blocked_write(
            &app,
            req.uri().path(),
            &axum_extra::extract::cookie::CookieJar::from_headers(req.headers()),
        )
        .await
    {
        return Error::denied(
            "Your account is banned. You can still manage account security, view your standing and appeal.",
        )
        .into_response();
    }
    // Stored media keys are content-addressed (a hash of the kind, crop and source bytes), so a
    // served object never changes: only successful /api/media responses may be cached.
    let media = req.uri().path().starts_with("/api/media/");
    let mut response = next.run(req).await;
    let cache = if media && response.status() == StatusCode::OK {
        "public, max-age=31536000, immutable"
    } else {
        "no-store"
    };
    for (key, value) in [
        ("cache-control", cache),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
    ] {
        response.headers_mut().insert(key, value.parse().unwrap());
    }
    response
}
pub fn router(app: App) -> Router {
    Router::new()
        .merge(videos::routes())
        .merge(beacons::routes())
        .route(
            "/api/health",
            get(|| async { Json(json!({"status":"ok"})) }),
        )
        .route("/api/auth/config", get(auth::public_config))
        .route("/api/roadmap", get(roadmap::read))
        .route(
            "/api/auth/username-availability",
            get(auth::username_availability),
        )
        .route("/api/auth/signup", post(auth::signup))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/mfa/login", post(auth::mfa_login))
        .route("/api/auth/me", get(auth::me))
        .route("/api/auth/logout", post(auth::logout))
        .route(
            "/api/auth/sessions",
            get(auth::sessions).delete(auth::revoke_all),
        )
        .route(
            "/api/auth/sessions/{id}",
            axum::routing::delete(auth::revoke_session),
        )
        .route("/api/auth/reauth", post(auth::reauth))
        .route("/api/auth/email/resend", post(auth::resend))
        .route("/api/auth/email/verify", post(auth::verify_email))
        .route("/api/auth/password/forgot", post(auth::forgot))
        .route("/api/auth/password/reset", post(auth::reset))
        .route("/api/auth/mfa/setup", post(auth::mfa_setup))
        .route("/api/auth/mfa/enable", post(auth::mfa_enable))
        .route("/api/auth/mfa/disable", post(auth::mfa_disable))
        .route("/api/auth/mfa/recovery", post(auth::mfa_recovery))
        .route("/api/auth/account/delete", post(auth::delete_account))
        .route("/api/auth/account/restore", post(auth::restore_account))
        .route(
            "/api/auth/streaming-eligibility",
            get(auth::streaming_eligibility),
        )
        .route("/api/auth/oauth/{provider}/start", post(oauth::start))
        .route(
            "/api/auth/oauth/signup",
            get(oauth::signup_details).post(oauth::finish_signup),
        )
        .route("/api/auth/oauth/{provider}/callback", get(oauth::callback))
        .route(
            "/api/auth/oauth/{provider}",
            axum::routing::delete(oauth::unlink),
        )
        .merge(profile_routes())
        .merge(streams::routes())
        .merge(playback::routes())
        .merge(plays::routes())
        .merge(guilds::routes())
        .merge(squads::routes())
        .merge(chat::routes())
        .merge(emotes::routes())
        .merge(switches::routes())
        .merge(staff_console::routes())
        .merge(money::routes())
        .merge(outside_emotes::routes())
        .merge(discord::routes())
        .merge(overlays::routes())
        .merge(alerts::routes())
        .merge(raids::routes())
        .merge(discovery::routes())
        .merge(magnet::routes())
        .merge(staff_streams::routes())
        .merge(factions::routes())
        .merge(moderation::routes())
        .merge(bans::routes())
        .merge(integrity::routes())
        .merge(take_down::routes())
        .merge(staff_push::routes())
        .merge(support::routes())
        .merge(subs::routes())
        .merge(open_data::routes())
        .merge(progression::routes())
        .merge(account::routes())
        .merge(restream::routes())
        .merge(linked_chat::routes())
        .merge(commands::routes())
        .merge(bot::routes())
        .merge(dms::routes())
        .merge(devapps::routes())
        .merge(events::routes())
        .merge(engagement::routes())
        .merge(tiers::routes())
        .merge(payouts::routes())
        .merge(shine::routes())
        .merge(boards::routes())
        .merge(crowd::routes())
        .merge(skills::routes())
        .merge(surge::routes())
        .merge(gateway::routes())
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), boundaries))
        .with_state(app)
}
/// Module 2 routes (docs/PROFILES.md, "API"). Upload routes carry their own 11 MB body limit and
/// larger JSON lists a 64 KB limit; everything else keeps Login's 16 KB default.
fn profile_routes() -> Router<App> {
    let upload = DefaultBodyLimit::max(media::UPLOAD_LIMIT);
    let lists = DefaultBodyLimit::max(64 * 1024);
    use crate::{
        media as m, profiles as p, rename as r, safety as sa, social as so, studio as st, wall as w,
    };
    Router::new()
        .route("/api/channels/{username}", get(p::channel))
        .route("/api/channels/{username}/resolve", get(p::resolve_channel))
        .route(
            "/api/channels/{username}/wall",
            get(w::wall).post(w::create_post),
        )
        .route(
            "/api/channels/{username}/schedule",
            get(st::channel_schedule),
        )
        .route("/api/channels/{username}/about", get(st::channel_about))
        .route(
            "/api/channels/{username}/activity",
            get(crate::activity::channel_activity),
        )
        .route(
            "/api/channels/{username}/fan-art",
            get(st::channel_fan_art)
                .post(st::submit_fan_art)
                .layer(upload),
        )
        .route("/api/channels/{username}/followers", get(so::followers))
        .route("/api/channels/{username}/following", get(so::following))
        .route("/api/users/{username}/card", get(so::card))
        .route(
            "/api/follows/{username}",
            put(so::follow).delete(so::unfollow),
        )
        .route("/api/me/following", get(so::my_following))
        .route("/api/me/suggestions", get(so::suggestions))
        .route(
            "/api/me/profile",
            get(p::my_profile).patch(p::update_profile),
        )
        .route("/api/me/username", post(r::rename))
        .route("/api/me/links", put(p::update_links))
        .route(
            "/api/me/avatar",
            post(m::upload_avatar)
                .delete(m::remove_avatar)
                .layer(upload),
        )
        .route(
            "/api/me/banner",
            post(m::upload_banner)
                .delete(m::remove_banner)
                .layer(upload),
        )
        .route(
            "/api/me/song",
            get(st::my_song).put(st::save_song).delete(st::delete_song),
        )
        .route("/api/me/song/preview", post(st::song_preview))
        .route(
            "/api/me/war-council",
            get(so::my_war_council).put(so::save_war_council),
        )
        .route("/api/me/war-council/search", get(so::war_council_search))
        .route("/api/me/wall", get(w::my_wall))
        .route("/api/me/wall/settings", put(w::save_settings))
        .route("/api/me/wall/pins", put(w::save_pins))
        .route("/api/me/wall/pending", get(w::pending))
        .route(
            "/api/wall/posts/{id}/replies",
            get(w::replies).post(w::create_reply),
        )
        .route(
            "/api/wall/posts/{id}/like",
            put(w::like_post).delete(w::unlike_post),
        )
        .route("/api/wall/posts/{id}", delete(w::delete_post))
        .route("/api/wall/replies/{id}", delete(w::delete_reply))
        .route("/api/wall/{kind}/{id}/{action}", post(w::review))
        .route(
            "/api/me/schedule",
            get(st::my_schedule).put(st::save_schedule).layer(lists),
        )
        .route(
            "/api/me/sponsors",
            get(st::my_sponsors).put(st::save_sponsors).layer(lists),
        )
        .route(
            "/api/me/sponsors/{id}/logo",
            post(st::sponsor_logo)
                .delete(st::delete_sponsor_logo)
                .layer(upload),
        )
        .route(
            "/api/me/setup",
            get(st::my_setup).put(st::save_setup).layer(lists),
        )
        .route(
            "/api/me/setup/photos",
            post(st::upload_setup_photo)
                .put(st::save_setup_photos)
                .layer(upload),
        )
        .route("/api/me/setup/photos/{id}", delete(st::delete_setup_photo))
        .route("/api/me/header", get(st::my_header).put(st::save_header))
        .route(
            "/api/me/readiness",
            get(st::my_readiness).put(st::save_readiness),
        )
        .route("/api/me/link-suggestions", get(p::link_suggestions))
        .route("/api/me/card-settings", put(p::card_settings))
        // `GET /me/blocks` is the user-block list, so the custom page blocks read from
        // `/me/page-blocks`; `PUT /me/blocks` saves page blocks as the spec lists.
        .route(
            "/api/me/blocks",
            get(so::my_blocks).put(st::save_blocks).layer(lists),
        )
        .route("/api/me/page-blocks", get(st::my_blocks))
        .route("/api/me/fan-art", get(st::my_fan_art))
        .route("/api/me/fan-art/settings", put(st::fan_art_settings))
        .route("/api/me/fan-art/pending", get(st::pending_fan_art))
        .route("/api/fan-art/{id}", delete(st::delete_fan_art))
        .route("/api/fan-art/{id}/{action}", post(st::review_fan_art))
        .route("/api/blocks/{username}", put(so::block).delete(so::unblock))
        .route("/api/reports", post(sa::report))
        .route("/api/me/reports", get(sa::my_reports))
        .route("/api/me/reports/seen", post(sa::reports_seen))
        .route("/api/me/reports/email", put(sa::reports_email))
        .route("/api/me/alerts", get(sa::alerts))
        .route("/api/me/standing", get(sa::standing))
        .route("/api/me/strikes/{id}/acknowledge", post(sa::acknowledge))
        .route("/api/me/strikes/{id}/appeal", post(sa::appeal))
        .route("/api/admin/reports", get(sa::admin_reports))
        .route(
            "/api/admin/reports/{target_type}/{target_id}/actions",
            post(sa::admin_action),
        )
        .route(
            "/api/admin/users/{username}/standing",
            get(sa::admin_standing),
        )
        .route(
            "/api/admin/users/{username}/username-reset",
            post(crate::rename::staff_reset),
        )
        .route(
            "/api/admin/users/{username}/strikes",
            post(sa::admin_strike),
        )
        .route(
            "/api/admin/users/{username}/interim-restriction",
            post(sa::admin_interim).delete(sa::admin_interim_lift),
        )
        .route(
            "/api/admin/users/{username}/restriction/lift",
            post(sa::admin_lift),
        )
        .route("/api/admin/confirm", post(sa::staff_confirm))
        .route("/api/admin/appeals", get(sa::admin_appeals))
        .route("/api/admin/parts", get(parts::admin_queue))
        .route("/api/admin/parts/{id}/decision", post(parts::admin_decide))
        .route("/api/parts", get(parts::search))
        .route("/api/admin/appeals/{id}/decision", post(sa::decide))
        .route("/api/admin/moderation-actions", get(sa::admin_actions))
        .route("/api/admin/{*rest}", get(admin_missing).post(admin_missing))
        .route("/api/media/{*key}", get(m::serve_local))
        .route("/api/me/rename-status", get(rename_status))
}
async fn admin_missing() -> Response {
    StatusCode::NOT_FOUND.into_response()
}
async fn rename_status(
    State(app): State<App>,
    jar: axum_extra::extract::cookie::CookieJar,
) -> profiles::Res<Json<serde_json::Value>> {
    let user = profiles::signed_in(&app, &jar).await?;
    let mut db = app.db.acquire().await?;
    Ok(Json(rename::status(&mut db, &user).await?))
}
pub fn user_agent(headers: &HeaderMap) -> String {
    headers
        .get("user-agent")
        .and_then(|s| s.to_str().ok())
        .unwrap_or("Unknown device")
        .chars()
        .take(250)
        .collect()
}
