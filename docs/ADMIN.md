# Admin tools

Specified October 4, 2026. This is the map of the staff console at `/admin`. Most of its pages are specified in the module that needs them. This doc lists them all in one place, sets the rules every page follows, and specifies the pieces no module owned: the home dashboard, staff roles beyond `admin`, emergency switches, the job queue, the audit log viewer and the site banner.

## Rules for every admin page

These carry over from [PROFILES.md](PROFILES.md), "Staff roles and the review queue":

- **Access:** a staff role and MFA are needed to see anything. Everyone else gets 404, for pages and the API alike.
- **Step-up:** changes need an MFA-verified session inside the staff window: unlocked for 15 minutes after a sign-in, password confirmation or authenticator/recovery-code confirmation; each staff action extends it by 15 more, up to 8 hours from the confirmation. Confirming with a code unlocks staff tools only, never account-security changes, which keep their own 5-minute rule (Joe, October 4, 2026; migration 0018). A lapsed window shows one prompt on any admin page and the refused change then finishes on its own.
- **Roles are granted only from the server**, with `sver-admin role grant|revoke`, never through the web. Channel roles (owner, channel moderator) never grant `/admin`.
- **Every change** needs a short note and writes a `moderation_actions` row in the same transaction. Logs carry fixed fields only, with no names, content or notes.
- **Least exposure:**
  - Staff see what the case needs and no more.
  - Emails and payment details show only on pages whose role allows them.
  - Raw IP addresses aren't stored anywhere, so they never show.
  - DMs are readable only through a report naming that conversation ([COMMUNITY.md](COMMUNITY.md)).
- **Tuning values stay out of the web.** Thresholds, weights and caps live in the private tuning config on the server and change through a deploy, not from a page, so a stolen staff session can't quietly change how the platform works.
- **Nobody acts on their own account**, and moderators and support staff can't act on another staff account. Only an admin can act on staff, and only an admin can act on another admin.
- **The last admin can't be removed:** the role command refuses to revoke the last remaining admin.
- **Self-review:** when the only staff member available made the original decision, they may decide its appeal with a required note, flagged `self_review` (already in Profiles).

## Staff roles

The single `admin` role grows into four roles, so moderators and support staff can be added later without handing out everything. Until anyone besides Joe has access, Joe holds `admin` and nothing else is needed.

| Area | admin | moderator | support | finance |
| --- | --- | --- | --- | --- |
| Reports, appeals, strikes, bans, user standing | Yes | Yes | View | No |
| Take It Down queue | Yes | Yes | No | No |
| Child safety (matches and NCMEC reports) | Yes | No | No | No |
| Copyright notices and counter-notices | Yes | Yes | No | No |
| Viewbot integrity cases | Yes | Yes | No | View |
| Categories, genres, setup parts, emotes | Yes | Yes | No | No |
| MAGNet controls and staff spotlights | Yes | Yes, including emergency stop | No | No |
| Account lookup (email, sign-in methods, MFA status) | Yes | No | Yes | No |
| Account recovery (lost MFA) | Yes | No | Yes, approved by a second staff member when one exists | No |
| Payments, payouts, refunds, chargebacks, guardian approvals | Yes | No | View | Yes |
| Valor ledger adjustments | Yes | No | No | Yes, approved by a second staff member when one exists |
| Emergency switches, site banner | Yes | Emergency switches only | No | No |
| Jobs, system health | Yes | View | View | View |
| Audit log | Yes | Own actions | Own actions | Own actions |

A person can hold more than one role. Every role needs MFA.

## Home (`/admin`)

One page answering "what needs me right now?":

- **Deadlines first:** open Take It Down requests with their 48-hour countdowns. Anything under 12 hours is shown in red.
- **Queue counts:** reports, appeals, integrity cases, copyright notices, guardian approvals, failed payouts and failed jobs, each linking to its queue.
- **Live now:** number of live streams, viewers by delivery path, active restreams and the Plays health status.
- **System:**
  - job queue backlog and its oldest job
  - email bounces in the last day
  - storage used by VODs, Highlights, clips and Beacons
  - the deployed version
- **Reminders:** the DMCA agent renewal ([VODS_CLIPS.md](VODS_CLIPS.md)) and any other dated legal task, 60 days ahead.

The page only reads. Every action happens on its own page.

## Pages by area

**Safety**

- **Reports and Appeals** (`/admin/reports`, `/admin/appeals`), **Users** (`/admin/users/{username}`) and **Bans**: already built in Profiles, then extended with the report targets each module adds:
  - live streams, chat messages and emotes
  - VODs, Highlights and clips
  - Beacons
  - guilds
  - DMs
- **Users page additions** (legacy had these without notes or notice):
  - End all sessions: support and admin only, needs a reason, and the user is told.
  - Resend a verification email.
  - Username reset for impersonation, as specified in Live streams.
  - Restore content: when an appeal is overturned, or a removal was a mistake.
  - Staff never set someone's email as verified, and never set a password.
- **Take It Down** (`/admin/take-it-down`): the request queue, countdowns, removal and copy-matching tools, and the 3-year records ([TAKE_IT_DOWN.md](TAKE_IT_DOWN.md)).
- **Child safety** (`/admin/child-safety`, admin only):
  - **Matches:** a hash match from the matching service in [BEACONS.md](BEACONS.md) blocks the media and opens a case here.
  - **Media stays hidden:** staff see the match details, not the media, unless viewing is unavoidable to confirm a report.
  - **NCMEC reports:** filed from here and logged.
- **Copyright** (`/admin/copyright`): notices, counter-notices, the waiting period before restoring content, and repeat-infringer counts ([VODS_CLIPS.md](VODS_CLIPS.md)).

**Integrity**

- **Viewbot cases** (`/admin/integrity`): flagged broadcasts with their count history and signals, and actions that can be appealed ([LIVE_STREAMS.md](LIVE_STREAMS.md)). A streamer with an open case isn't promoted to a higher tier ([SUPPORT.md](SUPPORT.md)).

**Content**

- **Categories and genres** (`/admin/categories`): add, rename and merge categories, and assign each to a genre. A category can't move to another genre mid-season ([FACTIONS.md](FACTIONS.md)).
- **Setup parts** (`/admin/parts`): already built.
- **Media review** (`/admin/media`): one queue for uploads that need a look before or after they go public:
  - custom emotes
  - guild emblems ([GUILDS.md](GUILDS.md))
  - CrowdSync effect uploads ([CROWDSYNC.md](CROWDSYNC.md))
- **Guilds** (`/admin/guilds`): requests to verify a guild as a real organization, plus rename, reset or disband for guilds that break the rules ([GUILDS.md](GUILDS.md)).
- **Plays** (`/admin/plays`): start, stop and pause, the game library and rotation, and the health alerts ([PLAYS.md](PLAYS.md)). Legacy's Plays admin routes always failed their sign-in check, so this is rebuilt rather than ported.
- **MAGNet** (`/admin/magnet`): Hype channels on and off, force a stream on and release it, emergency stop, staff spotlights, and the 7-day decision log ([MAGNET.md](MAGNET.md)).
- **Factions** (`/admin/factions`):
  - The checkpoint and season timeline, read-only.
  - If a checkpoint job failed, staff can re-run it, and it runs exactly once.
  - The log of faction switches.
  - Staff can't change scores or territory by hand.

**Money** (`/admin/money`)

- **Read-only views** of payments, subscriptions, tributes, payouts and Early Pay, refunds and chargebacks, negative balances, and accounts waiting for guardian approval ([SUPPORT.md](SUPPORT.md)). Stripe stays the source of truth for card and bank details, which never show here.
- **Good Works badges:** review the amount a streamer reports raising for charity, with its proof, and award or decline the badge ([SUPPORT.md](SUPPORT.md)).
- **Tax forms:** Stripe collects streamers' tax information and issues their tax forms. This page shows only whether each streamer's tax information is complete. S.V.E.R doesn't collect W-9s itself.
- **Refunds** go through Stripe from this page and post reversing ledger entries.
- **Valor adjustments** are double-entry ledger rows with a reason. Any adjustment above a cap (in the private tuning config) needs a second staff member's approval once there's more than one person who can approve. Legacy allowed one admin to grant any amount. They are never edits to a balance.

*Money as built (October 10, 2026; `money.rs`):* `/admin/money` shows the latest 50 payments, subscription invoices, tributes, payouts and Early Pay, refunds and chargebacks; active subscription counts; negative Valor and earnings balances by username; payout accounts with tax-information and guardian status. "Refund" sends a full refund to Stripe for a payment S.V.E.R took (the `charge.refunded` webhook posts the reversal). "Adjust Valor" posts a balanced `valor_adjustment` against `valor:adjustments` (up to 100,000 either way, never on the staff member's own account). Every action has a note and an audit row. The second approval above a private cap waits until someone besides Joe can approve.

**Operations**

- **Emergency switches** (`/admin/switches`): instant on and off for the risky features, to use during an incident.
  - The switches: sign-ups, going live, restreaming, Linked chat (per platform), clipping, Beacon uploads, DMs, purchases and payouts.
  - Turning one off shows a short message where the feature would be. It doesn't break pages.
  - Every flip is audited, and it can't reach any setting other than on and off.
  - *As built (October 10, 2026; `switches.rs`, migration 0077):* `/admin/switches` holds the switches and the banner. Sign-ups (every new account, email or OAuth), going live (the publish hook; running broadcasts continue), restreaming (relays stop within a pass), Linked chat per platform (outside messages dropped, replies refused), clipping (viewer clips), Beacon uploads, DMs (sending), purchases (Valor packs, subscriptions, upgrades, gifts) and payouts (Early Pay refused; payday waits until they're back on). A refusal reads "Paused right now: {feature}. Please try again soon." `GET /api/site` lists paused features and the banner for every page.
- **Site banner** (`/admin/banner`): one dismissible message across the top of every page for maintenance or incidents, with an optional end time.
- **Jobs** (`/admin/jobs`): failed and stuck jobs from the Postgres queue (media, notifications, payouts, checkpoints), with their error and a retry button. A job retried by hand still runs exactly once.
  - *Jobs as built (October 10, 2026; `staff_console.rs`):* video, Beacon, push, email, staff push, board webhook and event webhook queues show waiting, stuck (tried, failed and overdue by 10 minutes) and gave-up counts, with up to 50 stuck jobs each and "Retry now" (a note; the job becomes due and its worker runs it once). Faction checkpoints and failed payouts are counted but retried elsewhere (`/admin/factions`) or not by hand.
- **Audit log** (`/admin/audit`): `moderation_actions`, searchable by staff member, action, target and date. It is read-only, and nobody can edit or delete rows, admins included.

*Home and audit log as built (October 10, 2026):* `/admin` shows Take It Down open requests, any past the 48-hour deadline (as an alert) and the next deadline, open reports, appeals, copyright and integrity cases, emotes awaiting review, stuck jobs, live broadcasts and switched-off features. `/admin/audit` searches by staff member, action, target and date, newest first, read-only.

## Raven's Eye (built October 10, 2026; `ravens_eye.rs`, migration 0083)

Joe's direction: staff platform analytics, not legacy's trust-and-safety console (its device fingerprints and IP bans break the rule that no admin page shows a raw IP; reports, strikes, appeals and the audit log already replace the rest). `/admin/ravens-eye` (staff) shows sign-ups, signed-in viewers, watch hours and peak viewers (real Counted and Trusted sessions from the integrity snapshots), streamers live, broadcast hours, chat messages and follows over 7, 30 or 90 days, each with a trend line; money moved by ledger kind; top channels by watch hours; top categories by broadcast hours (the recording's category, else the channel's current one); and faction joins. Each finished UTC day is computed once and stored (`ravens_eye_days`, filled back 90 days on the first run), because chat expires after 7 days; today is computed live. No IP, device or personal data. ClickHouse waits until event volume needs it.

## Not carried over from legacy

| Legacy | Rebuild |
| --- | --- |
| IP and device bans, shared-IP lookups | Raw IPs aren't stored. Ban evasion shows up through viewer-integrity signals and account bans, and staff never see an IP address. |
| Shadowbans (never actually enforced) | Dropped. Every penalty is visible to the person and can be appealed. |
| Two separate role systems; a moderator could ban an admin | One role table, with the staff safeguards above. |
| Role changes from the web with no reason or last-admin guard | Command line only, with the last-admin guard and an audit row. |
| Separate audit tables for each feature, and many actions not logged | One audit log for every staff action. |
| Read-only feature-flag page driven by environment variables | Emergency switches that act instantly. |
| Force-verify email, and disable 2FA with no reason | Resend verification only. 2FA recovery goes through the audited recovery process. |
| Analytics and top-creator dashboards | Raven's Eye in Phase 2. The home page shows only what's needed to run things. |
| Ads admin | Phase 4, with ads. |
| Creator applications, partnerships, mentorship | Dropped: anyone verified with 2FA can stream, and mentorship isn't returning. |
| OAuth key rotation page | Done from the server command line, never the web. |
| Plays admin routes that always failed their sign-in check | Rebuilt on the new rules. |

## When each piece is built

- **Pages tied to a module** arrive with it. For example, Take It Down comes first in Live streams, Money comes with Support, and Factions comes with Factions.
- **Live streams** also adds the home dashboard, emergency switches, the site banner, the jobs page and the audit log viewer, because it's the first module that has to run live.
- **The new roles** (moderator, support, finance) come just before anyone besides Joe gets staff access. Until then, `admin` covers everything.

## Done when

1. Non-staff get 404 everywhere under `/admin` and its API, including a signed-in user who guesses a URL.
2. Each role sees exactly what the table above allows. A moderator gets 404 on Money, for example.
3. Every change needs step-up and a note, and writes an audit row. The audit log can't be edited by anyone.
4. Emergency switches turn a feature off in seconds with a clear message to users, and back on, with no deploy.
5. The home page shows Take It Down deadlines and queue counts accurately.
6. A failed job can be retried from the jobs page and still runs only once.
7. Nobody can act on their own account, moderators get refused on staff accounts, and the role command refuses to remove the last admin.
8. No admin page shows a raw IP address, a full card or bank number, or a DM outside a report.
