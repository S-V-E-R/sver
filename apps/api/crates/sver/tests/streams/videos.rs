use super::*;

/// URLs the fake CDN was asked to purge.
static PURGED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
async fn bytes(app: &App, path: &str, cookie: Option<&str>) -> (StatusCode, Vec<u8>) {
    let mut request = Request::builder()
        .uri(path)
        .extension(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
    if let Some(cookie) = cookie {
        request = request.header("cookie", format!("{}={cookie}", app.config.cookie_name()));
    }
    let response = sver::router(app.clone())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let code = response.status();
    let headers = response.headers().clone();
    assert_eq!(headers["cache-control"], "no-store");
    (
        code,
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recordings_retry_privacy_cuts_and_retention() {
    let (admin, db, schema) = isolated_database().await;
    let root = std::env::temp_dir().join(format!("sver-videos-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&root).unwrap();
    let segment_template = root.join("source-%d.ts");
    let result = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440",
            "-t",
            "8",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "25",
            "-sc_threshold",
            "0",
            "-bf",
            "0",
            "-c:a",
            "aac",
            "-f",
            "hls",
            "-hls_time",
            "1",
            "-hls_list_size",
            "0",
            "-hls_segment_filename",
        ])
        .arg(&segment_template)
        .arg(root.join("source.m3u8"))
        .output()
        .expect("FFmpeg is required for recording acceptance");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let segments = Arc::new(
        (0..8)
            .map(|n| std::fs::read(root.join(format!("source-{n}.ts"))).unwrap())
            .collect::<Vec<_>>(),
    );
    let fake = Arc::new(Mutex::new(Media {
        service: "boot-one".into(),
        ..Default::default()
    }));
    let media = Router::new()
        .route("/api/v1/versions", get(versions))
        .route(
            "/turnstile",
            post(|body: String| async move {
                Json(json!({"success":body.split('&').any(|part|part=="response=pass")}))
            }),
        )
        .route(
            "/rebuild/{name}",
            get({
                let segments = segments.clone();
                move |Path(name): Path<String>| {
                    let segments = segments.clone();
                    async move {
                        let number = name
                            .trim_end_matches(".ts")
                            .rsplit('-')
                            .next()
                            .unwrap()
                            .parse::<usize>()
                            .unwrap();
                        ([("content-type", "video/mp2t")], segments[number].clone())
                    }
                }
            }),
        )
        .route(
            "/storage/{*key}",
            axum::routing::any(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        )
        .route(
            "/purge",
            post(|Json(body): Json<Value>| async move {
                let files = body["files"].as_array().cloned().unwrap_or_default();
                PURGED.lock().unwrap().extend(
                    files
                        .into_iter()
                        .filter_map(|f| f.as_str().map(str::to_owned)),
                );
                Json(json!({"success": true}))
            }),
        )
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, media).await.unwrap() });
    let mut config = Config::from_env().unwrap();
    config.resend_key.clear();
    config.turnstile_url = format!("http://{address}/turnstile");
    if std::env::var_os("SVER_VIDEO_PREVIEW_DIR").is_some() {
        config.origin = "http://127.0.0.1:3308".into();
    }
    config.streaming = Some(streams::Config {
        api_url: format!("http://{address}"),
        ingest_url: "rtmp://127.0.0.1:1935/rebuild".into(),
        whip_url: None,
        srt_url: None,
        hook_secret: "synthetic-hook-secret-only-for-this-test".into(),
        hook_ip: "127.0.0.1".parse().unwrap(),
        vhost: "__defaultVhost__".into(),
        app: "rebuild".into(),
    });
    config.videos.storage = sver::media::Storage::Filesystem(root.join("private"));
    config.videos.segment_base = format!("http://{address}");
    config.media.storage = sver::media::Storage::Filesystem(root.join("public"));
    config.take_down.purge_url = format!("http://{address}/purge");
    config.take_down.purge_token = "synthetic-purge".into();
    let app = App::new(db.clone(), config).await.unwrap();
    let e = synthetic_owner(app, fake).await;
    let mut outage = e.app.clone();
    Arc::make_mut(&mut outage.config).videos.storage = sver::media::Storage::S3(sver::media::S3 {
        endpoint: format!("http://{address}/storage"),
        bucket: "synthetic".into(),
        region: "auto".into(),
        access_key: "synthetic".into(),
        secret_key: "synthetic".into(),
        prefix: String::new(),
    });
    let category: String =
        sqlx::query_scalar("SELECT id FROM stream_categories WHERE active LIMIT 1")
            .fetch_one(&db)
            .await
            .unwrap();
    e.call(
        "PATCH",
        "/api/me/stream",
        json!({"title":"Synthetic recording","category_id":category,"revision":0}),
    )
    .await;
    let key = e.key("create").await;
    assert_eq!(
        e.hook(&key, "record-client", "publish").await,
        StatusCode::OK
    );
    let public = key.split('?').next().unwrap();
    for n in 0..8 {
        let payload = json!({"action":"on_hls","server_id":"test-server","client_id":"record-client","vhost":"__defaultVhost__","app":"rebuild","stream":public,"url":format!("rebuild/{public}-123456-{n}.ts"),"duration":1.0,"seq_no":n});
        if n == 0 {
            assert_eq!(
                e.request(
                    "POST",
                    "/api/internal/srs/segment",
                    payload.clone(),
                    false,
                    true,
                    false
                )
                .await
                .0,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            e.request(
                "POST",
                "/api/internal/srs/segment",
                payload.clone(),
                false,
                false,
                true
            )
            .await
            .0,
            StatusCode::OK
        );
        // Retries are acknowledged without another segment or any repeated source download.
        assert_eq!(
            e.request(
                "POST",
                "/api/internal/srs/segment",
                payload,
                false,
                false,
                true
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let video: String = sqlx::query_scalar("SELECT id FROM videos")
        .fetch_one(&db)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM video_segments")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(count, 8);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT duration_ms FROM videos")
            .fetch_one(&db)
            .await
            .unwrap(),
        8000
    );
    // The 24/7 Plays channel is never captured: the callback is acknowledged, nothing is stored.
    let plays: Option<String> = sqlx::query_scalar("SELECT channel_id FROM plays_runtime")
        .fetch_optional(&db)
        .await
        .unwrap();
    match &plays {
        Some(_) => e.sql("UPDATE plays_runtime SET channel_id='stream-owner'").await,
        None => e.sql("INSERT INTO plays_runtime(channel_id,game,bridge_hash) VALUES('stream-owner','Synthetic game',repeat('0',64))").await,
    }
    let payload = json!({"action":"on_hls","server_id":"test-server","client_id":"record-client","vhost":"__defaultVhost__","app":"rebuild","stream":public,"url":format!("rebuild/{public}-123456-8.ts"),"duration":1.0,"seq_no":8});
    assert_eq!(
        e.request(
            "POST",
            "/api/internal/srs/segment",
            payload,
            false,
            false,
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM video_segments")
            .fetch_one(&db)
            .await
            .unwrap(),
        8,
        "Plays segments are not recorded"
    );
    match plays {
        Some(channel) => {
            sqlx::query("UPDATE plays_runtime SET channel_id=$1")
                .bind(channel)
                .execute(&db)
                .await
                .unwrap();
        }
        None => e.sql("DELETE FROM plays_runtime").await,
    }
    // A process that died with a lease is reclaimed; it does not duplicate the private object.
    e.sql("UPDATE video_jobs SET lease_until=now()-interval '1 second',lease_token='dead-process'")
        .await;
    assert!(sver::videos::worker::run_one(&outage, true).await.is_err());
    let queued: (i64, i64) =
        sqlx::query_as("SELECT count(*),count(payload) FROM video_jobs WHERE kind='SEGMENT'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        queued,
        (8, 8),
        "Storage failure must retain the durable segment spool"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM video_objects WHERE ready")
            .fetch_one(&db)
            .await
            .unwrap(),
        0
    );
    e.sql("UPDATE video_jobs SET available_at=CASE WHEN object_key=(SELECT object_key FROM video_segments WHERE start_ms=0) THEN now()+interval '1 hour' ELSE now() END").await;
    for _ in 0..7 {
        assert!(sver::videos::worker::run_one(&e.app, true).await.unwrap());
    }
    let pending = e
        .call("GET", &format!("/api/videos/{video}"), Value::Null)
        .await;
    assert_eq!(
        bytes(
            &e.app,
            pending["playback"].as_str().unwrap(),
            Some(&e.cookie)
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE,
        "Never start an EVENT playlist after a missing segment and prepend it later"
    );
    e.sql("UPDATE video_jobs SET available_at=now()").await;
    assert!(sver::videos::worker::run_one(&e.app, true).await.unwrap());
    e.sql("UPDATE video_objects SET ready=false WHERE key=(SELECT object_key FROM video_segments WHERE start_ms=7000)").await;
    let incomplete = e.request("POST", &format!("/api/videos/{video}/cuts"), json!({"kind":"HIGHLIGHT","title":"Pending tail","start_ms":0,"end_ms":8000,"request_id":uuid::Uuid::new_v4().to_string()}), true, true, false).await;
    assert_eq!(
        incomplete.0,
        StatusCode::SERVICE_UNAVAILABLE,
        "Never silently shorten a cut while its last segment uploads"
    );
    e.sql("UPDATE video_objects SET ready=true WHERE key=(SELECT object_key FROM video_segments WHERE start_ms=7000)").await;
    let page = e
        .call("GET", &format!("/api/videos/{video}"), Value::Null)
        .await;
    let playlist = page["playback"].as_str().unwrap();
    let (status, body) = bytes(&e.app, playlist, Some(&e.cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let manifest = String::from_utf8(body).unwrap();
    assert!(manifest.contains("#EXT-X-PLAYLIST-TYPE:EVENT"));
    let segment = manifest
        .lines()
        .find(|line| line.starts_with("/api/"))
        .unwrap();
    assert_eq!(
        bytes(&e.app, segment, Some(&e.cookie)).await.0,
        StatusCode::OK
    );
    let guest = e
        .request(
            "GET",
            &format!("/api/videos/{video}"),
            Value::Null,
            false,
            false,
            false,
        )
        .await
        .1;
    let guest_playlist = guest["playback"].as_str().unwrap().to_string();
    e.call(
        "PATCH",
        &format!("/api/videos/{video}"),
        json!({"title":"Private recording","visibility":"PRIVATE"}),
    )
    .await;
    assert_eq!(
        bytes(&e.app, &guest_playlist, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        e.request(
            "GET",
            &format!("/api/videos/{video}"),
            Value::Null,
            false,
            false,
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    e.call(
        "PATCH",
        &format!("/api/videos/{video}"),
        json!({"title":"Public recording","visibility":"PUBLIC"}),
    )
    .await;
    let request = uuid::Uuid::new_v4().to_string();
    let cut =
        json!({"kind":"CLIP","title":"A moment","start_ms":0,"end_ms":6000,"request_id":request});
    let clip = e
        .call("POST", &format!("/api/videos/{video}/cuts"), cut.clone())
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        e.call("POST", &format!("/api/videos/{video}/cuts"), cut)
            .await["id"],
        clip
    );
    let highlight=e.call("POST",&format!("/api/videos/{video}/cuts"),json!({"kind":"HIGHLIGHT","title":"Keep this","start_ms":0,"end_ms":8000,"request_id":uuid::Uuid::new_v4().to_string()})).await["id"].as_str().unwrap().to_string();
    for _ in 0..8 {
        if !sver::videos::worker::run_one(&e.app, false).await.unwrap() {
            break;
        }
    }
    let clip_page = e
        .call("GET", &format!("/api/videos/{clip}"), Value::Null)
        .await;
    assert_eq!(clip_page["video"]["status"], "READY");
    let (status, mp4) = bytes(
        &e.app,
        clip_page["playback"].as_str().unwrap(),
        Some(&e.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&mp4[4..8], b"ftyp");
    std::fs::write(root.join("clip.mp4"), mp4).unwrap();
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
        ])
        .arg(root.join("clip.mp4"))
        .output()
        .unwrap();
    assert!(probe.status.success());
    let info: Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert!(
        info["streams"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["codec_name"] == "h264")
    );
    let duration: f64 = info["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((5.8..6.3).contains(&duration), "{duration}");
    if let Ok(directory) = std::env::var("SVER_VIDEO_PREVIEW_DIR") {
        preview(&e, &directory, &video, &highlight, &clip).await;
        server.abort();
        db.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
        std::fs::remove_dir_all(root).unwrap();
        return;
    }
    library_pages(&e, &highlight).await;
    chapters_caps_and_retention(&e, &video, &highlight).await;
    for n in 0..105 {
        let key = format!("{video}/cleanup-{n}.ts");
        std::fs::write(
            root.join("private").join(&key),
            b"synthetic deletion fixture",
        )
        .unwrap();
        sqlx::query("INSERT INTO video_objects(key,video_id,content_type,ready) VALUES($1,$2,'video/mp2t',true)")
            .bind(&key).bind(&video).execute(&db).await.unwrap();
    }
    sqlx::query("UPDATE videos SET ended_at=now()-interval '2 days',expires_at=now()-interval '1 second',status='READY' WHERE id=$1").bind(&video).execute(&db).await.unwrap();
    sver::videos::worker::maintain(&e.app).await.unwrap();
    for batch in 0..5 {
        if !sver::videos::worker::run_one(&e.app, false).await.unwrap() {
            break;
        }
        if batch == 0 {
            let progress: (String, i64) = sqlx::query_as("SELECT status,(SELECT count(*) FROM video_objects WHERE video_id=$1 AND deleted_at IS NOT NULL) FROM videos WHERE id=$1")
                .bind(&video).fetch_one(&db).await.unwrap();
            assert_eq!(progress, ("DELETING".into(), 100));
            assert!(sver::videos::worker::run_one(&outage, false).await.is_err());
            let progress: (String, i64) = sqlx::query_as("SELECT status,(SELECT count(*) FROM video_objects WHERE video_id=$1 AND deleted_at IS NOT NULL) FROM videos WHERE id=$1")
                .bind(&video).fetch_one(&db).await.unwrap();
            assert_eq!(
                progress,
                ("DELETING".into(), 100),
                "A failed delete cannot expire metadata or undo the previous batch"
            );
            e.sql("UPDATE video_jobs SET available_at=now() WHERE kind='DELETE'")
                .await;
        }
    }
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM videos WHERE id=$1")
            .bind(&video)
            .fetch_one(&db)
            .await
            .unwrap(),
        "EXPIRED"
    );
    let purged = PURGED.lock().unwrap().clone();
    for page in ["videos", "clips", "embed"] {
        let url = format!("{}/{page}/{video}", e.app.config.origin);
        assert!(purged.contains(&url), "{url} purged from the CDN");
    }
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM video_objects WHERE video_id=$1 AND deleted_at IS NULL",
    )
    .bind(&video)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(remaining, 0);
    // A delayed old storage writer cannot leave an untracked object behind.
    let tombstone: String =
        sqlx::query_scalar("SELECT key FROM video_objects WHERE video_id=$1 LIMIT 1")
            .bind(&video)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(!root.join("private").join(&tombstone).exists());
    std::fs::write(
        root.join("private").join(&tombstone),
        b"late synthetic write",
    )
    .unwrap();
    sqlx::query(
        "UPDATE video_objects SET delete_after=now()-interval '1 second' WHERE video_id=$1",
    )
    .bind(&video)
    .execute(&db)
    .await
    .unwrap();
    // More than one cleanup batch of held objects must not starve unrelated expired files.
    sqlx::query(
        "INSERT INTO video_holds(video_id,kind,reference) VALUES($1,'REPORT','synthetic-gc-hold')",
    )
    .bind(&highlight)
    .execute(&db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO video_objects(key,video_id,content_type,delete_after) SELECT $1||'/held-'||n||'.webp',$1,'image/webp',now()-interval '1 day' FROM generate_series(1,101) n")
        .bind(&highlight).execute(&db).await.unwrap();
    for _ in 0..3 {
        sver::videos::worker::maintain(&e.app).await.unwrap();
    }
    assert!(!root.join("private").join(&tombstone).exists());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM video_objects WHERE video_id=$1")
            .bind(&video)
            .fetch_one(&db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM video_objects WHERE video_id=$1 AND key LIKE '%/held-%'"
        )
        .bind(&highlight)
        .fetch_one(&db)
        .await
        .unwrap(),
        101
    );
    sqlx::query("DELETE FROM video_objects WHERE video_id=$1 AND key LIKE '%/held-%'")
        .bind(&highlight)
        .execute(&db)
        .await
        .unwrap();
    e.sql("DELETE FROM video_holds WHERE reference='synthetic-gc-hold'")
        .await;
    let highlight_page = e
        .call("GET", &format!("/api/videos/{highlight}"), Value::Null)
        .await;
    assert_eq!(
        bytes(
            &e.app,
            highlight_page["playback"].as_str().unwrap(),
            Some(&e.cookie)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        bytes(
            &e.app,
            clip_page["playback"].as_str().unwrap(),
            Some(&e.cookie)
        )
        .await
        .0,
        StatusCode::OK
    );
    permissions_and_holds(&e, &highlight, &clip).await;
    server.abort();
    db.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

async fn library_pages(e: &Env, highlight: &str) {
    let mut fixtures = Vec::new();
    for _ in 0..101 {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO videos(id,owner_id,kind,status,visibility,title,started_at) VALUES($1,'stream-owner','CLIP','READY','PUBLIC','Synthetic pagination fixture',now())")
            .bind(&id).execute(&e.app.db).await.unwrap();
        fixtures.push(id);
    }
    let first = e.call("GET", "/api/me/videos?kind=CLIP", Value::Null).await;
    assert_eq!(first["videos"].as_array().unwrap().len(), 100);
    assert_eq!(first["has_more"], true);
    let second = e
        .call("GET", "/api/me/videos?kind=CLIP&offset=100", Value::Null)
        .await;
    assert!(!second["videos"].as_array().unwrap().is_empty());
    let highlights = e
        .call("GET", "/api/me/videos?kind=HIGHLIGHT", Value::Null)
        .await;
    assert!(
        highlights["videos"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == highlight)
    );
    let channel = e
        .call("GET", "/api/channels/Streamer/videos", Value::Null)
        .await;
    assert_eq!(channel["has_more"], true);
    assert!(
        channel["videos"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["video"]["id"] == highlight)
    );
    assert_eq!(
        channel["videos"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["video"]["kind"] == "CLIP")
            .count(),
        20
    );
    sqlx::query("DELETE FROM videos WHERE id=ANY($1)")
        .bind(&fixtures)
        .execute(&e.app.db)
        .await
        .unwrap();
}

/// Optional isolated fixture for browser acceptance. No real accounts, mail delivery or live DB.
async fn preview(e: &Env, directory: &str, video: &str, highlight: &str, clip: &str) {
    let directory = std::path::PathBuf::from(directory).canonicalize().unwrap();
    assert!(!directory.starts_with(std::env::current_dir().unwrap().canonicalize().unwrap()));
    e.sql("INSERT INTO staff_roles(user_id,role) VALUES('stream-owner','admin') ON CONFLICT DO NOTHING").await;
    e.sql("UPDATE videos SET status='READY',ended_at=now(),expires_at=CASE WHEN kind='VOD' THEN now()+interval '24 hours' ELSE NULL END").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = sver::router(e.app.clone());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    std::fs::write(directory.join("preview.json"),serde_json::to_vec_pretty(&json!({"api":format!("http://{address}"),"origin":e.app.config.origin,"cookie":e.cookie,"cookie_name":e.app.config.cookie_name(),"video":video,"highlight":highlight,"clip":clip})).unwrap()).unwrap();
    println!("Synthetic video preview ready.");
    while !directory.join("stop-preview").exists() {
        let _ = sver::videos::worker::run_one(&e.app, false).await;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    server.abort();
}

async fn permissions_and_holds(e: &Env, highlight: &str, clip: &str) {
    use super::chat::{call, person};
    let viewer = person(e, "video-viewer", "VideoViewer", true).await;
    let staff = person(e, "video-staff", "VideoStaff", true).await;
    e.sql("UPDATE users SET mfa_enabled=true,mfa_secret='synthetic' WHERE id='video-staff'")
        .await;
    e.sql("UPDATE sessions SET mfa_verified=true WHERE user_id='video-staff'")
        .await;
    e.sql("INSERT INTO staff_roles(user_id,role) VALUES('video-staff','admin')")
        .await;
    // Short clips count actual advancing playback once; a seek does not earn a view.
    let beat_path = format!("/api/videos/{clip}/beat");
    let browser = uuid::Uuid::new_v4().to_string();
    let beat = json!({"browser_id":browser,"media_time":0.0,"visible":true});
    assert_eq!(
        call(e, "POST", &beat_path, Some(&viewer), beat.clone())
            .await
            .1["level"],
        "pending"
    );
    let mut seek = beat.clone();
    seek["media_time"] = json!(6.0);
    assert_eq!(
        call(e, "POST", &beat_path, Some(&viewer), seek).await.1["level"],
        "pending"
    );
    sqlx::query("UPDATE video_playback SET updated_at=now()-interval '4 seconds',media_time=0 WHERE video_id=$1").bind(clip).execute(&e.app.db).await.unwrap();
    let mut played = beat;
    played["media_time"] = json!(4.0);
    assert_eq!(
        call(e, "POST", &beat_path, Some(&viewer), played.clone())
            .await
            .1["level"],
        "counted"
    );
    call(e, "POST", &beat_path, Some(&viewer), played).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT views FROM videos WHERE id=$1")
            .bind(clip)
            .fetch_one(&e.app.db)
            .await
            .unwrap(),
        1
    );
    // A subscriber ticket is revoked by benefit expiry on its very next segment request.
    e.call(
        "PATCH",
        &format!("/api/videos/{highlight}"),
        json!({"title":"Subscriber Highlight","visibility":"SUBSCRIBERS"}),
    )
    .await;
    assert_eq!(
        call(
            e,
            "GET",
            &format!("/api/videos/{highlight}"),
            Some(&viewer),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    e.sql("INSERT INTO channel_subs(channel_id,user_id,tier,paid_through) VALUES('stream-owner','video-viewer',1,now()+interval '1 day')").await;
    let subscriber = call(
        e,
        "GET",
        &format!("/api/videos/{highlight}"),
        Some(&viewer),
        Value::Null,
    )
    .await
    .1;
    let signed = subscriber["playback"].as_str().unwrap();
    let manifest = String::from_utf8(bytes(&e.app, signed, Some(&viewer)).await.1).unwrap();
    let segment = manifest
        .lines()
        .find(|line| line.starts_with("/api/"))
        .unwrap();
    assert_eq!(
        bytes(&e.app, segment, Some(&viewer)).await.0,
        StatusCode::OK
    );
    e.sql("UPDATE channel_subs SET paid_through=now()-interval '1 second'")
        .await;
    assert_eq!(
        bytes(&e.app, signed, Some(&viewer)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        bytes(&e.app, segment, Some(&viewer)).await.0,
        StatusCode::FORBIDDEN
    );
    e.call(
        "PATCH",
        &format!("/api/videos/{highlight}"),
        json!({"title":"Public Highlight","visibility":"PUBLIC"}),
    )
    .await;
    // An expired signed link is refused before its media can be read.
    let page = e
        .call("GET", &format!("/api/videos/{highlight}"), Value::Null)
        .await;
    let url = url::Url::parse(&format!(
        "http://localhost{}",
        page["playback"].as_str().unwrap()
    ))
    .unwrap();
    let raw = url
        .query_pairs()
        .find(|(k, _)| k == "ticket")
        .unwrap()
        .1
        .into_owned();
    let mut ticket: Value =
        serde_json::from_str(&sec::unseal(&e.app, "video-ticket", &raw).unwrap()).unwrap();
    ticket["until"] = json!(0);
    let expired = sec::seal(&e.app, "video-ticket", &ticket.to_string()).unwrap();
    let expired: String = url::form_urlencoded::byte_serialize(expired.as_bytes()).collect();
    assert_eq!(
        bytes(
            &e.app,
            &format!("/api/videos/{highlight}/playlist?ticket={expired}"),
            Some(&e.cookie)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    e.call(
        "PATCH",
        &format!("/api/videos/{highlight}"),
        json!({"title":"Private Highlight","visibility":"PRIVATE"}),
    )
    .await;
    assert_eq!(
        call(
            e,
            "GET",
            &format!("/api/videos/{highlight}"),
            Some(&viewer),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    e.call(
        "PUT",
        "/api/me/videos/editors",
        json!({"username":"VideoViewer","enabled":true}),
    )
    .await;
    assert_eq!(
        call(
            e,
            "POST",
            &format!("/api/videos/{highlight}/download"),
            Some(&viewer),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    while sver::videos::worker::run_one(&e.app, false).await.unwrap() {}
    let ready = call(
        e,
        "POST",
        &format!("/api/videos/{highlight}/download"),
        Some(&viewer),
        json!({}),
    )
    .await;
    assert_eq!(ready.0, StatusCode::OK);
    let download = ready.1["url"].as_str().unwrap();
    assert_eq!(
        bytes(&e.app, download, Some(&viewer)).await.0,
        StatusCode::OK
    );
    assert_eq!(bytes(&e.app, download, None).await.0, StatusCode::FORBIDDEN);
    e.call(
        "PUT",
        "/api/me/videos/editors",
        json!({"username":"VideoViewer","enabled":false}),
    )
    .await;
    assert_eq!(
        bytes(&e.app, download, Some(&viewer)).await.0,
        StatusCode::FORBIDDEN
    );
    e.call(
        "PATCH",
        &format!("/api/videos/{highlight}"),
        json!({"title":"Public Highlight","visibility":"PUBLIC"}),
    )
    .await;
    // The full permission and approval flow uses the public API.
    let settings = json!({"recording":true,"visibility":"PUBLIC","clip_permission":"MODS","clip_approval":true,"chat_replay":true,"mature":false});
    e.call("PUT", "/api/me/videos", settings.clone()).await;
    let cut = json!({"kind":"CLIP","title":"Viewer clip","start_ms":0,"end_ms":6000,"request_id":uuid::Uuid::new_v4().to_string()});
    assert_eq!(
        call(
            e,
            "POST",
            &format!("/api/videos/{highlight}/cuts"),
            Some(&viewer),
            cut.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut settings = settings;
    settings["clip_permission"] = json!("SIGNED_IN");
    e.call("PUT", "/api/me/videos", settings.clone()).await;
    let made = call(
        e,
        "POST",
        &format!("/api/videos/{highlight}/cuts"),
        Some(&viewer),
        cut,
    )
    .await;
    assert_eq!(made.0, StatusCode::OK);
    let queued = made.1["id"].as_str().unwrap();
    while sver::videos::worker::run_one(&e.app, false).await.unwrap() {}
    assert_eq!(
        call(
            e,
            "GET",
            &format!("/api/videos/{queued}"),
            None,
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            e,
            "GET",
            &format!("/api/videos/{queued}"),
            Some(&viewer),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    e.call(
        "POST",
        &format!("/api/videos/{queued}/approval"),
        json!({"approve":true,"reason":"Suitable clip"}),
    )
    .await;
    assert_eq!(
        call(
            e,
            "GET",
            &format!("/api/videos/{queued}/share"),
            None,
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    let embed = format!("/api/oembed?url={}/clips/{queued}", e.app.config.origin);
    assert_eq!(
        call(e, "GET", &embed, None, Value::Null).await.1["type"],
        "video"
    );
    // A report revokes existing links and prevents owner deletion until review closes.
    let guest = call(e, "GET", &format!("/api/videos/{clip}"), None, Value::Null)
        .await
        .1;
    assert_eq!(call(e,"POST","/api/reports",Some(&viewer),json!({"target_type":"clip","target_id":clip,"reason":"harassment","note":"Synthetic review"})).await.0,StatusCode::OK);
    assert_ne!(
        bytes(&e.app, guest["playback"].as_str().unwrap(), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            e,
            "DELETE",
            &format!("/api/videos/{clip}"),
            Some(&e.cookie),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let evidence = call(
        e,
        "GET",
        &format!("/api/admin/videos/{clip}/review"),
        Some(&staff),
        Value::Null,
    )
    .await;
    assert_eq!(evidence.0, StatusCode::OK);
    assert_eq!(
        bytes(
            &e.app,
            evidence.1["playback"].as_str().unwrap(),
            Some(&staff)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        bytes(
            &e.app,
            evidence.1["playback"].as_str().unwrap(),
            Some(&viewer)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            e,
            "POST",
            &format!("/api/admin/reports/clip/{clip}/actions"),
            Some(&staff),
            json!({"action":"dismiss","note":"No violation"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(e, "GET", &format!("/api/videos/{clip}"), None, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    // Copies keep chat beyond original expiry, but deletions never come back.
    let chat = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO chat_messages(id,channel_id,author_id,body) VALUES($1,'stream-owner','video-viewer','replay body')").bind(&chat).execute(&e.app.db).await.unwrap();
    sqlx::query("INSERT INTO video_chat(video_id,message_id,author_id,offset_ms,message) VALUES($1,$2,'video-viewer',1000,$3)").bind(clip).bind(&chat).bind(json!({"id":chat,"body":"replay body"})).execute(&e.app.db).await.unwrap();
    let replay = format!("/api/videos/{clip}/chat?at_ms=3000");
    assert!(
        call(e, "GET", &replay, None, Value::Null)
            .await
            .1
            .to_string()
            .contains("replay body")
    );
    sqlx::query("UPDATE chat_messages SET deleted_at=now(),expires_at=now()-interval '1 second' WHERE id=$1").bind(&chat).execute(&e.app.db).await.unwrap();
    assert!(
        !call(e, "GET", &replay, None, Value::Null)
            .await
            .1
            .to_string()
            .contains("replay body")
    );
    sver::chat::expire(&e.app).await.unwrap();
    assert!(
        !call(e, "GET", &replay, None, Value::Null)
            .await
            .1
            .to_string()
            .contains("replay body")
    );
    settings["chat_replay"] = json!(false);
    e.call("PUT", "/api/me/videos", settings).await;
    assert_eq!(
        call(e, "GET", &replay, None, Value::Null).await.1["enabled"],
        false
    );
    copyright_flow(e, highlight, &staff).await;
    // Legal removal follows the Highlight's descendant clip and waits for file deletion.
    let mut tx = e.app.db.begin().await.unwrap();
    sver::videos::review::hold(&mut tx, highlight, "TAKE_DOWN", "synthetic-case", true)
        .await
        .unwrap();
    sver::videos::review::remove(&mut tx, highlight, true, true)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    for _ in 0..10 {
        if !sver::videos::worker::run_one(&e.app, false).await.unwrap() {
            break;
        }
    }
    assert!(
        sver::videos::review::removed(&mut e.app.db.acquire().await.unwrap(), highlight)
            .await
            .unwrap()
    );
    assert_eq!(
        call(
            e,
            "GET",
            &format!("/api/videos/{queued}"),
            Some(&viewer),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

async fn chapters_caps_and_retention(e: &Env, video: &str, highlight: &str) {
    let chat_marker = uuid::Uuid::new_v4().to_string();
    e.call(
        "POST",
        "/api/channels/Streamer/marker",
        json!({"label":"Dashboard moment"}),
    )
    .await;
    e.call(
        "POST",
        "/api/channels/Streamer/chat",
        json!({"id":chat_marker,"body":"!marker Chat moment"}),
    )
    .await;
    e.call(
        "POST",
        "/api/channels/Streamer/chat",
        json!({"id":chat_marker,"body":"!marker Chat moment"}),
    )
    .await;
    let mut tx = e.app.db.begin().await.unwrap();
    sver::videos::spotlight(&mut tx, "stream-owner", "synthetic-spotlight")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let page = e
        .call("GET", &format!("/api/videos/{video}"), Value::Null)
        .await;
    let chapters = page["chapters"].as_array().unwrap();
    assert_eq!(
        chapters
            .iter()
            .filter(|c| c["label"] == "Chat moment")
            .count(),
        1
    );
    assert!(chapters.iter().any(|c| c["source"] == "MAGNET"));
    assert!(
        chapters
            .iter()
            .any(|c| c["source"] == "CATEGORY" && c["offset_ms"] == 0)
    );
    let marker = chapters
        .iter()
        .find(|c| c["label"] == "Dashboard moment")
        .unwrap()["id"]
        .clone();
    e.call(
        "PUT",
        &format!("/api/videos/{video}/chapters"),
        json!({"id":marker,"label":"Renamed moment"}),
    )
    .await;
    let suggestion: (String, String) = sqlx::query_as(
        "SELECT id,approval FROM videos WHERE request_key='magnet:synthetic-spotlight'",
    )
    .fetch_one(&e.app.db)
    .await
    .unwrap();
    assert_eq!(suggestion.1, "PENDING");
    while sver::videos::worker::run_one(&e.app, false).await.unwrap() {}
    // Count the approved cap before enqueueing any new media work.
    sqlx::query("UPDATE videos SET duration_ms=36000000 WHERE id=$1")
        .bind(highlight)
        .execute(&e.app.db)
        .await
        .unwrap();
    let full=e.request("POST",&format!("/api/videos/{video}/cuts"),json!({"kind":"HIGHLIGHT","title":"Over quota","start_ms":0,"end_ms":8000,"request_id":uuid::Uuid::new_v4().to_string()}),true,true,false).await;
    assert_eq!(full.0, StatusCode::BAD_REQUEST);
    assert!(full.1.to_string().contains("storage is full"));
    sqlx::query("UPDATE videos SET duration_ms=8000 WHERE id=$1")
        .bind(highlight)
        .execute(&e.app.db)
        .await
        .unwrap();
    for (tier, hours) in [24, 48, 72, 168].into_iter().enumerate() {
        sqlx::query(
            "UPDATE videos SET retention_hours=$2,ended_at=NULL,expires_at=NULL WHERE id=$1",
        )
        .bind(video)
        .bind(hours)
        .execute(&e.app.db)
        .await
        .unwrap();
        sver::videos::ended(&mut e.app.db.acquire().await.unwrap(), "stream-owner")
            .await
            .unwrap();
        let span: f64 = sqlx::query_scalar(
            "SELECT extract(epoch FROM expires_at-ended_at)::float8/3600 FROM videos WHERE id=$1",
        )
        .bind(video)
        .fetch_one(&e.app.db)
        .await
        .unwrap();
        assert!((span - f64::from(hours)).abs() < 0.001, "tier {tier}");
    }
    sqlx::query("UPDATE videos SET retention_hours=24,ended_at=now(),expires_at=now()+interval '24 hours' WHERE id=$1").bind(video).execute(&e.app.db).await.unwrap();
    sver::videos::extend_retention(&mut e.app.db.acquire().await.unwrap(), "stream-owner", 2)
        .await
        .unwrap();
    let hours: i32 = sqlx::query_scalar("SELECT retention_hours FROM videos WHERE id=$1")
        .bind(video)
        .fetch_one(&e.app.db)
        .await
        .unwrap();
    assert_eq!(hours, 72);
    sqlx::query("UPDATE videos SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(video)
        .execute(&e.app.db)
        .await
        .unwrap();
    sver::videos::extend_retention(&mut e.app.db.acquire().await.unwrap(), "stream-owner", 3)
        .await
        .unwrap();
    let hours: i32 = sqlx::query_scalar("SELECT retention_hours FROM videos WHERE id=$1")
        .bind(video)
        .fetch_one(&e.app.db)
        .await
        .unwrap();
    assert_eq!(hours, 72, "An upgrade never resurrects an expired VOD");
    // A recording-off stream keeps only its real clipping window.
    sqlx::query("UPDATE videos SET recording=false,duration_ms=128000,ended_at=NULL,expires_at=NULL WHERE id=$1").bind(video).execute(&e.app.db).await.unwrap();
    sqlx::query(
        "UPDATE video_segments SET start_ms=start_ms+120000 WHERE video_id=$1 AND start_ms>=2000",
    )
    .bind(video)
    .execute(&e.app.db)
    .await
    .unwrap();
    let window = e
        .call("GET", &format!("/api/videos/{video}"), Value::Null)
        .await;
    assert_eq!(
        bytes(
            &e.app,
            window["playback"].as_str().unwrap(),
            Some(&e.cookie)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let cut=e.call("POST",&format!("/api/videos/{video}/cuts"),json!({"kind":"CLIP","title":"Recording-off moment","start_ms":122000,"end_ms":128000,"request_id":uuid::Uuid::new_v4().to_string()})).await;
    // A storage outage can leave an old segment queued after it leaves the window.
    // Keep its ownership until the queued upload completes, then collect it.
    let pending: String = sqlx::query_scalar(
        "SELECT object_key FROM video_segments WHERE video_id=$1 AND start_ms<8000 LIMIT 1",
    )
    .bind(video)
    .fetch_one(&e.app.db)
    .await
    .unwrap();
    let payload = e
        .app
        .config
        .videos
        .storage
        .get(&e.app.http, &pending)
        .await
        .unwrap()
        .unwrap();
    sqlx::query(
        "INSERT INTO video_jobs(id,video_id,kind,object_key,payload) VALUES($1,$2,'SEGMENT',$3,$4)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(video)
    .bind(&pending)
    .bind(payload)
    .execute(&e.app.db)
    .await
    .unwrap();
    // A live broadcast always has a new segment upload in flight; it must not block trimming.
    let in_flight = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO video_jobs(id,video_id,kind,object_key,lease_until,lease_token) VALUES($1,$2,'SEGMENT','in-flight/new-segment.ts',now()+interval '1 minute','other-worker')")
        .bind(&in_flight).bind(video).execute(&e.app.db).await.unwrap();
    sver::videos::worker::maintain(&e.app).await.unwrap();
    sqlx::query("DELETE FROM video_jobs WHERE id=$1")
        .bind(&in_flight)
        .execute(&e.app.db)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM video_segments WHERE video_id=$1 AND start_ms<8000"
        )
        .bind(video)
        .fetch_one(&e.app.db)
        .await
        .unwrap(),
        1
    );
    assert!(sver::videos::worker::run_one(&e.app, true).await.unwrap());
    sver::videos::worker::maintain(&e.app).await.unwrap();
    let old: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM video_segments WHERE video_id=$1 AND start_ms<8000",
    )
    .bind(video)
    .fetch_one(&e.app.db)
    .await
    .unwrap();
    assert_eq!(old, 0, "Only the real rolling window remains");
    while sver::videos::worker::run_one(&e.app, false).await.unwrap() {}
    let clip = e
        .call(
            "GET",
            &format!("/api/videos/{}", cut["id"].as_str().unwrap()),
            Value::Null,
        )
        .await;
    assert_eq!(clip["video"]["status"], "READY");
    assert_eq!(
        bytes(&e.app, clip["playback"].as_str().unwrap(), Some(&e.cookie))
            .await
            .0,
        StatusCode::OK
    );
}

async fn copyright_flow(e: &Env, video: &str, staff: &str) {
    use super::chat::call;
    let notice = json!({"name":"Synthetic Rights Holder","email":"claimant@example.test","address":"123 Test Street, Example, NC 00000","phone":"555-0100","signature":"Synthetic Rights Holder","location":format!("{}/videos/{video}",e.app.config.origin),"description":"Synthetic work and ownership statement for the acceptance test.","good_faith":true,"perjury":true,"jurisdiction":true,"turnstile":"pass"});
    let (status, submitted) = call(e, "POST", "/api/copyright", None, notice.clone()).await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    let id = submitted["id"].as_str().unwrap();
    let sealed: String = sqlx::query_scalar("SELECT notice FROM copyright_cases WHERE id=$1")
        .bind(id)
        .fetch_one(&e.app.db)
        .await
        .unwrap();
    assert!(!sealed.contains("Synthetic Rights Holder"));
    assert_eq!(
        call(
            e,
            "POST",
            &format!("/api/admin/copyright/{id}"),
            Some(staff),
            json!({"action":"remove","reason":"Valid test notice"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(e, "GET", &format!("/api/videos/{video}"), None, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            e,
            "POST",
            &format!("/api/me/copyright/{id}/counter"),
            Some(&e.cookie),
            notice.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            e,
            "POST",
            &format!("/api/admin/copyright/{id}"),
            Some(staff),
            json!({"action":"accept_counter","reason":"Valid test counter-notice"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (after, by, mail): (
        chrono::DateTime<chrono::Utc>,
        chrono::DateTime<chrono::Utc>,
        String,
    ) = sqlx::query_as(
        "SELECT restore_after,restore_by,forward_mail_id FROM copyright_cases WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&e.app.db)
    .await
    .unwrap();
    assert!(after > chrono::Utc::now() + chrono::Duration::days(10));
    assert!(by > after);
    sqlx::query("UPDATE copyright_cases SET restore_after=now()-interval '1 second' WHERE id=$1")
        .bind(id)
        .execute(&e.app.db)
        .await
        .unwrap();
    sver::videos::copyright::tick(&e.app).await.unwrap();
    assert_eq!(
        call(e, "GET", &format!("/api/videos/{video}"), None, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND,
        "No restoration until the counter-notice was forwarded"
    );
    sver::videos::copyright::mail_result(&mut e.app.db.acquire().await.unwrap(), &mail, "accepted")
        .await
        .unwrap();
    sver::videos::copyright::tick(&e.app).await.unwrap();
    assert_eq!(
        call(e, "GET", &format!("/api/videos/{video}"), None, Value::Null)
            .await
            .0,
        StatusCode::OK
    );

    // An uploaded Beacon gets its own case: upheld, it leaves the feeds; a forwarded counter-notice
    // brings it back. A Beacon made from a clip names the clip's case instead.
    let beacon = uuid::Uuid::new_v4().to_string();
    let from_clip = uuid::Uuid::new_v4().to_string();
    for (id, source, clip) in [(&beacon, "UPLOAD", None), (&from_clip, "CLIP", Some(video))] {
        sqlx::query("INSERT INTO beacons(id,owner_id,source,status,title,seed,request_key,clip_id,published_at,duration_ms) VALUES($1,'stream-owner',$2,'PUBLISHED','Synthetic',1,$1,$3,now(),6000)")
            .bind(id).bind(source).bind(clip).execute(&e.app.db).await.unwrap();
    }
    let file = |target: &str| {
        let mut n = notice.clone();
        n["location"] = json!(format!("{}/beacons/{target}", e.app.config.origin));
        n
    };
    let (_, clip_case) = call(e, "POST", "/api/copyright", None, file(&from_clip)).await;
    let named: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT video_id,beacon_id FROM copyright_cases WHERE id=$1")
            .bind(clip_case["id"].as_str().unwrap())
            .fetch_one(&e.app.db)
            .await
            .unwrap();
    assert_eq!(
        named,
        (Some(video.to_string()), None),
        "a clip Beacon's notice names its clip"
    );
    let (status, filed) = call(e, "POST", "/api/copyright", None, file(&beacon)).await;
    assert_eq!(status, StatusCode::OK, "{filed}");
    let case = filed["id"].as_str().unwrap();
    let hidden = || async {
        sqlx::query_scalar::<_, bool>("SELECT hidden FROM beacons WHERE id=$1")
            .bind(&beacon)
            .fetch_one(&e.app.db)
            .await
            .unwrap()
    };
    let decide = |action: &'static str| async move {
        call(
            e,
            "POST",
            &format!("/api/admin/copyright/{case}"),
            Some(staff),
            json!({"action": action, "reason": "Synthetic Beacon decision"}),
        )
        .await
        .0
    };
    assert!(!hidden().await);
    assert_eq!(decide("remove").await, StatusCode::OK);
    assert!(
        hidden().await,
        "an upheld notice takes the Beacon out of the feeds"
    );
    // The counter-notice has to name the removed Beacon, not something else.
    let wrong = call(
        e,
        "POST",
        &format!("/api/me/copyright/{case}/counter"),
        Some(&e.cookie),
        notice.clone(),
    )
    .await
    .0;
    assert_eq!(wrong, StatusCode::BAD_REQUEST);
    let counter = call(
        e,
        "POST",
        &format!("/api/me/copyright/{case}/counter"),
        Some(&e.cookie),
        file(&beacon),
    )
    .await
    .0;
    assert_eq!(counter, StatusCode::OK);
    assert_eq!(decide("accept_counter").await, StatusCode::OK);
    let mail: String =
        sqlx::query_scalar("SELECT forward_mail_id FROM copyright_cases WHERE id=$1")
            .bind(case)
            .fetch_one(&e.app.db)
            .await
            .unwrap();
    sqlx::query("UPDATE copyright_cases SET restore_after=now()-interval '1 second' WHERE id=$1")
        .bind(case)
        .execute(&e.app.db)
        .await
        .unwrap();
    sver::videos::copyright::mail_result(&mut e.app.db.acquire().await.unwrap(), &mail, "accepted")
        .await
        .unwrap();
    sver::videos::copyright::tick(&e.app).await.unwrap();
    assert!(!hidden().await, "restored to how it was before the removal");
    // Leave the clip Beacon's case closed so it can't count as a strike below, and the synthetic
    // Beacons gone so the Highlight's later legal removal isn't waiting on them.
    sqlx::query("UPDATE copyright_cases SET status='REJECTED' WHERE id=$1")
        .bind(clip_case["id"].as_str().unwrap())
        .execute(&e.app.db)
        .await
        .unwrap();
    sqlx::query("UPDATE beacons SET status='DELETED' WHERE id=ANY($1)")
        .bind([&beacon, &from_clip])
        .execute(&e.app.db)
        .await
        .unwrap();

    // Repeat infringers: an upheld notice is a strike for 12 months unless a counter-notice
    // restored it (the case above); the third active strike restricts the channel.
    let owner: String = sqlx::query_scalar("SELECT owner_id FROM copyright_cases WHERE id=$1")
        .bind(id)
        .fetch_one(&e.app.db)
        .await
        .unwrap();
    for (case, status, removed) in [
        ("cr-active", "REMOVED", Some("30 days")),
        ("cr-expired", "REMOVED", Some("13 months")),
        ("cr-open-1", "OPEN", None),
        ("cr-open-2", "OPEN", None),
    ] {
        sqlx::query("INSERT INTO copyright_cases(id,video_id,owner_id,status,notice,contact_hash,removed_at) SELECT $1,video_id,owner_id,$2,notice,contact_hash,now()-$3::interval FROM copyright_cases WHERE id=$4")
            .bind(case).bind(status).bind(removed).bind(id).execute(&e.app.db).await.unwrap();
    }
    let uphold = |case: &'static str| async move {
        call(
            e,
            "POST",
            &format!("/api/admin/copyright/{case}"),
            Some(staff),
            json!({"action":"remove","reason":"Valid test notice"}),
        )
        .await
        .0
    };
    let copyright_strikes = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM strikes WHERE user_id=$1 AND reason='copyright' AND severity='SEVERE' AND status='ACTIVE'")
            .bind(&owner).fetch_one(&e.app.db).await.unwrap()
    };
    assert_eq!(uphold("cr-open-1").await, StatusCode::OK);
    assert_eq!(
        copyright_strikes().await,
        0,
        "two active strikes: the restored and the expired case don't count"
    );
    assert_eq!(uphold("cr-open-2").await, StatusCode::OK);
    assert_eq!(copyright_strikes().await, 1, "the third active strike");
    let restricted: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT restricted_until FROM profiles WHERE user_id=$1")
            .bind(&owner)
            .fetch_one(&e.app.db)
            .await
            .unwrap();
    assert!(restricted.is_some_and(|until| until > chrono::Utc::now()));
    // Later steps reuse this channel and recording: lift the synthetic strike and holds.
    sqlx::query("UPDATE strikes SET status='OVERTURNED' WHERE user_id=$1 AND reason='copyright'")
        .bind(&owner)
        .execute(&e.app.db)
        .await
        .unwrap();
    let mut tx = e.app.db.begin().await.unwrap();
    sver::safety::recompute(&mut tx, &owner).await.unwrap();
    sver::videos::review::release(
        &mut tx,
        "COPYRIGHT",
        &["cr-open-1".to_string(), "cr-open-2".to_string()],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("DELETE FROM copyright_cases WHERE id LIKE 'cr-%'")
        .execute(&e.app.db)
        .await
        .unwrap();
}
