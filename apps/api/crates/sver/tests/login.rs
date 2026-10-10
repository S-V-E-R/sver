use axum::{
    Form, Json, Router,
    body::Body,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, Request, StatusCode},
    routing::{get, post},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use sqlx::postgres::PgPoolOptions;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use sver::{App, Config, security as sec};
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Mock {
    profiles: Arc<Mutex<HashMap<String, Value>>>,
    exchanges: Arc<Mutex<Vec<HashMap<String, String>>>>,
    mail: Arc<Mutex<Vec<(String, Value)>>>,
    mail_fails: Arc<Mutex<bool>>,
}
async fn deliver_mail(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> StatusCode {
    assert_eq!(headers["authorization"], "Bearer test-only-mail-key");
    mock.mail.lock().unwrap().push((
        headers["idempotency-key"].to_str().unwrap().to_string(),
        payload,
    ));
    if *mock.mail_fails.lock().unwrap() {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}
async fn breach(Path(prefix): Path<String>) -> String {
    let hash = Sha1::digest(b"password123456")
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<String>();
    if prefix == hash[..5] {
        format!("{}:99\r\n", &hash[5..])
    } else {
        format!("{}:0\r\n", "0".repeat(35))
    }
}
async fn turnstile(Form(body): Form<HashMap<String, String>>) -> Json<Value> {
    let token = body.get("response").map(String::as_str).unwrap_or("");
    Json(
        json!({"success":token.ends_with("-ok"),"hostname":"localhost","action":if token=="signup-ok" {"signup"} else {"recovery"}}),
    )
}
async fn exchange(
    State(mock): State<Mock>,
    Path(provider): Path<String>,
    Form(body): Form<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(body["client_secret"], "test-only-secret");
    assert_eq!(body["grant_type"], "authorization_code");
    assert_eq!(
        body["redirect_uri"],
        format!(
            "{}/api/auth/oauth/{provider}/callback",
            if provider == "google" {
                "https://api.sver.tv"
            } else {
                "http://localhost:3000"
            }
        )
    );
    if provider != "twitch" {
        assert!(body["code_verifier"].len() >= 43);
    }
    let code = body["code"].clone();
    mock.exchanges.lock().unwrap().push(body);
    if code == "invalid-authorization-code" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_grant"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({"access_token":code,"token_type":"bearer"})),
    )
}
async fn profile(
    State(mock): State<Mock>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Json<Value> {
    let code = headers["authorization"]
        .to_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap();
    let identity = mock.profiles.lock().unwrap().get(code).cloned().unwrap();
    if provider == "twitch" {
        assert_eq!(headers["client-id"], "test-client");
        Json(json!({"data":[identity]}))
    } else {
        Json(identity)
    }
}
struct Browser {
    app: Router,
    cookies: HashMap<String, String>,
    ip: SocketAddr,
}
impl Browser {
    fn new(app: Router, octet: u8) -> Self {
        Self {
            app,
            cookies: HashMap::new(),
            ip: format!("192.0.2.{octet}:12000").parse().unwrap(),
        }
    }
    async fn call(
        &mut self,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value, HeaderMap) {
        self.origin_call(method, path, body, "http://localhost:3000")
            .await
    }
    async fn origin_call(
        &mut self,
        method: &str,
        path: &str,
        body: Value,
        origin: &str,
    ) -> (StatusCode, Value, HeaderMap) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("origin", origin)
            .header("content-type", "application/json")
            .header("user-agent", "SVER integration check")
            .header(
                "cookie",
                self.cookies
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
            .extension(ConnectInfo(self.ip))
            .body(if body.is_null() {
                Body::empty()
            } else {
                Body::from(body.to_string())
            })
            .unwrap();
        let response = self.app.clone().oneshot(req).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        for raw in headers.get_all("set-cookie") {
            let cookie = axum_extra::extract::cookie::Cookie::parse(raw.to_str().unwrap()).unwrap();
            if cookie.value().is_empty() {
                self.cookies.remove(cookie.name());
            } else {
                self.cookies
                    .insert(cookie.name().to_string(), cookie.value().to_string());
            }
        }
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            headers,
        )
    }
    async fn ok(&mut self, method: &str, path: &str, body: Value) -> Value {
        let (status, value, _) = self.call(method, path, body).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "request failed: {method} {path}: {}",
            value["error"]
        );
        value
    }
    async fn login(&mut self, identifier: &str, password: &str) -> Value {
        self.ok(
            "POST",
            "/api/auth/login",
            json!({"identifier":identifier,"password":password}),
        )
        .await
    }
}
/// Signed-in password and email changes, security notices and the data export (docs/LOGIN.md).
async fn exercise_account_changes(app: &App, a: &mut Browser, router: &Router, password: &str) {
    let me = a.ok("GET", "/api/auth/me", Value::Null).await;
    let user_id = me["id"].as_str().unwrap().to_string();
    let mut other = Browser::new(router.clone(), 61);
    other.login("first@example.invalid", password).await;
    let changed = "Synthetic-only:changed passphrase 4471!";
    assert_eq!(
        a.call(
            "POST",
            "/api/auth/password/change",
            json!({"password":"short"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    a.ok(
        "POST",
        "/api/auth/password/change",
        json!({"password":changed}),
    )
    .await;
    assert_eq!(
        other.call("GET", "/api/auth/me", Value::Null).await.0,
        StatusCode::UNAUTHORIZED,
        "other devices are signed out"
    );
    other.login("first@example.invalid", changed).await;
    a.ok(
        "POST",
        "/api/auth/password/change",
        json!({"password":password}),
    )
    .await;
    // Mail for this account: (to, text), newest first.
    let mails = || async {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT payload FROM mail_jobs WHERE user_id=$1 ORDER BY created_at DESC",
        )
        .bind(&user_id)
        .fetch_all(&app.db)
        .await
        .unwrap();
        rows.iter()
            .map(|r| serde_json::from_str::<Value>(&sec::unseal(app, "mail", r).unwrap()).unwrap())
            .map(|m| {
                (
                    m["to"][0].as_str().unwrap().to_string(),
                    m["text"].as_str().unwrap().to_string(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert!(
        mails()
            .await
            .iter()
            .any(|(to, text)| to == "first@example.invalid"
                && text.contains("password was just changed"))
    );
    let pending = a
        .ok(
            "POST",
            "/api/auth/email/change",
            json!({"email":"Changed@Example.invalid"}),
        )
        .await;
    assert_eq!(pending["pending"], "c***@example.invalid");
    assert_eq!(
        a.ok("GET", "/api/auth/me", Value::Null).await["pending_email"],
        "c***@example.invalid"
    );
    let sent = mails().await;
    let (_, link) = sent
        .iter()
        .find(|(to, _)| to == "changed@example.invalid")
        .expect("confirmation to the new address");
    assert!(
        sent.iter().any(
            |(to, text)| to == "first@example.invalid" && text.contains("c***@example.invalid")
        )
    );
    let token = link
        .split("#token=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    a.ok("POST", "/api/auth/email/verify", json!({"token":token}))
        .await;
    let me = a.ok("GET", "/api/auth/me", Value::Null).await;
    assert_eq!(
        (&me["email"], &me["pending_email"]),
        (&json!("changed@example.invalid"), &Value::Null)
    );
    assert_eq!(
        a.call("POST", "/api/auth/email/verify", json!({"token":token}))
            .await
            .0,
        StatusCode::BAD_REQUEST,
        "links work once"
    );
    // The export: the account and its rows, never secrets.
    let (status, file, headers) = a.call("GET", "/api/auth/export", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("attachment")
    );
    assert_eq!(file["account"]["email"], "changed@example.invalid");
    let text = file.to_string();
    assert!(
        file["data"]["sessions"]
            .as_array()
            .is_some_and(|s| !s.is_empty()),
        "sign-in history"
    );
    assert!(file["readme"].is_string());
    for secret in ["password_hash", "token_hash", "mfa_secret"] {
        assert!(!text.contains(secret), "export leaks {secret}");
    }
    sqlx::query("UPDATE users SET email='first@example.invalid' WHERE id=$1")
        .bind(&user_id)
        .execute(&app.db)
        .await
        .unwrap();
}
async fn mail_token(app: &App, email: &str, kind: &str) -> String {
    let rows:Vec<String>=sqlx::query_scalar("SELECT m.payload FROM mail_jobs m JOIN users u ON u.id=m.user_id WHERE u.email=$1 ORDER BY m.created_at DESC").bind(email).fetch_all(&app.db).await.unwrap();
    for row in rows {
        let body: Value = serde_json::from_str(&sec::unseal(app, "mail", &row).unwrap()).unwrap();
        let text = body["text"].as_str().unwrap();
        if text.contains(&format!("/{kind}#token=")) {
            return text
                .split("#token=")
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .to_string();
        }
    }
    panic!("No queued email for expected flow");
}
async fn start_oauth(
    browser: &mut Browser,
    provider: &str,
    intent: &str,
    _username: &str,
    code: &str,
) -> url::Url {
    let v = browser
        .ok(
            "POST",
            &format!("/api/auth/oauth/{provider}/start"),
            json!({"intent":intent,"code":code}),
        )
        .await;
    url::Url::parse(v["url"].as_str().unwrap()).unwrap()
}
async fn finish_oauth(
    browser: &mut Browser,
    provider: &str,
    url: &url::Url,
    code: &str,
) -> HeaderMap {
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .to_string();
    let (status, _, headers) = browser
        .call(
            "GET",
            &format!("/api/auth/oauth/{provider}/callback?state={state}&code={code}"),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    headers
}
fn identity(mock: &Mock, code: &str, subject: &str, email: &str, verified: bool) {
    mock.profiles.lock().unwrap().insert(code.into(),json!({"id":subject,"sub":subject,"email":email,"verified":verified,"email_verified":verified}));
}

#[tokio::test]
async fn login_lifecycle_and_security_boundaries() {
    let database_url = std::env::var("DATABASE_URL")
        .expect("Use scripts/dev.ps1 test with an isolated local database");
    let parsed = url::Url::parse(&database_url).unwrap();
    assert!(
        matches!(parsed.host_str(), Some("localhost" | "127.0.0.1"))
            && parsed.path() == "/sver_rebuild",
        "Tests require the isolated local sver_rebuild database"
    );
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("login_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await
        .unwrap();
    let search_path = format!("SET search_path TO {schema}");
    let db = PgPoolOptions::new()
        .max_connections(12)
        .after_connect(move |connection, _| {
            let statement = search_path.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(statement))
                    .execute(connection)
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
    let mock = Mock::default();
    let upstream = Router::new()
        .route("/range/{prefix}", get(breach))
        .route("/turnstile", post(turnstile))
        .route("/emails", post(deliver_mail))
        .route("/{provider}/token", post(exchange))
        .route("/{provider}/profile", get(profile))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mock_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let mut config = Config::from_env().unwrap();
    config.turnstile_secret = "test-only-secret".into();
    config.turnstile_url = format!("{base}/turnstile");
    config.breach_url = format!("{base}/range/");
    config.resend_key.clear();
    config.resend_url = format!("{base}/emails");
    config.providers = ["google", "twitch", "discord"]
        .iter()
        .map(|name| sver::oauth::Provider {
            name: (*name).into(),
            client_id: "test-client".into(),
            client_secret: "test-only-secret".into(),
            authorize_url: format!("{base}/{name}/authorize"),
            token_url: format!("{base}/{name}/token"),
            profile_url: format!("{base}/{name}/profile"),
            scopes: "email".into(),
            pkce: *name != "twitch",
            redirect_uri: (*name == "google")
                .then(|| "https://api.sver.tv/api/auth/oauth/google/callback".into()),
        })
        .collect();
    let app = App::new(db.clone(), config).await.unwrap();
    let test = tokio::spawn(async move {
        exercise(app.clone(), mock.clone()).await;
        exercise_mail(app.clone(), mock).await;
        if std::env::var("SVER_TEST_RESEND").as_deref() == Ok("1") {
            exercise_resend(app).await;
        }
    })
    .await;
    db.close().await;
    // Identifier consists solely of our fixed prefix and a generated UUID; never user input.
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await
        .unwrap();
    mock_task.abort();
    assert!(
        test.is_ok(),
        "Login acceptance check failed; isolated test schema was removed"
    );
}

async fn exercise_resend(mut app: App) {
    // Explicit opt-in; Resend's documented simulator never delivers to a real person.
    let live = Config::from_env().unwrap();
    assert!(!live.resend_key.is_empty() && !live.mail_from.is_empty());
    let mut config = (*app.config).clone();
    config.resend_url = live.resend_url;
    config.mail_from = live.mail_from;
    config.resend_key = "re_invalid_acceptance_test".into();
    app.config = Arc::new(config.clone());
    sqlx::query("UPDATE users SET email='delivered@resend.dev' WHERE id='legacy-id'")
        .execute(&app.db)
        .await
        .unwrap();
    let mut tx = app.db.begin().await.unwrap();
    let user = sver::auth::lock_user(&mut tx, "legacy-id").await.unwrap();
    sver::jobs::queue_email(&app, &mut tx, &user, "verify")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sver::jobs::tick(&app).await.unwrap();
    let retry: bool = sqlx::query_scalar("SELECT attempts=1 AND available_at>now() FROM mail_jobs")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(
        retry,
        "A real Resend rejection must remain queued for retry"
    );
    config.resend_key = live.resend_key;
    app.config = Arc::new(config);
    sqlx::query("UPDATE mail_jobs SET available_at=now()")
        .execute(&app.db)
        .await
        .unwrap();
    sver::jobs::tick(&app).await.unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_jobs")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(
        remaining, 0,
        "Resend must accept the same queued job after credentials recover"
    );
    println!(
        "Passed: real Resend rejection and successful retry to the documented delivery simulator."
    );
}

async fn exercise_mail(mut app: App, mock: Mock) {
    // This pool is confined to the generated test schema, never the preview's data.
    sqlx::query("DELETE FROM mail_jobs")
        .execute(&app.db)
        .await
        .unwrap();
    let mut tx = app.db.begin().await.unwrap();
    let user = sver::auth::lock_user(&mut tx, "legacy-id").await.unwrap();
    sver::jobs::queue_email(&app, &mut tx, &user, "verify")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (job_id, encrypted): (String, String) = sqlx::query_as("SELECT id,payload FROM mail_jobs")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(!encrypted.contains("legacy@example.invalid"));
    assert!(!encrypted.contains("#token="));

    sver::jobs::tick(&app).await.unwrap();
    assert!(
        mock.mail.lock().unwrap().is_empty(),
        "No key means no delivery"
    );
    let mut config = (*app.config).clone();
    config.resend_key = "test-only-mail-key".into();
    config.mail_from = "SVER Test <sender@example.invalid>".into();
    app.config = Arc::new(config);
    *mock.mail_fails.lock().unwrap() = true;
    sver::jobs::tick(&app).await.unwrap();
    let (attempts, delayed): (i32, bool) =
        sqlx::query_as("SELECT attempts,available_at>now() FROM mail_jobs WHERE id=$1")
            .bind(&job_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(attempts, 1);
    assert!(delayed);
    sver::jobs::tick(&app).await.unwrap();
    assert_eq!(
        mock.mail.lock().unwrap().len(),
        1,
        "Wait for the retry time"
    );

    sqlx::query("UPDATE mail_jobs SET available_at=now() WHERE id=$1")
        .bind(&job_id)
        .execute(&app.db)
        .await
        .unwrap();
    *mock.mail_fails.lock().unwrap() = false;
    sver::jobs::tick(&app).await.unwrap();
    {
        let sent = mock.mail.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(
            sent[0], sent[1],
            "Retry the same payload and idempotency key"
        );
        assert_eq!(sent[1].0, job_id);
        assert_eq!(sent[1].1["from"], app.config.mail_from);
        assert_eq!(sent[1].1["to"], json!(["legacy@example.invalid"]));
        assert!(
            sent[1].1["text"]
                .as_str()
                .unwrap()
                .contains("/verify#token=")
        );
    }
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_jobs")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(remaining, 0, "Successful delivery removes the job");

    let mut tx = app.db.begin().await.unwrap();
    sver::jobs::queue_email(&app, &mut tx, &user, "reset")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE mail_jobs SET attempts=9")
        .execute(&app.db)
        .await
        .unwrap();
    *mock.mail_fails.lock().unwrap() = true;
    sver::jobs::tick(&app).await.unwrap();
    let bounded: bool = sqlx::query_scalar(
        "SELECT attempts=10 AND available_at BETWEEN now()+interval '29 minutes' AND now()+interval '31 minutes' FROM mail_jobs",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert!(bounded, "Retry delay and total attempts are capped");
    sqlx::query("UPDATE mail_jobs SET available_at=now()")
        .execute(&app.db)
        .await
        .unwrap();
    sver::jobs::tick(&app).await.unwrap();
    assert_eq!(
        mock.mail.lock().unwrap().len(),
        3,
        "Exhausted jobs are not sent"
    );
    sqlx::query("UPDATE mail_jobs SET attempts=0,expires_at=now()-interval '1 second'")
        .execute(&app.db)
        .await
        .unwrap();
    sver::jobs::tick(&app).await.unwrap();
    assert_eq!(
        mock.mail.lock().unwrap().len(),
        3,
        "Expired links are not sent"
    );
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_jobs")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    println!(
        "Passed: mail encryption, disabled delivery, retry scheduling, idempotency, success cleanup, retry caps and expiry."
    );
}

async fn exercise(app: App, mock: Mock) {
    let router = sver::router(app.clone());
    let mut a = Browser::new(router.clone(), 1);
    let password = "Synthetic-only:unique passphrase 726!";
    let signup = json!({"email":"first@example.invalid","username":"First_User","date_of_birth":"1995-10-02","password":password,"turnstile_token":"signup-ok"});

    let mut underage = signup.clone();
    underage["date_of_birth"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
    assert_eq!(
        a.call("POST", "/api/auth/signup", underage).await.0,
        StatusCode::BAD_REQUEST
    );
    let count:i64=sqlx::query_scalar("SELECT (SELECT count(*) FROM users)+(SELECT count(*) FROM rate_limits)+(SELECT count(*) FROM oauth_states)").fetch_one(&app.db).await.unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        a.call("GET", "/api/auth/oauth/signup", Value::Null).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        a.call(
            "POST",
            "/api/auth/oauth/signup",
            json!({"username":"NoProof","date_of_birth":"1995-01-01","turnstile_token":"signup-ok"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mut availability = Browser::new(router.clone(), 173);
    assert_eq!(
        availability
            .ok(
                "GET",
                "/api/auth/username-availability?username=First_User",
                Value::Null
            )
            .await["available"],
        true
    );
    for name in [
        "ab",
        "bad-name",
        "a_d_m_1_n",
        "forgot",
        "reset",
        "verify",
        "mfa",
        "_next",
    ] {
        let result = availability
            .ok(
                "GET",
                &format!("/api/auth/username-availability?username={name}"),
                Value::Null,
            )
            .await;
        assert_eq!(result["available"], false, "Rejected username {name}");
        assert!(sec::signup_identity(name, "1995-01-01").is_err());
    }
    assert!(sec::signup_identity("a_d_m_1_n", "1990-01-01").is_err());
    assert!(sec::signup_identity("n1gg3r", "1990-01-01").is_err());
    assert_eq!(
        a.origin_call(
            "POST",
            "/api/auth/signup",
            signup.clone(),
            "https://evil.invalid"
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut weak = signup.clone();
    weak["password"] = json!("password123456");
    assert_eq!(
        a.call("POST", "/api/auth/signup", weak).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut bad_bot = signup.clone();
    bad_bot["turnstile_token"] = json!("invalid");
    assert_eq!(
        a.call("POST", "/api/auth/signup", bad_bot).await.0,
        StatusCode::BAD_REQUEST
    );
    let (_, _, headers) = a.call("POST", "/api/auth/signup", signup.clone()).await;
    let cookie = headers.get("set-cookie").unwrap().to_str().unwrap();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Lax")
            && cookie.contains("Max-Age=2592000")
    );
    let user = a.ok("GET", "/api/auth/me", Value::Null).await;
    let user_id = user["id"].as_str().unwrap().to_string();
    assert_eq!(user["email_verified"], false);
    let (status, checked, check_headers) = availability
        .call(
            "GET",
            "/api/auth/username-availability?username=fIRST_uSER",
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        checked,
        json!({"available":false,"message":"That username isn't available."})
    );
    assert_eq!(check_headers["cache-control"], "no-store");
    assert!(
        availability.cookies.is_empty(),
        "Availability must not authenticate a visitor"
    );
    sqlx::query("UPDATE rate_limits SET count=60 WHERE key='username-check:192.0.2.173'")
        .execute(&app.db)
        .await
        .unwrap();
    let (status, _, headers) = availability
        .call(
            "GET",
            "/api/auth/username-availability?username=AnotherName",
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(headers.contains_key("retry-after"));
    sqlx::query("UPDATE rate_limits SET expires_at=now()-interval '1 second' WHERE key='username-check:192.0.2.173'").execute(&app.db).await.unwrap();
    assert_eq!(
        availability
            .ok(
                "GET",
                "/api/auth/username-availability?username=AnotherName",
                Value::Null
            )
            .await["available"],
        true
    );
    let mut same_name = Browser::new(router.clone(), 174);
    let mut collision = signup.clone();
    collision["email"] = json!("another@example.invalid");
    collision["username"] = json!("fIRST_uSER");
    assert_eq!(
        same_name
            .call("POST", "/api/auth/signup", collision)
            .await
            .0,
        StatusCode::BAD_REQUEST,
        "The database must still reject a name taken after an availability check"
    );
    let mut duplicate = signup.clone();
    duplicate["email"] = json!("FIRST@EXAMPLE.INVALID");
    duplicate["username"] = json!("OtherUser");
    assert_eq!(
        a.call("POST", "/api/auth/signup", duplicate).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        a.call("GET", "/api/auth/streaming-eligibility", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let verify = mail_token(&app, "first@example.invalid", "verify").await;
    sqlx::query("UPDATE challenges SET expires_at=now()-interval '1 second' WHERE token_hash=$1")
        .bind(sec::digest(&verify))
        .execute(&app.db)
        .await
        .unwrap();
    assert_eq!(
        a.call("POST", "/api/auth/email/verify", json!({"token":verify}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    sqlx::query("UPDATE challenges SET expires_at=now()+interval '24 hours' WHERE token_hash=$1")
        .bind(sec::digest(&verify))
        .execute(&app.db)
        .await
        .unwrap();
    a.ok("POST", "/api/auth/email/verify", json!({"token":verify}))
        .await;
    assert_eq!(
        a.call("POST", "/api/auth/email/verify", json!({"token":verify}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        a.call("GET", "/api/auth/streaming-eligibility", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut b = Browser::new(router.clone(), 2);
    b.login("  fIRsT_uSER  ", password).await;
    assert_eq!(
        b.ok("GET", "/api/auth/me", Value::Null).await["id"],
        user_id
    );
    assert_eq!(
        a.ok("GET", "/api/auth/sessions", Value::Null).await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let sid = b.ok("GET", "/api/auth/me", Value::Null).await["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok("DELETE", &format!("/api/auth/sessions/{sid}"), Value::Null)
        .await;
    assert_eq!(
        b.call("GET", "/api/auth/me", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    b.ok(
        "POST",
        "/api/auth/login",
        json!({"email":"  FIRST@EXAMPLE.INVALID  ","password":password}),
    )
    .await;

    let setup = a.ok("POST", "/api/auth/mfa/setup", json!({})).await;
    let secret = setup["secret"].as_str().unwrap();
    let otp = sec::totp(secret, "first@example.invalid").unwrap();
    let code = otp.generate_current().to_string();
    let recovery = a
        .ok("POST", "/api/auth/mfa/enable", json!({"code":code}))
        .await["recovery_codes"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(recovery.len(), 10);
    assert_eq!(
        b.call("GET", "/api/auth/me", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    a.ok("GET", "/api/auth/streaming-eligibility", Value::Null)
        .await;
    assert_eq!(b.login("First_User", password).await["requires_mfa"], true);
    assert_eq!(
        b.call("GET", "/api/auth/me", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        b.call("POST", "/api/auth/mfa/login", json!({"code":code}))
            .await
            .0,
        StatusCode::BAD_REQUEST,
        "TOTP step used for enrollment must not replay"
    );
    b.ok("POST", "/api/auth/mfa/login", json!({"code":recovery[0]}))
        .await;
    b.ok("POST", "/api/auth/logout", json!({})).await;
    b.login("first@example.invalid", password).await;
    assert_eq!(
        b.call("POST", "/api/auth/mfa/login", json!({"code":recovery[0]}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    b.ok("POST", "/api/auth/mfa/login", json!({"code":recovery[1]}))
        .await;
    assert_eq!(
        a.ok("GET", "/api/auth/me", Value::Null).await["recovery_codes_remaining"],
        8
    );

    let mut recovery_client = Browser::new(router.clone(), 3);
    recovery_client
        .ok(
            "POST",
            "/api/auth/password/forgot",
            json!({"email":"FIRST@EXAMPLE.INVALID","turnstile_token":"recovery-ok"}),
        )
        .await;
    let reset = mail_token(&app, "first@example.invalid", "reset").await;
    let new_password = "Synthetic-only:replacement password 938!";
    let mut racing = Browser::new(router.clone(), 4);
    let (one, two) = tokio::join!(
        recovery_client.call(
            "POST",
            "/api/auth/password/reset",
            json!({"token":reset,"password":new_password})
        ),
        racing.call(
            "POST",
            "/api/auth/password/reset",
            json!({"token":reset,"password":new_password})
        )
    );
    assert!(
        (one.0 == StatusCode::OK && two.0 == StatusCode::BAD_REQUEST)
            || (two.0 == StatusCode::OK && one.0 == StatusCode::BAD_REQUEST)
    );
    assert_eq!(
        a.call("GET", "/api/auth/me", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        b.call("GET", "/api/auth/me", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        a.call(
            "POST",
            "/api/auth/login",
            json!({"email":"first@example.invalid","password":password})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        a.login("first@example.invalid", new_password).await["requires_mfa"],
        true
    );
    a.ok("POST", "/api/auth/mfa/login", json!({"code":recovery[2]}))
        .await;

    // Linking requires recent proof, cannot use a session revoked while away at the provider.
    identity(
        &mock,
        "google-first",
        "google-subject",
        "first@example.invalid",
        true,
    );
    let url = start_oauth(&mut a, "google", "link", "", recovery[3].as_str().unwrap()).await;
    assert!(url.query_pairs().any(|(key, value)| key == "redirect_uri"
        && value == "https://api.sver.tv/api/auth/oauth/google/callback"));
    assert!(
        url.query_pairs()
            .any(|(k, v)| k == "code_challenge_method" && v == "S256")
    );
    let redirect = finish_oauth(&mut a, "google", &url, "google-first").await;
    assert_eq!(redirect["location"], "http://localhost:3000/account");
    assert_eq!(
        a.ok("GET", "/api/auth/me", Value::Null).await["providers"],
        json!(["google"])
    );
    let again = finish_oauth(&mut a, "google", &url, "google-first").await;
    assert!(again["location"].to_str().unwrap().contains("error="));
    let mut provider_login = Browser::new(router.clone(), 5);
    let url = start_oauth(&mut provider_login, "google", "login", "", "").await;
    assert_eq!(
        finish_oauth(&mut provider_login, "google", &url, "google-first").await["location"],
        "http://localhost:3000/mfa"
    );
    assert_eq!(
        provider_login
            .call("GET", "/api/auth/me", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    provider_login
        .ok("POST", "/api/auth/mfa/login", json!({"code":recovery[4]}))
        .await;
    // Mismatched browser cannot consume the real browser's state.
    let url = start_oauth(&mut provider_login, "google", "login", "", "").await;
    let mut thief = Browser::new(router.clone(), 6);
    assert!(
        finish_oauth(&mut thief, "google", &url, "google-first").await["location"]
            .to_str()
            .unwrap()
            .contains("error=")
    );
    assert_eq!(
        finish_oauth(&mut provider_login, "google", &url, "google-first").await["location"],
        "http://localhost:3000/mfa"
    );

    // Email collision must not link or create another account.
    identity(
        &mock,
        "discord-collision",
        "unlinked-discord",
        "first@example.invalid",
        true,
    );
    let mut collision = Browser::new(router.clone(), 7);
    let url = start_oauth(&mut collision, "discord", "signup", "NewIdentity", "").await;
    assert!(
        finish_oauth(&mut collision, "discord", &url, "discord-collision").await["location"]
            .to_str()
            .unwrap()
            .contains("error=")
    );
    let linked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM identities WHERE subject='unlinked-discord'")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(linked, 0);

    // Exercise each real adapter's response shape and PKCE token exchange with synthetic HTTP.
    for (index, p) in ["google", "discord", "twitch"].iter().enumerate() {
        // A valid browser/state cannot turn a rejected provider code into either a session or MFA challenge.
        let mut invalid = Browser::new(router.clone(), 30 + index as u8);
        let url = start_oauth(&mut invalid, p, "login", "", "").await;
        assert!(
            finish_oauth(&mut invalid, p, &url, "invalid-authorization-code").await["location"]
                .to_str()
                .unwrap()
                .contains("Provider+authorization+failed")
        );
        assert_eq!(
            invalid.call("GET", "/api/auth/me", Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            invalid
                .call("POST", "/api/auth/mfa/login", json!({"code":"123456"}))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let email = format!("{p}@example.invalid");
        let subject = format!("{p}-only");
        identity(&mock, &subject, &subject, &email, true);
        let mut browser = Browser::new(router.clone(), 20 + index as u8);
        let intent = if *p == "twitch" { "login" } else { "signup" };
        let url = start_oauth(&mut browser, p, intent, "", "").await;
        assert_eq!(
            url.query_pairs().any(|(k, _)| k == "code_challenge"),
            *p != "twitch"
        );
        assert_eq!(
            finish_oauth(&mut browser, p, &url, &subject).await["location"],
            "http://localhost:3000/oauth-signup"
        );
        assert_eq!(
            browser.call("GET", "/api/auth/me", Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
        let pending = browser
            .ok("GET", "/api/auth/oauth/signup", Value::Null)
            .await;
        assert_eq!(pending["provider"], *p);
        assert!(sec::validate_username(pending["username"].as_str().unwrap()).is_ok());
        assert!(pending.get("email").is_none() && pending.get("subject").is_none());
        let pending_hash = sec::digest(&browser.cookies["sver_dev_signup"]);
        let encrypted: String =
            sqlx::query_scalar("SELECT payload FROM oauth_signups WHERE token_hash=$1")
                .bind(&pending_hash)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert!(!encrypted.contains(&email) && encrypted.starts_with("v1."));
        let body = json!({"username":format!("{p}user"),"date_of_birth":"1995-01-01","turnstile_token":"signup-ok"});
        assert_eq!(
            browser
                .origin_call(
                    "POST",
                    "/api/auth/oauth/signup",
                    body.clone(),
                    "https://evil.invalid"
                )
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let mut underage = body.clone();
        underage["date_of_birth"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
        assert_eq!(
            browser
                .call("POST", "/api/auth/oauth/signup", underage)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE email=$1)")
            .bind(&email)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert!(
            !exists,
            "No account exists before age-checked signup completion"
        );
        sqlx::query(
            "UPDATE oauth_signups SET expires_at=now()-interval '1 second' WHERE token_hash=$1",
        )
        .bind(&pending_hash)
        .execute(&app.db)
        .await
        .unwrap();
        assert_eq!(
            browser
                .call("GET", "/api/auth/oauth/signup", Value::Null)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            browser
                .call("POST", "/api/auth/oauth/signup", body.clone())
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        sqlx::query(
            "UPDATE oauth_signups SET expires_at=now()+interval '10 minutes' WHERE token_hash=$1",
        )
        .bind(&pending_hash)
        .execute(&app.db)
        .await
        .unwrap();
        let mut taken = body.clone();
        taken["username"] = json!("fIRST_uSER");
        assert_eq!(
            browser
                .call("POST", "/api/auth/oauth/signup", taken)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        let mut bad_bot = body.clone();
        bad_bot["turnstile_token"] = json!("invalid");
        assert_eq!(
            browser
                .call("POST", "/api/auth/oauth/signup", bad_bot)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        let mut replay = Browser::new(router.clone(), 180 + index as u8);
        replay.cookies = browser.cookies.clone();
        browser
            .ok("POST", "/api/auth/oauth/signup", body.clone())
            .await;
        assert_eq!(
            replay.call("POST", "/api/auth/oauth/signup", body).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            replay.call("GET", "/api/auth/me", Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
        if *p != "twitch" {
            use base64::Engine;
            let sent = mock.exchanges.lock().unwrap().last().unwrap()["code_verifier"].clone();
            let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(sha2::Sha256::digest(sent.as_bytes()));
            assert_eq!(
                url.query_pairs()
                    .find(|(key, _)| key == "code_challenge")
                    .unwrap()
                    .1,
                challenge
            );
        }
        let user = browser.ok("GET", "/api/auth/me", Value::Null).await;
        assert_eq!(user["has_password"], false);
        assert_eq!(user["email_verified"], *p != "twitch");
        assert_eq!(
            browser
                .call("DELETE", &format!("/api/auth/oauth/{p}"), json!({}))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        sqlx::query(
            "UPDATE sessions SET authenticated_at=now()-interval '6 minutes' WHERE user_id=$1",
        )
        .bind(user["id"].as_str().unwrap())
        .execute(&app.db)
        .await
        .unwrap();
        assert_eq!(
            browser
                .call("POST", "/api/auth/mfa/setup", json!({}))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let url = start_oauth(&mut browser, p, "reauth", "", "").await;
        assert_eq!(
            finish_oauth(&mut browser, p, &url, &subject).await["location"],
            "http://localhost:3000/account"
        );
        assert_eq!(
            browser.ok("GET", "/api/auth/me", Value::Null).await["reauthenticated"],
            true
        );
        if *p == "twitch" {
            let token = mail_token(&app, &email, "verify").await;
            browser
                .ok("POST", "/api/auth/email/verify", json!({"token":token}))
                .await;
        }
    }
    assert!(
        mock.exchanges
            .lock()
            .unwrap()
            .iter()
            .any(|x| x.contains_key("code_verifier"))
    );

    // Revoke the initiating session while linking; callback must fail without linking.
    identity(
        &mock,
        "discord-first",
        "discord-first",
        "different@example.invalid",
        true,
    );
    let url = start_oauth(&mut a, "discord", "link", "", recovery[5].as_str().unwrap()).await;
    a.ok("DELETE", "/api/auth/sessions", Value::Null).await;
    assert!(
        finish_oauth(&mut a, "discord", &url, "discord-first").await["location"]
            .to_str()
            .unwrap()
            .contains("error=")
    );
    assert_eq!(
        provider_login
            .call("GET", "/api/auth/me", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );

    // Preserve short legacy bcrypt passwords, then upgrade after successful sign-in.
    let bcrypt = bcrypt::hash("oldpass", 4).unwrap();
    sqlx::query("INSERT INTO users(id,email,username,password_hash,email_verified) VALUES('legacy-id','legacy@example.invalid','Legacy_User',$1,true)").bind(&bcrypt).execute(&app.db).await.unwrap();
    let mut legacy = Browser::new(router.clone(), 40);
    legacy.login("legacy@example.invalid", "oldpass").await;
    assert_eq!(
        legacy.ok("GET", "/api/auth/me", Value::Null).await["id"],
        "legacy-id"
    );
    let upgraded: String =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id='legacy-id'")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(upgraded.starts_with("$argon2id$"));

    // Expired reset/session, rolling lifetime, encrypted-secret integrity and secure cookies.
    legacy
        .ok(
            "POST",
            "/api/auth/password/forgot",
            json!({"email":"legacy@example.invalid","turnstile_token":"recovery-ok"}),
        )
        .await;
    let expired_reset = mail_token(&app, "legacy@example.invalid", "reset").await;
    sqlx::query("UPDATE challenges SET expires_at=now()-interval '1 second' WHERE token_hash=$1")
        .bind(sec::digest(&expired_reset))
        .execute(&app.db)
        .await
        .unwrap();
    assert_eq!(
        legacy
            .call(
                "POST",
                "/api/auth/password/reset",
                json!({"token":expired_reset,"password":new_password})
            )
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    sqlx::query("UPDATE sessions SET expires_at=now()+interval '1 day' WHERE user_id='legacy-id'")
        .execute(&app.db)
        .await
        .unwrap();
    legacy.ok("GET", "/api/auth/me", Value::Null).await;
    let rolling:bool=sqlx::query_scalar("SELECT bool_and(expires_at>now()+interval '29 days') FROM sessions WHERE user_id='legacy-id'").fetch_one(&app.db).await.unwrap();
    assert!(rolling);
    sqlx::query(
        "UPDATE sessions SET expires_at=now()-interval '1 second' WHERE user_id='legacy-id'",
    )
    .execute(&app.db)
    .await
    .unwrap();
    assert_eq!(
        legacy.call("GET", "/api/auth/me", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    let sealed = sec::seal(&app, "test-purpose", "synthetic secret").unwrap();
    assert!(sec::unseal(&app, "different-purpose", &sealed).is_err());
    assert!(sec::unseal(&app, "test-purpose", &(sealed + "broken")).is_err());
    let mut production = app.clone();
    let mut production_config = (*app.config).clone();
    production_config.production = true;
    production.config = Arc::new(production_config);
    let cookie = sver::auth::cookie(
        &production,
        production.config.cookie_name(),
        "synthetic",
        60,
    )
    .to_string();
    assert!(
        cookie.starts_with("__Host-sver=")
            && cookie.contains("Secure")
            && !cookie.contains("Domain=")
    );

    // A bcrypt credential must not be rewritten before its second factor is proved.
    let legacy_secret = "GEZDGNBVGY3TQOJQ"; // gitleaks:allow (base32 of "1234567890", RFC 6238 test key)
    // Independent Node crypto/HMAC-SHA1 fixture for this legacy 80-bit key.
    assert_eq!(
        sec::totp(legacy_secret, "legacy-mfa@example.invalid")
            .unwrap()
            .generate(1_111_111_111)
            .to_string(),
        "624539"
    );
    let encrypted = sec::seal(&app, "totp:legacy-mfa", legacy_secret).unwrap();
    sqlx::query("INSERT INTO users(id,email,username,password_hash,mfa_enabled,mfa_secret) VALUES('legacy-mfa','legacy-mfa@example.invalid','LegacyMfa',$1,true,$2)").bind(&bcrypt).bind(encrypted).execute(&app.db).await.unwrap();
    sqlx::query("INSERT INTO recovery_codes(id,user_id,code_hash) VALUES('legacy-mfa-code','legacy-mfa',$1)").bind(bcrypt::hash("SYNTHETICRECOVERYCODE", 4).unwrap()).execute(&app.db).await.unwrap();
    let mut legacy_mfa = Browser::new(router.clone(), 42);
    assert_eq!(
        legacy_mfa
            .login("legacy-mfa@example.invalid", "oldpass")
            .await["requires_mfa"],
        true
    );
    let unchanged: bool =
        sqlx::query_scalar("SELECT password_hash=$1 FROM users WHERE id='legacy-mfa'")
            .bind(&bcrypt)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(unchanged);
    legacy_mfa.ok("POST", "/api/auth/logout", json!({})).await;
    assert_eq!(
        legacy_mfa
            .call(
                "POST",
                "/api/auth/mfa/login",
                json!({"code":"SYNTHETICRECOVERYCODE"})
            )
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    legacy_mfa
        .login("legacy-mfa@example.invalid", "oldpass")
        .await;
    legacy_mfa
        .ok(
            "POST",
            "/api/auth/mfa/login",
            json!({"code":"SYNTHETICRECOVERYCODE"}),
        )
        .await;
    let upgraded: bool = sqlx::query_scalar(
        "SELECT password_hash LIKE '$argon2id$%' FROM users WHERE id='legacy-mfa'",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert!(upgraded);

    legacy_mfa.ok("POST", "/api/auth/logout", json!({})).await;
    legacy_mfa
        .login("legacy-mfa@example.invalid", "oldpass")
        .await;
    let legacy_code = sec::totp(legacy_secret, "legacy-mfa@example.invalid")
        .unwrap()
        .generate_current()
        .to_string();
    legacy_mfa
        .ok("POST", "/api/auth/mfa/login", json!({"code":legacy_code}))
        .await;

    // Email and username share the five-failure limit even from different IPs.
    let mut alternating = Browser::new(router.clone(), 44);
    let mut other_ip = Browser::new(router.clone(), 45);
    for attempt in 0..5 {
        let (browser, identifier) = if attempt % 2 == 0 {
            (&mut alternating, "LEGACY@EXAMPLE.INVALID")
        } else {
            (&mut other_ip, "  legacy_USER  ")
        };
        assert_eq!(
            browser
                .call(
                    "POST",
                    "/api/auth/login",
                    json!({"identifier":identifier,"password":"wrong"})
                )
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    for identifier in ["legacy@example.invalid", "Legacy_User"] {
        let mut fresh_ip = Browser::new(router.clone(), 46);
        assert_eq!(
            fresh_ip
                .call(
                    "POST",
                    "/api/auth/login",
                    json!({"identifier":identifier,"password":"oldpass"})
                )
                .await
                .0,
            StatusCode::TOO_MANY_REQUESTS
        );
    }
    sqlx::query("UPDATE rate_limits SET expires_at=now()-interval '1 second' WHERE key='login:user:legacy-id'").execute(&app.db).await.unwrap();
    other_ip.login("LEGACY_USER", "oldpass").await;

    // Five failed attempts, fixed cooldown that blocked retries cannot extend.
    let mut attacker = Browser::new(router.clone(), 41);
    for _ in 0..5 {
        assert_eq!(
            attacker
                .call(
                    "POST",
                    "/api/auth/login",
                    json!({"email":"legacy@example.invalid","password":"wrong"})
                )
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let blocked = attacker
        .call(
            "POST",
            "/api/auth/login",
            json!({"email":"legacy@example.invalid","password":"oldpass"}),
        )
        .await;
    assert_eq!(blocked.0, StatusCode::TOO_MANY_REQUESTS);
    assert!(blocked.2.contains_key("retry-after"));
    let expiration: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT expires_at FROM rate_limits WHERE key='login:ip:192.0.2.41'")
            .fetch_one(&app.db)
            .await
            .unwrap();
    let _ = attacker
        .call(
            "POST",
            "/api/auth/login",
            json!({"email":"legacy@example.invalid","password":"wrong"}),
        )
        .await;
    let unchanged: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT expires_at FROM rate_limits WHERE key='login:ip:192.0.2.41'")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(expiration, unchanged);
    sqlx::query("UPDATE rate_limits SET expires_at=now()-interval '1 second'")
        .execute(&app.db)
        .await
        .unwrap();
    attacker.login("legacy@example.invalid", "oldpass").await;

    // Deletion, cancellation, expiry, worker erasure. MFA disable revokes streaming immediately.
    a.login("first@example.invalid", new_password).await;
    a.ok("POST", "/api/auth/mfa/login", json!({"code":recovery[6]}))
        .await;
    let mut pending = Browser::new(router.clone(), 43);
    let url = start_oauth(&mut pending, "google", "login", "", "").await;
    assert_eq!(
        finish_oauth(&mut pending, "google", &url, "google-first").await["location"],
        "http://localhost:3000/mfa"
    );
    a.ok(
        "DELETE",
        "/api/auth/oauth/google",
        json!({"code":recovery[7]}),
    )
    .await;
    assert_eq!(
        pending
            .call("POST", "/api/auth/mfa/login", json!({"code":recovery[8]}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    a.ok("POST", "/api/auth/mfa/disable", json!({"code":recovery[8]}))
        .await;
    assert_eq!(
        a.call("GET", "/api/auth/streaming-eligibility", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    a.ok("POST", "/api/auth/account/delete", json!({})).await;
    assert_eq!(
        a.call("GET", "/api/auth/me", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    a.login("first@example.invalid", new_password).await;
    assert!(a.ok("GET", "/api/auth/me", Value::Null).await["deletion_due"].is_string());
    assert_eq!(
        a.call("GET", "/api/auth/sessions", Value::Null).await.0,
        StatusCode::FORBIDDEN
    );
    a.ok("POST", "/api/auth/account/restore", json!({})).await;
    assert!(a.ok("GET", "/api/auth/me", Value::Null).await["deletion_due"].is_null());
    exercise_account_changes(&app, &mut a, &router, new_password).await;
    assert_eq!(
        a.call("DELETE", "/api/auth/oauth/google", json!({}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    a.ok("POST", "/api/auth/account/delete", json!({})).await;
    sqlx::query("UPDATE users SET deleted_at=now()-interval '15 days' WHERE id=$1")
        .bind(&user_id)
        .execute(&app.db)
        .await
        .unwrap();
    assert_eq!(
        a.call(
            "POST",
            "/api/auth/login",
            json!({"email":"first@example.invalid","password":new_password})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    sver::jobs::tick(&app).await.unwrap();
    let erased: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM users WHERE id=$1)")
        .bind(&user_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(erased);
    assert_eq!(
        a.call("GET", "/api/auth/me", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Imported deletions cannot authenticate, recover, restore or enter automatic erasure.
    sqlx::query("INSERT INTO users(id,email,username,password_hash,deleted_at,legacy_deletion_hold) VALUES('legacy-held','held@example.invalid','LegacyHeld',$1,now(),true)")
        .bind(&bcrypt).execute(&app.db).await.unwrap();
    let mut held = Browser::new(router.clone(), 50);
    assert_eq!(
        held.call(
            "POST",
            "/api/auth/login",
            json!({"identifier":"LegacyHeld","password":"oldpass"})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    held.ok(
        "POST",
        "/api/auth/password/forgot",
        json!({"email":"held@example.invalid","turnstile_token":"recovery-ok"}),
    )
    .await;
    let no_mail: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS(SELECT 1 FROM mail_jobs WHERE user_id='legacy-held')",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert!(no_mail);
    sqlx::query("INSERT INTO sessions(id,token_hash,user_id,auth_version,user_agent) VALUES('held-session',$1,'legacy-held',0,'synthetic')")
        .bind(sec::digest("held-session-token")).execute(&app.db).await.unwrap();
    held.cookies
        .insert(app.config.cookie_name().into(), "held-session-token".into());
    assert_eq!(
        held.call("GET", "/api/auth/me", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        held.call("POST", "/api/auth/account/restore", json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("UPDATE users SET deleted_at=now()-interval '30 days' WHERE id='legacy-held'")
        .execute(&app.db)
        .await
        .unwrap();
    sver::jobs::tick(&app).await.unwrap();
    let preserved: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id='legacy-held' AND legacy_deletion_hold)",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert!(preserved);
    println!(
        "Passed: email lifecycle, CSRF, age/policy checks, session revocation, reset race, MFA/recovery replay, three OAuth adapters, linking ownership, bcrypt upgrade, cooldown, deletion and legacy deletion holds."
    );
}
