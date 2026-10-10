//! Follows, public follower lists, War Council, blocks and the user card (docs/PROFILES.md).
use crate::{
    App,
    profiles::{
        ChannelUser, CursorQuery, Fail, Res, avatar_json, blocked_between, bump_section,
        channel_user_by_id, chip_sql, eligible_by_name, ensure_profile, ensure_unrestricted,
        follower_counts, hydrate, make_cursor, parse_cursor, rate, section_revision, signed_in,
        viewer,
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use axum_extra::extract::cookie::CookieJar;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;

const PAGE: i64 = 50;

/// Recomputes the cached raw follow counters for both users (the displayed counts are computed
/// exactly at read time with the visibility rules).
pub async fn refresh_counts(db: &mut PgConnection, ids: &[&str]) -> Res<()> {
    for id in ids {
        ensure_profile(db, id).await?;
        sqlx::query("UPDATE profiles SET follower_count=(SELECT count(*) FROM follows WHERE following_id=$1),following_count=(SELECT count(*) FROM follows WHERE follower_id=$1) WHERE user_id=$1")
            .bind(id)
            .execute(&mut *db)
            .await?;
    }
    Ok(())
}

/// PUT /api/follows/{username}
pub async fn follow(
    State(app): State<App>,
    jar: CookieJar,
    Path(name): Path<String>,
) -> Res<Json<Value>> {
    let user = signed_in(&app, &jar).await?;
    let mut tx = app.db.begin().await?;
    let target = eligible_by_name(&mut tx, &name)
        .await?
        .ok_or_else(Fail::channel_missing)?;
    if target.id == user.id {
        return Err(Fail::bad("You can't follow yourself."));
    }
    if blocked_between(&mut tx, &user.id, &target.id).await? {
        return Err(Fail::denied("You can't follow this channel."));
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM follows WHERE follower_id=$1 AND following_id=$2)",
    )
    .bind(&user.id)
    .bind(&target.id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        rate(&app, format!("follow:hour:{}", user.id), 30, 3600).await?;
        rate(&app, format!("follow:day:{}", user.id), 100, 86400).await?;
        // Lock both profiles in a fixed order so concurrent follows keep exact counters.
        ensure_profile(&mut tx, &user.id).await?;
        ensure_profile(&mut tx, &target.id).await?;
        sqlx::query("SELECT 1 FROM profiles WHERE user_id IN ($1,$2) ORDER BY user_id FOR UPDATE")
            .bind(&user.id)
            .bind(&target.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO follows(follower_id,following_id) VALUES($1,$2) ON CONFLICT DO NOTHING",
        )
        .bind(&user.id)
        .bind(&target.id)
        .execute(&mut *tx)
        .await?;
        refresh_counts(&mut tx, &[&user.id, &target.id]).await?;
        crate::engagement::followed(&mut tx, &app.config.engagement, &target.id, &user.id).await?;
        crate::bot::record(&mut tx, &target.id, "follow", &user.id, 0).await?;
        // Live events: the count is public, who followed is the channel's private topic.
        let followers: i64 =
            sqlx::query_scalar("SELECT count(*) FROM follows WHERE following_id=$1")
                .bind(&target.id)
                .fetch_one(&mut *tx)
                .await?;
        crate::events::emit(
            &mut tx,
            &target.id,
            "follows",
            json!({"followers": followers}),
        )
        .await?;
        crate::events::emit(
            &mut tx,
            &target.id,
            "follows:detail",
            json!({"user": user.username}),
        )
        .await?;
        crate::activity::record(
            &mut tx,
            &user.id,
            "follow",
            Some(&target.id),
            None,
            json!({}),
        )
        .await?;
    }
    let (followers, _) = follower_counts(&mut tx, &target.id).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"following": true, "follower_count": followers}),
    ))
}
/// DELETE /api/follows/{username}
pub async fn unfollow(
    State(app): State<App>,
    jar: CookieJar,
    Path(name): Path<String>,
) -> Res<Json<Value>> {
    let user = signed_in(&app, &jar).await?;
    let mut tx = app.db.begin().await?;
    // Unfollow works for any existing account so hidden channels can still be removed.
    let target: Option<ChannelUser> =
        sqlx::query_as("SELECT * FROM channel_users WHERE lower(username)=lower($1)")
            .bind(&name)
            .fetch_optional(&mut *tx)
            .await?;
    let target = target.ok_or_else(Fail::channel_missing)?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM follows WHERE follower_id=$1 AND following_id=$2)",
    )
    .bind(&user.id)
    .bind(&target.id)
    .fetch_one(&mut *tx)
    .await?;
    if exists {
        rate(&app, format!("follow:hour:{}", user.id), 30, 3600).await?;
        ensure_profile(&mut tx, &user.id).await?;
        ensure_profile(&mut tx, &target.id).await?;
        sqlx::query("SELECT 1 FROM profiles WHERE user_id IN ($1,$2) ORDER BY user_id FOR UPDATE")
            .bind(&user.id)
            .bind(&target.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM follows WHERE follower_id=$1 AND following_id=$2")
            .bind(&user.id)
            .bind(&target.id)
            .execute(&mut *tx)
            .await?;
        refresh_counts(&mut tx, &[&user.id, &target.id]).await?;
    }
    let (followers, _) = follower_counts(&mut tx, &target.id).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"following": false, "follower_count": followers}),
    ))
}

async fn follow_list(
    app: &App,
    jar: &CookieJar,
    name: &str,
    cursor: &Option<String>,
    followers: bool,
) -> Res<Json<Value>> {
    let viewer = viewer(app, jar).await?;
    let mut db = app.db.acquire().await?;
    let owner = eligible_by_name(&mut db, name)
        .await?
        .ok_or_else(Fail::channel_missing)?;
    let after = parse_cursor(cursor)?;
    let (join_col, filter_col) = if followers {
        ("follower_id", "following_id")
    } else {
        ("following_id", "follower_id")
    };
    // Only literal column choices and chip SQL are interpolated; request values are bound.
    let rows: Vec<(Value, DateTime<Utc>, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {chip}, f.created_at, c.id FROM follows f JOIN channel_users c ON c.id=f.{join_col} \
         WHERE f.{filter_col}=$1 AND c.eligible \
         AND ($2::text IS NULL OR NOT EXISTS(SELECT 1 FROM user_blocks b WHERE (b.blocker_id=$2 AND b.blocked_id=c.id) OR (b.blocker_id=c.id AND b.blocked_id=$2))) \
         AND ($3::timestamptz IS NULL OR (f.created_at,c.id) < ($3,$4)) \
         ORDER BY f.created_at DESC, c.id DESC LIMIT $5",
        chip = chip_sql("c")
    )))
    .bind(&owner.id)
    .bind(viewer.as_ref().map(|v| v.id.clone()))
    .bind(after.as_ref().map(|a| a.0))
    .bind(after.as_ref().map(|a| a.1.clone()).unwrap_or_default())
    .bind(PAGE + 1)
    .fetch_all(&mut *db)
    .await?;
    let more = rows.len() as i64 > PAGE;
    let rows = &rows[..rows.len().min(PAGE as usize)];
    let mut items: Vec<Value> = rows
        .iter()
        .map(|(chip, at, _)| json!({"user": chip, "followed_at": at}))
        .collect();
    items.iter_mut().for_each(|v| hydrate(app, v));
    let next = if more {
        rows.last().map(|(_, at, id)| make_cursor(*at, id))
    } else {
        None
    };
    Ok(Json(
        json!({"owner": {"username": owner.username, "display_name": owner.display_name}, "items": items, "next_cursor": next}),
    ))
}
pub async fn followers(
    State(app): State<App>,
    jar: CookieJar,
    Path(name): Path<String>,
    Query(q): CursorQuery,
) -> Res<Json<Value>> {
    follow_list(&app, &jar, &name, &q.cursor, true).await
}
pub async fn following(
    State(app): State<App>,
    jar: CookieJar,
    Path(name): Path<String>,
    Query(q): CursorQuery,
) -> Res<Json<Value>> {
    follow_list(&app, &jar, &name, &q.cursor, false).await
}
#[derive(Deserialize)]
pub struct FollowingQuery {
    cursor: Option<String>,
    #[serde(default)]
    live: bool,
}
/// Followed channels plus live members of followed guilds, without duplicate channels.
pub async fn my_following(
    State(app): State<App>,
    jar: CookieJar,
    Query(q): Query<FollowingQuery>,
) -> Res<Json<Value>> {
    let user = signed_in(&app, &jar).await?;
    let after = parse_cursor(&q.cursor)?;
    let mut db = app.db.acquire().await?;
    let guilds = crate::guilds::followed_members(&mut db, &user.id).await?;
    let guild_ids: Vec<_> = guilds.iter().map(|r| r.0.clone()).collect();
    let guild_dates: Vec<_> = guilds.iter().map(|r| r.1).collect();
    // Only literal column choices and chip SQL are interpolated; request values are bound.
    let rows: Vec<(Value, DateTime<Utc>, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "WITH sources AS (SELECT following_id AS id,created_at FROM follows WHERE follower_id=$1 UNION ALL SELECT gs.id,gs.at FROM unnest($5::text[],$6::timestamptz[]) gs(id,at) WHERE {guild_live}), \
         f AS (SELECT id,max(created_at) AS created_at FROM sources GROUP BY id) \
         SELECT {chip} || jsonb_build_object('direct_follow',EXISTS(SELECT 1 FROM follows WHERE follower_id=$1 AND following_id=c.id)), f.created_at, c.id FROM f JOIN channel_users c ON c.id=f.id WHERE c.eligible \
         AND (NOT $7 OR {live}) AND NOT EXISTS(SELECT 1 FROM user_blocks b WHERE (b.blocker_id=$1 AND b.blocked_id=c.id) OR (b.blocker_id=c.id AND b.blocked_id=$1)) \
         AND ($2::timestamptz IS NULL OR (f.created_at,c.id) < ($2,$3)) ORDER BY f.created_at DESC, c.id DESC LIMIT $4",
        chip = chip_sql("c"), live = crate::playback::live_sql("c.id"), guild_live = crate::playback::live_sql("gs.id")
    )))
    .bind(&user.id)
    .bind(after.as_ref().map(|a| a.0))
    .bind(after.as_ref().map(|a| a.1.clone()).unwrap_or_default())
    .bind(PAGE + 1)
    .bind(guild_ids)
    .bind(guild_dates)
    .bind(q.live)
    .fetch_all(&mut *db)
    .await?;
    let more = rows.len() as i64 > PAGE;
    let rows = &rows[..rows.len().min(PAGE as usize)];
    let mature = crate::streams::mature_hidden(&mut db, Some(&user.id)).await?;
    let mut items: Vec<Value> = rows
        .iter()
        .filter(|(_, _, id)| !mature.contains(id))
        .map(|(chip, at, id)| json!({"user": chip, "followed_at": at,"guilds":guilds.iter().find(|g|g.0==*id).map(|g|&g.2)}))
        .collect();
    items.iter_mut().for_each(|v| hydrate(&app, v));
    Ok(Json(
        json!({"items": items, "next_cursor": if more { rows.last().map(|(_, at, id)| make_cursor(*at, id)) } else { None }}),
    ))
}

/// GET /api/me/suggestions: up to 8 channels to follow during onboarding. Live channels first,
/// then the viewer's own faction, then whoever streamed most recently. Never ranked by follower
/// or viewer counts. Excludes the viewer, channels already followed, and blocks either way.
pub async fn suggestions(State(app): State<App>, jar: CookieJar) -> Res<Json<Value>> {
    let user = signed_in(&app, &jar).await?;
    let mut db = app.db.acquire().await?;
    let faction = crate::factions::membership(&mut db, &user.id).await?;
    // Only chip SQL and the live expression are interpolated; request values are bound.
    let rows: Vec<(Value,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {chip} FROM channel_users c JOIN users u ON u.id=c.id \
         WHERE c.eligible AND c.id<>$1 \
         AND NOT EXISTS(SELECT 1 FROM follows f WHERE f.follower_id=$1 AND f.following_id=c.id) \
         AND NOT EXISTS(SELECT 1 FROM user_blocks b WHERE (b.blocker_id=$1 AND b.blocked_id=c.id) OR (b.blocker_id=c.id AND b.blocked_id=$1)) \
         AND EXISTS(SELECT 1 FROM broadcasts s WHERE s.owner_id=c.id) \
         ORDER BY {live} DESC, (u.faction IS NOT DISTINCT FROM $2) DESC, \
         (SELECT max(s.started_at) FROM broadcasts s WHERE s.owner_id=c.id) DESC NULLS LAST, c.id LIMIT 8",
        chip = chip_sql("c"),
        live = crate::playback::live_sql("c.id")
    )))
    .bind(&user.id)
    .bind(&faction)
    .fetch_all(&mut *db)
    .await?;
    let mut items: Vec<Value> = rows.into_iter().map(|(chip,)| chip).collect();
    items.iter_mut().for_each(|v| hydrate(&app, v));
    Ok(Json(json!({ "items": items })))
}

/// War Council for display: eligible members in stored order (gaps filled), plus the owner notice.
pub async fn war_council_read(
    app: &App,
    db: &mut PgConnection,
    owner_id: &str,
    viewer_id: Option<&str>,
    is_owner: bool,
) -> Res<Value> {
    // Only literal column choices and chip SQL are interpolated; request values are bound.
    let rows: Vec<(i32, Value, bool, bool)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT w.position, {chip}, c.eligible, ($2::text IS NOT NULL AND EXISTS(SELECT 1 FROM user_blocks b WHERE (b.blocker_id=$2 AND b.blocked_id=c.id) OR (b.blocker_id=c.id AND b.blocked_id=$2))) \
         FROM war_council w JOIN channel_users c ON c.id=w.member_id WHERE w.user_id=$1 ORDER BY w.position",
        chip = chip_sql("c")
    )))
    .bind(owner_id)
    .bind(viewer_id)
    .fetch_all(&mut *db)
    .await?;
    let mut members: Vec<Value> = rows
        .iter()
        .filter(|r| r.2)
        .map(|r| json!({"position": r.0, "user": r.1, "crown": false}))
        .collect();
    if let Some(first) = members.first_mut() {
        first["crown"] = json!(true);
    }
    members.iter_mut().for_each(|v| hydrate(app, v));
    let unavailable = rows.iter().filter(|r| !r.2).count();
    Ok(json!({"members": members, "unavailable_count": if is_owner { unavailable } else { 0 }}))
}
/// GET /api/me/war-council: the owner's stored list including unavailable members.
pub async fn my_war_council(State(app): State<App>, jar: CookieJar) -> Res<Json<Value>> {
    let user = signed_in(&app, &jar).await?;
    let mut db = app.db.acquire().await?;
    // Only literal column choices and chip SQL are interpolated; request values are bound.
    let mut rows: Vec<Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT jsonb_build_object('position',w.position,'user',{chip},'available',c.eligible) FROM war_council w JOIN channel_users c ON c.id=w.member_id WHERE w.user_id=$1 ORDER BY w.position",
        chip = chip_sql("c")
    )))
    .bind(&user.id)
    .fetch_all(&mut *db)
    .await?;
    rows.iter_mut().for_each(|v| hydrate(&app, v));
    Ok(Json(
        json!({"members": rows, "revision": section_revision(&mut db, &user.id, "war_council").await?}),
    ))
}
#[derive(Deserialize)]
pub struct Search {
    q: String,
}
/// GET /api/me/war-council/search?q=: eligible users by username or display name (10 results).
pub async fn war_council_search(
    State(app): State<App>,
    jar: CookieJar,
    Query(input): Query<Search>,
) -> Res<Json<Value>> {
    let user = signed_in(&app, &jar).await?;
    let q = input.q.trim();
    if q.is_empty() || q.chars().count() > 32 {
        return Ok(Json(json!({"results": []})));
    }
    rate(&app, format!("search:{}", user.id), 60, 60).await?;
    let pattern = format!(
        "{}%",
        q.to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let mut db = app.db.acquire().await?;
    // Only literal column choices and chip SQL are interpolated; request values are bound.
    let mut rows: Vec<Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT {chip} FROM channel_users c WHERE c.eligible AND c.id<>$1 \
         AND NOT EXISTS(SELECT 1 FROM user_blocks b WHERE (b.blocker_id=$1 AND b.blocked_id=c.id) OR (b.blocker_id=c.id AND b.blocked_id=$1)) \
         AND (lower(c.username) LIKE $2 OR lower(c.display_name) LIKE $2) ORDER BY (lower(c.username)=lower($3)) DESC, lower(c.username) LIMIT 10",
        chip = chip_sql("c")
    )))
    .bind(&user.id)
    .bind(&pattern)
    .bind(q)
    .fetch_all(&mut *db)
    .await?;
    rows.iter_mut().for_each(|v| hydrate(&app, v));
    Ok(Json(json!({"results": rows})))
}
#[derive(Deserialize)]
pub struct CouncilInput {
    members: Vec<String>,
    revision: Option<i64>,
}
/// PUT /api/me/war-council: replaces the ordered list (usernames) in one transaction.
pub async fn save_war_council(
    State(app): State<App>,
    jar: CookieJar,
    Json(input): Json<CouncilInput>,
) -> Res<Json<Value>> {
    let user = signed_in(&app, &jar).await?;
    if input.members.len() > 8 {
        return Err(Fail::conflict("Your War Council is full."));
    }
    let mut tx = app.db.begin().await?;
    ensure_unrestricted(&mut tx, &user.id).await?;
    ensure_profile(&mut tx, &user.id).await?;
    let revision = bump_section(&mut tx, &user.id, "war_council", input.revision).await?;
    let existing: Vec<String> =
        sqlx::query_scalar("SELECT member_id FROM war_council WHERE user_id=$1")
            .bind(&user.id)
            .fetch_all(&mut *tx)
            .await?;
    let mut ids: Vec<String> = Vec::new();
    for name in &input.members {
        let member: Option<ChannelUser> =
            sqlx::query_as("SELECT * FROM channel_users WHERE lower(username)=lower($1)")
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?;
        let member =
            member.ok_or_else(|| Fail::new(StatusCode::NOT_FOUND, "That user can't be added."))?;
        if member.id == user.id {
            return Err(Fail::bad("You can't add yourself."));
        }
        if ids.contains(&member.id) {
            return Err(Fail::bad("Each member can appear once."));
        }
        // Members who became unavailable may stay where they were; new members must be eligible.
        if !member.eligible && !existing.contains(&member.id) {
            return Err(Fail::new(
                StatusCode::NOT_FOUND,
                "That user can't be added.",
            ));
        }
        if blocked_between(&mut tx, &user.id, &member.id).await? {
            return Err(Fail::denied("That user can't be added."));
        }
        ids.push(member.id);
    }
    sqlx::query("DELETE FROM war_council WHERE user_id=$1")
        .bind(&user.id)
        .execute(&mut *tx)
        .await?;
    for (i, id) in ids.iter().enumerate() {
        sqlx::query("INSERT INTO war_council(user_id,position,member_id) VALUES($1,$2,$3)")
            .bind(&user.id)
            .bind(i as i32 + 1)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    if !ids.is_empty() {
        crate::activity::record(
            &mut tx,
            &user.id,
            "war_council",
            None,
            None,
            json!({"count": ids.len()}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"saved": true, "revision": revision})))
}

/// Applies block side effects in the caller's transaction.
pub async fn apply_block(db: &mut PgConnection, blocker: &str, blocked: &str) -> Res<()> {
    sqlx::query(
        "INSERT INTO user_blocks(blocker_id,blocked_id) VALUES($1,$2) ON CONFLICT DO NOTHING",
    )
    .bind(blocker)
    .bind(blocked)
    .execute(&mut *db)
    .await?;
    ensure_profile(db, blocker).await?;
    ensure_profile(db, blocked).await?;
    sqlx::query("SELECT 1 FROM profiles WHERE user_id IN ($1,$2) ORDER BY user_id FOR UPDATE")
        .bind(blocker)
        .bind(blocked)
        .execute(&mut *db)
        .await?;
    sqlx::query("DELETE FROM follows WHERE (follower_id=$1 AND following_id=$2) OR (follower_id=$2 AND following_id=$1)").bind(blocker).bind(blocked).execute(&mut *db).await?;
    refresh_counts(db, &[blocker, blocked]).await?;
    for (owner, member) in [(blocker, blocked), (blocked, blocker)] {
        let removed = sqlx::query("DELETE FROM war_council WHERE user_id=$1 AND member_id=$2")
            .bind(owner)
            .bind(member)
            .execute(&mut *db)
            .await?;
        if removed.rows_affected() > 0 {
            // Compact positions so the remaining members keep their order.
            let rest: Vec<String> = sqlx::query_scalar(
                "SELECT member_id FROM war_council WHERE user_id=$1 ORDER BY position",
            )
            .bind(owner)
            .fetch_all(&mut *db)
            .await?;
            sqlx::query("DELETE FROM war_council WHERE user_id=$1")
                .bind(owner)
                .execute(&mut *db)
                .await?;
            for (i, id) in rest.iter().enumerate() {
                sqlx::query("INSERT INTO war_council(user_id,position,member_id) VALUES($1,$2,$3)")
                    .bind(owner)
                    .bind(i as i32 + 1)
                    .bind(id)
                    .execute(&mut *db)
                    .await?;
            }
        }
    }
    // The blocked user's pending content on the blocker's channel is rejected.
    sqlx::query("UPDATE wall_posts SET status='REJECTED',moderated_at=now() WHERE wall_owner_id=$1 AND author_id=$2 AND status='PENDING'").bind(blocker).bind(blocked).execute(&mut *db).await?;
    sqlx::query("UPDATE wall_replies r SET status='REJECTED',moderated_at=now() FROM wall_posts p WHERE r.post_id=p.id AND p.wall_owner_id=$1 AND r.author_id=$2 AND r.status='PENDING'").bind(blocker).bind(blocked).execute(&mut *db).await?;
    sqlx::query("UPDATE fan_art SET status='REJECTED',reviewed_at=now() WHERE channel_id=$1 AND submitter_id=$2 AND status='PENDING'").bind(blocker).bind(blocked).execute(&mut *db).await?;
    Ok(())
}
/// PUT /api/blocks/{username}: silent, immediate, capped at 1,000.
pub async fn block(
    State(app): State<App>,
    jar: CookieJar,
    Path(name): Path<String>,
) -> Res<Json<Value>> {
    let user = signed_in(&app, &jar).await?;
    let mut tx = app.db.begin().await?;
    let target: Option<ChannelUser> = sqlx::query_as(
        "SELECT * FROM channel_users WHERE lower(username)=lower($1) AND deleted_at IS NULL",
    )
    .bind(&name)
    .fetch_optional(&mut *tx)
    .await?;
    let target = target.filter(|t| !t.internal).ok_or_else(Fail::missing)?;
    if target.id == user.id {
        return Err(Fail::bad("You can't block yourself."));
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM user_blocks WHERE blocker_id=$1 AND blocked_id=$2)",
    )
    .bind(&user.id)
    .bind(&target.id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        rate(&app, format!("block:{}", user.id), 30, 3600).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM user_blocks WHERE blocker_id=$1")
            .bind(&user.id)
            .fetch_one(&mut *tx)
            .await?;
        if count >= 1000 {
            return Err(Fail::conflict(
                "You've reached the limit of 1,000 blocked users.",
            ));
        }
        apply_block(&mut tx, &user.id, &target.id).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"blocked": true})))
}
/// DELETE /api/blocks/{username}: restores nothing that was removed.
pub async fn unblock(
    State(app): State<App>,
    jar: CookieJar,
    Path(name): Path<String>,
) -> Res<Json<Value>> {
    let user = signed_in(&app, &jar).await?;
    let mut db = app.db.acquire().await?;
    let removed = sqlx::query("DELETE FROM user_blocks WHERE blocker_id=$1 AND blocked_id=(SELECT id FROM users WHERE lower(username)=lower($2))").bind(&user.id).bind(&name).execute(&mut *db).await?;
    if removed.rows_affected() > 0 {
        rate(&app, format!("block:{}", user.id), 30, 3600).await?;
    }
    Ok(Json(json!({"blocked": false})))
}
/// GET /api/me/blocks
pub async fn my_blocks(State(app): State<App>, jar: CookieJar) -> Res<Json<Value>> {
    let user = signed_in(&app, &jar).await?;
    let mut db = app.db.acquire().await?;
    let rows: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('username',u.username,'blocked_at',b.created_at) FROM user_blocks b JOIN users u ON u.id=b.blocked_id WHERE b.blocker_id=$1 ORDER BY b.created_at DESC")
        .bind(&user.id)
        .fetch_all(&mut *db)
        .await?;
    Ok(Json(json!({"items": rows})))
}

/// Linked Twitch and Discord handles, only when the owner opted in (decision P6).
pub async fn also_known_as(db: &mut PgConnection, user_id: &str) -> Res<Vec<Value>> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT i.provider,i.handle FROM identities i JOIN profiles p ON p.user_id=i.user_id WHERE i.user_id=$1 AND p.show_linked_accounts AND i.handle IS NOT NULL AND i.provider IN ('twitch','discord') ORDER BY i.provider DESC")
        .bind(user_id)
        .fetch_all(&mut *db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(provider, handle)| {
            let url = (provider == "twitch").then(|| format!("https://twitch.tv/{handle}"));
            json!({"platform": provider, "handle": handle, "url": url})
        })
        .collect())
}
/// GET /api/users/{username}/card
pub async fn card(
    State(app): State<App>,
    jar: CookieJar,
    Path(name): Path<String>,
) -> Res<Json<Value>> {
    let viewer = viewer(&app, &jar).await?;
    let mut db = app.db.acquire().await?;
    let user = eligible_by_name(&mut db, &name)
        .await?
        .ok_or_else(Fail::missing)?;
    let (followers, _) = follower_counts(&mut db, &user.id).await?;
    let (following, blocked, is_self) = match &viewer {
        Some(v) if v.id != user.id => {
            let (f, b): (bool, bool) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM follows WHERE follower_id=$1 AND following_id=$2),EXISTS(SELECT 1 FROM user_blocks WHERE blocker_id=$1 AND blocked_id=$2)").bind(&v.id).bind(&user.id).fetch_one(&mut *db).await?;
            (f, b, false)
        }
        Some(_) => (false, false, true),
        None => (false, false, false),
    };
    let me = channel_user_by_id(&mut db, &user.id)
        .await?
        .ok_or_else(Fail::missing)?;
    let also_known_as = match &viewer {
        Some(v) if v.id != user.id && blocked_between(&mut db, &v.id, &user.id).await? => {
            Vec::new()
        }
        _ => also_known_as(&mut db, &user.id).await?,
    };
    let faction = crate::factions::membership(&mut db, &me.id).await?;
    let level = crate::progression::level(crate::progression::total(&mut db, &me.id).await?);
    Ok(Json(json!({
        "username": me.username,
        "also_known_as": also_known_as,
        "display_name": me.display_name,
        "avatar": avatar_json(&app, me.avatar_key.as_deref()),
        "bio": me.bio.chars().take(160).collect::<String>(),
        "follower_count": followers,
        "joined_at": me.created_at,
        "viewer": {"signed_in": viewer.is_some(), "is_self": is_self, "following": following, "blocked": blocked},
        "faction": faction,
        "live": crate::playback::is_live(&mut db, &me.id).await?,
        "level": level,
        "title": crate::progression::title(level, faction.as_deref()),
        "frame": crate::progression::frame(level),
    })))
}
