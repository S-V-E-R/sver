//! Real Postgres transactions and HTTP permissions; synthetic accounts only, isolated schema.
use super::*;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;

async fn call(
    app: &App,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("origin", &app.config.origin)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("cookie", format!("sver_dev={token}"));
    }
    let response = crate::router(app.clone())
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}
async fn sql(db: &PgPool, query: impl sqlx::SqlSafeStr) {
    sqlx::query(query).execute(db).await.unwrap();
}
async fn advance(app: &App, at: DateTime<Utc>) {
    let mut tx = app.db.begin().await.unwrap();
    engine::lock(&mut tx).await.unwrap();
    engine::advance(&mut tx, &app.config.factions, at)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}
async fn fixture(app: &App, n: usize, faction: Option<&str>) -> String {
    let id = format!("faction-user-{n}");
    let token = crate::security::token();
    let mut tx = app.db.begin().await.unwrap();
    sqlx::query("INSERT INTO users(id,username,email,date_of_birth,email_verified) VALUES($1,$2,$3,'1990-01-01',true)")
        .bind(&id).bind(format!("Faction{n}")).bind(format!("faction{n}@example.test")).execute(&mut *tx).await.unwrap();
    profiles::ensure_profile(&mut tx, &id).await.unwrap();
    sqlx::query("INSERT INTO sessions(id,user_id,token_hash,auth_version,mfa_verified,user_agent) SELECT $1,id,$2,auth_version,true,'synthetic' FROM users WHERE id=$1")
        .bind(&id).bind(crate::security::digest(&token)).execute(&mut *tx).await.unwrap();
    if let Some(f) = faction {
        import_membership(&mut tx, &id, f, Utc::now() - Duration::days(1))
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    token
}
async fn exercise(app: App) {
    let start = app.config.factions.starts_at.unwrap();
    advance(&app, Utc::now()).await;
    let mut tokens = Vec::new();
    for n in 0..10 {
        tokens.push(
            fixture(
                &app,
                n,
                if n == 9 {
                    None
                } else {
                    Some(if n == 2 {
                        "glint"
                    } else if n == 0 {
                        "myria"
                    } else {
                        "aetheron"
                    })
                },
            )
            .await,
        );
    }
    let now = Utc::now();
    let mut db = app.db.acquire().await.unwrap();
    let week = engine::current(&mut db, now).await.unwrap().unwrap();
    drop(db);
    // The war map: fixed spots, one capital per faction, and neighbors that touch on the map.
    let war = call(&app, "GET", "/api/factions/war", None, Value::Null).await;
    assert_eq!(war.0, StatusCode::OK, "{}", war.1);
    let genre = |id: &str| {
        war.1["genres"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(genre("art")["map"], json!({"q":1,"r":0}));
    assert_eq!(genre("strategy_4x")["capital"], true);
    assert_eq!(genre("art")["capital"], false);
    assert_eq!(
        genre("art")["neighbors"],
        json!(["strategy_4x", "puzzle_simulation", "education_coding"])
    );
    assert_eq!(
        genre("coop_party")["neighbors"],
        json!([
            "fps_battle_royale",
            "fighting",
            "rts_moba",
            "community_events"
        ])
    );
    for g in war.1["genres"].as_array().unwrap() {
        if g["map"].is_null() {
            continue;
        }
        for n in g["neighbors"].as_array().unwrap() {
            assert!(
                genre(n.as_str().unwrap())["neighbors"]
                    .as_array()
                    .unwrap()
                    .contains(&g["id"]),
                "{} and {n} must border each other",
                g["id"]
            );
        }
    }
    let capitals: Vec<_> = war.1["genres"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["capital"] == true)
        .map(|g| g["home"].as_str().unwrap())
        .collect();
    assert_eq!(capitals.len(), 3);
    for f in FACTIONS {
        assert!(capitals.contains(&f));
    }
    // Enrollment, original choice clock, one free change and no second free switch.
    let chosen = call(
        &app,
        "PUT",
        "/api/me/faction",
        Some(&tokens[9]),
        json!({"faction":"glint"}),
    )
    .await;
    assert_eq!(chosen.0, StatusCode::OK, "{}", chosen.1);
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/me/faction",
            None,
            json!({"faction":"myria"})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/me/faction",
            Some(&tokens[9]),
            json!({"faction":"invented"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let first = call(
        &app,
        "PUT",
        "/api/me/faction",
        Some(&tokens[9]),
        json!({"faction":"myria"}),
    )
    .await;
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(first.1["chosen_at"], chosen.1["chosen_at"]);
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/me/faction",
            Some(&tokens[9]),
            json!({"faction":"aetheron"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    sql(&app.db,"UPDATE faction_members SET chosen_at=now()-interval '8 days' WHERE user_id='faction-user-2'").await;
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/me/faction",
            Some(&tokens[2]),
            json!({"faction":"myria"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut tx = app.db.begin().await.unwrap();
    assert!(
        !import_membership(&mut tx, "faction-user-9", "glint", start)
            .await
            .unwrap(),
        "An import cannot overwrite a later choice"
    );
    tx.commit().await.unwrap();

    // Private votes, no voter identifiers, and post/report visibility confined to members.
    for path in [
        "/api/factions/aetheron/council",
        "/api/factions/aetheron/board",
        "/api/factions/aetheron/election",
    ] {
        assert_eq!(
            call(&app, "GET", path, Some(&tokens[2]), Value::Null)
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
        assert_eq!(
            call(&app, "GET", path, None, Value::Null).await.0,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    assert_eq!(
        call(&app, "GET", "/api/factions/aetheron", None, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/factions/aetheron/council",
            Some(&tokens[1]),
            json!({"genre":"art"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let council = call(
        &app,
        "GET",
        "/api/factions/aetheron/council",
        Some(&tokens[1]),
        Value::Null,
    )
    .await
    .1;
    assert_eq!(council["my_vote"], "art");
    assert!(!council.to_string().contains("faction-user"));
    let post_id = profiles::new_id();
    let body = json!({"id":post_id,"body":"Let us build something together."});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/factions/aetheron/board",
            Some(&tokens[1]),
            body.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/factions/aetheron/board",
            Some(&tokens[1]),
            body
        )
        .await
        .0,
        StatusCode::OK,
        "Replay is harmless"
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/factions/aetheron/board",
            Some(&tokens[1]),
            json!({"id":profiles::new_id(),"body":"Second post"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let report = json!({"target_type":"faction_post","target_id":post_id,"reason":"spam"});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/reports",
            Some(&tokens[2]),
            report.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "POST", "/api/reports", Some(&tokens[3]), report)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/admin/factions",
            Some(&tokens[1]),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/admin/factions/retry",
            Some(&tokens[1]),
            json!({"note":"retry"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/factions/aetheron/candidate",
            Some(&tokens[3]),
            json!({"enabled":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    for n in [1, 4, 5] {
        assert_eq!(
            call(
                &app,
                "PUT",
                "/api/factions/aetheron/election",
                Some(&tokens[n]),
                json!({"username":"Faction3"})
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    sql(
        &app.db,
        "UPDATE users SET email_verified=false WHERE id='faction-user-8'",
    )
    .await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/factions/aetheron/board",
            Some(&tokens[8]),
            json!({"id":profiles::new_id(),"body":"Unverified"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    // Catalog writes need staff step-up, and a running season freezes genre assignments.
    sql(
        &app.db,
        "UPDATE users SET mfa_enabled=true,mfa_secret='synthetic' WHERE id='faction-user-7'",
    )
    .await;
    sql(
        &app.db,
        "INSERT INTO staff_roles(user_id,role) VALUES('faction-user-7','admin')",
    )
    .await;
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/admin/factions",
            Some(&tokens[7]),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/admin/categories",
            Some(&tokens[7]),
            json!({"name":"Synthetic strategy","genre":"strategy_4x","note":"Test catalog"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            "/api/admin/categories/synthetic-strategy",
            Some(&tokens[7]),
            json!({"genre":"art","note":"Test move"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/admin/categories/synthetic-strategy/merge",
            Some(&tokens[7]),
            json!({"into":"art","note":"Test merge"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/admin/genres",
            Some(&tokens[7]),
            json!({"name":"Synthetic genre","note":"Test genre"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/admin/genres",
            Some(&tokens[7]),
            json!({"name":"Synthetic genre","note":"Test duplicate"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/admin/factions/retry",
            Some(&tokens[7]),
            json!({"note":"Nothing due yet"})
        )
        .await
        .0,
        StatusCode::OK
    );
    // The existing staff report workflow sees the private snapshot and removes the post.
    let queue = call(
        &app,
        "GET",
        "/api/admin/reports?type=faction_post",
        Some(&tokens[7]),
        Value::Null,
    )
    .await;
    assert_eq!(queue.0, StatusCode::OK);
    assert_eq!(
        queue.1["groups"][0]["current"]["body"],
        "Let us build something together."
    );
    let removed = call(
        &app,
        "POST",
        &format!("/api/admin/reports/faction_post/{post_id}/actions"),
        Some(&tokens[7]),
        json!({"action":"remove_content","note":"Synthetic moderation test"}),
    )
    .await;
    assert_eq!(removed.0, StatusCode::OK, "{}", removed.1);
    assert!(
        call(
            &app,
            "GET",
            "/api/factions/aetheron/board",
            Some(&tokens[1]),
            Value::Null
        )
        .await
        .1["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut tx = app.db.begin().await.unwrap();
    restore(&mut tx, &post_id, "VISIBLE").await.unwrap();
    tx.commit().await.unwrap();
    // Elected authority is scoped to one faction and term; a normal member cannot remove it.
    sqlx::query("INSERT INTO faction_moderators(week_id,user_id,faction) VALUES($1,'faction-user-3','aetheron')").bind(week.id).execute(&app.db).await.unwrap();
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/factions/aetheron/board/{post_id}"),
            Some(&tokens[4]),
            json!({"note":"No authority"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/factions/aetheron/board/{post_id}"),
            Some(&tokens[3]),
            json!({"note":"Elected moderator removal"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/factions/aetheron/board",
            Some(&tokens[8]),
            json!({"id":profiles::new_id(),"body":"x".repeat(501)})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // Verified source and audience facts, actual categories, bonuses, caps and time overlap.
    sql(&app.db,"INSERT INTO stream_settings(owner_id,title,category_id) VALUES('faction-user-0','Making art','art')").await;
    sql(&app.db,"INSERT INTO broadcasts(id,owner_id,public_id,generation,state,server_id,service_id,client_id,started_at,publisher_started_at,startup_deadline,observed_at) VALUES('faction-live','faction-user-0','faction-live',1,'LIVE','s','v','c',now(),now(),now(),now())").await;
    sql(&app.db,"INSERT INTO playback_leases(broadcast_id,viewer_key,expires_at,level,signed_in,verified) VALUES('faction-live','u:faction-user-2',now()+interval '10 minutes','trusted',true,true),('faction-live','u:faction-user-1',now()+interval '10 minutes','counted',true,true)").await;
    let mut tx = app.db.begin().await.unwrap();
    sqlx::query("INSERT INTO faction_targets(week_id,faction,genre) VALUES($1,'glint','art')")
        .bind(week.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let at = Utc::now();
    assert_eq!(
        watch(&app, &mut tx, "faction-live", "faction-user-1", 10_000, at)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        watch(&app, &mut tx, "faction-live", "faction-user-2", 10_000, at)
            .await
            .unwrap(),
        825,
        "Enemy turf and council bonus stack"
    );
    assert_eq!(
        watch(&app, &mut tx, "faction-live", "faction-user-2", 10_000, at)
            .await
            .unwrap(),
        0,
        "Duplicate heartbeat"
    );
    assert_eq!(
        stream(&app, &mut tx, "faction-live", 10_000, at)
            .await
            .unwrap(),
        1500
    );
    assert_eq!(
        chat(
            &app,
            &mut tx,
            "faction-user-0",
            "faction-user-2",
            "message-one"
        )
        .await
        .unwrap(),
        2
    );
    assert_eq!(
        chat(
            &app,
            &mut tx,
            "faction-user-0",
            "faction-user-2",
            "message-two"
        )
        .await
        .unwrap(),
        2,
        "Hourly cap is applied after bonuses"
    );
    assert_eq!(
        chat(
            &app,
            &mut tx,
            "faction-user-0",
            "faction-user-2",
            "message-three"
        )
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        support(
            &app,
            &mut tx,
            "faction-live",
            "faction-user-2",
            "settled-one"
        )
        .await
        .unwrap(),
        2
    );
    assert_eq!(
        support(
            &app,
            &mut tx,
            "faction-live",
            "faction-user-2",
            "settled-two"
        )
        .await
        .unwrap(),
        0,
        "One distinct supporter per UTC day"
    );
    assert_eq!(
        support(
            &app,
            &mut tx,
            "faction-live",
            "faction-user-2",
            "settled-one"
        )
        .await
        .unwrap(),
        0,
        "Payment replay"
    );
    tx.commit().await.unwrap();
    assert!(
        sqlx::query("UPDATE faction_influence SET points=points+1")
            .execute(&app.db)
            .await
            .is_err()
    );
    // Concurrent attempts on different event IDs still share the same person/day cap.
    let mut tasks = Vec::new();
    for i in 0..5 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            let mut tx = app.db.begin().await.unwrap();
            let at = Utc::now() + Duration::seconds(15 * (i + 1));
            watch(&app, &mut tx, "faction-live", "faction-user-2", 15_000, at)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    // The cap is per UTC day of `at`, and a run just before midnight spreads these events over two days.
    let earned:i64=sqlx::query_scalar("SELECT max(points)::bigint FROM (SELECT sum(points) AS points FROM faction_influence WHERE user_id='faction-user-2' AND source='watch' GROUP BY date_trunc('day',happened_at AT TIME ZONE 'UTC')) days").fetch_one(&app.db).await.unwrap();
    assert!(earned <= app.config.factions.daily_caps[1]);
    // No Trusted viewer, no streaming points; unverified supporter and expired playback also fail.
    sql(
        &app.db,
        "UPDATE playback_leases SET expires_at=now()-interval '1 second'",
    )
    .await;
    let mut tx = app.db.begin().await.unwrap();
    assert_eq!(
        stream(&app, &mut tx, "faction-live", 10_000, Utc::now())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        watch(
            &app,
            &mut tx,
            "faction-live",
            "faction-user-2",
            10_000,
            Utc::now()
        )
        .await
        .unwrap(),
        0
    );
    tx.commit().await.unwrap();

    // Synthetic ledger facts drive a future checkpoint without weakening production clocks.
    sql(
        &app.db,
        "INSERT INTO faction_genres(id,name,position) VALUES('neutral_test','Neutral test',999)",
    )
    .await;
    sqlx::query("INSERT INTO faction_territories(season_id,genre) VALUES($1,'neutral_test')")
        .bind(week.season_id)
        .execute(&app.db)
        .await
        .unwrap();
    for (event, genre, side, points) in [
        ("held-a", "music", "glint", 100),
        ("held-b", "music", "myria", 105),
        ("neutral-a", "neutral_test", "myria", 5000),
        ("art-a", "art", "myria", 5000),
    ] {
        sqlx::query("INSERT INTO faction_influence(event_key,user_id,faction,week_id,genre,source,points,happened_at) VALUES($1,'faction-user-0',$2,$3,$4,'stream',$5,$6)")
            .bind(event).bind(side).bind(week.id).bind(genre).bind(points as i64).bind(now).execute(&app.db).await.unwrap();
    }
    advance(&app, week.ends_at).await;
    let mut db = app.db.acquire().await.unwrap();
    let next = engine::current(&mut db, week.ends_at)
        .await
        .unwrap()
        .unwrap();
    let holder: Option<String> = sqlx::query_scalar(
        "SELECT holder FROM faction_territories WHERE season_id=$1 AND genre='music'",
    )
    .bind(week.season_id)
    .fetch_one(&mut *db)
    .await
    .unwrap();
    assert_eq!(
        holder.as_deref(),
        Some("glint"),
        "An exact 5% lead keeps the holder"
    );
    let claimed: String = sqlx::query_scalar(
        "SELECT holder FROM faction_territories WHERE season_id=$1 AND genre='neutral_test'",
    )
    .bind(week.season_id)
    .fetch_one(&mut *db)
    .await
    .unwrap();
    assert_eq!(claimed, "myria");
    let elected:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM faction_moderators WHERE week_id=$1 AND user_id='faction-user-3')").bind(next.id).fetch_one(&mut *db).await.unwrap();
    assert!(elected);
    let target: String = sqlx::query_scalar(
        "SELECT genre FROM faction_targets WHERE week_id=$1 AND faction='aetheron'",
    )
    .bind(next.id)
    .fetch_one(&mut *db)
    .await
    .unwrap();
    assert_eq!(target, "art");
    let season = engine::latest(&mut db).await.unwrap().unwrap();
    drop(db);
    advance(&app, week.ends_at).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM faction_weeks WHERE starts_at=$1")
        .bind(week.ends_at)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(n, 1);
    advance(&app, season.ends_at).await;
    let mut db = app.db.acquire().await.unwrap();
    let ended = engine::latest(&mut db).await.unwrap().unwrap();
    assert!(ended.finished_at.is_some());
    assert_eq!(
        ended.winners,
        vec!["myria"],
        "The captured neutral genre and art give Myria the final majority"
    );
    assert_eq!(
        status(&mut db, "faction-user-2", season.ends_at)
            .await
            .unwrap()["can_choose"],
        true
    );
    let reward_count: i64 = sqlx::query_scalar("SELECT count(*) FROM faction_rewards")
        .fetch_one(&mut *db)
        .await
        .unwrap();
    assert!(reward_count > 0);
    drop(db);
    advance(&app, season.ends_at).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM faction_rewards")
            .fetch_one(&app.db)
            .await
            .unwrap(),
        reward_count
    );
    advance(&app, season.next_starts_at).await;
    let mut db = app.db.acquire().await.unwrap();
    let new = engine::latest(&mut db).await.unwrap().unwrap();
    assert_eq!(new.number, 2);
    let reset:bool=sqlx::query_scalar("SELECT bool_and(t.holder IS NOT DISTINCT FROM g.home) FROM faction_territories t JOIN faction_genres g ON g.id=t.genre WHERE t.season_id=$1").bind(new.id).fetch_one(&mut *db).await.unwrap();
    assert!(reset);
    drop(db);
    // Erasure removes attribution without rewriting season totals.
    sql(&app.db, "DELETE FROM users WHERE id='faction-user-2'").await;
    assert!(sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM faction_influence WHERE user_id IS NULL AND faction='glint')").fetch_one(&app.db).await.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn faction_lifecycle_permissions_and_replay() {
    let url = std::env::var("DATABASE_URL").expect("Use scripts/dev.ps1 test");
    let parsed = url::Url::parse(&url).unwrap();
    assert_eq!(parsed.path(), "/sver_rebuild");
    assert!(matches!(parsed.host_str(), Some("localhost" | "127.0.0.1")));
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("factions_test_{}", uuid::Uuid::new_v4().simple());
    sql(
        &admin,
        sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")),
    )
    .await;
    let search = format!("SET search_path TO {schema}");
    let db = PgPoolOptions::new()
        .max_connections(12)
        .after_connect(move |db, _| {
            let search = search.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(search)).execute(db).await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../../../migrations")
        .run(&db)
        .await
        .unwrap();
    let mut config = crate::Config::from_env().unwrap();
    config.factions = Tuning {
        starts_at: Some(Utc::now() - Duration::hours(1)),
        points_per_second: [100, 50],
        chat_points: 2,
        supporter_points: 2,
        daily_caps: [10_000, 2500, 10, 100],
        chat_hourly_cap: 4,
        minimum_divisor: 25,
        flip_margin_bps: 500,
        neutral_minimum: 100,
        ..Tuning::default()
    };
    let app = App::new(db.clone(), config).await.unwrap();
    let result = tokio::spawn(exercise(app)).await;
    db.close().await;
    sql(
        &admin,
        sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")),
    )
    .await;
    admin.close().await;
    result.unwrap();
}
