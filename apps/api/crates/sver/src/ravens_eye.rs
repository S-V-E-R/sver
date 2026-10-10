//! Raven's Eye (docs/ADMIN.md "Raven's Eye"): staff-only platform analytics on Postgres. Each
//! finished UTC day is computed once and kept in `ravens_eye_days`, because some sources expire
//! (chat after 7 days); today is computed live. No IP addresses, devices or personal data: counts,
//! hours, money totals by kind and the top channels and categories.
use crate::{
    App,
    profiles::{Fail, Res},
};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use axum_extra::extract::cookie::CookieJar;
use chrono::{Days, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;

/// How far back the first run fills in (chat counts only exist for the last 7 days).
const BACKFILL_DAYS: u64 = 90;

/// One UTC day's numbers. Watch hours and the peak come from the viewer-integrity snapshots
/// (about one a minute per live broadcast, Counted + Trusted sessions only).
pub async fn compute(db: &mut PgConnection, day: NaiveDate) -> Res<Value> {
    Ok(sqlx::query_scalar(
        "WITH w AS (SELECT $1::date::timestamp AT TIME ZONE 'UTC' AS a, ($1::date+1)::timestamp AT TIME ZONE 'UTC' AS b)
        SELECT jsonb_build_object(
            'signups', (SELECT count(*) FROM users, w WHERE created_at>=a AND created_at<b),
            'verified_signups', (SELECT count(*) FROM users, w WHERE created_at>=a AND created_at<b AND email_verified),
            'viewers', (SELECT count(DISTINCT user_id) FROM xp_days WHERE day=$1 AND source='watch' AND minutes>0),
            'chatters', (SELECT count(DISTINCT author_id) FROM chat_messages, w WHERE created_at>=a AND created_at<b),
            'messages', (SELECT count(*) FROM chat_messages, w WHERE created_at>=a AND created_at<b),
            'follows', (SELECT count(*) FROM follows, w WHERE created_at>=a AND created_at<b),
            'faction_joins', (SELECT coalesce(jsonb_object_agg(faction,n),'{}') FROM (SELECT faction,count(*) n FROM faction_members, w WHERE joined_at>=a AND joined_at<b GROUP BY faction) f),
            'broadcasts', (SELECT count(*) FROM broadcasts, w WHERE started_at>=a AND started_at<b),
            'streamers', (SELECT count(DISTINCT owner_id) FROM broadcasts, w WHERE started_at>=a AND started_at<b),
            'broadcast_hours', (SELECT round(coalesce(sum(extract(epoch FROM coalesce(ended_at,now())-started_at)),0)::numeric/3600,1) FROM broadcasts, w WHERE started_at>=a AND started_at<b),
            'watch_hours', (SELECT round(coalesce(sum(counted+trusted),0)::numeric/60,1) FROM integrity_snapshots, w WHERE taken_at>=a AND taken_at<b),
            'peak_viewers', (SELECT coalesce(max(n),0) FROM (SELECT sum(counted+trusted) n FROM integrity_snapshots, w WHERE taken_at>=a AND taken_at<b GROUP BY date_trunc('minute',taken_at)) p),
            'money', (SELECT coalesce(jsonb_object_agg(kind,jsonb_build_object('count',c,'valor',v,'usd_cents',u)),'{}') FROM (
                SELECT t.kind,count(DISTINCT t.id) c,
                    coalesce(sum(e.amount) FILTER (WHERE e.unit='valor' AND e.amount>0),0) v,
                    (coalesce(sum(e.amount) FILTER (WHERE e.unit='usd' AND e.amount>0),0)/10)::bigint u
                FROM ledger_transactions t JOIN ledger_entries e ON e.transaction_id=t.id, w
                WHERE t.created_at>=a AND t.created_at<b GROUP BY t.kind) m),
            'top_channels', (SELECT coalesce(jsonb_agg(jsonb_build_object('username',username,'watch_hours',round(minutes::numeric/60,1)) ORDER BY minutes DESC),'[]') FROM (
                SELECT u.username,sum(s.counted+s.trusted) minutes FROM integrity_snapshots s JOIN broadcasts b ON b.id=s.broadcast_id JOIN users u ON u.id=b.owner_id, w
                WHERE s.taken_at>=a AND s.taken_at<b GROUP BY u.username ORDER BY 2 DESC LIMIT 10) t),
            'top_categories', (SELECT coalesce(jsonb_agg(jsonb_build_object('category',category,'broadcast_hours',round(seconds::numeric/3600,1)) ORDER BY seconds DESC),'[]') FROM (
                SELECT coalesce(v.category,c.name,'Uncategorized') category,sum(extract(epoch FROM coalesce(b.ended_at,now())-b.started_at)) seconds
                FROM broadcasts b LEFT JOIN videos v ON v.broadcast_id=b.id AND v.kind='VOD'
                LEFT JOIN stream_settings st ON st.owner_id=b.owner_id LEFT JOIN stream_categories c ON c.id=st.category_id, w
                WHERE b.started_at>=a AND b.started_at<b GROUP BY 1 ORDER BY 2 DESC LIMIT 10) k)
        )",
    )
    .bind(day)
    .fetch_one(db)
    .await?)
}

/// Hourly: stores every finished day not yet kept, newest first, a few per pass.
pub async fn tick(app: &App) -> Res<()> {
    let today = Utc::now().date_naive();
    let first = today
        .checked_sub_days(Days::new(BACKFILL_DAYS))
        .unwrap_or(today);
    let missing: Vec<NaiveDate> = sqlx::query_scalar("SELECT d::date FROM generate_series($1::date,$2::date-1,'1 day') d WHERE NOT EXISTS(SELECT 1 FROM ravens_eye_days WHERE day=d::date) ORDER BY d DESC LIMIT 10")
        .bind(first).bind(today).fetch_all(&app.db).await?;
    for day in missing {
        let mut db = app.db.acquire().await?;
        let stats = compute(&mut db, day).await?;
        sqlx::query("INSERT INTO ravens_eye_days(day,stats) VALUES($1,$2) ON CONFLICT DO NOTHING")
            .bind(day)
            .bind(stats)
            .execute(&mut *db)
            .await?;
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct Range {
    days: Option<u64>,
}
/// GET /api/admin/ravens-eye?days=30: the stored days in range plus today so far.
async fn view(
    State(app): State<App>,
    jar: CookieJar,
    Query(range): Query<Range>,
) -> Res<Json<Value>> {
    crate::safety::staff(&app, &jar).await?;
    let days = range.days.unwrap_or(30);
    if !(7..=BACKFILL_DAYS).contains(&days) {
        return Err(Fail::bad("Choose 7 to 90 days."));
    }
    let today = Utc::now().date_naive();
    let from = today.checked_sub_days(Days::new(days)).unwrap_or(today);
    let rows: Vec<(NaiveDate, Value)> = sqlx::query_as(
        "SELECT day,stats FROM ravens_eye_days WHERE day>=$1 AND day<$2 ORDER BY day",
    )
    .bind(from)
    .bind(today)
    .fetch_all(&app.db)
    .await?;
    let mut series: Vec<Value> = rows
        .into_iter()
        .map(|(day, stats)| json!({"day": day, "stats": stats, "partial": false}))
        .collect();
    let live = compute(&mut *app.db.acquire().await?, today).await?;
    series.push(json!({"day": today, "stats": live, "partial": true}));
    Ok(Json(json!({"days": series})))
}

pub fn routes() -> Router<App> {
    Router::new().route("/api/admin/ravens-eye", get(view))
}
