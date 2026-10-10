use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, Path, State},
    http::{Request, StatusCode},
    routing::{delete, get, post},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use sver::{App, Config, security as sec, streams};
use tower::ServiceExt;

#[path = "streams/alerts.rs"]
mod alerts;
#[path = "streams/bans.rs"]
mod bans;
#[path = "streams/beacons.rs"]
mod beacons;
#[path = "streams/boards.rs"]
mod boards;
#[path = "streams/chat.rs"]
mod chat;
#[path = "streams/chat_social.rs"]
mod chat_social;
#[path = "streams/commands.rs"]
mod commands;
#[path = "streams/crowd.rs"]
mod crowd;
#[path = "streams/devapps.rs"]
mod devapps;
#[path = "streams/discord.rs"]
mod discord;
#[path = "streams/discovery.rs"]
mod discovery;
#[path = "streams/dms.rs"]
mod dms;
#[path = "streams/engagement.rs"]
mod engagement;
#[path = "streams/events.rs"]
mod events;
#[path = "streams/gateway.rs"]
mod gateway;
#[path = "streams/gifs.rs"]
mod gifs;
#[path = "streams/integrity.rs"]
mod integrity;
#[path = "streams/linked_chat.rs"]
mod linked_chat;
#[path = "streams/magnet.rs"]
mod magnet;
#[path = "streams/moderation.rs"]
mod moderation;
#[path = "streams/moments.rs"]
mod moments;
#[path = "streams/outside_emotes.rs"]
mod outside_emotes;
#[path = "streams/overlays.rs"]
mod overlays;
#[path = "streams/playback.rs"]
mod playback;
#[path = "streams/plays.rs"]
mod plays;
#[path = "streams/raids.rs"]
mod raids;
#[path = "streams/real_media.rs"]
mod real_media;
#[path = "streams/reports.rs"]
mod reports;
#[path = "streams/resets.rs"]
mod resets;
#[path = "streams/rest.rs"]
mod rest;
#[path = "streams/restream.rs"]
mod restream;
#[path = "streams/staff_streams.rs"]
mod staff_streams;
#[path = "streams/staff_window.rs"]
mod staff_window;
#[path = "streams/subs.rs"]
mod subs;
#[path = "streams/support.rs"]
mod support;
#[path = "streams/switches.rs"]
mod switches;
#[path = "streams/teams.rs"]
mod teams;
#[path = "streams/videos.rs"]
mod videos;

#[derive(Default)]
struct Media {
    service: String,
    streams: Vec<Value>,
    failed: bool,
    acknowledge_only: bool,
    kicked: Vec<String>,
    /// Synthetic Stripe: every API call (path, form body), and the Connect account GET returns.
    stripe: Vec<(String, String)>,
    account: Value,
    /// What GET /v1/checkout/sessions/{id} returns.
    session: Value,
    /// Board webhooks received: (signature header, body); `hook_fail` answers 500.
    hooks: Vec<(String, String)>,
    hook_fail: bool,
}
async fn board_hook(
    State(fake): State<Fake>,
    headers: axum::http::HeaderMap,
    body: String,
) -> StatusCode {
    let mut m = fake.lock().unwrap();
    if m.hook_fail {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    let signature = headers["sver-signature"].to_str().unwrap().to_string();
    m.hooks.push((signature, body));
    StatusCode::NO_CONTENT
}
type Fake = Arc<Mutex<Media>>;
async fn versions(State(fake): State<Fake>) -> (StatusCode, Json<Value>) {
    let m = fake.lock().unwrap();
    (
        if m.failed {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::OK
        },
        Json(json!({"code":0,"server":"test-server","service":m.service})),
    )
}
async fn inventory(State(fake): State<Fake>) -> (StatusCode, Json<Value>) {
    let m = fake.lock().unwrap();
    (
        if m.failed {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::OK
        },
        Json(json!({"code":0,"server":"test-server","service":m.service,"streams":m.streams})),
    )
}
async fn kick(State(fake): State<Fake>, Path(client): Path<String>) -> (StatusCode, Json<Value>) {
    let mut m = fake.lock().unwrap();
    if m.failed {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"code":1})));
    }
    if !m.acknowledge_only {
        m.streams.retain(|s| s["publish"]["cid"] != client);
    }
    m.kicked.push(client);
    (
        StatusCode::OK,
        Json(json!({"code":0,"server":"test-server","service":m.service})),
    )
}
async fn stripe(
    State(fake): State<Fake>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: String,
) -> Json<Value> {
    let mut m = fake.lock().unwrap();
    let path = uri.path().to_string();
    // Only calls that create something are logged (and number the synthetic ids).
    if method == axum::http::Method::POST {
        // Calls made on behalf of a connected account carry it, for the assertions.
        let account = headers
            .get("stripe-account")
            .and_then(|v| v.to_str().ok())
            .map(|a| format!("|account={a}"))
            .unwrap_or_default();
        m.stripe.push((path.clone(), format!("{body}{account}")));
    }
    let n = m.stripe.len();
    Json(match path.as_str() {
        "/v1/checkout/sessions" => {
            json!({"id": format!("cs_test_{n}"), "url": format!("https://checkout.stripe.test/{n}")})
        }
        "/v1/accounts" => json!({"id": "acct_test_owner"}),
        "/v1/transfers" => json!({"id": format!("tr_test_{n}")}),
        "/v1/payouts" => json!({"id": format!("po_test_{n}")}),
        "/v1/account_links" => json!({"url": "https://connect.stripe.test/onboarding"}),
        p if p.ends_with("/login_links") => json!({"url": "https://connect.stripe.test/express"}),
        "/v1/invoice_payments" => {
            let invoice = uri
                .query()
                .unwrap_or_default()
                .trim_start_matches("invoice=");
            json!({"data": [{"payment": {"payment_intent": format!("pi_{invoice}")}}]})
        }
        p if p.starts_with("/v1/subscriptions/") => {
            json!({"items": {"data": [{"id": "si_test", "price": {"product": "prod_test"}}]}})
        }
        p if p.starts_with("/v1/checkout/sessions/") => m.session.clone(),
        _ => m.account.clone(),
    })
}
struct Env {
    app: App,
    fake: Fake,
    cookie: String,
    proof: AtomicUsize,
}
impl Env {
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Value,
        cookie: bool,
        origin: bool,
        hook: bool,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .extension(ConnectInfo(
                "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
            ));
        if cookie {
            builder = builder.header("cookie", format!("sver_dev={}", self.cookie));
        }
        if origin {
            builder = builder.header("origin", &self.app.config.origin);
        }
        if hook {
            builder = builder.header(
                "x-srs-secret",
                &self.app.config.streaming.as_ref().unwrap().hook_secret,
            );
        }
        let response = sver::router(self.app.clone())
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        if status.is_success() {
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    async fn call(&self, method: &str, path: &str, body: Value) -> Value {
        let (status, value) = self.request(method, path, body, true, true, false).await;
        assert_eq!(status, StatusCode::OK, "{method} {path} failed");
        value
    }
    async fn code(&self) -> String {
        let code = format!("RECOVERY{:08}", self.proof.fetch_add(1, Ordering::Relaxed));
        sqlx::query(
            "INSERT INTO recovery_codes(id,user_id,code_hash) VALUES($1,'stream-owner',$2)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(sec::digest(&code))
        .execute(&self.app.db)
        .await
        .unwrap();
        code
    }
    async fn key(&self, action: &str) -> String {
        let code = self.code().await;
        let path = if action == "create" {
            "/api/me/stream/key".into()
        } else {
            format!("/api/me/stream/key/{action}")
        };
        self.call("POST", &path, json!({"code":code})).await["key"]
            .as_str()
            .unwrap()
            .into()
    }
    fn payload(&self, key: &str, client: &str, action: &str) -> Value {
        let (public_id, query) = key.split_once('?').unwrap_or((key, ""));
        json!({"action":format!("on_{action}"),"server_id":"test-server","service_id":self.fake.lock().unwrap().service,
            "client_id":client,"vhost":"__defaultVhost__","app":"rebuild","stream":public_id,"param":format!("?{query}")})
    }
    async fn hook(&self, key: &str, client: &str, action: &str) -> StatusCode {
        self.request(
            "POST",
            &format!("/api/internal/srs/{action}"),
            self.payload(key, client, action),
            false,
            false,
            true,
        )
        .await
        .0
    }
    fn publishing(&self, key: &str, client: &str, bytes: i64) {
        self.fake.lock().unwrap().streams = vec![
            json!({"name":key.split('?').next().unwrap(),"app":"rebuild",
            "publish":{"active":true,"cid":client},"recv_bytes":bytes,"kbps":{"recv_30s":9000},
            "video":{"codec":"H264","width":1920,"height":1080},"audio":{"codec":"AAC"}}),
        ];
    }
    async fn mine(&self) -> Value {
        self.call("GET", "/api/me/stream", Value::Null).await
    }
    async fn sql(&self, query: impl sqlx::SqlSafeStr) {
        sqlx::query(query).execute(&self.app.db).await.unwrap();
    }
}

async fn isolated_database() -> (sqlx::PgPool, sqlx::PgPool, String) {
    let database_url =
        std::env::var("DATABASE_URL").expect("Use the isolated local development database");
    let parsed = url::Url::parse(&database_url).unwrap();
    assert!(
        matches!(parsed.host_str(), Some("localhost" | "127.0.0.1"))
            && parsed.path() == "/sver_rebuild"
    );
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("streams_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await
        .unwrap();
    let statement = format!("SET search_path TO {schema}");
    let db = PgPoolOptions::new()
        .max_connections(12)
        .after_connect(move |conn, _| {
            let statement = statement.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(statement))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&database_url)
        .await
        .unwrap();
    sqlx::migrate!("../../../../migrations")
        .run(&db)
        .await
        .unwrap();
    (admin, db, schema)
}

async fn synthetic_owner(app: App, fake: Fake) -> Env {
    let cookie = sec::token();
    sqlx::query("INSERT INTO users(id,email,username,email_verified,mfa_enabled,mfa_secret,date_of_birth) VALUES('stream-owner','stream@example.test','Streamer',true,true,$1,'1990-01-01')")
        .bind(sec::seal(&app,"totp:stream-owner","JBSWY3DPEHPK3PXP").unwrap()).execute(&app.db).await.unwrap();
    sqlx::query("INSERT INTO sessions(id,user_id,token_hash,auth_version,mfa_verified,user_agent) SELECT 'stream-session',id,$1,auth_version,true,'synthetic' FROM users WHERE id='stream-owner'")
        .bind(sec::digest(&cookie)).execute(&app.db).await.unwrap();
    Env {
        app,
        fake,
        cookie,
        proof: AtomicUsize::new(0),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streaming_lifecycle_and_security() {
    let (admin, db, schema) = isolated_database().await;
    let fake = Arc::new(Mutex::new(Media {
        service: "boot-one".into(),
        ..Default::default()
    }));
    let media = Router::new()
        .route("/api/v1/versions", get(versions))
        .route(
            "/api/v1/streams",
            get(|| async { axum::response::Redirect::to("/api/v1/streams/") }),
        )
        .route("/api/v1/streams/", get(inventory))
        .route("/api/v1/clients/{client}", delete(kick))
        .route(
            "/range/{prefix}",
            get(|| async { "00000000000000000000000000000000000:0" }),
        )
        // Synthetic Turnstile: the token "pass" succeeds, anything else fails.
        .route(
            "/turnstile",
            post(|body: String| async move {
                Json(json!({"success": body.split('&').any(|kv| kv == "response=pass")}))
            }),
        )
        .route("/v1/checkout/sessions", post(stripe))
        .route("/v1/accounts", post(stripe))
        .route("/v1/account_links", post(stripe))
        .route("/v1/accounts/{id}", get(stripe))
        .route("/v1/checkout/sessions/{id}", get(stripe))
        .route("/v1/invoice_payments", get(stripe))
        .route("/v1/transfers", post(stripe))
        .route("/v1/payouts", post(stripe))
        .route("/v1/refunds", post(stripe))
        .route("/v1/subscriptions/{id}", get(stripe).post(stripe))
        .route("/v1/accounts/{id}/login_links", post(stripe))
        .route("/hook", post(board_hook))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move { axum::serve(listener, media).await.unwrap() });
    let mut config = Config::from_env().unwrap();
    config.resend_key.clear();
    config.breach_url = format!("http://{address}/range/");
    config.turnstile_url = format!("http://{address}/turnstile");
    config.streaming = Some(streams::Config {
        api_url: format!("http://{address}"),
        ingest_url: "rtmp://127.0.0.1:1935/rebuild".into(),
        whip_url: Some("https://media.example/rebuild/whip/".into()),
        srt_url: Some("srt://media.example:10081".into()),
        hook_secret: "synthetic-hook-secret-only-for-this-test".into(),
        hook_ip: "127.0.0.1".parse().unwrap(),
        vhost: "__defaultVhost__".into(),
        app: "rebuild".into(),
    });
    config.stripe = sver::stripe::Config {
        secret_key: "sk_test_synthetic".into(),
        webhook_secret: "whsec_synthetic".into(),
        api_url: format!("http://{address}"),
    };
    config.playback = sver::playback::Config {
        hls_url: Some("https://media.example/rebuild".into()),
        whep_url: Some("https://media.example/rtc/v1/whep".into()),
        ..Default::default()
    };
    let app = App::new(db.clone(), config).await.unwrap();
    let env = synthetic_owner(app, fake).await;
    // A spawned task catches assertion panics so the disposable schema is still cleaned.
    let result = tokio::spawn(async move { exercise(&env).await }).await;
    upstream.abort();
    db.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    result.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discord_bot() {
    let (admin, db, schema) = isolated_database().await;
    let mut config = Config::from_env().unwrap();
    config.resend_key.clear();
    config.providers = vec![sver::oauth::Provider {
        name: "discord".into(),
        client_id: "test-client".into(),
        client_secret: "test-only-secret".into(),
        authorize_url: "https://discord.example/authorize".into(),
        token_url: "https://discord.example/token".into(),
        profile_url: "https://discord.example/me".into(),
        scopes: "identify".into(),
        pkce: true,
        redirect_uri: None,
    }];
    let app = App::new(db.clone(), config).await.unwrap();
    let env = synthetic_owner(app, Arc::new(Mutex::new(Media::default()))).await;
    let result = tokio::spawn(async move { discord::exercise(&env).await }).await;
    db.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    result.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn third_party_emotes() {
    let (admin, db, schema) = isolated_database().await;
    let mut config = Config::from_env().unwrap();
    config.resend_key.clear();
    let media_dir =
        std::env::temp_dir().join(format!("sver-outside-media-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&media_dir).unwrap();
    config.media = sver::media::MediaConfig {
        storage: sver::media::Storage::Filesystem(media_dir.clone()),
        public_base: format!("{}/api/media", config.origin),
    };
    let app = App::new(db.clone(), config).await.unwrap();
    let env = synthetic_owner(app, Arc::new(Mutex::new(Media::default()))).await;
    let dir = media_dir.clone();
    let result = tokio::spawn(async move { outside_emotes::exercise(&env, &dir).await }).await;
    db.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    std::fs::remove_dir_all(media_dir).unwrap();
    result.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guilds_and_squads() {
    let (admin, db, schema) = isolated_database().await;
    let mut config = Config::from_env().unwrap();
    config.resend_key.clear();
    let media_dir = std::env::temp_dir().join(format!("sver-teams-media-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&media_dir).unwrap();
    config.media = sver::media::MediaConfig {
        storage: sver::media::Storage::Filesystem(media_dir.clone()),
        public_base: format!("{}/api/media", config.origin),
    };
    let app = App::new(db.clone(), config).await.unwrap();
    let env = synthetic_owner(app, Arc::new(Mutex::new(Media::default()))).await;
    let result = tokio::spawn(async move { teams::exercise(&env).await }).await;
    db.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    std::fs::remove_dir_all(media_dir).unwrap();
    result.unwrap();
}
async fn exercise(e: &Env) {
    playback::exercise(e).await;
    chat::exercise(e).await;
    support::exercise(e).await;
    subs::exercise(e).await;
    engagement::exercise(e).await;
    boards::exercise(e).await;
    crowd::exercise(e).await;
    moments::exercise(e).await;
    gateway::exercise(e).await;
    rest::exercise(e).await;
    restream::exercise(e).await;
    linked_chat::exercise(e).await;
    commands::exercise(e).await;
    dms::exercise(e).await;
    devapps::exercise(e).await;
    events::exercise(e).await;
    overlays::exercise(e).await;
    moderation::exercise(e).await;
    gifs::exercise(e).await;
    chat_social::exercise(e).await;
    reports::exercise(e).await;
    bans::exercise(e).await;
    resets::exercise(e).await;
    integrity::exercise(e).await;
    staff_window::exercise(e).await;
    switches::exercise(e).await;
    alerts::exercise(e).await;
    raids::exercise(e).await;
    staff_streams::exercise(e).await;
    discovery::exercise(e).await;
    plays::exercise(e).await;
    magnet::exercise(e).await;
    // Live events: each flow above wrote its topic (docs/DEVELOPER_PLATFORM.md section 2).
    for kind in ["moderation", "raid:incoming"] {
        let written: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM events WHERE topic LIKE 'channel:%:'||$1)",
        )
        .bind(kind)
        .fetch_one(&e.app.db)
        .await
        .unwrap();
        assert!(written, "no {kind} event");
    }
    let forged = Request::builder()
        .method("POST")
        .uri("/api/internal/srs/publish")
        .header(
            "x-srs-secret",
            &e.app.config.streaming.as_ref().unwrap().hook_secret,
        )
        .header("x-real-ip", "127.0.0.1")
        .header("content-type", "application/json")
        .extension(ConnectInfo(
            "127.0.0.2:12345".parse::<SocketAddr>().unwrap(),
        ))
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(
        sver::router(e.app.clone())
            .oneshot(forged)
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN,
        "Forwarded IP cannot impersonate the hook proxy"
    );
    assert_eq!(
        e.request("GET", "/api/me/stream", Value::Null, false, true, false)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        e.request(
            "POST",
            "/api/me/stream/stop",
            Value::Null,
            true,
            false,
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let state = e.mine().await;
    assert_eq!(state["eligible"], true);
    assert!(state["credential"].is_null());
    let categories = e.call("GET", "/api/categories", Value::Null).await;
    assert!(
        !categories
            .to_string()
            .to_lowercase()
            .contains("just chatting")
    );
    let code = e.code().await;
    assert_eq!(
        e.request(
            "POST",
            "/api/me/stream/key",
            json!({"code":code}),
            true,
            false,
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    e.sql("UPDATE sessions SET authenticated_at=now()-interval '6 minutes'")
        .await;
    assert_eq!(
        e.request(
            "POST",
            "/api/me/stream/key",
            json!({"code":code}),
            true,
            true,
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    e.sql("UPDATE sessions SET authenticated_at=now()").await;
    let created = e
        .call("POST", "/api/me/stream/key", json!({"code":code}))
        .await;
    let key = created["key"].as_str().unwrap().to_string();
    // WHIP keeps the key out of the URL (bearer token); SRT carries it in the stream ID.
    let (public_id, secret) = key.split_once("?key=").unwrap();
    assert_eq!(
        created["whip"],
        json!({"url": format!("https://media.example/rebuild/whip/?app=rebuild&stream={public_id}"), "token": secret})
    );
    assert_eq!(
        created["srt"],
        format!(
            "srt://media.example:10081?streamid=#!::r=rebuild/{public_id}?key={secret},m=publish"
        )
    );
    assert_eq!(
        e.request(
            "POST",
            "/api/me/stream/key/reveal",
            json!({"code":code}),
            true,
            true,
            false
        )
        .await
        .0,
        StatusCode::BAD_REQUEST,
        "MFA proof replay"
    );
    assert_eq!(e.key("reveal").await, key);
    let saved: (String, String) =
        sqlx::query_as("SELECT secret_hash,secret_cipher FROM stream_credentials")
            .fetch_one(&e.app.db)
            .await
            .unwrap();
    let secret = key.split("key=").nth(1).unwrap();
    assert_eq!(saved.0, sec::digest(secret));
    assert!(!saved.1.contains(secret));
    assert!(sec::unseal(&e.app, "stream-key:another-owner:1", &saved.1).is_err());
    assert!(!e.mine().await.to_string().contains(secret));
    assert_eq!(
        e.hook(&key, "first", "publish").await,
        StatusCode::FORBIDDEN,
        "category required"
    );
    for title in ["".to_string(), "x".repeat(141), "bad\u{0001}title".into()] {
        assert_eq!(
            e.request(
                "PATCH",
                "/api/me/stream",
                json!({"title":title,"category_id":"coding","revision":0}),
                true,
                true,
                false
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    e.call(
        "PATCH",
        "/api/me/stream",
        json!({"title":"Building together","category_id":"coding","revision":0}),
    )
    .await;
    assert_eq!(
        e.request(
            "PATCH",
            "/api/me/stream",
            json!({"title":"Stale","category_id":"coding","revision":0}),
            true,
            true,
            false
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // Channel editors: title, category, language and Mature on; never Mature off, never the key;
    // every edit is attributed and removal is immediate.
    let editor = chat::person(e, "stream-editor", "StreamEditor", true).await;
    let appointed = e
        .call(
            "POST",
            "/api/me/editors",
            json!({"username":"StreamEditor"}),
        )
        .await;
    assert_eq!(
        appointed["editors"][0]["username"], "StreamEditor",
        "{appointed}"
    );
    let (_, editing) = chat::call(e, "GET", "/api/me/editing", Some(&editor), Value::Null).await;
    assert_eq!(editing["channels"][0]["username"], "Streamer");
    let edit = |body: Value| {
        chat::call(
            e,
            "PATCH",
            "/api/channels/streamer/stream",
            Some(&editor),
            body,
        )
    };
    let (status, saved) = edit(json!({"title":"Edited by the editor","category_id":"coding","revision":1,"language":"en","mature":true})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        edit(json!({"title":"Stale edit","category_id":"coding","revision":1}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        edit(json!({"title":"Off","category_id":"coding","revision":2,"mature":false}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(e.mine().await["settings"]["edited_by"], "StreamEditor");
    let (status, _) = chat::call(
        e,
        "POST",
        "/api/me/stream/key/reveal",
        Some(&editor),
        json!({"code":"000000"}),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "no stream key for editors");
    e.call("DELETE", "/api/me/editors/StreamEditor", Value::Null)
        .await;
    assert_eq!(
        edit(json!({"title":"After removal","category_id":"coding","revision":2}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    e.call(
        "PATCH",
        "/api/me/stream",
        json!({"title":"Building together","category_id":"coding","revision":2,"mature":false}),
    )
    .await;
    assert_eq!(
        e.mine().await["settings"]["edited_by"],
        Value::Null,
        "owner edits are the owner's"
    );
    // Later checks expect the settings as they were before this block.
    e.sql("UPDATE stream_settings SET revision=1,language=NULL,mature=false WHERE owner_id='stream-owner'")
        .await;
    assert_eq!(
        e.request(
            "POST",
            "/api/internal/srs/publish",
            e.payload(&key, "first", "publish"),
            false,
            false,
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut bad = e.payload(&key, "first", "publish");
    bad["app"] = json!("live");
    assert_eq!(
        e.request("POST", "/api/internal/srs/publish", bad, false, false, true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut bad = e.payload(&key, "first", "publish");
    bad["service_id"] = json!("retired-boot");
    assert_eq!(
        e.request("POST", "/api/internal/srs/publish", bad, false, false, true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        e.hook(key.split('?').next().unwrap(), "first", "publish")
            .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        e.hook(&format!("{key}&key={secret}"), "first", "publish")
            .await,
        StatusCode::FORBIDDEN
    );
    for update in [
        "UPDATE users SET email_verified=false WHERE id='stream-owner'",
        "UPDATE users SET mfa_enabled=false WHERE id='stream-owner'",
        "UPDATE users SET deleted_at=now() WHERE id='stream-owner'",
        "UPDATE users SET legacy_deletion_hold=true,deleted_at=now() WHERE id='stream-owner'",
    ] {
        e.sql(update).await;
        assert_eq!(
            e.hook(&key, "first", "publish").await,
            StatusCode::FORBIDDEN
        );
        e.sql("UPDATE users SET email_verified=true,mfa_enabled=true,deleted_at=NULL,legacy_deletion_hold=false WHERE id='stream-owner'").await;
    }
    assert_eq!(
        e.hook(&key, "out-of-order", "unpublish").await,
        StatusCode::OK
    );
    assert_eq!(
        e.hook(&key, "out-of-order", "publish").await,
        StatusCode::FORBIDDEN
    );
    let (a, b) = tokio::join!(
        e.hook(&key, "publisher-a", "publish"),
        e.hook(&key, "publisher-b", "publish")
    );
    assert!(
        (a == StatusCode::OK && b == StatusCode::CONFLICT)
            || (b == StatusCode::OK && a == StatusCode::CONFLICT)
    );
    let client = if a == StatusCode::OK {
        "publisher-a"
    } else {
        "publisher-b"
    };
    assert_eq!(
        e.hook(&key, client, "publish").await,
        StatusCode::OK,
        "duplicate publish"
    );
    // WHIP reaches the hook as `app=..&stream=..&key=..` (nginx moves the bearer token there): the
    // same credential, so a second publisher is refused for the one-publisher rule, not the key.
    let (public_id, secret) = key.split_once("?key=").unwrap();
    assert_eq!(
        e.hook(
            &format!("{public_id}?app=rebuild&stream={public_id}&key={secret}"),
            "whip-second",
            "publish"
        )
        .await,
        StatusCode::CONFLICT
    );
    let first = e.mine().await["broadcast"].clone();
    assert_eq!(first["state"], "STARTING");
    streams::tick(&e.app).await.unwrap();
    assert_eq!(
        e.mine().await["broadcast"]["state"],
        "STARTING",
        "Inventory can precede SRS accepting the callback response"
    );
    e.publishing(&key, client, 1000);
    streams::tick(&e.app).await.unwrap();
    e.publishing(&key, client, 2000);
    streams::tick(&e.app).await.unwrap();
    let live = e.mine().await;
    assert_eq!(live["broadcast"]["state"], "LIVE");
    assert!(live["broadcast"]["health"]["keyframe_seconds"].is_null());
    assert_eq!(live["broadcast"]["health"]["bitrate_warning"], true);
    e.call(
        "PATCH",
        "/api/me/stream",
        json!({"title":"Changed while live","category_id":"art","revision":1}),
    )
    .await;
    assert_eq!(e.mine().await["broadcast"]["id"], first["id"]);
    assert_eq!(e.hook(&key, client, "unpublish").await, StatusCode::OK);
    let deadline = e.mine().await["broadcast"]["reconnect_deadline"].clone();
    assert_eq!(e.hook(&key, client, "unpublish").await, StatusCode::OK);
    assert_eq!(
        e.mine().await["broadcast"]["reconnect_deadline"],
        deadline,
        "duplicate cannot extend grace"
    );
    assert_eq!(
        e.hook(&key, client, "publish").await,
        StatusCode::FORBIDDEN,
        "retired publish cannot cancel grace"
    );
    e.sql("UPDATE broadcasts SET reconnect_deadline=clock_timestamp()+interval '1 second' WHERE state='RECONNECTING'").await;
    assert_eq!(e.hook(&key, "reconnected", "publish").await, StatusCode::OK);
    assert_eq!(e.mine().await["broadcast"]["id"], first["id"]);
    assert_eq!(
        e.mine().await["broadcast"]["started_at"],
        first["started_at"]
    );
    assert_eq!(
        e.hook(&key, client, "unpublish").await,
        StatusCode::OK,
        "stale disconnect"
    );
    assert_eq!(e.mine().await["broadcast"]["state"], "STARTING");
    assert_eq!(
        e.hook(&key, "reconnected", "unpublish").await,
        StatusCode::OK
    );
    e.sql("UPDATE broadcasts SET reconnect_deadline=clock_timestamp() WHERE state='RECONNECTING'")
        .await;
    assert_eq!(
        e.hook(&key, "after-deadline", "publish").await,
        StatusCode::OK
    );
    assert_ne!(e.mine().await["broadcast"]["id"], first["id"]);

    // SRS reboot uses a new service identity; old hooks/jobs cannot touch its reused client ID.
    e.fake.lock().unwrap().service = "boot-two".into();
    assert_eq!(
        e.hook(&key, "after-deadline", "publish").await,
        StatusCode::OK
    );
    let reboot = e.mine().await["broadcast"].clone();
    assert_eq!(reboot["state"], "STARTING");
    let mut stale = e.payload(&key, "after-deadline", "unpublish");
    stale["service_id"] = json!("boot-one");
    assert_eq!(
        e.request(
            "POST",
            "/api/internal/srs/unpublish",
            stale,
            false,
            false,
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(e.mine().await["broadcast"]["state"], "STARTING");
    e.publishing(&key, "after-deadline", 4000);
    streams::tick(&e.app).await.unwrap();
    e.publishing(&key, "after-deadline", 5000);
    streams::tick(&e.app).await.unwrap();
    e.fake.lock().unwrap().streams.clear();
    streams::tick(&e.app).await.unwrap(); // Lost unpublish callback is reconciled.
    assert_eq!(e.mine().await["broadcast"]["state"], "RECONNECTING");
    let deadline = e.mine().await["broadcast"]["reconnect_deadline"].clone();
    streams::tick(&e.app).await.unwrap();
    assert_eq!(e.mine().await["broadcast"]["reconnect_deadline"], deadline);
    e.sql("UPDATE broadcasts SET reconnect_deadline=clock_timestamp() WHERE state='RECONNECTING'")
        .await;
    let restarted = App::new(e.app.db.clone(), (*e.app.config).clone())
        .await
        .unwrap();
    streams::tick(&restarted).await.unwrap();
    assert_eq!(
        e.mine().await["broadcast"]["state"],
        "ENDED",
        "API restart recovers persisted expiry"
    );
    assert_eq!(
        e.hook(&key, "startup-timeout", "publish").await,
        StatusCode::OK
    );
    e.sql("UPDATE broadcasts SET startup_deadline=clock_timestamp() WHERE state='STARTING'")
        .await;
    streams::tick(&restarted).await.unwrap();
    assert_eq!(
        e.mine().await["broadcast"]["state"],
        "ENDED",
        "startup expiry cannot be resurrected by a missing stream"
    );

    assert_eq!(
        e.hook(&key, "before-rotation", "publish").await,
        StatusCode::OK
    );
    e.publishing(&key, "before-rotation", 3000);
    e.fake.lock().unwrap().failed = true;
    let rotated = e.key("rotate").await;
    assert_ne!(rotated, key);
    assert_eq!(
        e.mine().await["disconnect_pending"],
        true,
        "control failure remains queued"
    );
    assert_eq!(
        e.hook(&rotated, "backend-down", "publish").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    e.fake.lock().unwrap().failed = false;
    assert_eq!(
        e.hook(&key, "old-key", "publish").await,
        StatusCode::FORBIDDEN
    );
    e.sql("UPDATE stream_stop_jobs SET available_at=clock_timestamp()")
        .await;
    streams::tick(&e.app).await.unwrap();
    assert!(
        e.fake
            .lock()
            .unwrap()
            .kicked
            .contains(&"before-rotation".to_string())
    );
    assert_eq!(e.mine().await["disconnect_pending"], false);
    assert_eq!(e.hook(&rotated, "stop-me", "publish").await, StatusCode::OK);
    e.publishing(&rotated, "stop-me", 3000);
    e.fake.lock().unwrap().acknowledge_only = true;
    e.call("POST", "/api/me/stream/stop", Value::Null).await;
    assert_eq!(
        e.mine().await["disconnect_pending"],
        true,
        "An acknowledged but unfinished kick stays pending"
    );
    e.fake.lock().unwrap().acknowledge_only = false;
    e.sql("UPDATE stream_stop_jobs SET available_at=clock_timestamp()")
        .await;
    streams::tick(&e.app).await.unwrap();
    assert_eq!(e.mine().await["disconnect_pending"], false);
    assert!(
        e.fake
            .lock()
            .unwrap()
            .kicked
            .contains(&"stop-me".to_string())
    );
    assert_eq!(
        e.hook(&rotated, "obs-auto-reconnect", "publish").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(e.mine().await["credential"]["revoked"], true);
    // A late SRS acceptance after Stop's first inventory is still found and kicked.
    e.publishing(&rotated, "stop-me", 4000);
    streams::tick(&e.app).await.unwrap();
    assert!(e.fake.lock().unwrap().streams.is_empty());

    let racing = e.key("create").await;
    let (published, replacement) =
        tokio::join!(e.hook(&racing, "rotation-race", "publish"), e.key("rotate"));
    assert!(matches!(published, StatusCode::OK | StatusCode::FORBIDDEN));
    assert_eq!(
        e.hook(&racing, "race-retry", "publish").await,
        StatusCode::FORBIDDEN
    );
    let old_open: i64 =
        sqlx::query_scalar("SELECT count(*) FROM broadcasts WHERE public_id=$1 AND state<>'ENDED'")
            .bind(racing.split('?').next().unwrap())
            .fetch_one(&e.app.db)
            .await
            .unwrap();
    assert_eq!(
        old_open, 0,
        "Rotation/publish race must not leave the old generation live"
    );
    assert_ne!(racing, replacement);
    e.call("POST", "/api/me/stream/stop", Value::Null).await;
    let standing = e.key("create").await;
    assert_eq!(
        e.hook(&standing, "standing", "publish").await,
        StatusCode::OK
    );
    let mut tx = e.app.db.begin().await.unwrap();
    sver::auth::stream_owner(&mut tx, "stream-owner")
        .await
        .unwrap();
    sver::profiles::ensure_profile(&mut tx, "stream-owner")
        .await
        .unwrap();
    sqlx::query("INSERT INTO interim_restrictions(id,user_id,until,created_by,note) VALUES('test-restriction','stream-owner',now()+interval '1 hour','stream-owner','synthetic')")
        .execute(&mut *tx).await.unwrap();
    sver::safety::recompute(&mut tx, "stream-owner")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        e.mine().await["credential"]["revoked"],
        true,
        "restriction atomically revokes"
    );
    assert_eq!(
        e.hook(&standing, "standing", "publish").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        e.request(
            "GET",
            "/api/auth/streaming-eligibility",
            Value::Null,
            true,
            true,
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    e.sql("UPDATE profiles SET restricted_until=NULL").await;
    e.sql("DELETE FROM interim_restrictions").await;
    let disabled = e.key("create").await;
    let code = e.code().await;
    e.call("POST", "/api/auth/mfa/disable", json!({"code":code}))
        .await;
    assert_eq!(
        e.hook(&disabled, "disabled-mfa", "publish").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(e.mine().await["credential"]["revoked"], true);
    sqlx::query("UPDATE users SET mfa_enabled=true,mfa_secret=$1")
        .bind(sec::seal(&e.app, "totp:stream-owner", "JBSWY3DPEHPK3PXP").unwrap())
        .execute(&e.app.db)
        .await
        .unwrap();
    let reset = e.key("create").await;
    let token = sec::token();
    sqlx::query("INSERT INTO challenges(token_hash,user_id,kind,auth_version,expires_at) SELECT $1,id,'reset',auth_version,now()+interval '1 hour' FROM users WHERE id='stream-owner'")
        .bind(sec::digest(&token)).execute(&e.app.db).await.unwrap();
    e.call(
        "POST",
        "/api/auth/password/reset",
        json!({"token":token,"password":"Synthetic-password-for-reset-928!"}),
    )
    .await;
    assert_eq!(
        e.hook(&reset, "reset-password", "publish").await,
        StatusCode::FORBIDDEN
    );
    let revoked: bool = sqlx::query_scalar("SELECT revoked_at IS NOT NULL FROM stream_credentials")
        .fetch_one(&e.app.db)
        .await
        .unwrap();
    assert!(revoked);
    assert_eq!(
        e.request("GET", "/api/me/stream", Value::Null, true, true, false)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // Reauthenticate synthetically after reset, then exercise the real account-deletion route.
    sqlx::query("INSERT INTO sessions(id,user_id,token_hash,auth_version,mfa_verified,user_agent) SELECT 'stream-session-2',id,$1,auth_version,true,'synthetic' FROM users WHERE id='stream-owner'")
        .bind(sec::digest(&e.cookie)).execute(&e.app.db).await.unwrap();
    let deletion = e.key("create").await;
    assert_eq!(
        e.hook(&deletion, "erase-active", "publish").await,
        StatusCode::OK
    );
    e.publishing(&deletion, "erase-active", 9000);
    let code = e.code().await;
    e.call("POST", "/api/auth/account/delete", json!({"code":code}))
        .await;
    assert_eq!(
        e.hook(&deletion, "deletion-retry", "publish").await,
        StatusCode::FORBIDDEN
    );
    let mut tx = e.app.db.begin().await.unwrap();
    sver::profile_jobs::erase(&mut tx, "stream-owner")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM stream_stop_jobs WHERE owner_id IS NULL")
            .fetch_one(&e.app.db)
            .await
            .unwrap();
    assert!(
        pending > 0,
        "Erasure must not discard a pending external disconnect"
    );
    e.sql("UPDATE stream_stop_jobs SET available_at=clock_timestamp()")
        .await;
    streams::tick(&e.app).await.unwrap();
    assert!(
        e.fake
            .lock()
            .unwrap()
            .kicked
            .contains(&"erase-active".to_string())
    );
}
