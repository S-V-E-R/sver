#[tokio::main]
async fn main() -> Result<(), String> {
    let config = sver::Config::from_env()?;
    let db = sver::connect(&std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is required")?)
        .await?;
    let app = sver::App::new(db, config)
        .await
        .map_err(|_| "Could not initialize Login")?;
    if std::env::args().nth(1).as_deref() == Some("preview-mail") {
        if app.config.production {
            return Err("Mail preview is only available in local development".into());
        }
        let directory = std::path::PathBuf::from(
            std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .map_err(|_| "User directory unavailable")?,
        )
        .join("SVER-dev");
        std::fs::create_dir_all(&directory)
            .map_err(|_| "Could not create the external mail preview directory")?;
        if directory
            .canonicalize()
            .map_err(|_| "Invalid preview directory")?
            .starts_with(
                std::env::current_dir()
                    .and_then(|path| path.canonicalize())
                    .map_err(|_| "Current directory unavailable")?,
            )
        {
            return Err("Mail preview must stay outside the workspace".into());
        }
        let rows: Vec<String> = sqlx::query_scalar("SELECT payload FROM mail_jobs WHERE expires_at>now() ORDER BY created_at DESC LIMIT 20").fetch_all(&app.db).await.map_err(|_| "Could not read local mail")?;
        let mut messages = Vec::new();
        for row in rows {
            let value: serde_json::Value = serde_json::from_str(
                &sver::security::unseal(&app, "mail", &row)
                    .map_err(|_| "Could not decrypt local mail")?,
            )
            .map_err(|_| "Invalid local mail")?;
            messages.push(format!(
                "To: {}\n{}",
                value["to"][0].as_str().unwrap_or(""),
                value["text"].as_str().unwrap_or("")
            ));
        }
        let path = directory.join("mail-preview.txt");
        std::fs::write(&path, messages.join("\n\n---\n\n"))
            .map_err(|_| "Could not write local mail preview")?;
        println!(
            "Local mail preview saved outside the workspace: {}",
            path.display()
        );
        return Ok(());
    }
    // Catalog networking cannot delay ingest, playback, mail or startup.
    if std::env::var("GAME_CATALOG_SYNC").as_deref() == Ok("1") {
        let catalog = app.clone();
        tokio::spawn(async move {
            loop {
                if sver::streams::catalog::tick(&catalog).await.is_err() {
                    eprintln!("catalog_event=refresh outcome=retry");
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        });
    }
    // The network file downloads on its own loop; integrity reads whatever copy is already loaded.
    let networks = app.clone();
    tokio::spawn(async move {
        loop {
            if sver::ipinfo::refresh(&networks).await.is_err() {
                eprintln!("ipinfo_event=refresh outcome=retry");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });
    // Restreaming relays (docs/LINKED_CHAT.md); off unless RESTREAM_SOURCE is set.
    if let Some(source) = sver::restream::source() {
        let relays = app.clone();
        tokio::spawn(async move {
            loop {
                if sver::restream::tick(&relays, &source).await.is_err() {
                    eprintln!("restream_event=supervise outcome=retry");
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        });
    }
    // Live events (docs/DEVELOPER_PLATFORM.md §2): the outbox goes out every second.
    let events = app.clone();
    tokio::spawn(async move {
        loop {
            if sver::events::drain(&events).await.is_err() {
                eprintln!("events_event=drain outcome=retry");
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
    let jobs = app.clone();
    tokio::spawn(async move {
        loop {
            if sver::jobs::tick(&jobs).await.is_err() {
                eprintln!("Login maintenance will retry.");
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
    // Board webhooks have their own loop, so a slow endpoint never holds up stream maintenance.
    let outbox = app.clone();
    tokio::spawn(async move {
        loop {
            if sver::boards::deliver_due(&outbox).await.is_err() {
                eprintln!("boards_event=outbox outcome=retry");
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
    // Third-party emote lists and images come from outside services; never hold up other work.
    let outside = app.clone();
    tokio::spawn(async move {
        loop {
            if sver::outside_emotes::sync(&outside).await.is_err() {
                eprintln!("outside_emotes_event=sync outcome=retry");
            }
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    });
    // Raven's Eye keeps each finished day's platform numbers (docs/ADMIN.md).
    let analytics = app.clone();
    tokio::spawn(async move {
        loop {
            if sver::ravens_eye::tick(&analytics).await.is_err() {
                eprintln!("ravens_eye_event=rollup outcome=retry");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });
    // The Discord bot (go-live posts, role sync) waits on Discord; keep it off the shared loops.
    let discord = app.clone();
    tokio::spawn(async move {
        loop {
            if sver::discord::tick(&discord).await.is_err() {
                eprintln!("discord_event=tick outcome=retry");
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
    // Event webhooks too: a slow developer endpoint never delays a game's board webhook.
    let hooks = app.clone();
    tokio::spawn(async move {
        loop {
            if sver::events::deliver_hooks(&hooks).await.is_err() {
                eprintln!("events_event=hooks outcome=retry");
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
    let media_jobs = app.clone();
    if app.config.videos.storage.available() {
        // Segment archival cannot wait behind a long MP4 download or retention sweep.
        for segments in [true, false] {
            let videos = app.clone();
            tokio::spawn(async move {
                loop {
                    match sver::videos::worker::run_one(&videos, segments).await {
                        Ok(true) => {}
                        _ => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
                    }
                }
            });
        }
        let videos = app.clone();
        tokio::spawn(async move {
            loop {
                if sver::videos::worker::maintain(&videos).await.is_err() {
                    eprintln!("video_event=maintenance outcome=retry");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }
    if app.config.videos.storage.available() {
        // Beacons share the private recording store; one worker keeps re-encodes one at a time.
        let beacons = app.clone();
        tokio::spawn(async move {
            loop {
                match sver::beacons::worker::run_one(&beacons).await {
                    Ok(true) => {}
                    _ => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
                }
            }
        });
        let beacons = app.clone();
        tokio::spawn(async move {
            loop {
                if sver::beacons::worker::maintain(&beacons).await.is_err() {
                    eprintln!("beacon_event=maintenance outcome=retry");
                }
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            }
        });
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if sver::streams::tick(&media_jobs).await.is_err() {
                eprintln!("Stream maintenance will retry.");
            }
            if sver::bot::drain(&media_jobs).await.is_err() {
                eprintln!("bot_event=drain outcome=retry");
            }
            if sver::linked_chat::tick(&media_jobs).await.is_err() {
                eprintln!("linked_chat_event=supervise outcome=retry");
            }
            if sver::playback::tick(&media_jobs).await.is_err() {
                eprintln!("playback_event=delivery outcome=retry");
            }
            if sver::raids::tick(&media_jobs).await.is_err() {
                eprintln!("raids_event=maintenance outcome=retry");
            }
            if sver::probe::tick(&media_jobs).await.is_err() {
                eprintln!("probe_event=measure outcome=retry");
            }
            if sver::probe::sweep_thumbnails(&media_jobs).await.is_err() {
                eprintln!("probe_event=thumbnail_sweep outcome=retry");
            }
            if sver::magnet::tick(&media_jobs).await.is_err() {
                eprintln!("magnet_event=tick outcome=retry");
            }
            if sver::engagement::tick(&media_jobs).await.is_err() {
                eprintln!("engagement_event=watch outcome=retry");
            }
            if sver::crowd::tick(&media_jobs).await.is_err() {
                eprintln!("crowd_event=polls outcome=retry");
            }
            if sver::surge::tick(&media_jobs).await.is_err() {
                eprintln!("surge_event=tick outcome=retry");
            }
            if sver::tiers::tick(&media_jobs).await.is_err() {
                eprintln!("tiers_event=tick outcome=retry");
            }
            if sver::shine::tick(&media_jobs).await.is_err() {
                eprintln!("shine_event=tick outcome=retry");
            }
            if sver::payouts::tick(&media_jobs).await.is_err() {
                eprintln!("payout_event=tick outcome=retry");
            }
        }
    });
    let bind = std::env::var("BIND_ADDRESS").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|_| "Could not bind Login listener")?;
    println!("SVER Login listening on {bind}");
    axum::serve(
        listener,
        sver::router(app).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|_| "Login server stopped unexpectedly".to_string())
}
