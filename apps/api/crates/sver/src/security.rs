use crate::{App, Error, Result};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::http::HeaderMap;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use rand::{TryRng, rngs::SysRng};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgConnection};
use std::net::{IpAddr, SocketAddr};

pub fn token() -> String {
    let mut bytes = [0u8; 32];
    SysRng
        .try_fill_bytes(&mut bytes)
        .expect("OS entropy unavailable");
    URL_SAFE_NO_PAD.encode(bytes)
}
pub fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn seal(app: &App, purpose: &str, value: &str) -> Result<String> {
    let mut nonce = [0u8; 12];
    SysRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| Error::internal())?;
    let cipher = Aes256Gcm::new_from_slice(&app.config.key).map_err(|_| Error::internal())?;
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: value.as_bytes(),
                aad: purpose.as_bytes(),
            },
        )
        .map_err(|_| Error::internal())?;
    Ok(format!(
        "v1.{}.{}",
        STANDARD.encode(nonce),
        STANDARD.encode(encrypted)
    ))
}
pub fn unseal(app: &App, purpose: &str, value: &str) -> Result<String> {
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3 || parts[0] != "v1" {
        return Err(Error::internal());
    }
    let nonce = STANDARD.decode(parts[1]).map_err(|_| Error::internal())?;
    if nonce.len() != 12 {
        return Err(Error::internal());
    }
    let encrypted = STANDARD.decode(parts[2]).map_err(|_| Error::internal())?;
    let cipher = Aes256Gcm::new_from_slice(&app.config.key).map_err(|_| Error::internal())?;
    String::from_utf8(
        cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &encrypted,
                    aad: purpose.as_bytes(),
                },
            )
            .map_err(|_| Error::internal())?,
    )
    .map_err(|_| Error::internal())
}
pub fn hash_sync(password: &str) -> Result<String> {
    // A 16-byte salt from the OS, as before the argon2 0.6 update, so new hashes keep their shape.
    let mut salt = [0u8; 16];
    SysRng
        .try_fill_bytes(&mut salt)
        .map_err(|_| Error::internal())?;
    Argon2::default()
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| Error::internal())
}

#[cfg(test)]
mod crypto_tests {
    use super::*;

    #[test]
    fn credentials_keep_their_formats() {
        assert_eq!(
            digest("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha1::Sha1::digest(b"abc")
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<String>(),
            "A9993E364706816ABA3E25717850C26C9CD0D89D"
        );
        let first = token();
        assert_eq!(URL_SAFE_NO_PAD.decode(&first).unwrap().len(), 32);
        assert_ne!(first, token());

        let password = "synthetic migration test password";
        let first = hash_sync(password).unwrap();
        let second = hash_sync(password).unwrap();
        assert_ne!(first, second);
        for encoded in [&first, &second] {
            let hash = PasswordHash::new(encoded).unwrap();
            assert_eq!(hash.algorithm.as_str(), "argon2id");
            assert_eq!(hash.salt.unwrap().as_ref().len(), 16);
            assert!(
                Argon2::default()
                    .verify_password(password.as_bytes(), &hash)
                    .is_ok()
            );
            assert!(
                Argon2::default()
                    .verify_password(b"wrong password", &hash)
                    .is_err()
            );
        }
        // A hash from another implementation (argon2-cffi, the stored accounts' parameters) still
        // verifies, so upgrading the argon2 crate never locks anyone out.
        let pinned = PasswordHash::new("$argon2id$v=19$m=19456,t=2,p=1$qznOpH76YmxW41OO8FgvLQ$QsIGE0uoXAkpo6sGzqwxFYSxzQMxXtghAzPF3rWwrNs").unwrap();
        assert!(
            Argon2::default()
                .verify_password(b"synthetic vector password", &pinned)
                .is_ok()
        );
        assert!(
            Argon2::default()
                .verify_password(b"synthetic vector passwore", &pinned)
                .is_err()
        );
    }
}
pub async fn hash_password(app: &App, password: String) -> Result<String> {
    let permit = app
        .hashing
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| Error::unavailable())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        hash_sync(&password)
    })
    .await
    .map_err(|_| Error::internal())?
}
pub async fn verify_password(app: &App, password: String, stored: String) -> Result<bool> {
    if password.len() > 4096 {
        return Ok(false);
    }
    let permit = app
        .hashing
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| Error::unavailable())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if stored.starts_with("$2") {
            bcrypt::verify(password, &stored).unwrap_or(false)
        } else {
            PasswordHash::new(&stored).is_ok_and(|hash| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &hash)
                    .is_ok()
            })
        }
    })
    .await
    .map_err(|_| Error::internal())
}
pub async fn new_password(app: &App, password: String) -> Result<String> {
    if !(10..=128).contains(&password.chars().count()) || password.len() > 512 {
        return Err(Error::bad("Use a password with 10–128 characters."));
    }
    let hash = sha1::Sha1::digest(password.as_bytes())
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<String>();
    let response = app
        .http
        .get(format!("{}{}", app.config.breach_url, &hash[..5]))
        .header("Add-Padding", "true")
        .send()
        .await
        .map_err(|_| Error::unavailable())?
        .error_for_status()
        .map_err(|_| Error::unavailable())?;
    let body = response.text().await.map_err(|_| Error::unavailable())?;
    // Fail closed on malformed upstream responses rather than treating an error page as safe.
    if body.len() > 2_000_000
        || !body.lines().any(|line| {
            line.split_once(':').is_some_and(|(suffix, count)| {
                suffix.len() == 35 && count.trim().parse::<u64>().is_ok()
            })
        })
    {
        return Err(Error::unavailable());
    }
    if body.lines().any(|line| {
        line.split_once(':').is_some_and(|(suffix, count)| {
            suffix.eq_ignore_ascii_case(&hash[5..]) && count.trim().parse::<u64>().unwrap_or(0) > 0
        })
    }) {
        return Err(Error::bad(
            "That password appears in known breaches. Choose a different password.",
        ));
    }
    hash_password(app, password).await
}
pub fn email(value: &str) -> Result<String> {
    let value = value.trim().to_lowercase();
    if value.len() > 320 || !email_address::EmailAddress::is_valid(&value) {
        return Err(Error::bad("Enter a valid email address."));
    }
    Ok(value)
}
pub fn signup_identity(username: &str, dob: &str) -> Result<NaiveDate> {
    let dob = NaiveDate::parse_from_str(dob, "%Y-%m-%d")
        .map_err(|_| Error::bad("Enter a valid date of birth."))?;
    let today = Utc::now().date_naive();
    let age = today.year()
        - dob.year()
        - i32::from((today.month(), today.day()) < (dob.month(), dob.day()));
    if age < 13 {
        return Err(Error::bad("You must be at least 13 to join S.V.E.R."));
    }
    if age > 120 {
        return Err(Error::bad("Enter a valid date of birth."));
    }
    validate_username(username)?;
    Ok(dob)
}
pub fn validate_username(username: &str) -> Result<()> {
    if let Some(message) = crate::rename::shape_error(username) {
        return Err(Error::bad(message));
    }
    // Same message as a taken name so the reserved list can't be probed (docs/PROFILES.md).
    if crate::reserved::is_reserved(username) {
        return Err(Error::bad(crate::rename::NOT_AVAILABLE));
    }
    Ok(())
}
pub fn client_ip(app: &App, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    if app.config.trusted_proxy == Some(peer.ip())
        && let Some(ip) = headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
    {
        return ip;
    }
    peer.ip()
}
#[derive(FromRow)]
struct Limit {
    count: i32,
    expires_at: DateTime<Utc>,
}
pub async fn reserve(
    app: &App,
    mut keys: Vec<String>,
    limit: i32,
    seconds: i64,
) -> Result<Vec<(String, DateTime<Utc>)>> {
    keys.sort();
    keys.dedup();
    let mut tx = app.db.begin().await?;
    let mut reserved = Vec::new();
    for key in keys {
        let value: Limit = sqlx::query_as("INSERT INTO rate_limits(key,count,expires_at) VALUES($1,1,now()+make_interval(secs => $2)) ON CONFLICT(key) DO UPDATE SET count=CASE WHEN rate_limits.expires_at<=now() THEN 1 ELSE rate_limits.count+1 END, expires_at=CASE WHEN rate_limits.expires_at<=now() THEN EXCLUDED.expires_at ELSE rate_limits.expires_at END RETURNING count, expires_at")
            .bind(&key).bind(seconds as f64).fetch_one(&mut *tx).await?;
        if value.count > limit {
            return Err(Error(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "Too many attempts. Please wait before trying again.",
                Some((value.expires_at - Utc::now()).num_seconds().max(1)),
            ));
        }
        reserved.push((key, value.expires_at));
    }
    tx.commit().await?;
    Ok(reserved)
}
pub async fn release(app: &App, reservations: Vec<(String, DateTime<Utc>)>) -> Result<()> {
    for (key, expiry) in reservations {
        sqlx::query(
            "UPDATE rate_limits SET count=greatest(count-1,0) WHERE key=$1 AND expires_at=$2",
        )
        .bind(key)
        .bind(expiry)
        .execute(&app.db)
        .await?;
    }
    Ok(())
}
pub async fn turnstile(app: &App, token: &str, action: &str, ip: IpAddr) -> Result<()> {
    if token.is_empty() || token.len() > 2048 {
        return Err(Error::bad("Complete the security check."));
    }
    let response: serde_json::Value = app
        .http
        .post(&app.config.turnstile_url)
        .form(&[
            ("secret", app.config.turnstile_secret.as_str()),
            ("response", token),
            ("remoteip", &ip.to_string()),
        ])
        .send()
        .await
        .map_err(|_| Error::unavailable())?
        .error_for_status()
        .map_err(|_| Error::unavailable())?
        .json()
        .await
        .map_err(|_| Error::unavailable())?;
    let hostname = url::Url::parse(&app.config.origin)
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();
    let is_test = !app.config.production && app.config.turnstile_secret.starts_with("1x000");
    if response["success"] != true
        || (!is_test && (response["hostname"] != hostname || response["action"] != action))
    {
        return Err(Error::bad(
            "Security check expired or failed. Please try again.",
        ));
    }
    Ok(())
}
pub fn totp(secret: &str, email: &str) -> Result<totp_rs::TOTP> {
    let secret = totp_rs::Secret::Encoded(secret.to_owned())
        .to_bytes()
        .map_err(|_| Error::internal())?;
    // Legacy otplib accounts have working 80-bit secrets. New enrollment generates 160 bits.
    // All other constructor parameters are fixed here; retain its label validation explicitly.
    if secret.len() < 10 || email.contains(':') {
        return Err(Error::internal());
    }
    Ok(totp_rs::TOTP::new_unchecked(
        totp_rs::Algorithm::SHA1,
        6,
        0,
        30,
        secret,
        Some("S.V.E.R".into()),
        email.into(),
    ))
}
pub async fn prove_mfa(
    app: &App,
    db: &mut PgConnection,
    user: &crate::auth::User,
    code: &str,
) -> Result<()> {
    if !user.mfa_enabled {
        return Ok(());
    }
    let secret = user
        .mfa_secret
        .as_ref()
        .ok_or_else(|| Error::denied("Authenticator recovery is required."))?;
    let otp = totp(
        &unseal(app, &format!("totp:{}", user.id), secret)?,
        &user.email,
    )?;
    let current = Utc::now().timestamp() / 30;
    for step in [current, current - 1, current + 1] {
        if step > user.mfa_last_step && code.len() == 6 && otp.check(code, (step * 30) as u64) {
            sqlx::query("UPDATE users SET mfa_last_step=$2 WHERE id=$1")
                .bind(&user.id)
                .bind(step)
                .execute(&mut *db)
                .await?;
            return Ok(());
        }
    }
    let clean = code.replace('-', "").to_ascii_uppercase();
    if !(10..=64).contains(&clean.len()) {
        return Err(Error::bad(
            "Invalid or already used authenticator/recovery code.",
        ));
    }
    let codes: Vec<(String, String)> =
        sqlx::query_as("SELECT id,code_hash FROM recovery_codes WHERE user_id=$1")
            .bind(&user.id)
            .fetch_all(&mut *db)
            .await?;
    for (id, hash) in codes {
        let matched = if hash.starts_with("$2") {
            verify_password(app, clean.clone(), hash).await?
        } else {
            hash == digest(&clean)
        };
        if matched {
            sqlx::query("DELETE FROM recovery_codes WHERE id=$1")
                .bind(id)
                .execute(&mut *db)
                .await?;
            return Ok(());
        }
    }
    Err(Error::bad(
        "Invalid or already used authenticator/recovery code.",
    ))
}
