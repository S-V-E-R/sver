//! Raven's Eye (docs/ADMIN.md): staff-only, finished days stored once, today live. Runs late in the
//! stream suite so the day already has broadcasts, follows and chat to count.
use super::Env;
use super::chat::{call, person};
use axum::http::StatusCode;
use serde_json::Value;

pub async fn exercise(e: &Env) {
    let viewer = person(e, "re-viewer", "RavenViewer", true).await;
    assert_eq!(
        call(
            e,
            "GET",
            "/api/admin/ravens-eye",
            Some(&viewer),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let staff = person(e, "re-staff", "RavenStaff", true).await;
    e.sql("UPDATE users SET mfa_enabled=true,mfa_secret='synthetic' WHERE id='re-staff'")
        .await;
    e.sql("UPDATE sessions SET mfa_verified=true WHERE user_id='re-staff'")
        .await;
    e.sql("INSERT INTO staff_roles(user_id,role) VALUES('re-staff','admin')")
        .await;

    sver::ravens_eye::tick(&e.app).await.unwrap();
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM ravens_eye_days")
        .fetch_one(&e.app.db)
        .await
        .unwrap();
    assert_eq!(stored, 10, "the newest 10 finished days per pass");
    let today: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM ravens_eye_days WHERE day=(now() AT TIME ZONE 'UTC')::date)",
    )
    .fetch_one(&e.app.db)
    .await
    .unwrap();
    assert!(!today, "today is never stored");

    let (status, view) = call(
        e,
        "GET",
        "/api/admin/ravens-eye?days=7",
        Some(&staff),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let days = view["days"].as_array().unwrap();
    let live = days.last().unwrap();
    assert_eq!(live["partial"], true);
    // Earlier tests leave broadcasts and sign-ups today; follows and chat may already be gone.
    for key in ["broadcasts", "signups", "streamers"] {
        assert!(live["stats"][key].as_i64().unwrap() > 0, "{key}: {live}");
    }
    for key in ["follows", "messages", "viewers", "peak_viewers"] {
        assert!(live["stats"][key].as_i64().is_some(), "{key}: {live}");
    }
    let tribute = &live["stats"]["money"]["tribute"];
    assert!(
        tribute["usd_cents"].is_i64() && tribute["count"].as_i64().unwrap() > 0,
        "{live}"
    );
    assert!(live["stats"]["money"].is_object());
    assert!(live["stats"]["top_categories"].is_array());
    assert_eq!(
        call(
            e,
            "GET",
            "/api/admin/ravens-eye?days=400",
            Some(&staff),
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
