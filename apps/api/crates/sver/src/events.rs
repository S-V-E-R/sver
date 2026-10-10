//! Live events (docs/DEVELOPER_PLATFORM.md §2, Mixer's "Constellation"): one WebSocket,
//! `/api/events`, where apps subscribe to topics. Events are written to the `events` outbox in the
//! same transaction as the change, drained every second onto the in-process hub, and replayed
//! after a client's last event ID for 5 minutes. Events never carry email, IP addresses or
//! internal IDs; follower names and subscriptions are private topics. Webhooks (`/api/hooks`)
//! receive the same events, queued in the statement that writes the event and signed like board
//! webhooks.
use crate::{
    App, boards,
    profiles::{self, Fail, Res},
    security as sec,
};
use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{delete, get},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicI64, Ordering},
    },
};

/// Public channel topics (client ID only) and private ones (owner or moderator, `events:private`).
const PUBLIC: [&str; 9] = [
    "live",
    "update",
    "follows",
    "raids",
    "costream",
    "board",
    "poll",
    "prediction",
    "surge",
];
const PRIVATE: [&str; 6] = [
    "follows:detail",
    "subs",
    "tributes",
    "skills",
    "moderation",
    "raid:incoming",
];
const MAX_TOPICS: usize = 200;
const MAX_CONNECTIONS: usize = 10;
/// The hub channel the drain publishes on.
pub(crate) const HUB: &str = "events";
type Row = (i64, String, Value, chrono::DateTime<chrono::Utc>);
type Person = (String, Vec<String>);

fn event((id, topic, data, at): Row) -> Value {
    json!({"type": "event", "id": id, "topic": topic, "data": data, "at": at})
}
/// Writes a channel event (`channel:{username}:{kind}`) in the caller's transaction, and queues it
/// for every enabled webhook on that topic in the same statement.
pub async fn emit(
    db: &mut PgConnection,
    channel: &str,
    kind: &str,
    data: Value,
) -> Result<(), sqlx::Error> {
    let topic: Option<String> =
        sqlx::query_scalar("SELECT 'channel:'||lower(username)||':'||$2 FROM users WHERE id=$1")
            .bind(channel)
            .bind(kind)
            .fetch_optional(&mut *db)
            .await?;
    // A channel's private events never show someone its owner has blocked.
    if matches!(kind, "follows:detail" | "subs" | "tributes" | "skills")
        && let Some(name) = data["user"].as_str()
        && sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM user_blocks k JOIN users u ON u.id=k.blocked_id WHERE k.blocker_id=$1 AND lower(u.username)=lower($2))")
            .bind(channel)
            .bind(name)
            .fetch_one(&mut *db)
            .await?
    {
        return Ok(());
    }
    match topic {
        Some(topic) => emit_topic(db, &topic, data).await,
        None => Ok(()),
    }
}
/// Writes an event on any topic (`faction:{slug}:war`) and queues its webhooks.
pub async fn emit_topic(
    db: &mut PgConnection,
    topic: &str,
    data: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("WITH e AS (INSERT INTO events(topic,data) VALUES($1,$2) RETURNING id,topic,data,at)
        INSERT INTO hook_deliveries(hook_id,payload) SELECT h.id,jsonb_build_object('type','event','id',e.id,'topic',e.topic,'data',e.data,'at',e.at)
        FROM e JOIN event_hooks h ON h.topics @> ARRAY[e.topic] AND h.disabled_at IS NULL")
        .bind(topic)
        .bind(data)
        .execute(db)
        .await?;
    Ok(())
}

/// Emits right after a change commits, beside the page's own live update (board presses, poll
/// tallies, Surge). Like that update, a crash between the commit and this can lose one display
/// event, never a record.
pub async fn emit_after(app: &App, channel: &str, kind: &str, data: Value) -> Res<()> {
    emit(&mut *app.db.acquire().await?, channel, kind, data).await?;
    Ok(())
}

static CURSOR: AtomicI64 = AtomicI64::new(-1);
/// Every second: new outbox rows go out on the hub (one API instance, as for chat).
pub async fn drain(app: &App) -> Res<()> {
    if CURSOR.load(Ordering::Relaxed) < 0 {
        let start: i64 = sqlx::query_scalar("SELECT coalesce(max(id),0) FROM events")
            .fetch_one(&app.db)
            .await?;
        CURSOR.store(start, Ordering::Relaxed);
    }
    let rows: Vec<Row> =
        sqlx::query_as("SELECT id,topic,data,at FROM events WHERE id>$1 ORDER BY id LIMIT 1000")
            .bind(CURSOR.load(Ordering::Relaxed))
            .fetch_all(&app.db)
            .await?;
    for row in rows {
        let id = row.0;
        app.chat.publish(HUB, None, id, event(row));
        CURSOR.store(id, Ordering::Relaxed);
    }
    Ok(())
}
/// Retention: events older than 10 minutes go.
pub async fn prune(app: &App) -> Res<()> {
    sqlx::query("DELETE FROM events WHERE at<now()-interval '10 minutes'")
        .execute(&app.db)
        .await?;
    Ok(())
}

/// A person's own topic (`user:{name}:notifications` with `user:read`, `user:{name}:whispers` with
/// `whispers:read`): only with their own token. Returns (person ID, kind).
async fn own_topic(app: &App, topic: &str, user: Option<&Person>) -> Res<Option<(String, String)>> {
    let Some((name, kind)) = topic
        .strip_prefix("user:")
        .and_then(|rest| rest.split_once(':'))
    else {
        return Ok(None);
    };
    let scope = match kind {
        "notifications" => "user:read",
        "whispers" => "whispers:read",
        _ => return Ok(None),
    };
    let Some((id, _)) = user.filter(|(_, s)| s.iter().any(|g| g == scope)) else {
        return Ok(None);
    };
    let mine: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1 AND lower(username)=$2)")
            .bind(id)
            .bind(name)
            .fetch_one(&app.db)
            .await?;
    Ok(mine.then(|| (id.clone(), kind.to_string())))
}
type Notice = (Value, chrono::DateTime<chrono::Utc>);
/// New in-site notifications for a person since a time, as the notifications list shows them.
async fn notifications_since(
    app: &App,
    user: &str,
    since: chrono::DateTime<chrono::Utc>,
) -> Res<Vec<Notice>> {
    Ok(sqlx::query_as("SELECT jsonb_build_object('kind',n.kind,'channel',c.username,'payload',n.payload,'created_at',n.created_at),n.created_at
        FROM notifications n JOIN channel_users c ON c.id=n.channel_id AND c.eligible
        WHERE n.user_id=$1 AND n.site_visible AND n.created_at>$2
        AND NOT EXISTS(SELECT 1 FROM user_blocks k WHERE (k.blocker_id=$1 AND k.blocked_id=n.channel_id) OR (k.blocker_id=n.channel_id AND k.blocked_id=$1))
        ORDER BY n.created_at LIMIT 50")
        .bind(user).bind(since).fetch_all(&app.db).await?)
}
/// The channel behind a `chat:{name}` topic this connection may read: an eligible channel, and
/// not a mature one for an under-18 person.
async fn chat_channel(app: &App, topic: &str, user: Option<&Person>) -> Res<Option<String>> {
    let Some(name) = topic.strip_prefix("chat:") else {
        return Ok(None);
    };
    let mut db = app.db.acquire().await?;
    let Some(channel) = profiles::eligible_by_name(&mut db, name).await? else {
        return Ok(None);
    };
    if crate::streams::mature_blocked(&mut db, &channel.id, user.map(|u| u.0.as_str())).await? {
        return Ok(None);
    }
    Ok(Some(channel.id))
}
/// Whether a topic exists and this connection may subscribe to it.
async fn allowed(app: &App, topic: &str, user: Option<&Person>) -> Res<bool> {
    if let Some(faction) = topic
        .strip_prefix("faction:")
        .and_then(|t| t.strip_suffix(":war"))
    {
        return Ok(crate::factions::FACTIONS.contains(&faction));
    }
    let Some((name, kind)) = topic
        .strip_prefix("channel:")
        .and_then(|rest| rest.split_once(':'))
    else {
        return Ok(false);
    };
    if PUBLIC.contains(&kind) {
        return Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM channel_users WHERE lower(username)=$1 AND eligible)",
        )
        .bind(name)
        .fetch_one(&app.db)
        .await?);
    }
    let Some((user, _)) = user.filter(|(_, s)| s.iter().any(|g| g == "events:private")) else {
        return Ok(false);
    };
    if !PRIVATE.contains(&kind) {
        return Ok(false);
    }
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM channel_users c WHERE lower(c.username)=$1 AND c.eligible AND (c.id=$2 OR EXISTS(SELECT 1 FROM channel_moderators m WHERE m.channel_id=c.id AND m.user_id=$2)))")
        .bind(name).bind(user).fetch_one(&app.db).await?)
}

static CONNECTIONS: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Mutex::default);
struct Slot(String);
impl Drop for Slot {
    fn drop(&mut self) {
        let mut open = CONNECTIONS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = open.get_mut(&self.0) {
            *n = n.saturating_sub(1);
        }
    }
}

#[derive(Deserialize)]
pub struct Connect {
    client_id: String,
    #[serde(default)]
    access_token: Option<String>,
}
/// GET /api/events?client_id=…[&access_token=…]: query parameters, because browsers can't set
/// headers on a WebSocket.
async fn socket(
    State(app): State<App>,
    Query(input): Query<Connect>,
    upgrade: WebSocketUpgrade,
) -> Res<Response> {
    let user =
        crate::devapps::identify(&app, &input.client_id, input.access_token.as_deref()).await?;
    let key = format!(
        "{}:{}",
        input.client_id,
        user.as_ref().map_or("-", |u| u.0.as_str())
    );
    {
        let mut open = CONNECTIONS.lock().unwrap_or_else(|e| e.into_inner());
        let n = open.entry(key.clone()).or_default();
        if *n >= MAX_CONNECTIONS {
            return Err(Fail::new(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "Up to 10 connections per app and person.",
            ));
        }
        *n += 1;
    }
    let slot = Slot(key);
    Ok(upgrade
        .max_message_size(16 * 1024)
        .on_upgrade(move |ws| session(app, user, slot, ws)))
}
async fn reply(ws: &mut WebSocket, value: Value) -> bool {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .is_ok()
}
async fn session(app: App, user: Option<Person>, _slot: Slot, mut ws: WebSocket) {
    let mut events = app.chat.subscribe();
    let mut topics: HashSet<String> = HashSet::new();
    // Chat topics: channel ID -> topic. Messages come straight from the chat hub, not the outbox.
    let mut chats: HashMap<String, String> = HashMap::new();
    // The person's own topics: hub channel `dm:{id}` -> whispers topic; notifications are polled.
    let mut own = Own::default();
    // ponytail: one indexed query per subscribed socket every 5 s; publish notifications on the hub
    // if many sockets subscribe.
    let mut poll = tokio::time::interval(std::time::Duration::from_secs(5));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = poll.tick() => {
                let Some((user, topic, since)) = own.notifications.clone() else { continue; };
                let Ok(rows) = notifications_since(&app, &user, since).await else { return; };
                for (data, at) in rows {
                    own.notifications = Some((user.clone(), topic.clone(), at));
                    if !reply(&mut ws, json!({"type": "event", "topic": topic, "data": data, "at": at})).await { return; }
                }
            }
            incoming = events.recv() => match incoming {
                Ok(e) if e.channel == HUB => {
                    if e.payload["topic"].as_str().is_some_and(|t| topics.contains(t)) && !reply(&mut ws, e.payload.clone()).await {
                        return;
                    }
                }
                Ok(e) if own.whispers.as_ref().is_some_and(|(hub, _)| *hub == e.channel) => {
                    let topic = own.whispers.as_ref().map(|w| w.1.clone()).unwrap_or_default();
                    if !reply(&mut ws, json!({"type": "event", "topic": topic, "data": e.payload})).await {
                        return;
                    }
                }
                Ok(e) if chats.contains_key(&e.channel) => {
                    let Ok(hidden) = crate::chat::hidden(&app, user.as_ref().map(|u| u.0.as_str())).await else { return; };
                    if e.author.as_ref().is_some_and(|a| hidden.contains(a)) { continue; }
                    let mut data = e.payload.clone();
                    match data["type"].as_str() {
                        Some("message") => {
                            let Ok(message) = crate::chat::fresh(&app, &data, &hidden).await else { return; };
                            let Some(message) = message else { continue; };
                            data["message"] = message;
                        }
                        Some("delete") => {}
                        _ => continue,
                    }
                    if !reply(&mut ws, json!({"type": "event", "topic": chats[&e.channel], "data": data})).await {
                        return;
                    }
                }
                Ok(_) => {}
                // Missed events: the client resubscribes with `since` to replay them.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if !reply(&mut ws, json!({"type": "resync"})).await { return; }
                }
                Err(_) => return,
            },
            incoming = ws.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let Ok(call) = serde_json::from_str::<Value>(&text) else {
                        if !reply(&mut ws, json!({"type": "reply", "error": "Send JSON."})).await { return; }
                        continue;
                    };
                    let mut out = json!({"type": "reply", "id": call["id"]});
                    match handle(&app, user.as_ref(), &mut topics, &mut chats, &mut own, &call).await {
                        Ok((result, replay)) => {
                            out["result"] = result;
                            if !reply(&mut ws, out).await { return; }
                            for event in replay {
                                if !reply(&mut ws, event).await { return; }
                            }
                        }
                        Err(why) => {
                            out["error"] = json!(why);
                            if !reply(&mut ws, out).await { return; }
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
        }
    }
}
/// A connection's own-topic subscriptions.
#[derive(Default)]
struct Own {
    /// (hub channel, topic)
    whispers: Option<(String, String)>,
    /// (person, topic, newest notification sent)
    notifications: Option<(String, String, chrono::DateTime<chrono::Utc>)>,
}
impl Own {
    fn forget(&mut self, topic: &str) {
        if self.whispers.as_ref().is_some_and(|w| w.1 == topic) {
            self.whispers = None;
        }
        if self.notifications.as_ref().is_some_and(|n| n.1 == topic) {
            self.notifications = None;
        }
    }
}
/// One method call: subscribe (with an optional `since` to replay), unsubscribe, ping, and for
/// chat `history` (the last 50 messages) and `send` (`chat:write`).
async fn handle(
    app: &App,
    user: Option<&Person>,
    topics: &mut HashSet<String>,
    chats: &mut HashMap<String, String>,
    own: &mut Own,
    call: &Value,
) -> Result<(Value, Vec<Value>), String> {
    let retry = |_| "Try again.".to_string();
    if call["type"] != "method" {
        return Err("Send {\"type\":\"method\",…}.".into());
    }
    let asked: Vec<String> = call["params"]["topics"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|x| x.as_str())
                .map(str::to_lowercase)
                .collect()
        })
        .unwrap_or_default();
    match call["method"].as_str() {
        Some("ping") => Ok((json!("pong"), Vec::new())),
        Some("history") => {
            let topic = call["params"]["topic"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase();
            let channel = chat_channel(app, &topic, user)
                .await
                .map_err(retry)?
                .ok_or("Use a chat:{name} topic you can read.")?;
            let hidden = crate::chat::hidden(app, user.map(|u| u.0.as_str()))
                .await
                .map_err(retry)?;
            let mut messages = crate::chat::history(app, &channel, &hidden, None)
                .await
                .map_err(retry)?;
            let extra = messages.len().saturating_sub(50);
            messages.drain(..extra);
            Ok((json!({"topic": topic, "messages": messages}), Vec::new()))
        }
        Some("send") => {
            let (person, scopes) = user.ok_or("Sending chat needs an access token.")?;
            if !scopes.iter().any(|s| s == "chat:write") {
                return Err("This token doesn't have chat:write.".into());
            }
            let name = call["params"]["channel"].as_str().unwrap_or_default();
            let channel = chat_channel(app, &format!("chat:{}", name.to_lowercase()), user)
                .await
                .map_err(retry)?
                .ok_or("That channel's chat isn't available.")?;
            crate::chat::send_as_app(app, person, &channel, &call["params"])
                .await
                .map(|message| (json!({"message": message}), Vec::new()))
                .map_err(|f| f.message.to_string())
        }
        Some("unsubscribe") => {
            for topic in &asked {
                topics.remove(topic);
                chats.retain(|_, t| t != topic);
                own.forget(topic);
            }
            Ok((
                json!({"topics": topics.iter().collect::<Vec<_>>()}),
                Vec::new(),
            ))
        }
        Some("subscribe") => {
            let (mut added, mut refused) = (Vec::new(), Vec::new());
            for topic in asked {
                if topics.len() + chats.len() + 2 >= MAX_TOPICS {
                    refused.push(topic);
                    continue;
                }
                if topic.starts_with("user:") {
                    match own_topic(app, &topic, user).await.map_err(retry)? {
                        Some((id, kind)) if kind == "whispers" => {
                            own.whispers = Some((format!("dm:{id}"), topic.clone()));
                            added.push(topic);
                        }
                        Some((id, _)) => {
                            own.notifications = Some((id, topic.clone(), chrono::Utc::now()));
                            added.push(topic);
                        }
                        None => refused.push(topic),
                    }
                    continue;
                }
                if topic.starts_with("chat:") {
                    match chat_channel(app, &topic, user).await.map_err(retry)? {
                        Some(channel) => {
                            chats.insert(channel, topic.clone());
                            added.push(topic);
                        }
                        None => refused.push(topic),
                    }
                    continue;
                }
                if allowed(app, &topic, user).await.map_err(retry)? {
                    topics.insert(topic.clone());
                    added.push(topic);
                } else {
                    refused.push(topic);
                }
            }
            let mut replay = Vec::new();
            if let Some(since) = call["params"]["since"].as_i64() {
                let rows: Vec<Row> = sqlx::query_as("SELECT id,topic,data,at FROM events WHERE id>$1 AND topic=ANY($2) AND at>now()-interval '5 minutes' ORDER BY id LIMIT 1000")
                    .bind(since).bind(&added).fetch_all(&app.db).await.map_err(|_| "Try again.".to_string())?;
                replay = rows.into_iter().map(event).collect();
            }
            Ok((json!({"subscribed": added, "refused": refused}), replay))
        }
        _ => Err("Use subscribe, unsubscribe, ping, history or send.".into()),
    }
}

// ---- Webhooks for the same events ----

const MAX_HOOKS: i64 = 20;
const HOOK_TOPICS: usize = 50;
const HOOK_ATTEMPTS: i32 = 8;
/// Failed deliveries in a row before a hook turns itself off.
const HOOK_FAILURES: i32 = 50;

/// Who manages hooks: an app with the person's bearer token (and `SVER-Client-Id`), or the
/// signed-in person with a verified email and two-factor sign-in. Returns (person, app, scopes).
/// A request with a bearer token never falls back to the cookie (lib.rs skips the origin check
/// for it).
async fn hook_owner(
    app: &App,
    headers: &HeaderMap,
    jar: &CookieJar,
) -> Res<(String, Option<String>, Vec<String>)> {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let Some(token) = bearer else {
        let user = crate::devapps::developer(app, jar).await?;
        return Ok((user.id, None, vec!["events:private".into()]));
    };
    let client = headers
        .get("sver-client-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            Fail::new(
                StatusCode::UNAUTHORIZED,
                "Send your app's SVER-Client-Id header.",
            )
        })?;
    let (user, scopes) = crate::devapps::identify(app, client, Some(token))
        .await?
        .ok_or_else(Fail::missing)?;
    profiles::rate(app, format!("api:{client}:{user}"), 600, 60).await?;
    Ok((user, Some(client.to_string()), scopes))
}
async fn hooks_of(app: &App, user: &str, client: Option<&str>) -> Res<Json<Value>> {
    let hooks: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',h.id,'app',a.name,'topics',h.topics,'url',h.url,'failures',h.failures,'disabled',h.disabled_at IS NOT NULL,'created_at',h.created_at)
        FROM event_hooks h LEFT JOIN dev_apps a ON a.id=h.app_id WHERE h.user_id=$1 AND ($2::text IS NULL OR h.app_id=$2) ORDER BY h.created_at")
        .bind(user).bind(client).fetch_all(&app.db).await?;
    Ok(Json(json!({"hooks": hooks, "max": MAX_HOOKS})))
}
/// GET /api/hooks: the caller's webhooks (through an app, only that app's).
async fn list_hooks(
    State(app): State<App>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Res<Json<Value>> {
    let (user, client, _) = hook_owner(&app, &headers, &jar).await?;
    hooks_of(&app, &user, client.as_deref()).await
}
#[derive(Deserialize)]
pub struct NewHook {
    topics: Vec<String>,
    url: String,
}
/// POST /api/hooks: registers an HTTPS URL for topics the caller may subscribe to. The signing
/// secret is returned once.
async fn create_hook(
    State(app): State<App>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(input): Json<NewHook>,
) -> Res<Json<Value>> {
    let (user, client, scopes) = hook_owner(&app, &headers, &jar).await?;
    let url = boards::webhook_target(&app, input.url.trim())
        .map_err(|m| Fail::field("url", m))?
        .to_string();
    let mut topics: Vec<String> = input
        .topics
        .iter()
        .map(|t| t.trim().to_lowercase())
        .collect();
    topics.sort();
    topics.dedup();
    if topics.is_empty() || topics.len() > HOOK_TOPICS {
        return Err(Fail::field("topics", "Choose 1 to 50 topics."));
    }
    let person = (user.clone(), scopes);
    for topic in &topics {
        if !allowed(&app, topic, Some(&person)).await? {
            return Err(Fail::field_owned(
                "topics",
                format!("You can't receive {topic}."),
            ));
        }
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM event_hooks WHERE user_id=$1")
        .bind(&user)
        .fetch_one(&app.db)
        .await?;
    if count >= MAX_HOOKS {
        return Err(Fail::denied("You can have up to 20 webhooks."));
    }
    let secret = format!("whsec_{}", sec::token());
    let id = profiles::new_id();
    sqlx::query(
        "INSERT INTO event_hooks(id,user_id,app_id,topics,url,secret) VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(&id)
    .bind(&user)
    .bind(&client)
    .bind(&topics)
    .bind(&url)
    .bind(sec::seal(&app, "event-hook", &secret)?)
    .execute(&app.db)
    .await?;
    let Json(mut body) = hooks_of(&app, &user, client.as_deref()).await?;
    body["created"] = json!({"id": id, "secret": secret});
    Ok(Json(body))
}
/// DELETE /api/hooks/{id}
async fn delete_hook(
    State(app): State<App>,
    headers: HeaderMap,
    jar: CookieJar,
    Path(id): Path<String>,
) -> Res<Json<Value>> {
    let (user, client, _) = hook_owner(&app, &headers, &jar).await?;
    sqlx::query(
        "DELETE FROM event_hooks WHERE id=$1 AND user_id=$2 AND ($3::text IS NULL OR app_id=$3)",
    )
    .bind(&id)
    .bind(&user)
    .bind(&client)
    .execute(&app.db)
    .await?;
    hooks_of(&app, &user, client.as_deref()).await
}

type Due = (
    i64,
    String,
    Value,
    i32,
    String,
    String,
    String,
    Option<String>,
);
/// Sends due webhook deliveries (its own loop, beside board webhooks). Access is checked again at
/// send time: a revoked or suspended app removes its hooks, and a topic the person may no longer
/// receive (a moderator removed, a channel gone) is dropped. 50 failures in a row turn the hook
/// off and tell its owner.
pub async fn deliver_hooks(app: &App) -> Res<()> {
    // ponytail: sequential, 20 per pass with a 5 s timeout each; send concurrently if hooks back up.
    let due: Vec<Due> = sqlx::query_as("SELECT d.id,d.hook_id,d.payload,d.attempts,h.url,h.secret,h.user_id,h.app_id FROM hook_deliveries d
        JOIN event_hooks h ON h.id=d.hook_id AND h.disabled_at IS NULL WHERE d.delivered_at IS NULL AND d.available_at<=now() ORDER BY d.id LIMIT 20")
        .fetch_all(&app.db).await?;
    for (id, hook, payload, attempts, url, sealed, user, client) in due {
        let scopes: Option<Vec<String>> = match &client {
            None => Some(vec!["events:private".into()]),
            Some(client) => sqlx::query_scalar("SELECT g.scopes FROM oauth_grants g JOIN dev_apps a ON a.id=g.app_id AND a.suspended_at IS NULL WHERE g.app_id=$1 AND g.user_id=$2 AND g.revoked_at IS NULL")
                .bind(client).bind(&user).fetch_optional(&app.db).await?,
        };
        let Some(scopes) = scopes else {
            sqlx::query("DELETE FROM event_hooks WHERE id=$1")
                .bind(&hook)
                .execute(&app.db)
                .await?;
            continue;
        };
        let topic = payload["topic"].as_str().unwrap_or_default();
        if !allowed(app, topic, Some(&(user.clone(), scopes))).await? {
            sqlx::query("DELETE FROM hook_deliveries WHERE id=$1")
                .bind(id)
                .execute(&app.db)
                .await?;
            continue;
        }
        let secret = sec::unseal(app, "event-hook", &sealed)?;
        let body = serde_json::to_vec(&payload).map_err(|_| Fail::internal())?;
        match boards::deliver(app, &url, &secret, &body).await {
            Ok(()) => {
                sqlx::query("UPDATE hook_deliveries SET delivered_at=now(), attempts=attempts+1, error=NULL WHERE id=$1")
                    .bind(id).execute(&app.db).await?;
                sqlx::query("UPDATE event_hooks SET failures=0 WHERE id=$1 AND failures>0")
                    .bind(&hook)
                    .execute(&app.db)
                    .await?;
            }
            Err(error) => {
                sqlx::query("UPDATE hook_deliveries SET attempts=$2, error=$3, available_at=CASE WHEN $2>=$4 THEN 'infinity' ELSE now()+make_interval(secs=>10*power(2,$2-1)) END WHERE id=$1")
                    .bind(id).bind(attempts + 1).bind(&error).bind(HOOK_ATTEMPTS)
                    .execute(&app.db).await?;
                let off: Option<bool> = sqlx::query_scalar("UPDATE event_hooks SET failures=failures+1, disabled_at=CASE WHEN failures+1>=$2 THEN now() END WHERE id=$1 AND disabled_at IS NULL RETURNING disabled_at IS NOT NULL")
                    .bind(&hook).bind(HOOK_FAILURES).fetch_optional(&app.db).await?;
                if off == Some(true) {
                    sqlx::query("INSERT INTO notifications(id,user_id,kind,channel_id,event_key,payload) VALUES(gen_random_uuid()::text,$1,'hook_disabled',$1,'hook_disabled:'||$2,
                        jsonb_build_object('title','A webhook was turned off','body','It failed 50 times in a row ('||$3||'). Fix the endpoint, then add it again.','url','/settings/developer')) ON CONFLICT DO NOTHING")
                        .bind(&user).bind(&hook).bind(&error).execute(&app.db).await?;
                }
            }
        }
    }
    sqlx::query("DELETE FROM hook_deliveries WHERE created_at<now()-interval '7 days'")
        .execute(&app.db)
        .await?;
    Ok(())
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/api/events", get(socket))
        .route("/api/hooks", get(list_hooks).post(create_hook))
        .route("/api/hooks/{id}", delete(delete_hook))
}
