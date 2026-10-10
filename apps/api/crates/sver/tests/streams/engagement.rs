//! Module 6 Support, part 3: Engagement Valor earned by following (once), chatting (cooldown) and
//! real playback (verified viewers on counted leases), spent on rewards (cooldown, per-stream
//! limit, required text, idempotent retries) and the highlight reward; the owner's queue marks
//! redemptions done or refunds them; a channel ban freezes earning and spending.
use super::Env;
use super::chat::{call, id, person};
use axum::http::StatusCode;
use serde_json::{Value, json};

const CHANNEL: &str = "/api/channels/evowner";

async fn balance(e: &Env, user: &str) -> i64 {
    sqlx::query_scalar("SELECT coalesce((SELECT balance FROM engagement WHERE channel_id='ev-owner' AND user_id=$1),0)")
        .bind(user).fetch_one(&e.app.db).await.unwrap()
}
async fn say(e: &Env, token: &str, body: Value) -> (StatusCode, Value) {
    call(e, "POST", &format!("{CHANNEL}/chat"), Some(token), body).await
}
fn reward<'a>(rewards: &'a Value, name: &str) -> &'a str {
    rewards
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap()["id"]
        .as_str()
        .unwrap()
}
async fn follow(e: &Env, token: &str, method: &str) -> (StatusCode, Value) {
    call(e, method, "/api/follows/evowner", Some(token), Value::Null).await
}
async fn resolve(e: &Env, owner: &str, id: String, action: &str) -> (StatusCode, Value) {
    call(
        e,
        "POST",
        &format!("{CHANNEL}/redemptions/{id}"),
        Some(owner),
        json!({"action": action}),
    )
    .await
}
async fn redeem(e: &Env, token: &str, reward: &str, body: Value) -> (StatusCode, Value) {
    call(
        e,
        "POST",
        &format!("{CHANNEL}/rewards/{reward}/redeem"),
        Some(token),
        body,
    )
    .await
}

pub async fn exercise(e: &Env) {
    let owner = person(e, "ev-owner", "EvOwner", true).await;
    let viewer = person(e, "ev-viewer", "EvViewer", true).await;
    let lurker = person(e, "ev-lurker", "EvLurker", false).await;

    // The owner's rewards; the built-in highlight is always there.
    let create = |body: Value| call(e, "POST", "/api/me/rewards", Some(&owner), body);
    assert_eq!(
        create(json!({"name": " ", "cost": 10, "enabled": true}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        create(json!({"name": "Free", "cost": 0, "enabled": true}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (status, _) =
        create(json!({"name": "Hydrate", "cost": 100, "cooldown_seconds": 60, "enabled": true}))
            .await;
    assert_eq!(status, StatusCode::OK);
    let (status, made) = create(
        json!({"name": "Song request", "cost": 50, "per_stream_limit": 1,
        "prompt": "Which song?", "enabled": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{made}");
    let rewards = made["rewards"].clone();
    assert_eq!(rewards.as_array().unwrap().len(), 3);
    assert_eq!(rewards[0]["kind"], "highlight");
    let (hydrate, song) = (
        reward(&rewards, "Hydrate"),
        reward(&rewards, "Song request"),
    );

    // Following pays once, never again on refollow; unverified viewers don't earn.
    assert_eq!(follow(e, &viewer, "PUT").await.0, StatusCode::OK);
    assert_eq!(balance(e, "ev-viewer").await, 300);
    follow(e, &viewer, "DELETE").await;
    follow(e, &viewer, "PUT").await;
    assert_eq!(balance(e, "ev-viewer").await, 300, "no refollow farming");
    follow(e, &lurker, "PUT").await;
    assert_eq!(balance(e, "ev-lurker").await, 0);
    // Chatting pays once per cooldown.
    assert_eq!(
        say(e, &viewer, json!({"id": id(), "body": "hello"}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(balance(e, "ev-viewer").await, 305);
    say(e, &viewer, json!({"id": id(), "body": "again"})).await;
    assert_eq!(balance(e, "ev-viewer").await, 305);
    // Real playback pays per interval: verified viewers on counted leases of a live stream.
    e.sql("INSERT INTO broadcasts(id,owner_id,public_id,generation,state,server_id,service_id,client_id,started_at,publisher_started_at,startup_deadline,alert_state) VALUES('ev-b','ev-owner','ev-b',1,'LIVE','s','v','c',now()-interval '10 minutes',now(),now(),'skipped')").await;
    e.sql("INSERT INTO playback_leases(broadcast_id,viewer_key,expires_at,level) VALUES('ev-b','u:ev-viewer',now()+interval '1 minute','trusted'),('ev-b','u:ev-lurker',now()+interval '1 minute','trusted')").await;
    sver::engagement::award_watch(&e.app).await.unwrap();
    assert_eq!(balance(e, "ev-viewer").await, 315);
    sver::engagement::award_watch(&e.app).await.unwrap();
    assert_eq!(balance(e, "ev-viewer").await, 315, "once per interval");
    assert_eq!(balance(e, "ev-lurker").await, 0);
    // Account XP (docs/PROGRESSION.md): chat at most once a minute, watching once per interval,
    // and a Scout bonus for 10+ minutes that began early in a new channel's broadcast, once.
    let progression = || async {
        call(e, "GET", "/api/me/progression", Some(&viewer), Value::Null)
            .await
            .1
    };
    assert_eq!(
        progression().await["xp"],
        12,
        "2 for chat (once) + 10 for a minute watched"
    );
    e.sql("UPDATE broadcasts SET started_at=now()-interval '12 minutes' WHERE id='ev-b'")
        .await;
    e.sql("UPDATE playback_leases SET created_at=now()-interval '11 minutes' WHERE broadcast_id='ev-b'").await;
    sver::engagement::award_watch(&e.app).await.unwrap();
    sver::engagement::award_watch(&e.app).await.unwrap();
    let mine = progression().await;
    assert_eq!(
        (&mine["xp"], &mine["level"], &mine["next_xp"]),
        (&json!(62), &json!(1), &json!(100)),
        "{mine}"
    );
    let (_, studio) = call(e, "GET", "/api/me/stream", Some(&owner), Value::Null).await;
    assert_eq!(studio["broadcast"]["scouts"], 1, "{studio}");
    // Daily orders: three on first view, completed from event records by the minute tick.
    let orders = || async {
        call(e, "GET", "/api/me/orders", Some(&viewer), Value::Null)
            .await
            .1
    };
    assert_eq!(orders().await["orders"].as_array().unwrap().len(), 3);
    e.sql("UPDATE daily_orders SET kind=CASE slot WHEN 0 THEN 'watch' ELSE 'poll' END,rarity=0,target=CASE slot WHEN 0 THEN 1 ELSE 5 END WHERE user_id='ev-viewer'").await;
    let before = balance(e, "ev-viewer").await;
    sver::engagement::award_watch(&e.app).await.unwrap();
    let today = orders().await;
    // A Common order also pays 10 Engagement Valor in the channel the viewer was watching.
    assert_eq!(today["orders"][0]["ev"], 10, "{today}");
    assert!(today["orders"][0]["ev_channel"].is_string(), "{today}");
    assert_eq!(balance(e, "ev-viewer").await, before + 10);
    // Take it back out so the reward arithmetic below stays as it was.
    e.sql("UPDATE engagement SET balance=balance-10,earned=earned-10 WHERE channel_id='ev-owner' AND user_id='ev-viewer'").await;
    assert_eq!(
        (
            &today["orders"][0]["done"],
            &today["orders"][0]["xp"],
            &today["orders"][1]["done"],
            &today["streak"],
            &today["week"]
        ),
        (
            &json!(true),
            &json!(40),
            &json!(false),
            &json!(1),
            &json!(1)
        ),
        "{today}"
    );
    let mine = progression().await;
    assert_eq!(mine["level"], 2, "62 + 40 XP");
    assert_eq!(mine["frame"], 0);
    assert!(mine["title"].is_string(), "{mine}");
    let token = viewer.as_str();
    let reroll = |slot: i32| async move {
        call(
            e,
            "POST",
            &format!("/api/me/orders/{slot}/reroll"),
            Some(token),
            Value::Null,
        )
        .await
        .0
    };
    assert_eq!(reroll(0).await, StatusCode::BAD_REQUEST, "done orders stay");
    assert_eq!(reroll(1).await, StatusCode::OK);
    assert_eq!(reroll(2).await, StatusCode::BAD_REQUEST, "one reroll a day");

    // The viewer's view of the channel's rewards and balance.
    let (_, listed) = call(
        e,
        "GET",
        &format!("{CHANNEL}/rewards"),
        Some(&viewer),
        Value::Null,
    )
    .await;
    assert_eq!(listed["balance"], 315);
    assert_eq!(listed["rewards"].as_array().unwrap().len(), 3);

    // Redeeming: idempotent by request ID, then the cooldown applies.
    let first = id();
    assert_eq!(
        redeem(e, &viewer, hydrate, json!({"id": first})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        redeem(e, &viewer, hydrate, json!({"id": first})).await.0,
        StatusCode::OK
    );
    assert_eq!(balance(e, "ev-viewer").await, 215);
    assert_eq!(
        redeem(e, &viewer, hydrate, json!({"id": id()})).await.0,
        StatusCode::CONFLICT
    );
    // A required text and a per-stream limit.
    let (status, needs) = redeem(e, &viewer, song, json!({"id": id()})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(needs["field"], "input");
    assert_eq!(
        redeem(
            e,
            &viewer,
            song,
            json!({"id": id(), "input": "Dead of Night"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        redeem(e, &viewer, song, json!({"id": id(), "input": "Another"}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(balance(e, "ev-viewer").await, 165);
    assert_eq!(
        redeem(e, &owner, hydrate, json!({"id": id()})).await.0,
        StatusCode::BAD_REQUEST,
        "not your own rewards"
    );
    // The highlight reward pays for a highlighted chat message, or nothing posts.
    e.sql("DELETE FROM rate_limits WHERE key LIKE 'chat-%:ev-viewer'")
        .await;
    let (status, poor) = say(
        e,
        &viewer,
        json!({"id": id(), "body": "Look at me", "highlight": true}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{poor}");
    e.sql("UPDATE engagement SET balance=1000 WHERE channel_id='ev-owner' AND user_id='ev-viewer'")
        .await;
    let (status, shown) = say(
        e,
        &viewer,
        json!({"id": id(), "body": "Look at me", "highlight": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown["message"]["highlighted"], true);
    assert_eq!(balance(e, "ev-viewer").await, 500);

    // The queue: owner and moderators only; done or refunded once.
    assert_eq!(
        call(
            e,
            "GET",
            &format!("{CHANNEL}/redemptions"),
            Some(&viewer),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, queue) = call(
        e,
        "GET",
        &format!("{CHANNEL}/redemptions"),
        Some(&owner),
        Value::Null,
    )
    .await;
    let items = queue["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    let pending: Vec<&Value> = items.iter().filter(|i| i["status"] == "pending").collect();
    assert_eq!(pending.len(), 2);
    let find = |name: &str| {
        pending.iter().find(|i| i["name"] == name).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(
        resolve(e, &owner, find("Hydrate"), "refund").await.0,
        StatusCode::OK
    );
    assert_eq!(balance(e, "ev-viewer").await, 600);
    assert_eq!(
        resolve(e, &owner, find("Song request"), "done").await.0,
        StatusCode::OK
    );
    assert_eq!(
        resolve(e, &owner, find("Song request"), "refund").await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(balance(e, "ev-viewer").await, 600);

    // A channel ban freezes earning and spending there.
    e.sql("INSERT INTO channel_restrictions(channel_id,user_id,kind) VALUES('ev-owner','ev-viewer','ban')").await;
    assert_eq!(
        redeem(e, &viewer, hydrate, json!({"id": id()})).await.0,
        StatusCode::FORBIDDEN
    );
    e.sql("UPDATE engagement SET last_watch_at=now()-interval '1 hour' WHERE user_id='ev-viewer'")
        .await;
    sver::engagement::award_watch(&e.app).await.unwrap();
    assert_eq!(balance(e, "ev-viewer").await, 600);
    e.sql("DELETE FROM channel_restrictions WHERE channel_id='ev-owner'")
        .await;
    e.sql("UPDATE broadcasts SET state='ENDED',ended_at=now(),end_reason='test',reconnect_deadline=NULL WHERE id='ev-b'").await;
}
