use crate::{App, Error, Result, security as sec, user_agent};
use axum::{
    Json,
    extract::{ConnectInfo, Path, Query, State},
    http::HeaderMap,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection, Postgres, Transaction};
use std::net::SocketAddr;

#[derive(FromRow, Clone)]
pub struct User {
    pub id: String,
    pub email: String,
    pub username: String,
    pub password_hash: Option<String>,
    pub email_verified: bool,
    pub deleted_at: Option<DateTime<Utc>>,
    pub legacy_deletion_hold: bool,
    pub mfa_secret: Option<String>,
    pub mfa_enabled: bool,
    pub mfa_last_step: i64,
    pub auth_version: i64,
}
#[derive(FromRow)]
pub struct Session {
    pub id: String,
    pub user_id: String,
    pub auth_version: i64,
    pub expires_at: DateTime<Utc>,
    pub authenticated_at: DateTime<Utc>,
    pub mfa_verified: bool,
}
/// Age gate for restricted playback; a missing birth date never passes.
pub async fn is_adult(db: &mut PgConnection, user: &str) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT coalesce(date_of_birth<=current_date-interval '18 years',false) FROM users WHERE id=$1")
        .bind(user).fetch_optional(db).await?.unwrap_or(false))
}
#[derive(FromRow)]
pub struct Challenge {
    pub user_id: String,
    pub auth_version: i64,
    pub payload: Option<String>,
}
pub fn cookie(app: &App, name: &str, token: &str, seconds: i64) -> Cookie<'static> {
    Cookie::build((name.to_owned(), token.to_owned()))
        .http_only(true)
        .secure(app.config.production)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(
            std::time::Duration::from_secs(seconds.max(0) as u64)
                .try_into()
                .unwrap(),
        )
        .build()
}
pub fn aux_name(app: &App, suffix: &str) -> String {
    format!("{}_{suffix}", app.config.cookie_name())
}
pub fn clear_cookie(app: &App, jar: CookieJar, name: &str) -> CookieJar {
    jar.add(cookie(app, name, "", 0))
}
pub fn recent(session: &Session) -> Result<()> {
    if session.authenticated_at < Utc::now() - Duration::minutes(5) {
        Err(Error::denied(
            "Confirm your sign-in method again before changing account security.",
        ))
    } else {
        Ok(())
    }
}
pub async fn lock_user(db: &mut PgConnection, id: &str) -> Result<User> {
    sqlx::query_as("SELECT * FROM users WHERE id=$1 AND NOT legacy_deletion_hold FOR UPDATE")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(Error::auth)
}
/// Module interface for media lifecycle locks, including held/deleting accounts so
/// their publisher can still be revoked. This does not authorize sign-in or streaming.
pub async fn stream_owner(db: &mut PgConnection, id: &str) -> Result<Option<User>> {
    Ok(sqlx::query_as("SELECT * FROM users WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(db)
        .await?)
}
pub async fn session<'a>(
    app: &'a App,
    jar: &CookieJar,
    allow_deleted: bool,
) -> Result<(Transaction<'a, Postgres>, User, Session)> {
    let token = jar
        .get(app.config.cookie_name())
        .ok_or_else(Error::auth)?
        .value();
    let mut tx = app.db.begin().await?;
    let user: User = sqlx::query_as("SELECT u.* FROM users u WHERE u.id=(SELECT user_id FROM sessions WHERE token_hash=$1) FOR UPDATE")
        .bind(sec::digest(token)).fetch_optional(&mut *tx).await?.ok_or_else(Error::auth)?;
    let session: Session =
        sqlx::query_as("SELECT * FROM sessions WHERE token_hash=$1 AND expires_at>now()")
            .bind(sec::digest(token))
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(Error::auth)?;
    if user.legacy_deletion_hold
        || session.auth_version != user.auth_version
        || (user.mfa_enabled && !session.mfa_verified)
    {
        return Err(Error::auth());
    }
    if user
        .deleted_at
        .is_some_and(|at| !allow_deleted || at + Duration::days(14) <= Utc::now())
    {
        return Err(Error::denied("This account is pending deletion."));
    }
    sqlx::query(
        "UPDATE sessions SET last_seen_at=now(),expires_at=now()+interval '30 days' WHERE id=$1",
    )
    .bind(&session.id)
    .execute(&mut *tx)
    .await?;
    Ok((tx, user, session))
}
pub fn renew(app: &App, jar: CookieJar) -> CookieJar {
    if let Some(value) = jar
        .get(app.config.cookie_name())
        .map(|c| c.value().to_string())
    {
        jar.add(cookie(app, app.config.cookie_name(), &value, 30 * 86400))
    } else {
        jar
    }
}
pub async fn new_session(
    app: &App,
    db: &mut PgConnection,
    user: &User,
    jar: CookieJar,
    headers: &HeaderMap,
    mfa: bool,
) -> Result<CookieJar> {
    let token = sec::token();
    sqlx::query("INSERT INTO sessions(id,token_hash,user_id,auth_version,user_agent,mfa_verified) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(uuid::Uuid::new_v4().to_string()).bind(sec::digest(&token)).bind(&user.id).bind(user.auth_version).bind(user_agent(headers)).bind(mfa).execute(&mut *db).await?;
    // Preserve the legacy ten-device ceiling, evicting the oldest sessions.
    sqlx::query("DELETE FROM sessions WHERE id IN (SELECT id FROM sessions WHERE user_id=$1 ORDER BY created_at DESC,id DESC OFFSET 10)").bind(&user.id).execute(&mut *db).await?;
    Ok(jar.add(cookie(app, app.config.cookie_name(), &token, 30 * 86400)))
}
pub async fn invalidate(
    db: &mut PgConnection,
    user_id: &str,
    keep_session: Option<&str>,
) -> Result<()> {
    sqlx::query("UPDATE users SET auth_version=auth_version+1 WHERE id=$1")
        .bind(user_id)
        .execute(&mut *db)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE user_id=$1 AND ($2::text IS NULL OR id<>$2)")
        .bind(user_id)
        .bind(keep_session)
        .execute(&mut *db)
        .await?;
    sqlx::query("UPDATE sessions SET auth_version=(SELECT auth_version FROM users WHERE id=$1) WHERE user_id=$1").bind(user_id).execute(&mut *db).await?;
    sqlx::query("DELETE FROM challenges WHERE user_id=$1")
        .bind(user_id)
        .execute(&mut *db)
        .await?;
    sqlx::query(
        "DELETE FROM oauth_states WHERE session_id IN (SELECT id FROM sessions WHERE user_id=$1)",
    )
    .bind(user_id)
    .execute(&mut *db)
    .await?;
    // Security resets invalidate login links, but must not silently discard removal notices.
    sqlx::query("DELETE FROM mail_jobs WHERE user_id=$1 AND NOT retry_until_expiry")
        .bind(user_id)
        .execute(&mut *db)
        .await?;
    Ok(())
}
pub async fn finish_login(
    app: &App,
    db: &mut PgConnection,
    user: &User,
    jar: CookieJar,
    headers: &HeaderMap,
    upgraded_hash: Option<String>,
) -> Result<(CookieJar, Value)> {
    if user
        .deleted_at
        .is_some_and(|at| at + Duration::days(14) <= Utc::now())
    {
        return Err(Error::auth());
    }
    if user.mfa_enabled {
        let token = sec::token();
        let browser = sec::token();
        sqlx::query("INSERT INTO challenges(token_hash,user_id,kind,auth_version,browser_hash,payload,expires_at) VALUES($1,$2,'mfa',$3,$4,$5,now()+interval '5 minutes')")
            .bind(sec::digest(&token)).bind(&user.id).bind(user.auth_version).bind(sec::digest(&browser))
            .bind(upgraded_hash.map(|h| sec::seal(app, "rehash", &h)).transpose()?).execute(&mut *db).await?;
        let jar = clear_cookie(app, jar, app.config.cookie_name())
            .add(cookie(app, &aux_name(app, "mfa"), &browser, 300))
            .add(cookie(app, &aux_name(app, "challenge"), &token, 300));
        return Ok((jar, json!({"requires_mfa":true})));
    }
    if let Some(hash) = upgraded_hash {
        sqlx::query("UPDATE users SET password_hash=$2 WHERE id=$1")
            .bind(&user.id)
            .bind(hash)
            .execute(&mut *db)
            .await?;
    }
    Ok((
        new_session(app, db, user, jar, headers, false).await?,
        json!({"signed_in":true, "pending_deletion":user.deleted_at.is_some()}),
    ))
}
pub async fn create_user(
    db: &mut PgConnection,
    email: &str,
    username: &str,
    dob: NaiveDate,
    password: Option<&str>,
    verified: bool,
) -> Result<User> {
    if crate::switches::off(&mut *db, "signups")
        .await
        .map_err(|_| Error::internal())?
    {
        return Err(Error(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Sign-ups are paused right now. Please try again soon.",
            None,
        ));
    }
    let held: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM username_holds WHERE handle_canonical=lower($1) AND released_at>now())")
        .bind(username)
        .fetch_one(&mut *db)
        .await?;
    if held {
        return Err(Error::bad(crate::rename::NOT_AVAILABLE));
    }
    Ok(sqlx::query_as("INSERT INTO users(id,email,username,date_of_birth,password_hash,email_verified) VALUES($1,$2,$3,$4,$5,$6) RETURNING *")
        .bind(uuid::Uuid::new_v4().to_string()).bind(email).bind(username).bind(dob).bind(password).bind(verified).fetch_one(db).await?)
}
pub async fn public_config(State(app): State<App>) -> Json<Value> {
    Json(
        json!({"turnstile_site_key":app.config.turnstile_site_key,"providers":app.config.providers.iter().map(|p| &p.name).collect::<Vec<_>>(),"development":!app.config.production}),
    )
}
#[derive(Deserialize)]
pub struct UsernameCheck {
    username: String,
}
pub async fn username_availability(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(input): Query<UsernameCheck>,
) -> Result<Json<Value>> {
    let ip = sec::client_ip(&app, peer, &headers);
    sec::reserve(&app, vec![format!("username-check:{ip}")], 60, 60).await?;
    if let Err(error) = sec::validate_username(&input.username) {
        return Ok(Json(json!({"available":false,"message":error.1})));
    }
    // Rename and erasure holds make a name unavailable just like an existing account.
    let taken: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE lower(username)=lower($1)) OR EXISTS(SELECT 1 FROM username_holds WHERE handle_canonical=lower($1) AND released_at>now())")
        .bind(&input.username)
        .fetch_one(&app.db)
        .await?;
    Ok(Json(json!({
        "available": !taken,
        "message": if taken { crate::rename::NOT_AVAILABLE } else { "Username is available." }
    })))
}
#[derive(Deserialize)]
pub struct Signup {
    email: String,
    username: String,
    date_of_birth: String,
    password: String,
    turnstile_token: String,
}
pub async fn signup(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(input): Json<Signup>,
) -> Result<(CookieJar, Json<Value>)> {
    let dob = sec::signup_identity(&input.username, &input.date_of_birth)?;
    let email = sec::email(&input.email)?;
    let ip = sec::client_ip(&app, peer, &headers);
    sec::reserve(&app, vec![format!("signup:{ip}")], 5, 900).await?;
    sec::turnstile(&app, &input.turnstile_token, "signup", ip).await?;
    let hash = sec::new_password(&app, input.password).await?;
    let mut tx = app.db.begin().await?;
    let user = create_user(&mut tx, &email, &input.username, dob, Some(&hash), false).await?;
    crate::jobs::queue_email(&app, &mut tx, &user, "verify").await?;
    let jar = new_session(&app, &mut tx, &user, jar, &headers, false).await?;
    tx.commit().await?;
    Ok((
        jar,
        Json(json!({"signed_in":true,"verification_queued":true})),
    ))
}
#[derive(Deserialize)]
pub struct Login {
    #[serde(alias = "email")]
    identifier: String,
    password: String,
}
pub async fn login(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(input): Json<Login>,
) -> Result<(CookieJar, Json<Value>)> {
    let identifier = input.identifier.trim().to_lowercase();
    if identifier.is_empty() || identifier.len() > 320 {
        return Err(Error::bad("Enter your email or username."));
    }
    // Match the legacy email-or-username lookup; signup restrictions do not apply here.
    let lookup = if identifier.contains('@') {
        "SELECT * FROM users WHERE lower(email)=$1"
    } else {
        "SELECT * FROM users WHERE lower(username)=$1"
    };
    let user: Option<User> = sqlx::query_as(lookup)
        .bind(&identifier)
        .fetch_optional(&app.db)
        .await?;
    // Both identifiers must share one account limit, including across different IPs.
    let account_key = user.as_ref().map_or_else(
        || format!("login:identifier:{}", sec::digest(&identifier)),
        |user| format!("login:user:{}", user.id),
    );
    let ip = sec::client_ip(&app, peer, &headers);
    let permits = sec::reserve(&app, vec![account_key, format!("login:ip:{ip}")], 5, 900).await?;
    let hash = user
        .as_ref()
        .and_then(|u| u.password_hash.clone())
        .unwrap_or_else(|| (*app.dummy_hash).clone());
    let valid = sec::verify_password(&app, input.password.clone(), hash.clone()).await?;
    let user = user
        .filter(|u| u.password_hash.is_some() && valid)
        .ok_or_else(|| Error::bad("Incorrect email/username or password."))?;
    let upgrade = if hash.starts_with("$2") {
        Some(sec::hash_password(&app, input.password).await?)
    } else {
        None
    };
    let mut tx = app.db.begin().await?;
    let locked = lock_user(&mut tx, &user.id).await?;
    if user.auth_version != locked.auth_version || user.password_hash != locked.password_hash {
        return Err(Error::auth());
    }
    let (jar, body) = finish_login(&app, &mut tx, &locked, jar, &headers, upgrade).await?;
    tx.commit().await?;
    sec::release(&app, permits).await?;
    Ok((jar, Json(body)))
}
#[derive(Deserialize, Default)]
pub struct Code {
    #[serde(default)]
    pub code: String,
}
pub async fn mfa_login(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(input): Json<Code>,
) -> Result<(CookieJar, Json<Value>)> {
    let token = jar
        .get(&aux_name(&app, "challenge"))
        .ok_or_else(Error::auth)?
        .value()
        .to_string();
    let browser = jar
        .get(&aux_name(&app, "mfa"))
        .ok_or_else(Error::auth)?
        .value();
    let ip = sec::client_ip(&app, peer, &headers);
    let challenge: Challenge = sqlx::query_as("SELECT * FROM challenges WHERE token_hash=$1 AND browser_hash=$2 AND kind='mfa' AND expires_at>now()")
        .bind(sec::digest(&token)).bind(sec::digest(browser)).fetch_optional(&app.db).await?.ok_or_else(Error::auth)?;
    let permits = sec::reserve(
        &app,
        vec![
            format!("mfa:user:{}", challenge.user_id),
            format!("mfa:ip:{ip}"),
        ],
        5,
        900,
    )
    .await?;
    let mut tx = app.db.begin().await?;
    let user = lock_user(&mut tx, &challenge.user_id).await?;
    if !user.mfa_enabled
        || user.auth_version != challenge.auth_version
        || user
            .deleted_at
            .is_some_and(|at| at + Duration::days(14) <= Utc::now())
    {
        return Err(Error::auth());
    }
    let consumed = sqlx::query("DELETE FROM challenges WHERE token_hash=$1 AND expires_at>now()")
        .bind(sec::digest(&token))
        .execute(&mut *tx)
        .await?;
    if consumed.rows_affected() != 1 {
        return Err(Error::auth());
    }
    sec::prove_mfa(&app, &mut tx, &user, &input.code).await?;
    if let Some(hash) = challenge.payload {
        sqlx::query("UPDATE users SET password_hash=$2 WHERE id=$1")
            .bind(&user.id)
            .bind(sec::unseal(&app, "rehash", &hash)?)
            .execute(&mut *tx)
            .await?;
    }
    let jar = clear_cookie(
        &app,
        clear_cookie(&app, jar, &aux_name(&app, "challenge")),
        &aux_name(&app, "mfa"),
    );
    let jar = new_session(&app, &mut tx, &user, jar, &headers, true).await?;
    tx.commit().await?;
    eprintln!("auth_event=mfa_login outcome=verified");
    sec::release(&app, permits).await?;
    Ok((
        jar,
        Json(json!({"signed_in":true,"pending_deletion":user.deleted_at.is_some()})),
    ))
}
pub async fn me(State(app): State<App>, jar: CookieJar) -> Result<(CookieJar, Json<Value>)> {
    let (mut tx, user, session) = session(&app, &jar, true).await?;
    let providers: Vec<String> =
        sqlx::query_scalar("SELECT provider FROM identities WHERE user_id=$1 ORDER BY provider")
            .bind(&user.id)
            .fetch_all(&mut *tx)
            .await?;
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM recovery_codes WHERE user_id=$1")
        .bind(&user.id)
        .fetch_one(&mut *tx)
        .await?;
    let faction = crate::factions::membership(&mut tx, &user.id)
        .await
        .map_err(|_| Error::internal())?;
    let pending_email = crate::account::pending_email(&mut tx, &user.id).await?;
    tx.commit().await?;
    Ok((
        renew(&app, jar),
        Json(
            json!({"id":user.id,"email":user.email,"username":user.username,"faction":faction,"email_verified":user.email_verified,"mfa_enabled":user.mfa_enabled,"has_password":user.password_hash.is_some(),"providers":providers,"recovery_codes_remaining":remaining,"session_id":session.id,"reauthenticated":recent(&session).is_ok(),"deletion_due":user.deleted_at.map(|d|d+Duration::days(14)),"pending_email":pending_email}),
        ),
    ))
}
pub async fn logout(State(app): State<App>, jar: CookieJar) -> Result<(CookieJar, Json<Value>)> {
    match session(&app, &jar, true).await {
        Ok((mut tx, _, session)) => {
            sqlx::query("DELETE FROM sessions WHERE id=$1")
                .bind(session.id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        Err(e)
            if e.0 == axum::http::StatusCode::UNAUTHORIZED
                || e.0 == axum::http::StatusCode::FORBIDDEN => {}
        Err(e) => return Err(e),
    }
    if let (Some(token), Some(browser)) = (
        jar.get(&aux_name(&app, "challenge")),
        jar.get(&aux_name(&app, "mfa")),
    ) {
        sqlx::query(
            "DELETE FROM challenges WHERE token_hash=$1 AND browser_hash=$2 AND kind='mfa'",
        )
        .bind(sec::digest(token.value()))
        .bind(sec::digest(browser.value()))
        .execute(&app.db)
        .await?;
    }
    if let Some(browser) = jar.get(&aux_name(&app, "oauth")) {
        sqlx::query("DELETE FROM oauth_states WHERE browser_hash=$1")
            .bind(sec::digest(browser.value()))
            .execute(&app.db)
            .await?;
    }
    let mut jar = clear_cookie(&app, jar, app.config.cookie_name());
    for name in ["challenge", "mfa", "oauth"] {
        jar = clear_cookie(&app, jar, &aux_name(&app, name));
    }
    Ok((jar, Json(json!({"signed_out":true}))))
}
pub async fn sessions(State(app): State<App>, jar: CookieJar) -> Result<(CookieJar, Json<Value>)> {
    let (mut tx, user, _) = session(&app, &jar, false).await?;
    let values: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'user_agent',user_agent,'created_at',created_at,'last_seen_at',last_seen_at,'expires_at',expires_at) FROM sessions WHERE user_id=$1 AND expires_at>now() ORDER BY last_seen_at DESC").bind(user.id).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok((renew(&app, jar), Json(json!({"sessions":values}))))
}
pub async fn revoke_session(
    State(app): State<App>,
    jar: CookieJar,
    Path(id): Path<String>,
) -> Result<(CookieJar, Json<Value>)> {
    let (mut tx, user, current) = session(&app, &jar, false).await?;
    sqlx::query("DELETE FROM sessions WHERE id=$1 AND user_id=$2")
        .bind(&id)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((
        if id == current.id {
            clear_cookie(&app, jar, app.config.cookie_name())
        } else {
            renew(&app, jar)
        },
        Json(json!({"revoked":true})),
    ))
}
pub async fn revoke_all(
    State(app): State<App>,
    jar: CookieJar,
) -> Result<(CookieJar, Json<Value>)> {
    let (mut tx, user, _) = session(&app, &jar, false).await?;
    invalidate(&mut tx, &user.id, None).await?;
    tx.commit().await?;
    Ok((
        clear_cookie(&app, jar, app.config.cookie_name()),
        Json(json!({"signed_out":true})),
    ))
}
#[derive(Deserialize)]
pub struct Password {
    password: String,
}
pub async fn reauth(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(input): Json<Password>,
) -> Result<Json<Value>> {
    let (tx, user, session_info) = session(&app, &jar, true).await?;
    tx.commit().await?;
    let ip = sec::client_ip(&app, peer, &headers);
    let permits = sec::reserve(
        &app,
        vec![
            format!("reauth:user:{}", user.id),
            format!("reauth:ip:{ip}"),
        ],
        5,
        900,
    )
    .await?;
    let hash = user
        .password_hash
        .clone()
        .ok_or_else(|| Error::bad("Use a linked provider to confirm your identity."))?;
    if !sec::verify_password(&app, input.password, hash).await? {
        return Err(Error::bad("Password is incorrect."));
    }
    let (mut tx, current, _) = session(&app, &jar, true).await?;
    if current.auth_version != user.auth_version {
        return Err(Error::auth());
    }
    sqlx::query("UPDATE sessions SET authenticated_at=now() WHERE id=$1")
        .bind(session_info.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    sec::release(&app, permits).await?;
    Ok(Json(json!({"confirmed":true})))
}
pub async fn resend(State(app): State<App>, jar: CookieJar) -> Result<Json<Value>> {
    let (mut tx, user, _) = session(&app, &jar, false).await?;
    if !user.email_verified {
        sec::reserve(&app, vec![format!("verify:resend:{}", user.id)], 1, 60).await?;
        crate::jobs::queue_email(&app, &mut tx, &user, "verify").await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"verification_queued":true})))
}
#[derive(Deserialize)]
pub struct Token {
    token: String,
}
pub async fn consume<'a>(
    app: &'a App,
    token: &str,
    kind: &str,
) -> Result<(Transaction<'a, Postgres>, User)> {
    if token.len() > 128 {
        return Err(Error::bad("Invalid or expired link."));
    }
    let mut tx = app.db.begin().await?;
    let user: User = sqlx::query_as("SELECT u.* FROM users u WHERE id=(SELECT user_id FROM challenges WHERE token_hash=$1 AND kind=$2) FOR UPDATE")
        .bind(sec::digest(token)).bind(kind).fetch_optional(&mut *tx).await?.ok_or_else(||Error::bad("Invalid or expired link."))?;
    let deleted = sqlx::query("DELETE FROM challenges WHERE token_hash=$1 AND kind=$2 AND expires_at>now() AND auth_version=$3").bind(sec::digest(token)).bind(kind).bind(user.auth_version).execute(&mut *tx).await?;
    if deleted.rows_affected() != 1
        || user
            .deleted_at
            .is_some_and(|d| d + Duration::days(14) <= Utc::now())
    {
        return Err(Error::bad("Invalid or expired link."));
    }
    Ok((tx, user))
}
pub async fn verify_email(State(app): State<App>, Json(input): Json<Token>) -> Result<Json<Value>> {
    // The same link page confirms a new address from an email change (docs/LOGIN.md).
    if crate::account::is_email_change(&app, &input.token).await? {
        crate::account::confirm_email(&app, &input.token).await?;
        return Ok(Json(json!({"verified":true,"email_changed":true})));
    }
    let (mut tx, user) = consume(&app, &input.token, "verify").await?;
    sqlx::query("UPDATE users SET email_verified=true WHERE id=$1")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({"verified":true})))
}
#[derive(Deserialize)]
pub struct Forgot {
    email: String,
    turnstile_token: String,
}
pub async fn forgot(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<Forgot>,
) -> Result<Json<Value>> {
    let email = sec::email(&input.email)?;
    let ip = sec::client_ip(&app, peer, &headers);
    sec::reserve(&app, vec![format!("forgot:ip:{ip}")], 5, 900).await?;
    sec::turnstile(&app, &input.turnstile_token, "recovery", ip).await?;
    // Apply the same per-email cooldown whether or not an account exists.
    let cooldown = sec::reserve(
        &app,
        vec![format!("forgot:email:{}", sec::digest(&email))],
        1,
        60,
    )
    .await;
    if let Err(error) = cooldown {
        if error.0 != axum::http::StatusCode::TOO_MANY_REQUESTS {
            return Err(error);
        }
    } else {
        let mut tx = app.db.begin().await?;
        let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE lower(email)=$1 AND NOT legacy_deletion_hold AND (deleted_at IS NULL OR deleted_at>now()-interval '14 days') FOR UPDATE").bind(email).fetch_optional(&mut *tx).await?;
        if let Some(user) = user {
            crate::jobs::queue_email(&app, &mut tx, &user, "reset").await?;
        }
        tx.commit().await?;
    }
    Ok(Json(
        json!({"message":"If that account exists, a recovery email has been queued."}),
    ))
}
#[derive(Deserialize)]
pub struct Reset {
    token: String,
    password: String,
}
pub async fn reset(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<Reset>,
) -> Result<Json<Value>> {
    let ip = sec::client_ip(&app, peer, &headers);
    sec::reserve(&app, vec![format!("reset:ip:{ip}")], 5, 900).await?;
    // Reject invalid links before doing a network lookup or expensive hash; consume atomically below.
    let (tx, _) = consume(&app, &input.token, "reset").await?;
    tx.rollback().await?;
    let hash = sec::new_password(&app, input.password).await?;
    let (mut tx, user) = consume(&app, &input.token, "reset").await?;
    crate::streams::revoke(&mut tx, &user.id).await?;
    sqlx::query("UPDATE users SET password_hash=$2 WHERE id=$1")
        .bind(&user.id)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
    invalidate(&mut tx, &user.id, None).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"reset":true,"message":"Password changed. Sign in again on your devices."}),
    ))
}
pub async fn sensitive<'a>(
    app: &'a App,
    jar: &CookieJar,
    code: &str,
) -> Result<(Transaction<'a, Postgres>, User, Session)> {
    let (mut tx, user, session) = session(app, jar, false).await?;
    recent(&session)?;
    let permits = sec::reserve(app, vec![format!("sensitive:{}", user.id)], 5, 900).await?;
    sec::prove_mfa(app, &mut tx, &user, code).await?;
    sec::release(app, permits).await?;
    Ok((tx, user, session))
}
pub async fn mfa_setup(State(app): State<App>, jar: CookieJar) -> Result<Json<Value>> {
    let (mut tx, user, session) = session(&app, &jar, false).await?;
    recent(&session)?;
    if user.mfa_enabled {
        return Err(Error::bad("An authenticator is already enabled."));
    }
    sec::reserve(&app, vec![format!("setup:{}", user.id)], 5, 900).await?;
    let secret = totp_rs::Secret::new({
        use rand::TryRng;
        let mut bytes = vec![0u8; 20];
        rand::rngs::SysRng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| Error::internal())?;
        bytes.into_boxed_slice()
    })
    .to_base32();
    sqlx::query("DELETE FROM challenges WHERE user_id=$1 AND kind='setup'")
        .bind(&user.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO challenges(token_hash,user_id,kind,auth_version,browser_hash,payload,expires_at) VALUES($1,$2,'setup',$3,$4,$5,now()+interval '10 minutes')")
        .bind(sec::digest(&sec::token())).bind(&user.id).bind(user.auth_version).bind(sec::digest(&session.id)).bind(sec::seal(&app,&format!("totp:{}",user.id),&secret)?).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"secret":secret,"uri":sec::totp(&secret,&user.email)?.to_url().map_err(|_| Error::internal())?}),
    ))
}
pub async fn codes(db: &mut PgConnection, user_id: &str) -> Result<Vec<String>> {
    sqlx::query("DELETE FROM recovery_codes WHERE user_id=$1")
        .bind(user_id)
        .execute(&mut *db)
        .await?;
    let mut codes = Vec::new();
    for _ in 0..10 {
        let code = uuid::Uuid::new_v4().simple().to_string().to_uppercase();
        sqlx::query("INSERT INTO recovery_codes(id,user_id,code_hash) VALUES($1,$2,$3)")
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(user_id)
            .bind(sec::digest(&code))
            .execute(&mut *db)
            .await?;
        codes.push(
            code.as_bytes()
                .chunks(8)
                .map(|b| std::str::from_utf8(b).unwrap())
                .collect::<Vec<_>>()
                .join("-"),
        );
    }
    Ok(codes)
}
pub async fn mfa_enable(
    State(app): State<App>,
    jar: CookieJar,
    Json(input): Json<Code>,
) -> Result<Json<Value>> {
    if input.code.len() != 6 || !input.code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::bad(
            "Enter the six-digit code from the authenticator you are setting up.",
        ));
    }
    let (mut tx, mut user, session) = session(&app, &jar, false).await?;
    recent(&session)?;
    if user.mfa_enabled {
        return Err(Error::bad("An authenticator is already enabled."));
    }
    let permits = sec::reserve(&app, vec![format!("enable:{}", user.id)], 5, 900).await?;
    let secret: String = sqlx::query_scalar("DELETE FROM challenges WHERE user_id=$1 AND kind='setup' AND browser_hash=$2 AND auth_version=$3 AND expires_at>now() RETURNING payload")
        .bind(&user.id).bind(sec::digest(&session.id)).bind(user.auth_version).fetch_optional(&mut *tx).await?.ok_or_else(||Error::bad("Authenticator setup expired. Start again."))?;
    user.mfa_enabled = true;
    user.mfa_secret = Some(secret.clone());
    sec::prove_mfa(&app, &mut tx, &user, &input.code).await?;
    sqlx::query("UPDATE users SET mfa_enabled=true,mfa_secret=$2 WHERE id=$1")
        .bind(&user.id)
        .bind(secret)
        .execute(&mut *tx)
        .await?;
    let recovery = codes(&mut tx, &user.id).await?;
    invalidate(&mut tx, &user.id, Some(&session.id)).await?;
    sqlx::query("UPDATE sessions SET mfa_verified=true WHERE id=$1")
        .bind(session.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    sec::release(&app, permits).await?;
    Ok(Json(json!({"enabled":true,"recovery_codes":recovery})))
}
pub async fn mfa_disable(
    State(app): State<App>,
    jar: CookieJar,
    Json(input): Json<Code>,
) -> Result<Json<Value>> {
    let (mut tx, user, session) = sensitive(&app, &jar, &input.code).await?;
    if !user.mfa_enabled {
        return Err(Error::bad("An authenticator is not enabled."));
    }
    crate::streams::revoke(&mut tx, &user.id).await?;
    sqlx::query("UPDATE users SET mfa_enabled=false,mfa_secret=NULL,mfa_last_step=-1 WHERE id=$1")
        .bind(&user.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM recovery_codes WHERE user_id=$1")
        .bind(&user.id)
        .execute(&mut *tx)
        .await?;
    invalidate(&mut tx, &user.id, Some(&session.id)).await?;
    tx.commit().await?;
    Ok(Json(json!({"disabled":true})))
}
pub async fn mfa_recovery(
    State(app): State<App>,
    jar: CookieJar,
    Json(input): Json<Code>,
) -> Result<Json<Value>> {
    let (mut tx, user, session) = sensitive(&app, &jar, &input.code).await?;
    if !user.mfa_enabled {
        return Err(Error::bad("Enable an authenticator first."));
    }
    let recovery = codes(&mut tx, &user.id).await?;
    invalidate(&mut tx, &user.id, Some(&session.id)).await?;
    tx.commit().await?;
    Ok(Json(json!({"recovery_codes":recovery})))
}
pub async fn delete_account(
    State(app): State<App>,
    jar: CookieJar,
    Json(input): Json<Code>,
) -> Result<(CookieJar, Json<Value>)> {
    let (mut tx, user, _) = sensitive(&app, &jar, &input.code).await?;
    crate::streams::revoke(&mut tx, &user.id).await?;
    sqlx::query("UPDATE users SET deleted_at=now() WHERE id=$1")
        .bind(&user.id)
        .execute(&mut *tx)
        .await?;
    invalidate(&mut tx, &user.id, None).await?;
    tx.commit().await?;
    Ok((
        clear_cookie(&app, jar, app.config.cookie_name()),
        Json(json!({"deletion_requested":true})),
    ))
}
pub async fn restore_account(
    State(app): State<App>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Result<(CookieJar, Json<Value>)> {
    let (mut tx, mut user, current) = session(&app, &jar, true).await?;
    recent(&current)?;
    if user.deleted_at.is_none() {
        return Err(Error::bad("This account is not pending deletion."));
    }
    sqlx::query("UPDATE users SET deleted_at=NULL WHERE id=$1")
        .bind(&user.id)
        .execute(&mut *tx)
        .await?;
    invalidate(&mut tx, &user.id, None).await?;
    user.auth_version += 1;
    user.deleted_at = None;
    let jar = new_session(&app, &mut tx, &user, jar, &headers, user.mfa_enabled).await?;
    tx.commit().await?;
    Ok((jar, Json(json!({"restored":true}))))
}
pub fn authorize_streaming(user: &User, session: &Session) -> Result<()> {
    if user.deleted_at.is_some() || !user.email_verified {
        return Err(Error::denied("Verify your email before streaming."));
    }
    if !user.mfa_enabled || !session.mfa_verified {
        return Err(Error::denied(
            "Enable an authenticator before accessing streaming credentials.",
        ));
    }
    Ok(())
}
pub async fn streaming_eligibility(State(app): State<App>, jar: CookieJar) -> Result<Json<Value>> {
    let (mut tx, user, session) = session(&app, &jar, false).await?;
    authorize_streaming(&user, &session)?;
    if !crate::streams::eligible(&mut tx, &user).await? {
        return Err(Error::denied("Your channel is not eligible to stream."));
    }
    tx.commit().await?;
    Ok(Json(json!({"eligible":true})))
}
/// Minimal preserved membership projection for the one-time faction import. No credentials leave Auth.
pub async fn legacy_factions(
    db: &mut sqlx::PgConnection,
) -> Result<Vec<(String, String, DateTime<Utc>)>> {
    Ok(sqlx::query_as("SELECT d.user_id,d.account->>'factionId',coalesce((d.account->>'factionJoinedAt')::timestamptz,u.created_at) FROM legacy_account_data d JOIN users u ON u.id=d.user_id WHERE d.account->>'factionId' IS NOT NULL ORDER BY d.user_id")
        .fetch_all(db).await?)
}
