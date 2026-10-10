# Native alerts and overlays

Phase 3, after the developer platform ([DEVELOPER_PLATFORM.md](DEVELOPER_PLATFORM.md) "Compatibility": StreamElements and Streamlabs alert boxes don't support S.V.E.R). Written October 10, 2026 with **Proposed** defaults, at Joe's direction to keep building until the platform is complete. Joe can change any of them.

## What a streamer gets

- **Creator Studio → Alerts & overlays.** One private link per channel, made and revoked in Studio. Pasted into OBS (or any broadcast app) as a browser source. Like the board overlay's link, it's the only credential, it reaches the channel's alerts and nothing else, and making a new one stops the old one at once.
- **Three widgets on the same link:**
  - **Alert box** (`/overlay/alerts/{token}`): one alert at a time, in order, for follows, subscriptions, tributes, Skills and incoming raids.
  - **Goal bar** (`?widget=goal`): followers or active subscribers toward a target, with a label.
  - **Recent events** (`?widget=events`): the last 5 alerts, newest first.
- **Per alert type:** on or off, a message template (`{name}`, plus `{tier}` and `{months}` for subs, `{valor}` and `{message}` for tributes, `{skill}` for Skills), how long it shows (3–15 seconds). Tributes also have a minimum (10 Valor or more) and whether the attached message shows.
- **Sound:** chime, horn or none, with a volume, made in the browser (Web Audio), so there are no sound files to host or review.
- **Test buttons** play a sample of each alert on open overlays only. Nothing is recorded, nothing reaches viewers or webhooks.
- Saving in Studio updates open overlays straight away.

## Rules

- Alerts come from the same records as the live events API (Section 2), so anything the events API leaves out also stays off the overlay: people the streamer blocked, chat refused by moderation, Valor that didn't move.
- Names are S.V.E.R usernames. A tribute's message is the chat message that carried it, which already passed the channel's chat rules.
- Background is transparent; motion is fades and short slides only (no glows), per DESIGN.md.
- The overlay page loads nothing from other sites.

## Not in this part

Uploaded images and sounds per alert (they need the emote-style media review), text-to-speech, and the StreamElements/Streamlabs custom-widget adapter. They come next, in that order.

## Done when

1. A follow, a subscription, a tribute over the minimum, a Skill and an incoming raid each play their alert, with the template filled in, once, in order.
2. A disabled type, a tribute under the minimum and a blocked person's event never show.
3. The goal bar moves when someone follows or subscribes; the recent list shows the last 5.
4. A revoked or replaced link stops working at once; test alerts reach only open overlays.

## As built (October 10, 2026; `overlays.rs`, migration 0082)

See the module comment in `apps/api/crates/sver/src/overlays.rs`. Covered by `tests/streams/overlays.rs`.
