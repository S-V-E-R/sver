"use client";
import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import type { AnimationSequence } from "motion";
import { Crest } from "./FactionIdentity";
import { FrontLine, holdsTheLight, seasonLine } from "./WarStanding";
import { useChoreography } from "../lib/choreography";
import { factions, factionInfo, type Faction } from "../lib/factions";
import { yearAF } from "../lib/lore";
import { type War, utcDate } from "../lib/war";

type Finished = NonNullable<War["season"]>;

// Per visit when the browser won't store it (docs/MOTION.md: "it plays once per visit").
const seenThisVisit = new Set<number>();
/** True the first time this browser sees the finished season; remembers it either way. */
function firstSight(season: number) {
  const key = `sver.season-reveal.${season}`;
  try {
    if (localStorage.getItem(key)) return false;
    localStorage.setItem(key, "1");
    return true;
  } catch {
    if (seenThisVisit.has(season)) return false;
    seenThisVisit.add(season);
    return true;
  }
}

/** The season reveal (docs/MOTION.md §2), 2.6 s, after which the banner settles. */
const reveal = (root: HTMLElement, { stagger }: typeof import("motion")): AnimationSequence => {
  const $ = (selector: string) => root.querySelectorAll(selector);
  const fade = { opacity: [0, 1] };
  const counts: AnimationSequence = [...root.querySelectorAll<HTMLElement>("[data-count]")].map(node =>
    [(v: number) => { node.textContent = String(Math.round(v)); }, [0, Number(node.dataset.count)], { at: 0.6, duration: 1 }]);
  return [
    [$(".season-reveal-year"), fade, { at: 0, duration: 0.3 }],
    [$(".season-reveal-crests li"), { opacity: [0, 1], transform: ["translateY(-24px)", "translateY(0px)"] }, { at: 0.3, duration: 0.35, delay: stagger(0.08) }],
    [$(".season-reveal-bar"), fade, { at: 0.6, duration: 0.2 }],
    [$(".season-reveal-bar i"), { transform: ["scaleX(0)", "scaleX(1)"] }, { at: 0.6, duration: 1 / 3, ease: "linear", delay: stagger(1 / 3) }],
    ...counts,
    [$(".season-reveal-crests .won .identity-crest"), { transform: ["translateY(0px)", "translateY(-10px)"] }, { at: 1.6, duration: 0.4 }],
    [$(".season-reveal-rule"), { opacity: [0, 1], transform: ["scaleX(0)", "scaleX(1)"] }, { at: 1.6, duration: 0.4 }],
    [$(".season-reveal h2"), fade, { at: 1.6, duration: 0.3 }],
    [$(".season-reveal-after"), fade, { at: 2.1, duration: 0.5 }],
  ];
};

/**
 * Home's front-line banner between seasons. The first visit after a season ends opens it into the
 * reveal with the real results, then it settles into the usual banner, which has a replay button.
 * The server renders the settled banner, so without JavaScript or with reduced motion that's all
 * anyone sees. The reveal pushes the page down while it plays and never covers it.
 */
export function SeasonReveal({ war, season, faction, signedIn }: { war: War; season: Finished; faction: Faction | null; signedIn: boolean }) {
  const [playing, setPlaying] = useState(false);
  const root = useRef<HTMLElement>(null);
  const skip = useChoreography(root, playing ? reveal : null, () => setPlaying(false));
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- whether this browser has seen the season is only known on the client
    if (!matchMedia("(prefers-reduced-motion: reduce)").matches && firstSight(season.number)) setPlaying(true);
  }, [season.number]);
  const settled = useRef(true);
  useEffect(() => {
    if (settled.current === !playing) return;
    settled.current = !playing;
    const heading = document.getElementById(playing ? "season-reveal-title" : "front-line-title");
    // Focus follows the heading only when it isn't taking it from something else (docs/MOTION.md rules).
    if (document.activeElement === document.body || document.activeElement?.closest(".season-reveal, .season-replay")) heading?.focus({ preventScroll: true });
  }, [playing]);

  if (!playing) return <FrontLine war={war} faction={faction} signedIn={signedIn}
    replay={<button type="button" className="season-replay" onClick={() => setPlaying(true)}>Replay Season {season.number}</button>} />;

  const board = factions.map(f => war.scoreboard.find(s => s.faction === f.slug) ?? { faction: f.slug as Faction, territories: 0 });
  const total = board.reduce((n, s) => n + s.territories, 0);
  const won = !!faction && season.winners.includes(faction);
  return <section ref={root} className="front-line frame season-reveal" aria-labelledby="season-reveal-title">
    <i className="season-reveal-rule" aria-hidden="true" data-play>{season.winners.map(w => <span key={w} data-theme={w} />)}</i>
    <p className="eyebrow season-reveal-year" data-play>Season {season.number} · {yearAF(season.number)} AF</p>
    <ul className="season-reveal-crests" aria-hidden="true">{factions.map(f => <li key={f.slug} className={season.winners.includes(f.slug) ? "won" : undefined} data-play><Crest faction={f.slug} size={56} /></li>)}</ul>
    {total > 0 && <div className="front-line-bar season-reveal-bar" aria-label="Territories held at the end of the season" data-play>
      {board.map(s => <span key={s.faction} data-theme={s.faction} style={{ flexGrow: Math.max(s.territories, 0.0001) }}><i aria-hidden="true" /><small>{factionInfo(s.faction).name} <b aria-hidden="true" data-count={s.territories}>{s.territories}</b><b className="sr-only">{s.territories}</b></small></span>)}
    </div>}
    <h2 id="season-reveal-title" tabIndex={-1} data-play>{holdsTheLight(season.winners)}</h2>
    <div className="season-reveal-after" data-play>
      <p>{seasonLine(season.winners, faction)}{won && <> <Link href={`/factions/${faction}`}>See your faction hub</Link></>}</p>
      <p className="muted">{war.previous_winners.length > 0 && <>Previous champions: {war.previous_winners.map(w => factionInfo(w).name).join(" and ")} · </>}Next season starts {utcDate(season.next_starts_at)}</p>
    </div>
    <button type="button" className="quiet small season-reveal-skip" onClick={skip}>Skip</button>
  </section>;
}
