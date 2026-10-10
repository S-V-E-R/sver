import type { Metadata } from "next";
import { Barlow, Barlow_Condensed, Cinzel } from "next/font/google";
import Link from "next/link";
import SiteShell from "../components/SiteShell";
import { StaffRemovalAlerts } from "../components/StaffRemovalAlerts";
import { SiteBanner, type Banner } from "../components/SiteBanner";
import { BellIcon, MagnetMark, MessageIcon } from "../components/shell/Icons";
import { SideNav } from "../components/shell/SideNav";
import { PlayerMenu } from "../components/shell/PlayerMenu";
import { DailyOrders } from "../components/shell/DailyOrders";
import type { LiveCard } from "../components/home/types";
import { apiGet } from "../lib/server-api";
import { themeFor } from "../lib/theme";
import { factionOf } from "../lib/factions";
import { Crest } from "../components/Crest";
import { currentAccount, hasAlerts } from "./session";
import "./globals.css";
import "../styles/profiles.css";
import "../styles/design.css";
import "../styles/site-pages.css";
// Module 4 pages (faction hubs, war map) and the shared stream shelves.
import "../styles/discovery.css";
import "../styles/factions.css";

// Type per docs/DESIGN.md "Type": self-hosted and subset by next/font.
const cinzel = Cinzel({ subsets: ["latin"], weight: ["700", "800"], variable: "--font-cinzel", display: "swap" });
const barlow = Barlow({ subsets: ["latin"], weight: ["400", "500", "600"], variable: "--font-barlow", display: "swap" });
const barlowCondensed = Barlow_Condensed({ subsets: ["latin"], weight: ["600", "700"], variable: "--font-barlow-condensed", display: "swap" });

const SITE_DESCRIPTION = "A live streaming platform built on community. Choose your faction, grow with your people, and get discovered: every live stream gets a fair turn.";

export const metadata: Metadata = {
  title: "S.V.E.R — Find Your People. Forge Your Legacy.",
  description: SITE_DESCRIPTION,
  applicationName: "S.V.E.R",
  openGraph: { siteName: "S.V.E.R", title: "S.V.E.R — Find Your People. Forge Your Legacy.", description: SITE_DESCRIPTION },
  twitter: { title: "S.V.E.R — Find Your People. Forge Your Legacy.", description: SITE_DESCRIPTION },
  robots: { index: false, follow: false },
};

/** Sidebar live list: followed channels that are live, or MAGNet's picks for a signed-out visitor. */
async function sidebarLive(signedIn: boolean): Promise<LiveCard[]> {
  const home = (await apiGet<{ live: LiveCard[]; following: LiveCard[] }>("/api/discovery/home")).data;
  return ((signedIn ? home?.following : home?.live) ?? []).slice(0, 8);
}

export default async function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  const account = await currentAccount();
  const [alerts, live, progress, dms, site] = await Promise.all([account ? hasAlerts() : false, sidebarLive(!!account), account ? apiGet<{ xp: number; level: number; level_xp: number; next_xp: number | null; title?: string; frame?: number }>("/api/me/progression").then(r => r.data) : null, account ? apiGet<{ unread: number }>("/api/dms/unread").then(r => r.data?.unread ?? 0) : 0, apiGet<{ banner: Banner | null }>("/api/site").then(r => r.data)]);
  // Level and XP bar on the player card (docs/PROGRESSION.md).
  const xpBar = progress && <span className="player-card-xp" title={`${progress.xp.toLocaleString()} XP`}><span className="player-level" data-frame={progress.frame ?? 0} title={progress.title}>Lv {progress.level}</span>{progress.title && <span className="player-title">{progress.title}</span>}<span className="xp-bar" aria-hidden="true"><span style={{ width: `${progress.next_xp ? Math.round(100 * (progress.xp - progress.level_xp) / (progress.next_xp - progress.level_xp)) : 100}%` }} /></span></span>;
  const initial = account?.username.slice(0, 1).toUpperCase();

  const faction = factionOf(account?.faction);

  const actions = account
    ? <>
      <StaffRemovalAlerts />
      <Link href="/messages" className="icon-button" aria-label={dms ? `Messages, ${dms} unread` : "Messages"}><MessageIcon />{dms > 0 && <span className="alert-badge" aria-hidden="true" />}</Link>
      <Link href="/notifications" className="icon-button" aria-label={alerts ? "Notifications, new notices" : "Notifications"}><BellIcon />{alerts && <span className="alert-badge" aria-hidden="true" />}</Link>
      <PlayerMenu username={account.username} chip={<>
        <Crest faction={account.faction} initial={initial ?? "?"} size={36} label={faction?.name} />
        <span className="player-chip-text"><span className="player-chip-name">{account.username}</span>{faction && <span className="player-chip-title">{faction.title}</span>}</span>
        <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" aria-hidden="true"><path d="m3 4.5 3 3 3-3" /></svg>
      </>} />
    </>
    : <>
      <Link href="/login" className="topbar-link">Log in</Link>
      <Link href="/signup" className="button">Enlist</Link>
    </>;

  const sidebar = <>
    {account && (faction
      ? <Link href={`/${account.username}`} className="player-card frame">
        <Crest faction={account.faction} initial={initial ?? "?"} size={56} label={faction.name} />
        <span className="player-card-text"><span className="player-card-name">{account.username}</span><span className="player-card-faction">{faction.title}</span>{xpBar}</span>
      </Link>
      : <Link href="/welcome" className="player-card frame unchosen">
        <Crest faction={null} initial={initial ?? "?"} size={56} />
        <span className="player-card-text"><span className="player-card-name">{account.username}</span><span className="player-card-faction">Choose your side</span>{xpBar}</span>
      </Link>)}
    <SideNav faction={faction ? { name: faction.name, slug: faction.slug } : null} />
    {account && <DailyOrders />}
    {(account || live.length > 0) && <section className="side-section" aria-labelledby="side-live">
      <div className="side-label"><span id="side-live">{account ? "Following · live" : "Picked for you"}</span><span className="magnet"><MagnetMark />MAGNet</span></div>
      {live.length === 0
        ? <p className="side-empty">Nobody you follow is live.</p>
        : <ul className="side-channels">{live.map(s => <li key={s.username}><Link href={`/${s.username}/live`}>
          <Crest faction={s.faction} initial={s.display_name.slice(0, 1).toUpperCase()} size={30} />
          <span className="side-channel-text"><span className="side-channel-name">{s.display_name}</span>{s.category && <span className="side-channel-category">{s.category}</span>}</span>
          <span className="side-channel-count"><span className="live-dot" aria-hidden="true" />{s.viewers.toLocaleString()}<span className="sr-only"> watching</span></span>
        </Link></li>)}</ul>}
    </section>}
  </>;

  return <html lang="en" data-theme={themeFor(account)} className={`${cinzel.variable} ${barlow.variable} ${barlowCondensed.variable}`}>
    <body>
      <SiteShell account={account} alerts={alerts} actions={actions} sidebar={sidebar} banner={site?.banner ? <SiteBanner banner={site.banner} /> : null}>{children}</SiteShell>
    </body>
  </html>;
}
