//! Progression, part 1 (docs/PROGRESSION.md): account XP and levels, and the Scout bonus. XP comes
//! only from verified viewers' Counted or Trusted playback, chat, and Scout bonuses, each capped per
//! UTC day. Purchases never give XP, and levels never touch MAGNet, rotation or influence.
use crate::{
    App,
    profiles::{self, Fail, Res},
};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use axum_extra::extract::cookie::CookieJar;
use serde_json::{Value, json};
use sqlx::PgConnection;

pub const MAX_LEVEL: i64 = 100;
/// Daily caps (Proposed): watching 600 (an hour at the base rate), chat 100, Scout 150 (3 bonuses).
const WATCH_CAP: i32 = 600;
const CHAT_CAP: i32 = 100;
const SCOUT_XP: i32 = 50;
const SCOUT_CAP: i32 = 150;

/// Total XP needed to reach level `l` (legacy's curve): floor(100 × (l−1)^1.5).
pub fn xp_for(level: i64) -> i64 {
    (100.0 * ((level - 1) as f64).powf(1.5)).floor() as i64
}
/// The level for a total, from 1 to 100.
pub fn level(xp: i64) -> i64 {
    (1..=MAX_LEVEL)
        .take_while(|l| xp_for(*l) <= xp)
        .last()
        .unwrap_or(1)
}
pub async fn total(db: &mut PgConnection, user: &str) -> Res<i64> {
    Ok(
        sqlx::query_scalar("SELECT coalesce(sum(xp),0)::bigint FROM xp_days WHERE user_id=$1")
            .bind(user)
            .fetch_one(db)
            .await?,
    )
}
/// Channel loyalty ranks by lifetime Engagement Valor earned there (Proposed thresholds).
pub const LOYALTY: [(&str, i64); 5] = [
    ("Newcomer", 0),
    ("Regular", 200),
    ("Devoted", 1_000),
    ("Veteran", 5_000),
    ("Legend", 20_000),
];
/// The loyalty rank (0-4) for an earned total.
pub fn loyalty(earned: i64) -> i16 {
    LOYALTY.iter().filter(|(_, at)| earned >= *at).count() as i16 - 1
}
/// Faction rank titles (docs/PROGRESSION.md section 2): the 22-rank ladder from the main S.V.E.R
/// Discord, so the site and the server read the same. Columns: no faction, Myria, Aetheron, Glint.
const TITLES: [[&str; 4]; 22] = [
    ["Private", "Initiate", "Novice", "Spark"],
    ["PFC", "Sentinel-II", "Seeker", "Flicker"],
    ["Specialist", "Sentinel-I", "Scholar", "Kindle"],
    ["Corporal", "Warblade", "Keeper", "Torch"],
    ["Sergeant", "Sentinel", "Warden", "Ember"],
    ["Staff Sgt", "Iron Guard", "High Warden", "Blazeborn"],
    ["SFC", "Shield Wall", "Sage", "Firebrand"],
    ["Master Sgt", "Warmaster", "Oracle", "Pyroclast"],
    ["First Sgt", "Champion", "Seer", "Infernus"],
    ["Sgt Major", "Grand Champ", "High Seer", "Magmus"],
    ["Cmd Sgt Maj", "Warlord", "Luminary", "Vulcanis"],
    ["2nd Lieutenant", "Blade Cmdr", "Arcanist", "Flamelord"],
    ["1st Lieutenant", "War Cmdr", "High Arc", "Emberlord"],
    ["Captain", "Vanguard", "Archon", "Blaze"],
    ["Major", "High Van", "Magistrate", "Scorchwind"],
    ["Lt Colonel", "Battlemaster", "Tribunal", "Ashbringer"],
    ["Colonel", "Grand Battle", "High Trib", "Cataclysm"],
    ["Brig General", "Siege Lord", "Consul", "Firestorm"],
    ["Maj General", "High Siege", "Praetor", "Conflagra"],
    ["Lt General", "Conquest", "Imperator", "Apocalypse"],
    ["General", "Grand Conq", "High Imp", "Ragnarok"],
    ["Commander", "Overlord", "Sovereign", "Inferno"],
];
/// The account level each title starts at: spread evenly from level 1 to 100.
fn title_level(rank: usize) -> i64 {
    1 + (rank as i64 * 99 + 10) / 21
}
/// The title for a level, in the faction's names (the base names without a faction).
pub fn title(level: i64, faction: Option<&str>) -> &'static str {
    let rank = (0..TITLES.len())
        .rev()
        .find(|r| level >= title_level(*r))
        .unwrap_or(0);
    let column = match faction {
        Some("myria") => 1,
        Some("aetheron") => 2,
        Some("glint") => 3,
        _ => 0,
    };
    TITLES[rank][column]
}
/// The level badge's frame (the level-up cosmetic, Joe's decision): a new frame every 10 levels.
pub fn frame(level: i64) -> i64 {
    level / 10
}
pub fn summary(xp: i64) -> Value {
    let level = level(xp);
    json!({"xp": xp, "level": level, "frame": frame(level), "level_xp": xp_for(level), "next_xp": (level < MAX_LEVEL).then(|| xp_for(level + 1))})
}

/// Each minute (with Engagement Valor's watch points): 10 XP a minute of Counted or Trusted
/// playback, 15 on a stream of the viewer's own faction; then Scout bonuses.
pub async fn award_watch(app: &App) -> Res<()> {
    sqlx::query("INSERT INTO xp_days(user_id,day,source,xp,minutes)
        SELECT v.id,current_date,'watch',v.xp,1 FROM (
          SELECT u.id,max(CASE WHEN vf.faction IS NOT NULL AND vf.faction=ofm.faction THEN 15 ELSE 10 END) AS xp
          FROM playback_leases l JOIN broadcasts b ON b.id=l.broadcast_id JOIN users u ON l.viewer_key='u:'||u.id
            LEFT JOIN faction_members vf ON vf.user_id=u.id LEFT JOIN faction_members ofm ON ofm.user_id=b.owner_id
          WHERE l.expires_at>now() AND l.level IN ('counted','trusted') AND b.state IN ('LIVE','RECONNECTING')
            AND u.email_verified AND u.deleted_at IS NULL AND u.id<>b.owner_id
          GROUP BY u.id) v
        ON CONFLICT(user_id,day,source) DO UPDATE SET xp=least($1,xp_days.xp+EXCLUDED.xp),minutes=xp_days.minutes+1,updated_at=now()
        WHERE xp_days.updated_at<=now()-interval '55 seconds'")
        .bind(WATCH_CAP)
        .execute(&app.db)
        .await?;
    // Scout: 10+ minutes of real playback that began in the broadcast's first 15 minutes, on a
    // channel with fewer than 10 broadcasts; once per channel per week, up to 3 a day.
    sqlx::query("WITH eligible AS (
          SELECT DISTINCT ON (u.id,b.owner_id) u.id AS user_id,b.owner_id,b.id AS broadcast_id
          FROM playback_leases l JOIN broadcasts b ON b.id=l.broadcast_id JOIN users u ON l.viewer_key='u:'||u.id
          WHERE l.expires_at>now() AND l.level IN ('counted','trusted') AND b.state IN ('LIVE','RECONNECTING')
            AND u.email_verified AND u.deleted_at IS NULL AND u.id<>b.owner_id
            AND l.created_at<=b.started_at+interval '15 minutes' AND l.created_at<=now()-interval '10 minutes'
            AND (SELECT count(*) FROM broadcasts p WHERE p.owner_id=b.owner_id)<10
            AND NOT EXISTS(SELECT 1 FROM scout_awards s WHERE s.user_id=u.id AND s.channel_id=b.owner_id AND s.at>now()-interval '7 days')
            AND (SELECT count(*) FROM scout_awards s WHERE s.user_id=u.id AND s.at>=current_date)<3),
        awarded AS (INSERT INTO scout_awards(user_id,channel_id,broadcast_id) SELECT user_id,owner_id,broadcast_id FROM eligible ON CONFLICT DO NOTHING RETURNING user_id)
        INSERT INTO xp_days(user_id,day,source,xp) SELECT user_id,current_date,'scout',$1*count(*)::int FROM awarded GROUP BY user_id
        ON CONFLICT(user_id,day,source) DO UPDATE SET xp=least($2,xp_days.xp+EXCLUDED.xp),updated_at=now()")
        .bind(SCOUT_XP)
        .bind(SCOUT_CAP)
        .execute(&app.db)
        .await?;
    complete_orders(app).await
}
/// 2 XP for a chat message, at most once a minute (inside the message's transaction).
pub(crate) async fn chatted(tx: &mut PgConnection, user: &str) -> Res<()> {
    sqlx::query(
        "INSERT INTO xp_days(user_id,day,source,xp) VALUES($1,current_date,'chat',2)
        ON CONFLICT(user_id,day,source) DO UPDATE SET xp=least($2,xp_days.xp+2),updated_at=now()
        WHERE xp_days.updated_at<=now()-interval '60 seconds'",
    )
    .bind(user)
    .bind(CHAT_CAP)
    .execute(tx)
    .await?;
    Ok(())
}

/// GET /api/me/progression: XP, level and the next level's threshold (the player card).
async fn mine(State(app): State<App>, jar: CookieJar) -> Res<Json<Value>> {
    let user = profiles::signed_in(&app, &jar).await?;
    let mut db = app.db.acquire().await?;
    let xp = total(&mut db, &user.id).await?;
    let faction = crate::factions::membership(&mut db, &user.id).await?;
    let mut value = summary(xp);
    value["title"] = json!(title(level(xp), faction.as_deref()));
    Ok(Json(value))
}

// ---- Daily orders (docs/PROGRESSION.md section 3) ----

/// Order types: kind, label (`{n}` is the target), and the target for each rarity.
const KINDS: [(&str, &str, [i32; 5]); 10] = [
    (
        "watch",
        "Watch {n} minutes of live streams",
        [15, 30, 45, 60, 90],
    ),
    ("chat", "Chat in {n} different channels", [1, 2, 3, 4, 5]),
    ("follow", "Follow {n} new channels", [1, 1, 2, 2, 3]),
    ("surge", "Take part in {n} Surges", [1, 1, 2, 2, 3]),
    ("rally", "Rally your faction {n} times", [1, 3, 5, 8, 12]),
    ("poll", "Vote in {n} polls", [1, 2, 3, 4, 5]),
    ("board", "Press a board control {n} times", [1, 3, 5, 8, 12]),
    ("beacon", "Watch {n} Beacons to the end", [1, 2, 3, 4, 5]),
    ("raid", "Join {n} raids", [1, 1, 1, 2, 2]),
    (
        "scout",
        "Scout {n} new streams (watch in their first 15 minutes)",
        [1, 1, 1, 2, 2],
    ),
];
pub const RARITY: [(&str, u32, f64); 5] = [
    ("Common", 50, 1.0),
    ("Uncommon", 30, 1.5),
    ("Rare", 15, 2.0),
    ("Epic", 4, 3.0),
    ("Legendary", 1, 5.0),
];
/// XP for a Common order before the streak (Proposed).
const ORDER_XP: f64 = 40.0;
/// Engagement Valor an order pays by rarity (Joe's decision: 10–50, free, no cash value), in the
/// channel the viewer was last in that day.
const ORDER_EV: [i64; 5] = [10, 15, 20, 30, 50];
/// Today's progress for order `o`, from the records each type names (UTC day).
const PROGRESS: &str = "CASE o.kind
    WHEN 'watch' THEN coalesce((SELECT minutes FROM xp_days WHERE user_id=o.user_id AND day=o.day AND source='watch'),0)
    WHEN 'chat' THEN (SELECT count(DISTINCT channel_id) FROM chat_messages WHERE author_id=o.user_id AND channel_id<>o.user_id AND created_at>=o.day::timestamp AT TIME ZONE 'UTC' AND deleted_at IS NULL)
    WHEN 'follow' THEN (SELECT count(*) FROM follows WHERE follower_id=o.user_id AND created_at>=o.day::timestamp AT TIME ZONE 'UTC')
    WHEN 'surge' THEN (SELECT count(*) FROM surge_awards WHERE user_id=o.user_id AND day=o.day)
    WHEN 'rally' THEN (SELECT count(*) FROM rallies WHERE user_id=o.user_id AND minute>=extract(epoch FROM o.day::timestamp AT TIME ZONE 'UTC')::bigint/60)
    WHEN 'poll' THEN (SELECT count(*) FROM poll_votes WHERE user_id=o.user_id AND created_at>=o.day::timestamp AT TIME ZONE 'UTC')
    WHEN 'board' THEN (SELECT count(*) FROM board_presses WHERE user_id=o.user_id AND created_at>=o.day::timestamp AT TIME ZONE 'UTC')
    WHEN 'beacon' THEN (SELECT count(*) FROM beacon_playback WHERE viewer_key='u:'||o.user_id AND day=o.day AND completed)
    WHEN 'raid' THEN (SELECT count(DISTINCT raid_id) FROM playback_leases WHERE viewer_key='u:'||o.user_id AND raid_id IS NOT NULL AND created_at>=o.day::timestamp AT TIME ZONE 'UTC')
    WHEN 'scout' THEN (SELECT count(*) FROM scout_awards WHERE user_id=o.user_id AND at>=o.day::timestamp AT TIME ZONE 'UTC')
    ELSE 0 END";

/// The streak multiplier: ×1.1 at 3 days rising to ×2 at 30.
pub fn streak_multiplier(days: i64) -> f64 {
    if days < 3 {
        1.0
    } else {
        (1.1 + (days - 3) as f64 * 0.9 / 27.0).min(2.0)
    }
}
/// Days in a row, ending today, with at least one completed order (today counts once one is done).
async fn streak(db: &mut PgConnection, user: &str) -> Res<i64> {
    let days: Vec<i32> = sqlx::query_scalar("SELECT DISTINCT current_date-day FROM daily_orders WHERE user_id=$1 AND completed_at IS NOT NULL AND day>current_date-31 ORDER BY 1")
        .bind(user)
        .fetch_all(db)
        .await?;
    let start = if days.first() == Some(&0) { 0 } else { 1 };
    Ok(days
        .iter()
        .skip_while(|d| **d < start)
        .zip(start..)
        .take_while(|(d, want)| **d == *want)
        .count() as i64)
}
/// A random draw (the v4 UUID's random bits are enough for order picks).
fn random(below: u32) -> u32 {
    (uuid::Uuid::new_v4().as_u128() % below as u128) as u32
}
fn draw_rarity() -> i16 {
    let mut roll = random(RARITY.iter().map(|r| r.1).sum());
    for (i, r) in RARITY.iter().enumerate() {
        if roll < r.1 {
            return i as i16;
        }
        roll -= r.1;
    }
    0
}
/// Draws an order type not in `taken` that the viewer can do (rally needs a faction).
fn draw_kind(taken: &[String], faction: bool) -> usize {
    let open: Vec<usize> = (0..KINDS.len())
        .filter(|i| !taken.iter().any(|t| t == KINDS[*i].0) && (faction || KINDS[*i].0 != "rally"))
        .collect();
    open[random(open.len() as u32) as usize]
}
async fn has_faction(db: &mut PgConnection, user: &str) -> Res<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM faction_members WHERE user_id=$1)")
            .bind(user)
            .fetch_one(db)
            .await?,
    )
}
/// Creates today's 3 orders if they don't exist yet.
async fn ensure_orders(db: &mut PgConnection, user: &str) -> Res<()> {
    let have: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM daily_orders WHERE user_id=$1 AND day=current_date",
    )
    .bind(user)
    .fetch_one(&mut *db)
    .await?;
    if have > 0 {
        return Ok(());
    }
    let faction = has_faction(db, user).await?;
    let mut taken = Vec::new();
    for slot in 0..3i16 {
        let (kind, rarity) = (draw_kind(&taken, faction), draw_rarity());
        taken.push(KINDS[kind].0.to_string());
        sqlx::query("INSERT INTO daily_orders(user_id,day,slot,kind,rarity,target) VALUES($1,current_date,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
            .bind(user).bind(slot).bind(KINDS[kind].0).bind(rarity).bind(KINDS[kind].2[rarity as usize])
            .execute(&mut *db).await?;
    }
    Ok(())
}
/// Each minute: stamps orders whose progress reached the target, pays their XP (rarity × streak)
/// and any weekly milestone (10, 20, 30 orders: 100, 200, 300 XP; Proposed).
async fn complete_orders(app: &App) -> Res<()> {
    let done: Vec<(String, i16, i16)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE daily_orders o SET completed_at=now() WHERE o.day=current_date AND o.completed_at IS NULL AND ({PROGRESS})>=o.target RETURNING o.user_id,o.slot,o.rarity"
    )))
    .fetch_all(&app.db)
    .await?;
    for (user, slot, rarity) in done {
        let mut tx = app.db.begin().await?;
        let days = streak(&mut tx, &user).await?;
        let mut xp =
            (ORDER_XP * RARITY[rarity as usize].2 * streak_multiplier(days)).round() as i32;
        sqlx::query(
            "UPDATE daily_orders SET xp=$3 WHERE user_id=$1 AND day=current_date AND slot=$2",
        )
        .bind(&user)
        .bind(slot)
        .bind(xp)
        .execute(&mut *tx)
        .await?;
        let paid: Vec<i16> = sqlx::query_scalar("INSERT INTO order_milestones(user_id,week,orders)
            SELECT $1,date_trunc('week',current_date)::date,m FROM unnest(ARRAY[10,20,30]::smallint[]) m
            WHERE m<=(SELECT count(*) FROM daily_orders WHERE user_id=$1 AND completed_at IS NOT NULL AND day>=date_trunc('week',current_date)::date)
            ON CONFLICT DO NOTHING RETURNING orders")
            .bind(&user).fetch_all(&mut *tx).await?;
        xp += paid.iter().map(|m| *m as i32 * 10).sum::<i32>();
        // ponytail: "where the order was done" = the channel of the latest playback or chat
        // today; an order finished with no channel activity (only Beacons, say) pays XP only.
        let channel: Option<String> = sqlx::query_scalar("SELECT a.channel FROM (
                SELECT b.owner_id AS channel,l.expires_at AS at FROM playback_leases l JOIN broadcasts b ON b.id=l.broadcast_id WHERE l.viewer_key='u:'||$1 AND l.expires_at>=current_date
                UNION ALL SELECT channel_id,created_at FROM chat_messages WHERE author_id=$1 AND channel_id IS NOT NULL AND created_at>=current_date
            ) a WHERE a.channel<>$1 AND NOT EXISTS(SELECT 1 FROM channel_restrictions r WHERE r.channel_id=a.channel AND r.user_id=$1 AND r.kind='ban')
            ORDER BY a.at DESC LIMIT 1")
            .bind(&user).fetch_optional(&mut *tx).await?;
        if let Some(channel) = &channel {
            let ev = ORDER_EV[rarity as usize];
            sqlx::query("INSERT INTO engagement(channel_id,user_id,balance,earned) VALUES($1,$2,$3,$3)
                ON CONFLICT(channel_id,user_id) DO UPDATE SET balance=engagement.balance+$3, earned=engagement.earned+$3")
                .bind(channel).bind(&user).bind(ev).execute(&mut *tx).await?;
            sqlx::query("UPDATE daily_orders SET ev=$3,ev_channel=$4 WHERE user_id=$1 AND day=current_date AND slot=$2")
                .bind(&user).bind(slot).bind(ev as i32).bind(channel).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO xp_days(user_id,day,source,xp) VALUES($1,current_date,'orders',$2)
            ON CONFLICT(user_id,day,source) DO UPDATE SET xp=xp_days.xp+EXCLUDED.xp,updated_at=now()")
            .bind(&user).bind(xp).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(())
}
/// slot, kind, rarity, target, progress, XP paid, Engagement Valor paid, its channel's name
type OrderRow = (
    i16,
    String,
    i16,
    i32,
    i64,
    Option<i32>,
    Option<i32>,
    Option<String>,
);
async fn orders_json(db: &mut PgConnection, user: &str) -> Res<Value> {
    let rows: Vec<OrderRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT o.slot,o.kind,o.rarity,o.target,least(({PROGRESS})::bigint,o.target),o.xp,o.ev,(SELECT display_name FROM channel_users WHERE id=o.ev_channel) FROM daily_orders o WHERE o.user_id=$1 AND o.day=current_date ORDER BY o.slot"
    )))
    .bind(user)
    .fetch_all(&mut *db)
    .await?;
    let days = streak(db, user).await?;
    let (week, rerolled): (i64, bool) = sqlx::query_as("SELECT (SELECT count(*) FROM daily_orders WHERE user_id=$1 AND completed_at IS NOT NULL AND day>=date_trunc('week',current_date)::date),EXISTS(SELECT 1 FROM daily_orders WHERE user_id=$1 AND day=current_date AND rerolled)")
        .bind(user).fetch_one(&mut *db).await?;
    let orders: Vec<Value> = rows
        .into_iter()
        .map(|(slot, kind, rarity, target, progress, xp, ev, ev_channel)| {
            let (_, label, _) = KINDS.iter().find(|k| k.0 == kind).copied().unwrap_or(KINDS[0]);
            let label = label.replace("{n}", &target.to_string());
            let label = if target == 1 { label.replace("channels", "channel").replace("Surges", "Surge").replace("polls", "poll").replace("Beacons", "Beacon").replace("raids", "raid").replace("streams", "stream").replace("times", "time") } else { label };
            let rarity = rarity as usize;
            json!({"slot": slot, "kind": kind, "label": label, "rarity": RARITY[rarity].0, "target": target, "progress": progress,
                "done": xp.is_some(), "xp": xp.unwrap_or((ORDER_XP * RARITY[rarity].2 * streak_multiplier(days.max(1))).round() as i32),
                "ev": ev.map_or(ORDER_EV[rarity], i64::from), "ev_channel": ev_channel})
        })
        .collect();
    Ok(
        json!({"orders": orders, "streak": days, "multiplier": streak_multiplier(days.max(1)), "week": week, "can_reroll": !rerolled}),
    )
}
/// GET /api/me/orders: today's orders (verified accounts only; created on first view).
async fn my_orders(State(app): State<App>, jar: CookieJar) -> Res<Json<Value>> {
    let user = profiles::signed_in(&app, &jar).await?;
    if !user.email_verified {
        return Ok(Json(json!({"orders": [], "verify": true})));
    }
    let mut db = app.db.acquire().await?;
    ensure_orders(&mut db, &user.id).await?;
    Ok(Json(orders_json(&mut db, &user.id).await?))
}
/// POST /api/me/orders/{slot}/reroll: once a day, an unfinished order is redrawn.
async fn reroll(State(app): State<App>, jar: CookieJar, Path(slot): Path<i16>) -> Res<Json<Value>> {
    let user = profiles::signed_in(&app, &jar).await?;
    let mut tx = app.db.begin().await?;
    let kinds: Vec<(i16, String, bool, bool)> = sqlx::query_as("SELECT slot,kind,rerolled,completed_at IS NOT NULL FROM daily_orders WHERE user_id=$1 AND day=current_date ORDER BY slot FOR UPDATE")
        .bind(&user.id).fetch_all(&mut *tx).await?;
    if kinds.iter().any(|k| k.2) {
        return Err(Fail::bad("You've used today's reroll."));
    }
    match kinds.iter().find(|k| k.0 == slot) {
        None => return Err(Fail::missing()),
        Some(k) if k.3 => return Err(Fail::bad("That order is already done.")),
        _ => {}
    }
    let taken: Vec<String> = kinds.into_iter().map(|k| k.1).collect();
    let faction = has_faction(&mut tx, &user.id).await?;
    let (kind, rarity) = (draw_kind(&taken, faction), draw_rarity());
    sqlx::query("UPDATE daily_orders SET kind=$3,rarity=$4,target=$5,rerolled=true WHERE user_id=$1 AND day=current_date AND slot=$2")
        .bind(&user.id).bind(slot).bind(KINDS[kind].0).bind(rarity).bind(KINDS[kind].2[rarity as usize])
        .execute(&mut *tx).await?;
    tx.commit().await?;
    let mut db = app.db.acquire().await?;
    Ok(Json(orders_json(&mut db, &user.id).await?))
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/api/me/progression", get(mine))
        .route("/api/me/orders", get(my_orders))
        .route("/api/me/orders/{slot}/reroll", post(reroll))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn faction_titles_follow_the_discord_ladder() {
        assert_eq!(title(1, Some("myria")), "Initiate");
        assert_eq!(title(5, None), "Private");
        assert_eq!(title(6, None), "PFC");
        assert_eq!(title(99, Some("aetheron")), "High Imp");
        assert_eq!(title(100, Some("glint")), "Inferno");
        assert_eq!((frame(9), frame(10), frame(100)), (0, 1, 10));
    }
    #[test]
    fn levels_follow_the_legacy_curve() {
        assert_eq!(
            (xp_for(1), xp_for(2), xp_for(10), xp_for(50)),
            (0, 100, 2700, 34300)
        );
        assert_eq!(
            (
                level(0),
                level(99),
                level(100),
                level(2700),
                level(10_000_000)
            ),
            (1, 1, 2, 10, 100)
        );
        assert_eq!(
            (
                streak_multiplier(2),
                streak_multiplier(3),
                streak_multiplier(30),
                streak_multiplier(90)
            ),
            (1.0, 1.1, 2.0, 2.0)
        );
        assert_eq!(
            draw_kind(
                &KINDS
                    .iter()
                    .filter(|k| k.0 != "scout")
                    .map(|k| k.0.to_string())
                    .collect::<Vec<_>>(),
                false
            ),
            9
        );
        assert_eq!(
            (
                loyalty(0),
                loyalty(199),
                loyalty(200),
                loyalty(19_999),
                loyalty(20_000)
            ),
            (0, 0, 1, 3, 4)
        );
    }
}
