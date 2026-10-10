import Link from "next/link";
import { Rotation } from "../components/home/Rotation";
import { RecentChannels, SectionHead, StreamGrid } from "../components/home/Shelves";
import type { LiveCard, Recent } from "../components/home/types";
import { apiGet, viewerLanguages } from "../lib/server-api";
import { factionOf, isFaction } from "../lib/factions";
import { currentAccount } from "./session";
import "../styles/home.css";
import { VideoGrid } from "../components/VideoGrid";
import type { VideoCard } from "../lib/videos";
import { BeaconGrid } from "../components/BeaconGrid";
import type { BeaconItem } from "../lib/beacons";
import { FrontLine } from "../components/WarStanding";
import { SeasonReveal } from "../components/SeasonReveal";
import { TerritoryGrid, type BrowseGenre } from "../components/Territories";
import type { War } from "../lib/war";

type Spotlight = { kind: "staff" | "first_stream" | "returning"; reason: string; stream: LiveCard | null; user?: { username: string; display_name: string } };
type Home = { live: LiveCard[]; following: LiveCard[]; faction: LiveCard[] | null; fresh: LiveCard[]; spotlights: Spotlight[]; recent: Recent[] };

/**
 * Home, in the Main mockup's order (docs/DESIGN.md "Home"): front-line banner, MAGNet rotation,
 * Live now, From your faction (docs/MAGNET.md), the Beacons shelf, Just went live, Territories,
 * Latest clips. Following · live is in the sidebar. Every list is in fair rotation, never by
 * viewer count; the Beacons shelf never by views or likes either.
 */
export default async function Home() {
  const account = await currentAccount();
  const languages = await viewerLanguages();
  const [homeRes, clipsRes, warRes, browseRes, beaconsRes] = await Promise.all([
    apiGet<Home>("/api/discovery/home"),
    apiGet<{ clips: VideoCard[] }>("/api/clips/latest"),
    apiGet<War>("/api/factions/war"),
    apiGet<{ genres: BrowseGenre[] }>("/api/discovery/browse"),
    apiGet<{ items: BeaconItem[] }>("/api/beacons/shelf"),
  ]);
  const beacons = (beaconsRes.data?.items ?? []).slice(0, 8);
  const home = homeRes.data;
  const clips = clipsRes.data?.clips ?? [];
  const war = warRes.data ?? null;
  const genres = browseRes.data?.genres ?? [];
  const mine = factionOf(account?.faction);
  const viewerFaction = account?.faction ?? null;
  const live = home?.live ?? [];
  const spotlit = (home?.spotlights ?? []).filter(s => s.stream);
  const reasons = Object.fromEntries(spotlit.map(s => [s.stream!.username, s.reason]));
  const rotation = [...spotlit.map(s => s.stream!), ...live.filter(s => !reasons[s.username])].slice(0, 8);
  const staffPicks = (home?.spotlights ?? []).filter(s => !s.stream && s.user);

  return <div className="home">
    {war?.season?.finished && war.season.winners.length
      ? <SeasonReveal war={war} season={war.season} faction={isFaction(viewerFaction) ? viewerFaction : null} signedIn={!!account} />
      : <FrontLine war={war} faction={isFaction(viewerFaction) ? viewerFaction : null} signedIn={!!account} />}

    {!home && <p className="notice" role="alert">Live channels couldn&apos;t be loaded. Please refresh.</p>}

    {rotation.length > 0 && <section aria-labelledby="rot-h" className="home-section">
      <SectionHead id="rot-h" title="MAGNet rotation" note="Every live stream gets a turn here, whether it has 3 viewers or 3,000." level={1} />
      <Rotation streams={rotation} reasons={reasons} viewerFaction={viewerFaction} />
      {staffPicks.length > 0 && <ul className="staff-picks">{staffPicks.map(s => <li key={s.user!.username}><span className="eyebrow">Staff pick</span> <Link href={`/${s.user!.username}`}>{s.user!.display_name}</Link> · {s.reason} <span className="muted">Offline now</span></li>)}</ul>}
    </section>}

    <section aria-labelledby="live-h" className="home-section">
      <SectionHead id="live-h" title="Live now" note="Ordered by MAGNet, not by viewer count" level={rotation.length ? 2 : 1} href="/browse" link="View all" />
      {live.length
        ? <StreamGrid streams={live} viewerFaction={viewerFaction} languages={languages} />
        : <div className="empty-live panel">
          <p><strong>Nothing live right now.</strong> These channels were live recently; follow them to hear when they&apos;re back.</p>
          {home && <RecentChannels recent={home.recent} />}
          <p><Link href="/war-map" className="button quiet">See the war map</Link> {account ? <Link href="/studio/stream" className="button">Go live</Link> : <Link href="/signup" className="button">Enlist</Link>}</p>
        </div>}
    </section>

    {mine && (home?.faction?.length ?? 0) > 0 && <section aria-labelledby="fac-h" className="home-section">
      <SectionHead id="fac-h" title={`From ${mine.name}`} note="Live from your side" href={`/browse?faction=${mine.slug}`} link="View all" />
      <StreamGrid streams={home!.faction!} viewerFaction={viewerFaction} languages={languages} />
    </section>}

    {beacons.length > 0 && <section aria-labelledby="bea-h" className="home-section frame beacon-shelf">
      <SectionHead id="bea-h" title="Beacons" note="Short videos that lead to the stream" href="/beacons" link="Open the feed" />
      <BeaconGrid items={beacons} />
    </section>}

    {(home?.fresh.length ?? 0) > 0 && <section aria-labelledby="new-h" className="home-section">
      <SectionHead id="new-h" title="Just went live" note="MAGNet gives fresh streams a head start." />
      <StreamGrid streams={home!.fresh} viewerFaction={viewerFaction} languages={languages} />
    </section>}

    {genres.length > 0 && <section aria-labelledby="ter-h" className="home-section">
      <SectionHead id="ter-h" title="Territories" note={war?.season ? "Every category is held by a faction this season" : "Control opens when the first season starts"} href="/browse" link="View all" />
      <TerritoryGrid genres={genres} war={war} viewerFaction={viewerFaction} limit={8} href={(g, c) => `/browse?genre=${g.id}&category=${c.id}`} />
    </section>}

    {clips.length > 0 && <section aria-labelledby="clips-h" className="home-section"><SectionHead id="clips-h" title="Latest clips" note="The newest moments from each channel" /><VideoGrid items={clips} /></section>}
  </div>;
}
