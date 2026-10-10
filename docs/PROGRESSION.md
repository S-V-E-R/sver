# Progression

**Status: draft for Joe's decisions (October 9, 2026). Built so far, the parts that don't depend on the questions (migration 0063): account XP and levels from watching and chat with the daily caps, the Scout bonus, the player card's level and XP bar, the level on the user card, and "Scouted by N viewers" in Studio. Channel loyalty ranks (shown when hovering a name in chat) and the chat rank gate followed (migration 0064). Daily orders followed with XP rewards only (migration 0065; 40 XP for a Common order, weekly milestones 100/200/300 XP, both Proposed). Level-up frames, faction titles and Engagement Valor for orders followed on October 10 (see Decisions).** Numbers marked **Proposed** are defaults Joe can change; the questions at the end must be answered before building. It follows the closure rule: specify, build, then test against "Done when".

The platform plan places Progression in Phase 2, "after all nine modules": account levels and XP, faction rank titles, daily orders (small quests that pay Valor, XP or influence), and the Scout bonus for early viewers of new streams. [DESIGN.md](DESIGN.md) already reserves the player card's level badge and XP bar and a Daily orders panel, [DEVELOPER_PLATFORM.md](DEVELOPER_PLATFORM.md) adds a chat rank gate on channel loyalty, and [SUPPORT.md](SUPPORT.md) puts Lights and Shine Moments here.

## What legacy did, and what changes

Legacy S.V.E.R had account levels, daily quests and per-channel loyalty. Ported: the level curve, quest rarity and streaks, and the loyalty ladder. Not ported:

| Legacy problem | Here |
| --- | --- |
| 10 of 15 quest types never progressed (no event fed them), so people were dealt orders they couldn't finish | An order type exists only if the event that completes it is already recorded; tests complete every type |
| Level-up Valor was paid outside the XP transaction, without an idempotency key, and only for watch-time XP | Level-up rewards are granted in the same transaction as the XP, once per level |
| No daily XP cap; chat-quest XP farmable after the Valor cap | A daily XP cap per source |
| Watch XP from any heartbeat, sessions counted per heartbeat, local-time days | XP only from Counted or Trusted playback (viewer integrity), UTC days, one ledger row per grant |
| Discord XP granted a whole minute's XP for any amount | No Discord XP (Linked chat is separate) |

## 1. Account XP and levels

- **Curve (ported):** total XP for level L is `floor(100 × (L−1)^1.5)`, levels 1 to 100 (level 10 at 2,700 XP, level 50 at 34,300, level 100 at about 98,500).
- **Sources (Proposed):**
  - Watching a live stream with a Counted or Trusted session: 10 XP a minute, ×1.5 on a stream of your own faction. Cap 600 XP a day (1 hour at the base rate).
  - Chatting where you are watching: 2 XP per message, at most one a minute counted, cap 100 a day.
  - Daily orders: their XP (section 3).
  - The Scout bonus (section 4).
- Only verified accounts earn XP, as with Engagement Valor. Purchases, tributes and Skills give no XP (no buying levels).
- **Where it shows:** the player card's hex badge and XP bar, the profile, and the chat hover card. Levels never affect MAGNet, fair rotation or faction influence.
- **Level-up reward:** see Question 1.

## 2. Faction rank titles

A title that follows the account level, different per faction (Proposed ladder at levels 1, 10, 25, 50, 75, 100). The names come from the lore ([LORE.md](LORE.md)); see Question 2. The title shows next to the crest on the player card and chat hover card. Switching faction keeps your level and gives the new faction's title at that level.

## 3. Daily orders

- **3 orders a day (Proposed; legacy used 4)**, reset at midnight UTC, 1 reroll a day. They're drawn by rarity weight (Common 50, Uncommon 30, Rare 15, Epic 4, Legendary 1; ported) from types the viewer can actually do: no streamer-only or subscriber-only orders for people who can't do them.
- **Types, each completed by an event S.V.E.R already records:** watch N minutes (Counted playback), chat in N channels, follow a new channel, take part in a Surge, rally your faction, vote in a poll, press a board control, watch a Beacon to the end, join a raid, watch a stream started in the last 30 minutes (Scout).
- **Rewards:** XP (rarity ×1 / 1.5 / 2 / 3 / 5; streak ×1.1 at 3 days up to ×2 at 30; ported, capped lower) plus what Question 3 decides. Weekly milestones at 10/20/30 orders (ported) pay XP only unless Question 3 says otherwise.
- **Integrity:** progress comes from the existing event records (watch leases, chat messages, Surge participation…), never from client reports, and each event counts once per order.

## 4. The Scout bonus

Early viewers help new streams get going. A **Scout** watches (Counted or Trusted, at least 10 minutes) a broadcast in its first 15 minutes, from a channel with fewer than 10 broadcasts (Proposed). The Scout gets 50 XP, up to 3 a day, and the streamer sees "Scouted by N viewers" in Studio. Never more than one Scout bonus per channel per viewer per week, so friends can't farm each other.

## 5. Channel loyalty

A per-channel rank from the viewer's lifetime **Engagement Valor earned** there (`engagement.earned`, so spending never demotes): Newcomer, Regular, Devoted, Veteran, Legend (ported names; thresholds rescaled to Engagement Valor rates, Proposed 0 / 200 / 1,000 / 5,000 / 20,000). It shows on the chat hover card, and powers the **chat rank gate**: a streamer can require a minimum loyalty rank to chat, alongside followers-only (DEVELOPER_PLATFORM.md §6).

## Not in this part

Lights and Shine Moments ([SUPPORT.md](SUPPORT.md) "Shine", Question 4), achievements and message cosmetics (Phase 3), streamer progression (legacy had a streamer XP track; not planned unless Joe asks).

## Done when

1. XP is granted only from the listed sources, within the caps, once per event, and a level-up and its reward happen in one transaction.
2. Every daily order type can be completed by doing the thing, and tests complete each one; rerolls and the midnight UTC reset work.
3. The Scout bonus pays only within its rules and can't be farmed between friends.
4. Faction titles and channel loyalty show where described, and the chat rank gate works with moderators and the owner exempt.
5. Nothing in Progression changes MAGNet, rotation, faction influence or money balances except where Questions 1 and 3 say so.

## Decisions (October 10, 2026) and as built (migration 0081)

- **Level-up reward (Q1): a cosmetic every 10 levels.** The level badge gets a new frame at levels 10, 20 … 100 (`progression::frame`, CSS borders only). It's derived from the level, so a level-up and its reward can't drift apart. Choosing among unlocked frames, and more cosmetics, come with achievements and message cosmetics (Phase 3).
- **Faction rank titles (Q2): the 22-rank ladder from the main S.V.E.R Discord** (Initiate … Overlord, Novice … Sovereign, Spark … Inferno; Private … Commander without a faction), spread evenly over levels 1–100 (levels 1, 6, 10, 15 … 95, 100). Shown on the player card and the user card (chat hover). Switching faction keeps the level and shows the new faction's name for it.
- **Daily orders (Q3): XP plus Engagement Valor**, 10 / 15 / 20 / 30 / 50 by rarity, in the channel of the viewer's latest playback or chat that day (where they were when it finished; never their own channel or one that banned them). An order finished with no channel activity pays XP only. The amount and channel are kept on the order (`daily_orders.ev`, `ev_channel`); the sidebar shows them.
- Q4 (Lights and Shine) remains open.

## Questions for Joe

1. **Level-up reward.** Legacy paid Valor (level × 10). Purchased Valor is money (creators are paid 0.8¢ for each one spent), so S.V.E.R would be minting a liability. Options: (a) nothing but the badge, (b) a cosmetic (badge frame, name color) every 10 levels, (c) Purchased Valor with a monthly budget. Proposed: (b).
2. **Faction rank titles.** Six titles per faction for levels 1, 10, 25, 50, 75, 100. Do you want to name them, or should they be drafted from LORE.md for your approval?
3. **Daily order rewards.** XP only, or also Engagement Valor in the channel where the order was done (free, per-channel, no cash value; Proposed 10–50 by rarity), or Purchased Valor (a money liability, as in Question 1)? Proposed: XP plus Engagement Valor.
4. **Lights and Shine Moments.** SUPPORT.md puts them with Progression. Build them in this module or as a follow-up?
