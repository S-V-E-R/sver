"use client";
import Link from "next/link";
import { usePathname } from "next/navigation";

const GROUPS: [string, [string, string][]][] = [
  ["Teams", [["/studio/guilds", "Guilds"], ["/studio/squads", "Co-streams"]]],
  ["Stream", [["/studio/stream", "Live stream"], ["/studio/chat", "Chat"], ["/studio/commands", "Commands & bot"], ["/studio/emotes", "Emotes"], ["/studio/raids", "Raids & hosting"], ["/studio/restream", "Restream"], ["/studio/linked-chat", "Linked chat"], ["/studio/discord", "Discord"], ["/studio/overlays", "Alerts & overlays"], ["/studio/magnet", "MAGNet"], ["/studio/rewards", "Rewards"], ["/studio/board", "Board"], ["/studio/counters", "Counters"]]],
  ["Earnings", [["/studio/payouts", "Payouts"], ["/studio/shine", "Shine"]]],
  ["Recordings", [["/studio/videos", "Videos & clips"], ["/studio/beacons", "Beacons"], ["/studio/copyright", "Copyright"]]],
  ["Channel page", [["/studio/channel", "Overview"], ["/studio/channel/header", "Page header"], ["/studio/channel/song", "Song"], ["/studio/channel/war-council", "War Council"], ["/studio/channel/wall", "Wall"], ["/studio/channel/schedule", "Schedule"], ["/studio/channel/sponsors", "Sponsors"], ["/studio/channel/setup", "Streaming setup"], ["/studio/channel/blocks", "About blocks"], ["/studio/channel/fan-art", "Fan Art"]]],
];

/** Creator Studio navigation, grouped so the list scans quickly; the current page is marked. */
export function StudioNav({ username }: { username: string }) {
  const pathname = usePathname() ?? "";
  return <nav className="settings-nav studio-nav" aria-label="Creator Studio">
    <span className="eyebrow">Creator Studio</span>
    {GROUPS.map(([label, links]) => <div key={label} className="studio-nav-group">
      <span className="studio-nav-label">{label}</span>
      {links.map(([href, text]) => <Link key={href} href={href} aria-current={pathname === href ? "page" : undefined}>{text}</Link>)}
    </div>)}
    <Link href={`/${username}`} className="studio-view">View channel</Link>
  </nav>;
}
