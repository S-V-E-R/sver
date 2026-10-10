//! Native alerts and overlays (docs/OVERLAYS.md): an OBS browser source with a private link that
//! plays the channel's follows, subscriptions, tributes, Skills and incoming raids as alerts, plus a
//! goal bar and a recent-events list. Alerts come from the live events records (`events`), so
//! whatever those leave out (blocked people, refused chat, Valor that didn't move) never shows.
//! Templates are filled here; the page only draws plain text.
use crate::{
    App,
    profiles::{self, Fail, Res},
    security as sec,
};
use axum::{
    Json, Router,
    extract::{
        ConnectInfo, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::HeaderMap,
    response::Response,
    routing::{get, post},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{net::SocketAddr, time::Duration};

/// Alert kinds and the live-events topic each comes from.
const KINDS: [(&str, &str); 5] = [
    ("follow", "follows:detail"),
    ("sub", "subs"),
    ("tribute", "tributes"),
    ("skill", "skills"),
    ("raid", "raid:incoming"),
];
const SOUNDS: [&str; 3] = ["chime", "horn", "none"];

/// The Proposed defaults; saved settings replace them type by type.
fn defaults() -> Value {
    json!({
        "follow": {"on": true, "text": "{name} followed!", "seconds": 6},
        "sub": {"on": true, "text": "{name} subscribed at Tier {tier} ({months} months)!", "seconds": 8},
        "tribute": {"on": true, "text": "{name} paid {valor} Valor", "seconds": 8, "min": 10, "show_message": true},
        "skill": {"on": true, "text": "{name} played {skill}!", "seconds": 6},
        "raid": {"on": true, "text": "{name} is raiding!", "seconds": 8},
        "sound": "chime", "volume": 60,
        "goal": {"kind": null, "target": 100, "label": ""},
    })
}
fn bad(field: &'static str, message: &'static str) -> Fail {
    Fail::field(field, message)
}
/// Checks a full settings object from Studio and returns it normalized.
fn validate(input: &Value) -> Res<Value> {
    let mut out = defaults();
    for (kind, _) in KINDS {
        let given = &input[kind];
        let text = given["text"].as_str().unwrap_or_default().trim();
        if text.is_empty() || text.chars().count() > 120 || text.chars().any(char::is_control) {
            return Err(bad("text", "Write each alert message in 1–120 characters."));
        }
        let seconds = given["seconds"].as_i64().unwrap_or(0);
        if !(3..=15).contains(&seconds) {
            return Err(bad("seconds", "Alerts show for 3–15 seconds."));
        }
        out[kind]["on"] = json!(given["on"].as_bool().unwrap_or(false));
        out[kind]["text"] = json!(text);
        out[kind]["seconds"] = json!(seconds);
    }
    let min = input["tribute"]["min"].as_i64().unwrap_or(0);
    if !(10..=1_000_000).contains(&min) {
        return Err(bad("min", "The tribute minimum is 10 Valor or more."));
    }
    out["tribute"]["min"] = json!(min);
    out["tribute"]["show_message"] =
        json!(input["tribute"]["show_message"].as_bool().unwrap_or(true));
    let sound = input["sound"].as_str().unwrap_or_default();
    if !SOUNDS.contains(&sound) {
        return Err(bad("sound", "Choose a sound."));
    }
    let volume = input["volume"].as_i64().unwrap_or(-1);
    if !(0..=100).contains(&volume) {
        return Err(bad("volume", "Volume is 0–100."));
    }
    out["sound"] = json!(sound);
    out["volume"] = json!(volume);
    let goal = &input["goal"];
    let kind = goal["kind"].as_str();
    if kind.is_some_and(|k| !["followers", "subs"].contains(&k)) {
        return Err(bad("goal", "Choose followers or subscribers for the goal."));
    }
    let target = goal["target"].as_i64().unwrap_or(0);
    let label = goal["label"].as_str().unwrap_or_default().trim();
    if !(1..=10_000_000).contains(&target)
        || label.chars().count() > 40
        || label.chars().any(char::is_control)
    {
        return Err(bad(
            "goal",
            "Set a goal of 1 or more with a label of up to 40 characters.",
        ));
    }
    out["goal"] = json!({"kind": kind, "target": target, "label": label});
    Ok(out)
}
async fn settings(app: &App, channel: &str) -> Res<Value> {
    let saved: Option<Value> =
        sqlx::query_scalar("SELECT settings FROM overlay_settings WHERE channel_id=$1")
            .bind(channel)
            .fetch_optional(&app.db)
            .await?
            .flatten();
    Ok(saved.unwrap_or_else(defaults))
}
/// The goal widget's current count.
async fn goal_count(app: &App, channel: &str, kind: Option<&str>) -> Res<Option<i64>> {
    let sql = match kind {
        Some("followers") => "SELECT count(*) FROM follows WHERE following_id=$1",
        Some("subs") => {
            "SELECT count(*) FROM channel_subs WHERE channel_id=$1 AND paid_through>now()"
        }
        _ => return Ok(None),
    };
    Ok(Some(
        sqlx::query_scalar(sql)
            .bind(channel)
            .fetch_one(&app.db)
            .await?,
    ))
}

/// An alert to show for a live event, or None when this channel's settings leave it out.
fn alert(settings: &Value, topic_kind: &str, data: &Value, id: i64) -> Option<Value> {
    let (kind, _) = KINDS.iter().find(|(_, t)| *t == topic_kind)?;
    let config = &settings[*kind];
    if config["on"] != true {
        return None;
    }
    if *kind == "tribute"
        && data["valor"].as_i64().unwrap_or(0) < config["min"].as_i64().unwrap_or(10)
    {
        return None;
    }
    let name = data["user"]
        .as_str()
        .or(data["from"].as_str())
        .unwrap_or("Someone");
    let fill = |key: &str| match &data[key] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let text = config["text"]
        .as_str()
        .unwrap_or_default()
        .replace("{name}", name)
        .replace("{tier}", &fill("tier"))
        .replace("{months}", &fill("months"))
        .replace("{valor}", &fill("valor"))
        .replace("{skill}", &fill("skill"))
        .replace("{message}", &fill("message"));
    let message = (*kind == "tribute" && config["show_message"] == true)
        .then(|| data["message"].as_str())
        .flatten();
    Some(
        json!({"type": "alert", "id": id, "kind": kind, "text": text, "message": message,
        "seconds": config["seconds"], "sound": settings["sound"], "volume": settings["volume"]}),
    )
}

/// GET /api/me/overlays: settings, and whether a link exists (the link itself is shown once).
async fn mine(State(app): State<App>, jar: CookieJar) -> Res<Json<Value>> {
    let me = profiles::signed_in(&app, &jar).await?;
    let linked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM overlay_settings WHERE channel_id=$1 AND token_hash IS NOT NULL)")
        .bind(&me.id).fetch_one(&app.db).await?;
    let seen: bool = sqlx::query_scalar("SELECT coalesce((SELECT seen_at>now()-interval '30 seconds' FROM overlay_settings WHERE channel_id=$1),false)")
        .bind(&me.id).fetch_one(&app.db).await?;
    Ok(Json(
        json!({"settings": settings(&app, &me.id).await?, "linked": linked, "connected": seen}),
    ))
}
/// PUT /api/me/overlays: saves settings; open overlays update at once.
async fn save(
    State(app): State<App>,
    jar: CookieJar,
    Json(input): Json<Value>,
) -> Res<Json<Value>> {
    let me = profiles::signed_in(&app, &jar).await?;
    let settings = validate(&input)?;
    sqlx::query("INSERT INTO overlay_settings(channel_id,settings) VALUES($1,$2) ON CONFLICT(channel_id) DO UPDATE SET settings=EXCLUDED.settings")
        .bind(&me.id).bind(&settings).execute(&app.db).await?;
    let count = goal_count(&app, &me.id, settings["goal"]["kind"].as_str()).await?;
    app.chat.publish(
        &format!("alerts:{}", me.id),
        None,
        0,
        json!({"type": "settings", "settings": settings, "goal_count": count}),
    );
    mine(State(app), jar).await
}
/// POST /api/me/overlays/link: a new private link (the old one stops working).
async fn link(State(app): State<App>, jar: CookieJar) -> Res<Json<Value>> {
    let me = profiles::signed_in(&app, &jar).await?;
    profiles::ensure_unrestricted(&mut *app.db.acquire().await?, &me.id).await?;
    let token = sec::token();
    sqlx::query("INSERT INTO overlay_settings(channel_id,token_hash) VALUES($1,$2) ON CONFLICT(channel_id) DO UPDATE SET token_hash=EXCLUDED.token_hash,seen_at=NULL")
        .bind(&me.id).bind(sec::digest(&token)).execute(&app.db).await?;
    Ok(Json(
        json!({"url": format!("{}/overlay/alerts/{token}", app.config.origin)}),
    ))
}
/// DELETE /api/me/overlays/link
async fn revoke(State(app): State<App>, jar: CookieJar) -> Res<Json<Value>> {
    let me = profiles::signed_in(&app, &jar).await?;
    sqlx::query("UPDATE overlay_settings SET token_hash=NULL,seen_at=NULL WHERE channel_id=$1")
        .bind(&me.id)
        .execute(&app.db)
        .await?;
    mine(State(app), jar).await
}
#[derive(Deserialize)]
pub struct Test {
    kind: String,
}
/// POST /api/me/overlays/test: a sample alert on open overlays only; nothing is recorded.
async fn test(State(app): State<App>, jar: CookieJar, Json(input): Json<Test>) -> Res<Json<Value>> {
    let me = profiles::signed_in(&app, &jar).await?;
    profiles::rate(&app, format!("overlay-test:{}", me.id), 30, 60).await?;
    let topic = KINDS
        .iter()
        .find(|(k, _)| *k == input.kind)
        .map(|(_, t)| *t)
        .ok_or_else(|| Fail::field("kind", "Unknown alert."))?;
    let data = json!({"user": "TestViewer", "from": "TestViewer", "tier": 1, "months": 3, "valor": 500, "skill": "Confetti", "message": "This is a test tribute."});
    let mut settings = settings(&app, &me.id).await?;
    // A test always plays, even for a type that's off.
    settings[input.kind.as_str()]["on"] = json!(true);
    settings["tribute"]["min"] = json!(10);
    if let Some(alert) = alert(&settings, topic, &data, 0) {
        app.chat
            .publish(&format!("alerts:{}", me.id), None, 0, alert);
    }
    Ok(Json(json!({"sent": true})))
}

#[derive(Deserialize)]
pub struct Connect {
    token: String,
}
/// GET /api/overlays/ws?token=: the browser source's socket.
async fn socket(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<Connect>,
    upgrade: WebSocketUpgrade,
) -> Res<Response> {
    if headers.get("origin").and_then(|v| v.to_str().ok()) != Some(app.config.origin.as_str()) {
        return Err(Fail::denied("Invalid request origin."));
    }
    let ip = sec::client_ip(&app, peer, &headers);
    sec::reserve(&app, vec![format!("alerts-connect:{ip}")], 30, 60).await?;
    let digest = sec::digest(&query.token);
    let (channel, username): (String, String) = sqlx::query_as("SELECT o.channel_id,lower(u.username) FROM overlay_settings o JOIN users u ON u.id=o.channel_id WHERE o.token_hash=$1")
        .bind(&digest).fetch_optional(&app.db).await?.ok_or_else(Fail::missing)?;
    Ok(upgrade
        .max_message_size(1024)
        .on_upgrade(move |ws| session(app, channel, username, digest, ws)))
}
/// The opening state: settings, the goal's count and the last 5 alerts.
async fn opening(app: &App, channel: &str, topics: &[String]) -> Res<Value> {
    let settings = settings(app, channel).await?;
    let count = goal_count(app, channel, settings["goal"]["kind"].as_str()).await?;
    let rows: Vec<(i64, String, Value)> = sqlx::query_as(
        "SELECT id,topic,data FROM events WHERE topic=ANY($1) ORDER BY id DESC LIMIT 20",
    )
    .bind(topics)
    .fetch_all(&app.db)
    .await?;
    let recent: Vec<Value> = rows
        .iter()
        .filter_map(|(id, topic, data)| alert(&settings, kind_of(topic), data, *id))
        .take(5)
        .collect();
    Ok(json!({"type": "hello", "settings": settings, "goal_count": count, "recent": recent}))
}
/// The event kind of a `channel:{name}:{kind}` topic (kinds may contain a colon).
fn kind_of(topic: &str) -> &str {
    topic.splitn(3, ':').nth(2).unwrap_or_default()
}
async fn session(app: App, channel: String, username: String, digest: String, mut ws: WebSocket) {
    let topics: Vec<String> = KINDS
        .iter()
        .map(|(_, k)| format!("channel:{username}:{k}"))
        .collect();
    let mut events = app.chat.subscribe();
    let room = format!("alerts:{channel}");
    let Ok(hello) = opening(&app, &channel, &topics).await else {
        return;
    };
    let mut current = hello["settings"].clone();
    if ws
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    let mut beat = tokio::time::interval(Duration::from_secs(10));
    beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let send = tokio::select! {
            _ = beat.tick() => {
                // Check in for Studio, and stop at once when the link was replaced or revoked.
                let still = sqlx::query("UPDATE overlay_settings SET seen_at=now() WHERE channel_id=$1 AND token_hash=$2")
                    .bind(&channel).bind(&digest).execute(&app.db).await;
                if !matches!(still, Ok(r) if r.rows_affected() == 1) { return; }
                None
            }
            event = events.recv() => match event {
                Ok(e) if e.channel == room => {
                    if e.payload["type"] == "settings" {
                        current = e.payload["settings"].clone();
                    }
                    Some(e.payload.clone())
                }
                Ok(e) if e.channel == crate::events::HUB && e.payload["topic"].as_str().is_some_and(|t| topics.iter().any(|x| x == t)) => {
                    let kind = kind_of(e.payload["topic"].as_str().unwrap_or_default()).to_string();
                    let out = alert(&current, &kind, &e.payload["data"], e.payload["id"].as_i64().unwrap_or(0));
                    // Follows and subscriptions move the goal bar.
                    let goal = current["goal"]["kind"].as_str();
                    if ((kind == "follows:detail" && goal == Some("followers")) || (kind == "subs" && goal == Some("subs")))
                        && let Ok(Some(n)) = goal_count(&app, &channel, goal).await {
                        let update = json!({"type": "goal", "goal_count": n}).to_string();
                        if ws.send(Message::Text(update.into())).await.is_err() { return; }
                    }
                    out
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
            incoming = ws.recv() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => None,
            },
        };
        if let Some(payload) = send
            && ws
                .send(Message::Text(payload.to_string().into()))
                .await
                .is_err()
        {
            return;
        }
    }
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/api/me/overlays", get(mine).put(save))
        .route("/api/me/overlays/link", post(link).delete(revoke))
        .route("/api/me/overlays/test", post(test))
        .route("/api/overlays/ws", get(socket))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fills_templates_and_applies_the_settings() {
        let s = defaults();
        let sub = alert(
            &s,
            "subs",
            &json!({"user": "Ann", "tier": 2, "months": 5}),
            1,
        )
        .unwrap();
        assert_eq!(sub["text"], "Ann subscribed at Tier 2 (5 months)!");
        assert!(
            alert(&s, "tributes", &json!({"user": "Bo", "valor": 9}), 2).is_none(),
            "under the minimum"
        );
        let tribute = alert(
            &s,
            "tributes",
            &json!({"user": "Bo", "valor": 50, "message": "gg"}),
            3,
        )
        .unwrap();
        assert_eq!(
            (tribute["text"].as_str(), tribute["message"].as_str()),
            (Some("Bo paid 50 Valor"), Some("gg"))
        );
        let raid = alert(
            &s,
            "raid:incoming",
            &json!({"from": "Cy", "seconds": 10}),
            4,
        )
        .unwrap();
        assert_eq!(raid["text"], "Cy is raiding!");
        let mut off = s.clone();
        off["follow"]["on"] = json!(false);
        assert!(alert(&off, "follows:detail", &json!({"user": "Di"}), 5).is_none());
        assert!(validate(&s).is_ok());
        let mut long = s.clone();
        long["follow"]["text"] = json!("x".repeat(121));
        assert!(validate(&long).is_err());
        assert_eq!(kind_of("channel:streamer:raid:incoming"), "raid:incoming");
    }
}
