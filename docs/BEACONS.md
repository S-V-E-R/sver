# Module 9: Beacons

Specified October 4, 2026, from the plan's hardened decisions and the legacy review. This module builds after VODs and clips ([VODS_CLIPS.md](VODS_CLIPS.md)). It is the last core module, and Phase 2 starts when it closes.

Beacons are short vertical videos that lead people to creators and their live streams. A Beacon is a light that shows where to go, so success means viewers who join a live stream or follow a creator, not time spent scrolling.

## Who posts

- Anyone who has streamed on S.V.E.R at least once, including guardian-approved accounts aged 13–17.
- **Limit:** up to 10 Beacons per creator per day.
- A creator posts to their own channel only. Viewers can't post Beacons, but a viewer's approved clip of a channel can become one of that channel's Beacons (see below), with the clipper credited.

## Sources

- **From a clip:** the creator picks one of their channel's approved clips ([VODS_CLIPS.md](VODS_CLIPS.md)), then drags a 9:16 crop window over it. The crop they last used is remembered. A clip from a VOD or Highlight works the same way. The Beacon links back to its clip and broadcast.
- **Upload:** MP4, MOV or WebM, up to 200 MB, 5–60 seconds.
  - The upload goes straight to private storage through a signed upload URL, then the creator marks it complete.
  - The server probes the file itself. File type, length and size come from the probe, never from the browser, and anything that isn't a real video in an allowed format is rejected.
- **Title:** up to 120 characters, and goes through automod.
- **Category:** chosen from the same list as streams. A Beacon from a clip inherits the clip's category.

## Processing (the one re-encode)

- Beacons are S.V.E.R's only re-encode ([AGENTS.md](../AGENTS.md)). It's cheap at 60 seconds or less.
- **One job per Beacon** in the Postgres job queue, with a processing lease, so it runs exactly once and recovers after a crash.
- **Output:** 9:16 H.264 and AAC MP4s at 1080×1920 and 720×1280, each set up for fast playback start, plus a thumbnail.
  - If the source isn't 9:16, it's framed by the creator's crop.
  - Uploads are padded rather than stretched.
- **Metadata is stripped.** Location, device and editing data are removed from every output.
- **Watermark:** the public copies carry a small "@username · sver.tv" mark that moves from corner to corner every few seconds, so cropping or blurring one spot can't remove it and a Beacon reposted elsewhere still points back to the creator.
  - The order of corners and the exact timing vary per Beacon (exact values live in the private tuning config), so there's no fixed pattern to mask.
  - The mark sits inside the safe area, clear of the feed's rail and captions, at low opacity.
  - The creator can download a clean copy of their own Beacon from private storage. It's never served publicly.
  - The watermark is applied in the re-encode Beacons already need, so it costs nothing extra.
- Before publishing, every upload is checked against the Take It Down blocklist of removed images ([TAKE_IT_DOWN.md](TAKE_IT_DOWN.md)). The same check runs on clip sources.
- A Beacon moves through the statuses Draft, Processing, Ready, Published and Removed. If processing fails, the creator gets the reason and can retry.

## The feed

- **Layout:** one 9:16 video at a time, centered on desktop and full screen on phones ([DESIGN.md](DESIGN.md)).
  - Swipe or use the arrow keys to move between Beacons.
  - Videos autoplay muted until tapped, then stay unmuted for the rest of the session.
- **The right-side rail shows:**
  - the creator's crest and name, with a link to their channel
  - their faction badge
  - a like button
  - the view count
  - a **Live now** button whenever the creator is streaming, which jumps to the stream
  - a share button
  - a report option
- **Live now row:** a row of live creators who have recent Beacons sits at the top of the feed.
- **What's in the feed:** a mix of three sources:
  - creators you follow
  - creators in your faction
  - a **fair rotation** of everyone else, so every eligible creator gets turns and new creators aren't buried
- **Ordering:** newest first within each source. The rotation hands out turns the way MAGNet does: every creator gets a turn before anyone gets a second one.
- **What never affects order:** money, viewer counts, view counts or like counts. This is the same rule as MAGNet. The legacy paid "visibility boost" isn't coming back.
- **Signed-out visitors** see the fair rotation only.
- **Muting:** viewers can mute a creator from the feed.

## Counts

- **A view counts** after 3 seconds of playback that is visible on screen and actually advancing. Each account or guest session counts at most once per Beacon per day.
- Views come only from sessions viewer integrity counts ([LIVE_STREAMS.md](LIVE_STREAMS.md)). Excluded sessions never add views.
- **Likes:**
  - signed-in only
  - one per account
  - can be taken back
  - rate-limited
- **Counter safety:** every counter endpoint needs a session and is rate-limited. Legacy's open click counter isn't repeated.
- **For creators only:** completions (90% watched), follows from a Beacon, and live joins from a Beacon (Live now taps that become a counted live session). These measure whether Beacons do their job.

## Where Beacons appear

- The Beacons feed (`/beacons`) and each Beacon's own page, with preview tags so links show the video on Discord, X and Reddit.
- The **Beacons shelf** on the home page (9:16 cards) and the channel's **Beacons tab**, replacing the stub from [PROFILES.md](PROFILES.md).
- Search results, alongside channels and categories.
- **Charity streams:** a Beacon made from a charity stream ([SUPPORT.md](SUPPORT.md), Shine) carries the Shine label and the charity's donate link. It does not change its place in the feed.

## Content rules and safety

- **Platform content rule:** play, build or make. No reaction videos, gambling or just chatting.
- **Reports:** any signed-in user can report a Beacon. Reports go to the admin review queue (Module 2 reports, target type BEACON), where staff can remove it. A Beacon with an open report keeps playing unless staff hide it.
- **Known-abuse matching (before uploads open):** uploads and clip sources are checked against a known child sexual abuse material hash list through an outside matching service, with matches blocked and reported to NCMEC as [TAKE_IT_DOWN.md](TAKE_IT_DOWN.md) describes. Joe picks the service; clip Beacons can launch first, but uploads stay off until it's live.
- **Take It Down:** a valid request removes the Beacon, everything it was made from and every copy within 48 hours, and purges the CDN.
- **Copyright:** claims go through the copyright process in [VODS_CLIPS.md](VODS_CLIPS.md), and repeat infringers lose posting.
- **18+ streams:** a Beacon from an 18+ stream keeps the 18+ gate and only appears in feeds for viewers who have passed it.
- **Deleting:** the creator can delete any of their Beacons, and deleting removes every rendition, the thumbnail and the clean copy, and purges the CDN.

## Not in this module

- Comments, remixing, faction-only feed tabs, Beacons counting toward faction influence, and personalized ranking. These are deferred, as in the plan.
- Push or email alerts for new Beacons, because followers already get go-live alerts.
- Music libraries or editing tools beyond cropping and trimming a clip.

## Legacy notes

| Legacy | Rebuild |
| --- | --- |
| A separate beacon service behind a proxy, which isn't in the repos | Part of the monolith. |
| Clips sent to Beacons fire-and-forget, never retried | Durable jobs with a lease. |
| Paid visibility boost (50 Valor, 1.5× multiplier) | Dropped. Discovery never takes money. |
| Ranking around view counts, seven lanes, embeddings and an "emotional score" | Three sources and a fair rotation. No count-based ranking. |
| Unauthenticated, unthrottled click and like counters | A session and rate limits on every counter. |
| Upload length and type taken from the browser | The server probes every file. |
| No way to report a Beacon | Reports go to the admin queue. |
| Media worker authenticated with the stream webhook secret | Each worker has its own credential. |
| "Beacon" also named the Shine boost and the viewer heartbeat | "Beacon" means only these videos. The heartbeat is called the viewer lease. |
| Kept: approved clips only, the Live now rail, Watch live on every video, counting only visible playback (complete at 90%), the direct-to-storage upload flow, the fairness lane, the watermark with a clean master (now moving corner to corner) | Carried over as above. |

## Done when

1. An eligible creator turns an approved clip into a 9:16 Beacon with a chosen crop, and uploads a video that the server probes, re-encodes and strips of metadata.
2. A file that isn't a real video, is too long or is too big is rejected. A match against the Take It Down blocklist never publishes. Every public copy carries the moving watermark, and the clean copy is reachable only by its creator.
3. The feed mixes followed, faction and fair-rotation Beacons, plays muted until tapped, and works with swipe and arrow keys.
4. Ordering never reads money, views or likes.
5. Views count only after 3 seconds of visible, advancing playback from counted sessions. Likes are one per account. Every counter rejects requests without a session and requests over the rate limit.
6. Live now jumps to the stream, and live joins and follows from a Beacon are recorded.
7. A reported Beacon reaches the admin queue and can be removed. Deleting removes every file and purges the CDN.
8. The home shelf, the channel Beacons tab, search and link previews all show Beacons.

## Implementation and activation

Module 9 is in development (started October 7, 2026). The backend lives in `apps/api/crates/sver/src/beacons/` with migration `0045_beacons`, and the web pages are `/beacons`, `/beacons/{id}`, `/{username}/beacons` and `/studio/beacons`. Production activation and the full acceptance run remain open.

- **Storage.** Beacons share the private recording store from [VODS_CLIPS.md](VODS_CLIPS.md) (`VOD_STORAGE`), under the `beacons/` key prefix. Every file plays through a five-minute ticket and `no-store`; link-preview tags (`/share`) get a 7-day ticket because Discord, X and Reddit cache them, which still stops working the moment the Beacon is hidden, removed or changed. The clean copy is served only on a `clean` ticket issued to its creator.
- **Processing.** One `PROCESS` job per Beacon in `beacon_jobs`, with a 90-second renewed lease and a token fence before anything is published. Attempts back off, and after six the Beacon fails with a reason the creator can retry. A single FFmpeg pass crops or pads to 9:16 and writes the 1080×1920 and 720×1280 watermarked H.264/AAC renditions plus the clean 1080×1920 copy. Every output is fast-start, with global, stream and chapter metadata and the encoder's SEI units removed. A WebP thumbnail is taken from the watermarked copy. FFprobe reads every source; the container, codec, length, size and rotation come from the probe.
- **Watermark.** `beacons::watermark::plan` turns each Beacon's random seed into a schedule. Each cycle visits all four corners in a shuffled order, no corner repeats across cycles, and each corner is held for a time between `watermark_min_ms` and `watermark_max_ms` from the private `BEACON_TUNING_FILE`. The corners sit inside the safe area: the right-hand corners stop at 80% of the width, clear of the rail, and the bottom corners sit 25% above the bottom edge, clear of the captions. The text is DejaVu Sans Bold at 50% opacity (`fonts-dejavu-core` in the API image; override with `BEACON_FONT_FILE`).
- **Take It Down.** SHA-256 hashes of the source and of each output are stored on the Beacon. Processing refuses a source whose hash is on `blocked_media_hashes`, matches held removal fingerprints, or belongs to a Beacon hidden for review, and it checks again under the publishing lock. A staff removal from the report queue keeps the files privately (for an overturn) and purges `/beacons/{id}`. A legal removal deletes every copy, purges `/beacons/{id}` and adds all of its hashes to the blocklist. Removing a clip or recording (Take It Down, a report or a copyright notice) also removes every Beacon made from it. While the source clip has a `TAKE_DOWN` or `COPYRIGHT` hold, those Beacons are out of public view. Take It Down requests accept `/beacons/{id}` links. Frame-level matching against removed images is still open, as for VODs.
- **Uploads.** `BEACON_UPLOADS=on` is refused in production until the known-abuse matching service is integrated, as this spec requires. With S3, the browser `PUT`s straight to a SigV4 query-signed URL valid for one hour for one key. The bucket's CORS must allow `PUT` from the site origin. Local filesystem storage uses a ticketed `PUT /api/beacons/{id}/upload`. Abandoned drafts are deleted after their upload window and give their daily slot back.
- **Feed.** Followed creators, then the viewer's faction (excluding followed), then the fair rotation. The rotation numbers each creator's Beacons newest first and orders by that turn number, then by `md5(creator || viewer seed)`, so every creator gets a turn before anyone gets a second. The three sources are interleaved one at a time and de-duplicated. Signed-out visitors see the rotation only. No query reads views, likes, viewer counts or money. Muted creators, blocks, channel bans, holds and ungated 18+ Beacons are filtered in SQL.
- **Counts.** `POST /api/beacons/{id}/beat` uses the viewer-integrity assessment with a 3-second window. Watch time accrues only while playback is visible and advancing, and a view counts once per account or guest session per day. A completion is 90% of the length watched. A Live now tap is recorded and becomes a live join once the same session has a counted lease on the creator's stream within 10 minutes. A follow made from the rail within five minutes is attributed once. Likes, beats, taps and mutes are rate-limited from the private tuning file. Because a guest can make up new browser IDs, new guest sessions are also capped per network per hour, and Live now taps per IP.
- **Reports.** The report target type is `beacon`. A report doesn't hide the Beacon. Staff can hide or return it (`POST /api/admin/beacons/{id}/hide`, audited) and open evidence (`GET /api/admin/beacons/{id}/review`, audited). Staff removal marks it Removed and keeps the files privately, so an overturned decision restores it.
- **Production settings.** In production with `VOD_STORAGE=s3`, the API refuses to start without `BEACON_TUNING_FILE`. `compose.videos.yaml` mounts it from `SVER_BEACON_TUNING_FILE`. `infra/beacons.tuning.example.json` holds development values only.

Tests: `tests/streams/beacons.rs` runs real FFmpeg output through the Done-when lines. It covers clip Beacons with a crop, recovery from a crashed lease, output sizes and codecs, metadata stripping, fast start, the watermark's planned corner and its move, clean-copy access, upload probe failures (not a video, too long, too big), padding, the blocklist, the daily limit, feed sources and fairness, mutes, the 18+ gate, views, likes and their rate limit, live joins, follows, completions, the shelf, channel tab, search and preview data, reports and staff hide and removal, deletion, and Take It Down cascading from a clip. Unit tests cover the watermark plan and filter escaping, the crop math and the probe rules.

Copyright notices against an uploaded Beacon (October 10, 2026, migration 0080): the notice form at /dmca and Studio's counter-notice accept a `/beacons/{id}` link. A Beacon made from a clip names its clip, so the clip's case (which holds the clip and every Beacon made from it) covers it. An uploaded Beacon gets its own case (`copyright_cases.beacon_id`). Upheld, it's hidden the way Take It Down hides it, and its earlier state is kept on the case. A forwarded counter-notice restores it to that state when the statutory period ends, and the repeat-infringer strikes count it like any other case. Staff review it with the Beacon evidence player in Admin → Copyright.

Open before closing the module: integrating the known-abuse matching service (Joe's choice) before uploads open, production storage, CORS and CDN purge verification, and real preview acceptance on Discord, X and Reddit.
