//! Encrypted notices and an audited, staff-reviewed counter-notice process.
use super::*;
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, State},
    http::HeaderMap,
    routing::{get, post},
};
use chrono::{Datelike, Days, NaiveDate, TimeZone, Weekday};
use std::net::SocketAddr;

#[derive(Serialize, Deserialize)]
struct Notice {
    name: String,
    email: String,
    address: String,
    phone: String,
    signature: String,
    location: String,
    description: String,
    good_faith: bool,
    perjury: bool,
    #[serde(default)]
    jurisdiction: bool,
    #[serde(default, skip_serializing)]
    turnstile: String,
}
impl Notice {
    fn validate(&mut self, counter: bool) -> Res<()> {
        self.email = security::email(&self.email)?;
        self.name = crate::text::plain(&self.name, "name", 1, 150, 0, false)?;
        self.address = crate::text::plain(&self.address, "address", 1, 500, 5, false)?;
        self.phone = crate::text::plain(&self.phone, "phone", 1, 80, 0, false)?;
        self.signature = crate::text::plain(&self.signature, "signature", 1, 150, 0, false)?;
        self.description =
            crate::text::plain(&self.description, "description", 1, 6000, 40, false)?;
        if !self.good_faith || !self.perjury || (counter && !self.jurisdiction) {
            return Err(Fail::bad("Confirm every required legal statement."));
        }
        Ok(())
    }
}
/// What a notice names: a video or clip, or an uploaded Beacon. A Beacon made from a clip is
/// covered by its clip's case (holding the clip hides its Beacons), so its link names the clip.
#[derive(PartialEq)]
enum Target {
    Video(String),
    Beacon(String),
}
async fn target(app: &App, db: &mut PgConnection, location: &str) -> Res<(Target, Option<String>)> {
    let wrong = || Fail::bad("Provide a S.V.E.R video, clip or Beacon link.");
    let url = url::Url::parse(location).map_err(|_| wrong())?;
    let origin = url::Url::parse(&app.config.origin).map_err(|_| Fail::internal())?;
    if url.origin() != origin.origin() || !url.username().is_empty() || url.password().is_some() {
        return Err(wrong());
    }
    let path: Vec<_> = url.path().trim_matches('/').split('/').collect();
    if path.len() != 2 || uuid::Uuid::parse_str(path[1]).is_err() {
        return Err(wrong());
    }
    let video = match path[0] {
        "videos" | "clips" => path[1].to_string(),
        "beacons" => {
            let beacon = crate::beacons::load(db, path[1]).await?;
            match beacon.clip_id.filter(|_| beacon.source == "CLIP") {
                Some(clip) => clip,
                None => return Ok((Target::Beacon(beacon.id), beacon.owner_id)),
            }
        }
        _ => return Err(wrong()),
    };
    let source = load(db, &video).await?;
    Ok((Target::Video(video), source.owner_id))
}
async fn alert(app: &App, db: &mut PgConnection) -> Res<()> {
    for staff in crate::safety::take_down::staff_ids(db).await? {
        crate::safety::queue_notice(
            app,
            db,
            &staff,
            "Copyright review needed",
            "A copyright case needs review. Open S.V.E.R Admin → Copyright.",
        )
        .await?;
    }
    Ok(())
}
async fn submit(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(mut input): Json<Notice>,
) -> Res<Json<Value>> {
    input.validate(false)?;
    let ip = security::client_ip(&app, peer, &headers);
    security::reserve(
        &app,
        vec![
            format!("copyright-ip:{ip}"),
            format!("copyright-mail:{}", security::digest(&input.email)),
        ],
        5,
        3600,
    )
    .await?;
    security::turnstile(&app, &input.turnstile, "copyright", ip).await?;
    let mut tx = app.db.begin().await?;
    let (target, owner) = target(&app, &mut tx, &input.location).await?;
    let (video, beacon) = match target {
        Target::Video(v) => (Some(v), None),
        Target::Beacon(b) => (None, Some(b)),
    };
    let id = profiles::new_id();
    let sealed = security::seal(
        &app,
        "copyright-notice",
        &serde_json::to_string(&input).map_err(|_| Fail::internal())?,
    )?;
    sqlx::query("INSERT INTO copyright_cases(id,video_id,beacon_id,owner_id,notice,contact_hash) VALUES($1,$2,$3,$4,$5,$6)").bind(&id).bind(video).bind(beacon).bind(owner).bind(sealed).bind(security::digest(&input.email)).execute(&mut *tx).await?;
    crate::jobs::queue_address(&app,&mut tx,None,&input.email,"Copyright notice received",&format!("Your S.V.E.R copyright case is {id}. We will review it promptly. Reply to dmca@sver.tv with this reference if you need to add information.")).await?;
    alert(&app, &mut tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"id":id,"message":"Your copyright notice was received."}),
    ))
}
#[derive(sqlx::FromRow, Serialize)]
struct Case {
    id: String,
    video_id: Option<String>,
    beacon_id: Option<String>,
    #[serde(skip_serializing)]
    beacon_restore: Option<Value>,
    owner_id: Option<String>,
    status: String,
    #[serde(skip_serializing)]
    notice: String,
    #[serde(skip_serializing)]
    counter_notice: Option<String>,
    created_at: DateTime<Utc>,
    counter_received_at: Option<DateTime<Utc>>,
    restore_after: Option<DateTime<Utc>>,
    restore_by: Option<DateTime<Utc>>,
    reason: Option<String>,
    forward_state: Option<String>,
}
impl Case {
    fn target(&self) -> Target {
        match (&self.video_id, &self.beacon_id) {
            (Some(v), _) => Target::Video(v.clone()),
            (None, b) => Target::Beacon(b.clone().unwrap_or_default()),
        }
    }
}
async fn cases(State(app): State<App>, jar: CookieJar) -> Res<Json<Value>> {
    let user = profiles::signed_in(&app, &jar).await?;
    let rows: Vec<Case> = sqlx::query_as(
        "SELECT * FROM copyright_cases WHERE owner_id=$1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(&user.id)
    .fetch_all(&app.db)
    .await?;
    let mut cases = Vec::new();
    for row in rows {
        let mut value = serde_json::to_value(&row).map_err(|_| Fail::internal())?;
        if row.status != "OPEN" && row.status != "REJECTED" {
            value["notice"] =
                serde_json::from_str(&security::unseal(&app, "copyright-notice", &row.notice)?)
                    .map_err(|_| Fail::internal())?;
        }
        cases.push(value);
    }
    Ok(Json(json!({"cases":cases})))
}
async fn counter(
    State(app): State<App>,
    jar: CookieJar,
    Path(id): Path<String>,
    Json(mut input): Json<Notice>,
) -> Res<Json<Value>> {
    let user = profiles::signed_in(&app, &jar).await?;
    input.validate(true)?;
    let mut tx = app.db.begin().await?;
    let (target, _) = target(&app, &mut tx, &input.location).await?;
    let case: Case =
        sqlx::query_as("SELECT * FROM copyright_cases WHERE id=$1 AND owner_id=$2 FOR UPDATE")
            .bind(&id)
            .bind(&user.id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(Fail::missing)?;
    if case.status != "REMOVED" || case.target() != target {
        return Err(Fail::bad(
            "Counter-notice must identify the removed video or Beacon in this case.",
        ));
    }
    profiles::rate(&app, format!("copyright-counter:{}", user.id), 5, 3600).await?;
    sqlx::query("UPDATE copyright_cases SET counter_notice=$2,status='COUNTER_PENDING',counter_received_at=now() WHERE id=$1").bind(&id).bind(security::seal(&app,"copyright-counter",&serde_json::to_string(&input).map_err(|_|Fail::internal())?)?).execute(&mut *tx).await?;
    alert(&app, &mut tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"saved":true})))
}
async fn queue(State(app): State<App>, jar: CookieJar) -> Res<Json<Value>> {
    crate::safety::staff(&app, &jar).await?;
    let rows:Vec<Case>=sqlx::query_as("SELECT * FROM copyright_cases ORDER BY status IN ('OPEN','COUNTER_PENDING','COUNTER') DESC,created_at DESC LIMIT 100").fetch_all(&app.db).await?;
    let reminder = Utc::now().date_naive()
        >= NaiveDate::from_ymd_opt(2029, 10, 4).unwrap() - chrono::Duration::days(60);
    Ok(Json(
        json!({"cases":rows,"agent_renew_by":"2029-10-04","agent_renewal_due":reminder}),
    ))
}
async fn detail(
    State(app): State<App>,
    jar: CookieJar,
    Path(id): Path<String>,
) -> Res<Json<Value>> {
    let actor = crate::safety::staff(&app, &jar).await?;
    let mut tx = app.db.begin().await?;
    let case: Case = sqlx::query_as("SELECT * FROM copyright_cases WHERE id=$1")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Fail::missing)?;
    let notice: Value =
        serde_json::from_str(&security::unseal(&app, "copyright-notice", &case.notice)?)
            .map_err(|_| Fail::internal())?;
    let counter = case
        .counter_notice
        .as_deref()
        .map(|s| security::unseal(&app, "copyright-counter", s))
        .transpose()?
        .map(|s| serde_json::from_str::<Value>(&s))
        .transpose()
        .map_err(|_| Fail::internal())?;
    crate::safety::audit(
        &mut tx,
        Some(&actor.id),
        "copyright_details_opened",
        "copyright",
        &id,
        &[],
        "Reviewed confidential notice",
        json!({}),
        false,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"case":case,"notice":notice,"counter":counter})))
}
#[derive(Deserialize)]
struct Decision {
    action: String,
    reason: String,
}
async fn decide(
    State(app): State<App>,
    jar: CookieJar,
    Path(id): Path<String>,
    Json(input): Json<Decision>,
) -> Res<Json<Value>> {
    let actor = crate::safety::staff_write(&app, &jar).await?;
    let reason = crate::text::plain(&input.reason, "reason", 1, 1000, 10, false)?;
    let mut tx = app.db.begin().await?;
    let case: Case = sqlx::query_as("SELECT * FROM copyright_cases WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Fail::missing)?;
    let notice: Notice =
        serde_json::from_str(&security::unseal(&app, "copyright-notice", &case.notice)?)
            .map_err(|_| Fail::internal())?;
    let status = match input.action.as_str() {
        "remove" if case.status == "OPEN" => {
            match case.target() {
                Target::Video(video) => {
                    review::hold(&mut tx, &video, "COPYRIGHT", &id, true).await?
                }
                Target::Beacon(beacon) => {
                    let was = crate::beacons::review::hide(&mut tx, &beacon).await?;
                    sqlx::query("UPDATE copyright_cases SET beacon_restore=$2 WHERE id=$1")
                        .bind(&id)
                        .bind(was)
                        .execute(&mut *tx)
                        .await?;
                }
            }
            sqlx::query("UPDATE copyright_cases SET removed_at=now() WHERE id=$1")
                .bind(&id)
                .execute(&mut *tx)
                .await?;
            if let Some(owner) = &case.owner_id {
                crate::safety::queue_notice(&app,&mut tx,owner,"Copyright removal",&format!("Access to a recording and its cuts was disabled following a copyright notice. Read case {id} at {}/studio/copyright. You may submit a counter-notice if this was a mistake or misidentification.",app.config.origin)).await?;
                repeat_infringer(&app, &mut tx, &actor, owner, &id).await?;
            }
            "REMOVED"
        }
        "reject" if case.status == "OPEN" => "REJECTED",
        "accept_counter" if case.status == "COUNTER_PENDING" => {
            let received = case.counter_received_at.ok_or_else(Fail::internal)?;
            let after = business_days(received, 10);
            let by = business_days(received, 14);
            let counter: Notice = serde_json::from_str(&security::unseal(
                &app,
                "copyright-counter",
                case.counter_notice.as_deref().ok_or_else(Fail::internal)?,
            )?)
            .map_err(|_| Fail::internal())?;
            let body = format!(
                "S.V.E.R copyright case {id}\n\nWe received the following counter-notification on {}. Access will be restored after 10 business days following receipt, unless our designated agent receives notice that you filed an action seeking a court order to restrain infringement concerning this material. Send any such notice promptly to dmca@sver.tv.\n\nCounter-notification:\n{}",
                received.to_rfc3339(),
                format_args!(
                    "Name: {}\nEmail: {}\nAddress: {}\nPhone: {}\nRemoved material and former location: {}\nExplanation: {}\n\nI declare under penalty of perjury that I have a good-faith belief that this material was removed or disabled by mistake or misidentification.\nI consent to the jurisdiction of the Federal District Court for the judicial district in which my address is located, or, if my address is outside the United States, any judicial district in which SVER LLC may be found. I will accept service of process from the original claimant or their agent.\n\nElectronic signature: {}",
                    counter.name,
                    counter.email,
                    counter.address,
                    counter.phone,
                    counter.location,
                    counter.description,
                    counter.signature
                )
            );
            let mail = crate::jobs::queue_address(
                &app,
                &mut tx,
                None,
                &notice.email,
                "Copyright counter-notification",
                &body,
            )
            .await?;
            sqlx::query("UPDATE copyright_cases SET restore_after=$2,restore_by=$3,forward_mail_id=$4,forward_state='queued' WHERE id=$1").bind(&id).bind(after).bind(by).bind(mail).execute(&mut *tx).await?;
            "COUNTER"
        }
        "reject_counter" if case.status == "COUNTER_PENDING" => "REMOVED",
        "litigation"
            if matches!(
                case.status.as_str(),
                "COUNTER" | "COUNTER_PENDING" | "REMOVED"
            ) =>
        {
            "LITIGATION"
        }
        "restore"
            if case.status == "COUNTER"
                && case.restore_after.is_some_and(|t| t <= Utc::now())
                && case.forward_state.as_deref() == Some("accepted") =>
        {
            restore(&mut tx, &case).await?;
            "RESTORED"
        }
        _ => return Err(Fail::bad("That decision is not available for this case.")),
    };
    sqlx::query("UPDATE copyright_cases SET status=$2,reason=$3,reviewer_id=$4,reviewed_at=now() WHERE id=$1").bind(&id).bind(status).bind(&reason).bind(&actor.id).execute(&mut *tx).await?;
    crate::safety::audit(
        &mut tx,
        Some(&actor.id),
        &format!("copyright_{}", input.action),
        "copyright",
        &id,
        &[],
        &reason,
        json!({"status":status}),
        false,
    )
    .await?;
    crate::jobs::queue_address(&app,&mut tx,None,&notice.email,"Copyright case update",&format!("Case {id}: {status}.\n\n{reason}\n\nContact dmca@sver.tv with this reference if you have further information.")).await?;
    if let Some(owner) = &case.owner_id {
        crate::safety::queue_notice(
            &app,
            &mut tx,
            owner,
            "Copyright case update",
            &format!(
                "Case {id}: {status}.\n\n{reason}\n\nRead the case in Creator Studio → Copyright."
            ),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"saved":true})))
}
/// Shared mail worker reports provider acceptance, including failures that must block restoration.
/// Repeat-infringer policy (docs/VODS_CLIPS.md, approved October 6, 2026): each upheld notice is a
/// strike for 12 months unless a counter-notice restores the material. The third active strike
/// restricts the channel indefinitely through account standing, where it can be appealed.
pub(crate) const STRIKE_LIMIT: i64 = 3;
async fn repeat_infringer(
    app: &App,
    db: &mut PgConnection,
    actor: &crate::auth::User,
    owner: &str,
    case: &str,
) -> Res<()> {
    // The case being upheld is still OPEN inside this transaction.
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM copyright_cases WHERE owner_id=$1 AND removed_at>now()-interval '12 months' AND (id=$2 OR status IN ('REMOVED','COUNTER_PENDING','COUNTER','LITIGATION'))")
        .bind(owner).bind(case).fetch_one(&mut *db).await?;
    if active < STRIKE_LIMIT {
        return Ok(());
    }
    let restricted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strikes WHERE user_id=$1 AND reason='copyright' AND severity='SEVERE' AND status='ACTIVE' AND expires_at>now() AND penalty_lifted_at IS NULL)")
        .bind(owner).fetch_one(&mut *db).await?;
    let internal = profiles::channel_user_by_id(db, owner)
        .await?
        .is_none_or(|u| u.internal);
    if restricted || internal {
        return Ok(());
    }
    let input = crate::safety::StrikeInput {
        reason: "copyright".into(),
        severity: "SEVERE".into(),
        message_to_user: "Your channel has three upheld copyright notices in the last 12 months, so it is restricted under the repeat-infringer policy. You can appeal in Account standing.".into(),
        interim_restriction_id: None,
    };
    crate::safety::issue_strike(
        app,
        db,
        actor,
        owner,
        &input,
        &[],
        json!({"copyright_case": case, "active_copyright_strikes": active}),
        json!([]),
        "Repeat-infringer policy: third active copyright strike",
    )
    .await?;
    Ok(())
}
/// Lifts a case's hold: the video hold, or the Beacon back to how it was before the removal.
async fn restore(db: &mut PgConnection, case: &Case) -> Res<()> {
    match case.target() {
        Target::Video(_) => review::release(db, "COPYRIGHT", std::slice::from_ref(&case.id)).await,
        Target::Beacon(beacon) => {
            let was = case
                .beacon_restore
                .clone()
                .unwrap_or(json!({"previous": true}));
            crate::beacons::review::unhide(db, &beacon, &was).await
        }
    }
}
pub async fn mail_result(db: &mut PgConnection, id: &str, state: &str) -> crate::Result<()> {
    sqlx::query("UPDATE copyright_cases SET forward_state=$2 WHERE forward_mail_id=$1")
        .bind(id)
        .bind(state)
        .execute(db)
        .await?;
    Ok(())
}
pub async fn tick(app: &App) -> Res<()> {
    let mut tx = app.db.begin().await?;
    let rows:Vec<Case>=sqlx::query_as("UPDATE copyright_cases SET status='RESTORED',reviewed_at=now(),reason='Statutory counter-notice period elapsed without notice of a court action.' WHERE status='COUNTER' AND restore_after<=now() AND forward_state='accepted' RETURNING *").fetch_all(&mut *tx).await?;
    for case in rows {
        restore(&mut tx, &case).await?;
        let (id, owner) = (case.id, case.owner_id);
        crate::safety::audit(
            &mut tx,
            None,
            "copyright_restored",
            "copyright",
            &id,
            &[],
            "Counter-notice period elapsed",
            json!({}),
            false,
        )
        .await?;
        if let Some(owner) = owner {
            crate::safety::queue_notice(app,&mut tx,&owner,"Copyright access restored",&format!("The copyright hold in case {id} ended. Other visibility rules and normal recording retention still apply.")).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}
// Weekends and observed US federal holidays are excluded. See OPM's federal holiday calendar.
fn business_day(date: NaiveDate) -> bool {
    if matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    for year in [date.year(), date.year() + 1] {
        for (month, day) in [(1, 1), (6, 19), (7, 4), (11, 11), (12, 25)] {
            let holiday = NaiveDate::from_ymd_opt(year, month, day).unwrap();
            let observed = match holiday.weekday() {
                Weekday::Sat => holiday - chrono::Duration::days(1),
                Weekday::Sun => holiday + chrono::Duration::days(1),
                _ => holiday,
            };
            if date == observed {
                return false;
            }
        }
    }
    let ordinal = (date.day() - 1) / 7 + 1;
    !matches!(
        (date.month(), date.weekday(), ordinal),
        (1 | 2, Weekday::Mon, 3)
            | (9, Weekday::Mon, 1)
            | (10, Weekday::Mon, 2)
            | (11, Weekday::Thu, 4)
    ) && !(date.month() == 5 && date.weekday() == Weekday::Mon && date.day() >= 25)
}
fn business_days(at: DateTime<Utc>, mut count: u32) -> DateTime<Utc> {
    let local = at.with_timezone(&chrono_tz::America::New_York);
    let mut date = local.date_naive();
    while count > 0 {
        date = date.checked_add_days(Days::new(1)).unwrap();
        if business_day(date) {
            count -= 1;
        }
    }
    chrono_tz::America::New_York
        .from_local_datetime(&date.and_time(local.time()))
        .single()
        .expect("business day has no DST transition")
        .with_timezone(&Utc)
}
pub fn routes() -> Router<App> {
    Router::new()
        .route("/api/copyright", post(submit))
        .route("/api/me/copyright", get(cases))
        .route("/api/me/copyright/{id}/counter", post(counter))
        .route("/api/admin/copyright", get(queue))
        .route("/api/admin/copyright/{id}", get(detail).post(decide))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadlines_skip_weekends_and_observed_holidays() {
        let received = Utc.with_ymd_and_hms(2026, 12, 18, 17, 0, 0).unwrap();
        assert_eq!(
            business_days(received, 10).date_naive(),
            NaiveDate::from_ymd_opt(2027, 1, 5).unwrap()
        );
        assert!(!business_day(
            NaiveDate::from_ymd_opt(2027, 12, 31).unwrap()
        ));
        assert!(business_day(NaiveDate::from_ymd_opt(2026, 10, 6).unwrap()));
    }
}
