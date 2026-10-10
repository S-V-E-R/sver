//! Native alerts and overlays (docs/OVERLAYS.md) over a real WebSocket: the private link, the
//! opening state, alerts from live events with the channel's settings applied, the goal bar,
//! test alerts and revocation.
use super::Env;
use super::chat::{call, next_json, person};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::net::SocketAddr;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

async fn emit(e: &Env, kind: &str, data: Value) {
    let mut db = e.app.db.acquire().await.unwrap();
    sver::events::emit(&mut db, "stream-owner", kind, data)
        .await
        .unwrap();
    drop(db);
    sver::events::drain(&e.app).await.unwrap();
}

pub async fn exercise(e: &Env) {
    sver::events::drain(&e.app).await.unwrap();
    let mut settings = e.call("GET", "/api/me/overlays", Value::Null).await["settings"].clone();
    settings["tribute"]["min"] = json!(100);
    settings["goal"] = json!({"kind": "followers", "target": 10, "label": "Road to 10"});
    e.call("PUT", "/api/me/overlays", settings.clone()).await;
    let url = e.call("POST", "/api/me/overlays/link", Value::Null).await["url"]
        .as_str()
        .unwrap()
        .to_string();
    let token = url.rsplit('/').next().unwrap().to_string();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = sver::router(e.app.clone());
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let connect = |token: String, origin: String| async move {
        let mut request = format!("ws://{address}/api/overlays/ws?token={token}")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
        tokio_tungstenite::connect_async(request).await
    };
    let origin = e.app.config.origin.clone();
    assert!(
        connect(token.clone(), "https://evil.example".into())
            .await
            .is_err(),
        "foreign origin"
    );
    assert!(
        connect("wrong".into(), origin.clone()).await.is_err(),
        "unknown link"
    );
    let (mut ws, _) = connect(token.clone(), origin.clone()).await.unwrap();
    let hello = next_json(&mut ws).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["settings"]["goal"]["label"], "Road to 10");
    let followers = hello["goal_count"].as_i64().unwrap();

    // A follow moves the goal bar and plays its alert.
    let fan = person(e, "ov-fan", "OverlayFan", true).await;
    call(e, "PUT", "/api/follows/Streamer", Some(&fan), Value::Null).await;
    sver::events::drain(&e.app).await.unwrap();
    let goal = next_json(&mut ws).await;
    assert_eq!(
        (goal["type"].as_str(), goal["goal_count"].as_i64()),
        (Some("goal"), Some(followers + 1))
    );
    let followed = next_json(&mut ws).await;
    assert_eq!(followed["text"], "OverlayFan followed!", "{followed}");

    // A tribute under the minimum stays off; one over it plays with its message.
    emit(
        e,
        "tributes",
        json!({"user": "OverlayFan", "valor": 50, "message": "small"}),
    )
    .await;
    emit(
        e,
        "tributes",
        json!({"user": "OverlayFan", "valor": 150, "message": "gg"}),
    )
    .await;
    let tribute = next_json(&mut ws).await;
    assert_eq!(
        (tribute["text"].as_str(), tribute["message"].as_str()),
        (Some("OverlayFan paid 150 Valor"), Some("gg"))
    );

    // Test alerts reach the open overlay only.
    e.call("POST", "/api/me/overlays/test", json!({"kind": "raid"}))
        .await;
    assert_eq!(next_json(&mut ws).await["text"], "TestViewer is raiding!");

    // Saving updates the overlay at once; a type turned off stays off.
    settings["sub"]["on"] = json!(false);
    e.call("PUT", "/api/me/overlays", settings.clone()).await;
    assert_eq!(next_json(&mut ws).await["type"], "settings");
    emit(
        e,
        "subs",
        json!({"user": "OverlayFan", "tier": 1, "months": 1}),
    )
    .await;
    emit(
        e,
        "skills",
        json!({"user": "OverlayFan", "skill": "Confetti", "valor": 300}),
    )
    .await;
    assert_eq!(
        next_json(&mut ws).await["text"],
        "OverlayFan played Confetti!",
        "the sub alert was off"
    );

    // Bad settings are refused; a new link stops the old one.
    let mut bad = settings.clone();
    bad["follow"]["seconds"] = json!(60);
    let (status, _) = e
        .request("PUT", "/api/me/overlays", bad, true, true, false)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    e.call("POST", "/api/me/overlays/link", Value::Null).await;
    assert!(
        connect(token.clone(), origin.clone()).await.is_err(),
        "the old link is gone"
    );
    e.call("DELETE", "/api/me/overlays/link", Value::Null).await;
    assert_eq!(
        e.call("GET", "/api/me/overlays", Value::Null).await["linked"],
        false
    );
}
