# Developer platform

Specified October 6, 2026, from a review of Mixer's open-source repositories (github.com/mixer, archived November 2024, mostly MIT-licensed). It opens S.V.E.R to third-party apps, bots, overlays and SVER Desktop the way Mixer's developer platform did, building on what CrowdSync already has. CrowdSync already provides scoped board tokens, the integration gateway, the OBS bridge, the JavaScript, Unity and Unreal SDKs, signed webhooks and the Postgres outbox ([CROWDSYNC.md](CROWDSYNC.md), Part 2).

**Build order:** Phase 3, first, before overlays and alerts, which use its events, and before SVER Desktop, which signs in with it. Two small pieces come sooner, because they protect money and input: the CrowdSync changes in "Board input that reaches games" ship with CrowdSync's remaining work, and WHIP and SRT ingest follow the load test ([LOAD_TEST.md](LOAD_TEST.md)).

Reusing Mixer's code: MIT repositories (`carina`, `shortcode-oauth`, `interactive-*` except Unreal, `cdk-std`, `developer-docs`) can be adapted with their license notice kept. Repositories with no license (`mixtok-service`, `devshow`, `interactive-unreal-plugin`, `cdk-preact-starter`) are ideas only. Nothing carries Mixer or Microsoft branding.

## 1. Apps and sign-in for developers

- **Developer apps** at `/settings/developer`: any verified account with 2FA can register up to 10 apps. Each app has:
  - a name and icon (reviewed like emotes)
  - a public client ID
  - redirect URIs (HTTPS, or `http://127.0.0.1` with any port for desktop apps)
  - an optional client secret for server apps, shown once with only its digest stored

  An app can be suspended by staff, and every app is listed in `/admin`.
- **OAuth 2.1** with authorization code and PKCE for every client; there is no implicit flow. Access tokens last 1 hour and refresh tokens 60 days, rotating on use, with reuse detection that revokes the whole family. Tokens are stored as digests.
- **Consent screen** in the S.V.E.R style: the app's name and icon, "made by @owner", and each requested scope in plain words. Users see and revoke connected apps in `/settings/connections`.
- **Device sign-in at `sver.tv/go`** (RFC 8628, Mixer's "shortcode"). SVER Desktop, TV and console apps show a 6-character code (no lookalike characters). The user enters it at `sver.tv/go`, signs in if needed and approves. Codes expire in 10 minutes, and the app polls no faster than every 5 seconds.
  - Rate-limited per IP address and per code.
  - Codes are single-use.
  - The approval screen shows the app's name and, in plain words, the device's approximate location from its network, so people don't approve a code someone else sent them.
- **Scopes** (start small):
  - `user:read`
  - `channel:read`
  - `chat:read`, `chat:write`
  - `channel:moderate`
  - `channel:edit` (title and category)
  - `events:private`: the channel's private events such as subs and tributes, for overlays
  - `board:control`: the same powers as a CrowdSync game token
  - `stream:key`: for SVER Desktop only. It needs 2FA and a fresh check, and it's never granted to other apps.

  Money is never movable by apps: no Valor spending, tributes, payouts or purchases.
- **Every API call identifies its app** with an `SVER-Client-Id` header, also for public reads. Rate limits are per app and per user, and each response says the remaining budget in `RateLimit-*` headers.
- **Lists use cursors**, returned in the body and in a `Link` header (Mixer's continuation tokens), never page numbers.
- CrowdSync's existing Studio tokens keep working. They are the "connection" type of app that a streamer creates for themselves.

**As built, part 1 (October 9, 2026; `devapps.rs`, migration 0072):** Settings → Developer registers up to 10 apps (verified email and two-factor sign-in), each with a client ID, 1–10 redirect URIs (https://, or http://127.0.0.1 on any port), and for server apps a secret shown once and stored as a digest (rotatable). The consent screen is `/oauth/authorize` (app name, "made by @owner", each scope in plain words, Allow/Cancel); the flow is authorization code with PKCE S256 only, codes last 10 minutes and work once. `POST /api/oauth/token` (form-encoded, no cookies) issues 1-hour access tokens and 60-day refresh tokens that work once each; a reused refresh token revokes the whole grant. `POST /api/oauth/revoke` (RFC 7009) ends the grant. Settings → Connected apps lists and removes grants, which stops the tokens at once. Every API call needs `SVER-Client-Id`; the first calls are `GET /api/v1/me` (`user:read`) and `GET /api/v1/channels/{name}` (client ID only), rate-limited to 600 a minute per app and person. Staff list apps at `GET /api/admin/apps` and suspend them (audited). `stream:key` is not offered. **Not yet:** app icons (reviewed like emotes), `RateLimit-*` headers and cursors (no list endpoints yet), the remaining scoped calls, and §2 events. **Device sign-in (migration 0073):** `POST /api/oauth/device` (form: `client_id`, `scope`; 20 an hour per address) returns a device code, a 6-character user code shown as `ABC-DEF` (no vowels, no 0/O/1/I/L), `https://sver.tv/go` and a 5-second interval; codes last 10 minutes. `/go` (signed in, 20 lookups per 10 minutes) shows the app, its permissions and the device's network in plain words (provider and country from the IPinfo file), with Allow and Cancel. The token endpoint's `urn:ietf:params:oauth:grant-type:device_code` answers `authorization_pending`, `slow_down` (polled within 5 seconds), `access_denied` or `expired_token`, and issues tokens once.

**App actions (October 10, 2026), for the Stream Deck plugin and bots.** With `Authorization: Bearer` and `SVER-Client-Id`, an app calls the same endpoints Studio uses, as the person: `PATCH /api/me/stream` (title and category, `channel:edit`), and with the new `channel:run` scope `POST /api/me/raids`, `POST /api/channels/{name}/polls` (polls and predictions), `POST /api/channels/{name}/polls/{id}/close`, `POST /api/channels/{name}/marker` and `PUT /api/channels/{name}/board/disabled`. Every rule is the page's (owner or moderator roles, live checks); deleted, held and banned accounts are refused. The origin check is skipped for these paths only when a bearer token is sent, and then the cookie is never used.

## 2. Live events (Mixer's "Constellation")

One WebSocket, `wss://sver.tv/api/events`, where apps subscribe to topics instead of polling. This is the backbone for overlays, alerts, bots, SVER Desktop and Discord integrations.

- **Protocol:** JSON messages `{"type":"method","id":…,"method":"subscribe","params":{"topics":[…]}}`, `unsubscribe` and `ping`. Replies carry the same `id`. Events arrive as `{"type":"event","topic":…,"data":…,"at":…}`. Compression is optional (permessage-deflate). Up to 200 topics per connection and 10 connections per app and user.
- **Public topics** (client ID only):
  - `channel:{name}:live`: went live or offline, title, category
  - `channel:{name}:update`: profile changes
  - `channel:{name}:follows`: a follow count, never who followed
  - `channel:{name}:raids`: raids and hosts in or out
  - `channel:{name}:costream`
  - `channel:{name}:board`: board changes and effects, already public on the page
  - `channel:{name}:poll` and `channel:{name}:prediction`
  - `channel:{name}:surge`
  - `faction:{slug}:war`: checkpoint results and genre changes
- **Private topics** (owner or moderator token with `events:private`):
  - `channel:{name}:follows:detail`: who followed
  - `channel:{name}:subs`: new, renewed and gifted subscriptions
  - `channel:{name}:tributes`
  - `channel:{name}:skills`
  - `channel:{name}:moderation`
  - `channel:{name}:raid:incoming`
- **User topics** (that user's token): `user:{name}:notifications` and `user:{name}:whispers`, the latter once DMs exist.
- **Chat** is a topic, `chat:{name}`, with `chat:write` used through the same socket. Bots can opt out of join and leave noise (Mixer's `optOutEvents`) and fetch the last 50 messages with `history` on connect.
- **Privacy:**
  - Events never include email, IP address or internal IDs, and they follow blocks: a user's own private stream of events never shows people they've blocked.
  - Under-18 accounts never appear by name in public events.
  - Follower names are private unless the follower's own settings make their follows public.
- **Delivery:** events are written to the Postgres outbox in the same transaction as the change that caused them (as CrowdSync does), then fanned out in-process to subscribed sockets. No Redis. If S.V.E.R ever runs more than one API instance, a fan-out layer is added then (Mixer built `redplex` for this). Each event has an ID, and a reconnecting client can ask for anything after its last ID from the past 5 minutes.
- **Webhooks for the same events:** an app or creator registers `POST /api/hooks` with a list of topics, an HTTPS URL and a secret. Delivery reuses CrowdSync's signer (`SVER-Signature`), its private-address blocking and the outbox's 8 retries. After 50 failed deliveries in a row, the hook is disabled and its owner notified.

**As built, part 1 (October 9, 2026; `events.rs`, migration 0074):** `wss://sver.tv/api/events?client_id=…[&access_token=…]` (query parameters, since browsers can't set WebSocket headers). Methods `subscribe` (with optional `since` to replay the past 5 minutes after an event ID), `unsubscribe` and `ping`, replies carrying the call's `id`; events `{"type":"event","id","topic","data","at"}`. Topics so far: public `channel:{name}:live` (`{"live":true|false}`), `channel:{name}:follows` (`{"followers":n}`, never who) and `channel:{name}:raids` (`{"from","viewers"}`); private (owner or moderator token with `events:private`) `channel:{name}:follows:detail` (`{"user"}`), `channel:{name}:subs` (`{"user","tier","months"}`) and `channel:{name}:tributes` (`{"user","valor","message"}`). Events are written to the `events` outbox in the same transaction as the follow, sub month, tribute or broadcast end (go-live is emitted once per broadcast by the alert step), drained to the in-process hub every second and kept 10 minutes. Up to 200 topics per connection and 10 connections per app and person. **Not yet:** the other topics (update, costream, board, poll, prediction, surge, faction war, skills, moderation, incoming raid), user topics, chat over the socket, permessage-deflate.

**As built, part 2 (October 9, 2026; `events.rs`, migration 0075): webhooks.** `POST /api/hooks` `{"topics":[…],"url":"https://…"}` registers up to 50 topics (20 hooks per person) and returns the signing secret once; `GET /api/hooks` lists and `DELETE /api/hooks/{id}` removes. Signed in, a person with a verified email and two-factor sign-in manages their own (Settings → Developer); an app manages hooks with the person's bearer token and `SVER-Client-Id`, sees only its own, and its hooks are removed when the grant is revoked or the app suspended. Topic access is the socket's (private topics need `events:private`) and is checked again at every delivery. Deliveries are queued in the same statement that writes the event, posted as the socket's event JSON with `SVER-Signature`, retried 8 times with doubling backoff from 10 s, and private addresses are refused at registration and at every delivery. 50 failures in a row turn the hook off and notify its owner. An app can't register hooks with its client ID alone.

**As built, part 3 (October 9, 2026): CrowdSync topics.** Public `channel:{name}:board` (`{"type":"press","id","control","label","effect","user","text","goal","held"}`, `{"type":"result","id","control","outcome"}` when a game captures or releases, `{"type":"changed"}` when the board is republished; `user` is null for under-18 accounts), `channel:{name}:poll` and `channel:{name}:prediction` (the poll's state on start, each vote and close), `channel:{name}:surge` (the Surge's state on each level); private `channel:{name}:skills` (`{"user","skill","valor"}`). Board, poll and Surge events are written right after the commit, beside the page's own live update, so a crash can lose one display event but never a record. Also public: `channel:{name}:update` (stream details `{"title","category","language","mature"}` when saved; profile `{"display_name","bio","status_text","mood_emoji"}`), `channel:{name}:costream` (`{"members":[usernames]}` on each join, leave and end; empty for whoever left) and `faction:{slug}:war` (`{"type":"checkpoint","week","result"}`, the weekly result with territory holders); private `channel:{name}:moderation` (`{"action","by","role","user","message","reason"}` from the channel's moderation log) and `channel:{name}:raid:incoming` (`{"from","seconds"}` when a raid's countdown starts). **As built, part 4 (October 10, 2026): chat over the socket.** `chat:{name}` is a topic (client ID only; a token applies the person's blocks, and an under-18 person can't read a mature channel), delivering `{"type":"message","message"}` and `{"type":"delete","id"}` straight from the chat hub (chat is already stored, so there is no outbox replay). `history` `{"topic":"chat:{name}"}` returns the last 50 messages. `send` `{"channel","id","body","reply_to"}` posts as the person with `chat:write`, under every chat rule, and refuses tributes, Skills and highlights (apps never spend money) and banned accounts. There are no join or leave events, so there is nothing to opt out of. Linked (outside) chat isn't on the topic yet. User topics, with the person's own token only: `user:{name}:notifications` (`user:read`; each new in-site notification `{"kind","channel","payload","created_at"}`, checked every 5 seconds, blocks applied as in the list) and `user:{name}:whispers` (new scope `whispers:read`; each direct message as the messages page receives it). User topics have no replay and no webhooks. **Still not built:** permessage-deflate (optional in the spec; not offered). A channel's private follower, sub, tribute and Skill events skip anyone its owner has blocked.

## 3. Board input that reaches games (CrowdSync additions)

From Mixer's MixPlay protocol (`developer-docs` → Interactive protocol), for controls whose effect happens in a game or through the OBS bridge rather than on S.V.E.R's own overlay.

- **Hold, then capture.** *(Built October 8, 2026; migration 0051. The S.V.E.R effect is the game's own, so these controls have no library effect.)* For a control marked "Game confirms", a press only *reserves* the viewer's Engagement Valor. The game or bridge sends `capture` (charged) or `release` (refunded) with the press ID. Anything not captured within 60 seconds is released automatically. Viewers are never charged for an effect that didn't happen. S.V.E.R's own effects keep charging at once, because S.V.E.R itself guarantees delivery.
  - Reserve, capture and release are ledger entries in one transaction with the press, and are idempotent.
  - The viewer sees "Waiting for the game…" and then the result.
  - Skills paid in Purchased Valor stay S.V.E.R-delivered only; they're never left for a game to confirm.
- **Groups.** *(Built October 8, 2026; migration 0052.)* A game can put viewers into groups (for example by faction, team or a random half) and show each group a different screen of the board. Groups are set over the gateway; viewers who aren't in one see the default screen.
- **Input cap.** *(Built October 8, 2026; migration 0050.)* The streamer, or the game, sets the most presses and joystick moves per second forwarded to the game (Mixer's bandwidth throttle). Over the cap, the newest input is dropped and viewers see "Busy, try again".
- **Ready state.** *(Built October 8, 2026.)* A board connected to a game stays "Starting…" until the game says `ready`, so viewers can't press into a game that isn't listening.
- **Pricing guidance.** *(Built October 8, 2026; shown when at least 5 viewers watched or chatted in the last 30 days.)* Mixer found most viewers held only 500–1,000 of its points. The board builder shows the channel's typical viewer balance (the median Engagement Valor of recent participants, never per person) next to each cost, and warns when a cost is above it.

## 4. Custom controls (later, after the above)

Mixer's `cdk` let developers ship their own HTML controls. The S.V.E.R version:

- Creators upload a bundle (HTML, JS and CSS, up to 2 MB, no remote code). It's served from a separate cookieless domain inside a sandboxed iframe (`sandbox="allow-scripts"`, a strict CSP, no network access except the bridge).
- A `postMessage` bridge exposes only `press`, the board state, the player's position and the viewer's own groups and balance. It never exposes tokens, identity beyond the display name, or the page.
- **Review:** an automated scan plus staff approval before first publish and on every version. Panic switch and Take It Down rules apply.
- Effort is large and the review load is real, so this comes only after Sections 1–3 have been used for a while.

## 5. Ingest options

*Built October 8, 2026.* SRS bridges WHIP (`rtc_to_rtmp`, Opus to AAC) and SRT (`srt_to_rtmp`, port 10081) into the same RTMP pipeline, so HLS, CDN, WebRTC playback, recording and the publish hook are unchanged. nginx's `/rebuild/whip/` moves the bearer token into SRS's `key` parameter (infra/media/nginx-stream-playback.conf); the API's publish hook applies the RTMP rules (key, eligibility, one publisher, lifecycle). Studio's OBS connection has RTMP, WHIP and SRT tabs. Load test: LOAD_TEST.md scenario 8.


- **WHIP (WebRTC) ingest** alongside RTMP, for sub-second glass-to-glass. OBS 30 and later have it built in, SRS supports it, and OBS 31 removed FTL, Mixer's old low-latency ingest. So WHIP is the modern replacement and FTL is not built.
  - **Authentication:** the stream key goes in the WHIP bearer token, never in the URL.
  - **Same rules as RTMP:** verification and 2FA eligibility, one publisher, and callbacks to the same broadcast lifecycle.
  - **Studio's OBS guide** gets a WHIP tab.
  - **Limits:** OBS's WHIP output has fewer encoder options, so RTMP stays the default.
- **SRT ingest** for streamers on unstable connections (mobile, travel): SRS's SRT listener, with the key in the stream ID and the same rules.
- Both are measured in the load test before they're offered. SRT moves out of the "Deferred" list in [LIVE_STREAMS.md](LIVE_STREAMS.md).

## 6. Smaller items

- **Embeds** (`/embed/{name}`) accept `muted`, `chat=0|1`, `lowLatency=0` (forces the CDN path) and `costream=0`. Embedding sites are recorded by referrer for staff (abuse only), never shown publicly.
- **Chat rank gate** (with Progression, Phase 2): a streamer can require a minimum channel loyalty level to chat, as Mixer allowed, alongside the existing followers-only mode.

## 7. Working with the tools streamers already use

The old platform's audit listed "no third-party extension support (StreamElements, Streamlabs)" and "no BTTV/7TV emote compatibility" among the gaps blocking streamers from moving over. One streamer has already left because using S.V.E.R next to their usual setup was a hassle. S.V.E.R can't make other companies' products support it, so the plan is to make S.V.E.R easy to plug into, ship the common pieces itself, and do the integrations where we control both ends.

| Tool streamers use | Status today | What S.V.E.R does |
| --- | --- | --- |
| OBS, Streamlabs Desktop, Meld and other broadcast software | Works (RTMP custom server) | WHIP and SRT from Section 5. A "Custom server" guide for each app in Help. |
| Multistream services (Restream, Aitum Multistream) | Works (custom RTMP destination) | Already covered by Multistream ([LINKED_CHAT.md](LINKED_CHAT.md)). The Help guide shows both directions. |
| StreamElements and Streamlabs alert boxes, overlays and widgets | Don't support S.V.E.R | **Native alerts and overlays** (Phase 3; first part built October 10, 2026: [OVERLAYS.md](OVERLAYS.md)) driven by the live events in Section 2, as browser sources with the same setup as theirs. Alongside that, a **widget import**: StreamElements and Streamlabs custom widgets are plain HTML/CSS/JS listening for events, so S.V.E.R's overlay runtime offers an adapter that sends S.V.E.R events in the shapes their widgets expect (follow, subscriber, tip-style tribute, raid, host) and runs imported widgets in the sandbox from Section 4. Meanwhile, S.V.E.R asks both companies for official support, using the public events API. |
| Streamer.bot, Mix It Up, SAMMI, Touch Portal (automation) | Don't support S.V.E.R | Section 2 already gives them what they need: a WebSocket and webhooks. S.V.E.R publishes:<br>• a Streamer.bot import (a WebSocket client and actions mapped to S.V.E.R events and chat)<br>• a SAMMI/Touch Portal plugin<br>• an offer of an open-source S.V.E.R service to Mix It Up, which is itself open source |
| Elgato Stream Deck | Not supported | An official S.V.E.R Stream Deck plugin (the Elgato SDK is JavaScript): go live or update the title, run a raid, start a poll or prediction, mark a highlight, toggle board panic. It signs in with `sver.tv/go` (Section 1). |
| BTTV, 7TV and FrankerFaceZ emotes | Not shown in S.V.E.R chat | **Opt-in third-party emotes:**<br>• A streamer who has linked their Twitch account (Login already supports it) can show their 7TV, BTTV and FFZ channel emote sets in S.V.E.R chat. S.V.E.R reads them from each service's public API every 10 minutes.<br>• Images are served through S.V.E.R's image CDN, so viewers' IP addresses aren't sent to those services.<br>• The streamer can hide any emote, and viewers can report one, which hides it on S.V.E.R. Names go through the channel's banned-word list.<br>• The channel's own S.V.E.R emotes win when names clash. Global sets are an optional switch.<br>• Each service's API terms are checked before launch, and the feature is switched off if they object. |
| Nightbot, Fossabot, Moobot and other chat bots | Platform-specific; can't connect | S.V.E.R's built-in commands, timers and faction bots ([COMMUNITY.md](COMMUNITY.md)) cover the common uses. Bot makers can connect through Section 2's chat topic with a bot account. A **command import** reads a pasted Nightbot or Fossabot command list (a `!command` and its response) into S.V.E.R's custom commands, translating the common variables (`$(user)`, `$(touser)`, `$(count)`, `$(uptime)`) and flagging the rest. |
| Linked accounts and chat from other platforms | Twitch built; YouTube and Kick waiting on their app approvals | Linked chat merges the streamer's Twitch chat with platform badges ([LINKED_CHAT.md](LINKED_CHAT.md)). |

**Third-party emotes as built (October 10, 2026; `outside_emotes.rs`, migration 0076).** Studio → Emotes: choose 7TV, BTTV and/or FFZ and whether to add their global sets. The channel is found by its linked Twitch account (sign-in link or Linked chat). Lists are read every 10 minutes; each emote's three sizes are copied to S.V.E.R's media storage once and served from there, never hot-linked. `GET /api/channels/{name}/outside-emotes` returns what chat shows: not hidden, passing the channel's banned words and links rule and the platform filter, not held by Take It Down, and not clashing with the channel's own emotes (channel sets beat global ones). Chat renders them at emote height and their own width. A signed-in viewer can report one (10 an hour), which hides it in that chat until the streamer shows it again; the streamer can hide or show any. Services are switched on per server with `OUTSIDE_EMOTES` (`7tv,bttv,ffz`); unset, the feature is off and Studio says it's coming. Before switching one on, check that service's API terms.

**Order:** the Streamer.bot import and the Stream Deck plugin come right after Sections 1 and 2, since they need nothing more. Third-party emotes and the command import are small and can follow. The StreamElements/Streamlabs widget adapter comes with native overlays in Phase 3.

**Not taken:** a Twitch-style IRC chat gateway. Mixer and Glimesh got little use from protocol imitations, the chat clients that speak IRC hard-code their own platforms, and Section 2 serves bots better.

## Considered and not taken

| Mixer idea | Why not |
| --- | --- |
| FTL ingest | Removed from OBS in version 31; WHIP replaces it. |
| Holding back stream keys until a staff review | Joe decided any verified account with 2FA can stream. Viewer integrity, reports and Take It Down cover abuse. |
| Public gifter and spender leaderboards | S.V.E.R's rule: no spending leaderboards. |
| A TikTok-style clip feed (`mixtok`) | Beacons already covers it. |
| Ad-break API | Comes with ads in Phase 4 if ever needed. Mixer's limits (at most 2 per 15 minutes, none during co-streams or hosting) are recorded here for then. |

## Done when

1. A developer registers an app, a user approves it on the consent screen with PKCE, and the app reads and writes only within its scopes. Revoking it in Settings ends access within a minute.
2. A desktop app signs in through `sver.tv/go` with a code. Expired, reused and rate-limited codes fail safely.
3. An overlay subscribes to public and private topics and receives follows, subs, tributes, raids and board events within a second. A reconnect with a last event ID recovers the past 5 minutes without duplicates.
4. Webhooks deliver the same events, signed, with retries and private addresses refused, and disable themselves after repeated failure.
5. A "Game confirms" press is charged only after capture, refunded on release or timeout, and never both. Groups, the input cap and the ready state work over the gateway.
6. OBS 30+ publishes over WHIP and an SRT client publishes over SRT, both under the same eligibility and broadcast rules as RTMP and within the load test's budgets.
7. No event, webhook or API response exposes email, IP address, internal IDs, blocked users or under-18 names.
8. A streamer imports a Streamer.bot action set and a Stream Deck profile that react to S.V.E.R events. Their 7TV/BTTV/FFZ emotes appear in S.V.E.R chat through S.V.E.R's CDN and can be hidden. A pasted Nightbot command list becomes S.V.E.R commands, with unsupported variables flagged.
