"use client";
import Image from "next/image";
import Link from "next/link";
import { FormEvent, useCallback, useEffect, useRef, useState } from "react";
import type { AnimationSequence } from "motion";
import { SignupSteps } from "../screens";
import { Avatar } from "../../components/Avatar";
import { Crest } from "../../components/Crest";
import { useChoreography } from "../../lib/choreography";
import { send, useLoad } from "../../lib/client-api";
import { FACTIONS, crestSrc, factionOf, type FactionSlug } from "../../lib/factions";
import type { Chip, Sizes } from "../../lib/types";

type Step = "side" | "joined" | "profile" | "follow" | "ready";
type Standing = { faction: FactionSlug | null; can_choose: boolean; free_switch_until: string | null };
type Profile = { display_name: string; bio: string; avatar: Sizes; revisions: { profile: number } };

/**
 * The onboarding wizard (legacy /onboarding, rebuilt on S.V.E.R's rules): choose your side with
 * each faction's story, the welcome to your faction, profile, follow creators, ready. The side
 * can't be skipped (every account has a faction, docs/FACTIONS.md); profile and follows can.
 */
export function Onboarding({ username, current, pick }: { username: string; current: FactionSlug | null; pick: string | null }) {
  const [step, setStep] = useState<Step>("side");
  const [faction, setFaction] = useState<FactionSlug | null>(current);
  const [followed, setFollowed] = useState(0);
  // The ceremony plays once per enlistment: not when Back returns to this step.
  const [ceremony, setCeremony] = useState(false);
  const played = useCallback(() => setCeremony(false), []);

  const go = useCallback((next: Step) => { setStep(next); window.scrollTo({ top: 0 }); }, []);
  const number = ({ side: 2, joined: 2, profile: 3, follow: 4, ready: 5 } as const)[step];

  return <div className="onboarding">
    <SignupSteps current={number} />
    {step === "side" && <ChooseSide current={current} pick={factionOf(pick)?.slug ?? null} onJoined={slug => { setFaction(slug); setCeremony(true); document.documentElement.dataset.theme = slug; go("joined"); }} onKeep={() => go("profile")} />}
    {step === "joined" && faction && <Joined slug={faction} play={ceremony} onPlayed={played} onNext={() => go("profile")} />}
    {step === "profile" && <ProfileStep username={username} onNext={() => go("follow")} onBack={() => go("joined")} />}
    {step === "follow" && <FollowStep onNext={count => { setFollowed(count); go("ready"); }} onBack={() => go("profile")} />}
    {step === "ready" && <Ready slug={faction} followed={followed} username={username} />}
  </div>;
}

function ChooseSide({ current, pick, onJoined, onKeep }: { current: FactionSlug | null; pick: FactionSlug | null; onJoined: (slug: FactionSlug) => void; onKeep: () => void }) {
  const [standing, setStanding] = useState<Standing | null>(null);
  const [choice, setChoice] = useState<FactionSlug | null>(pick ?? current);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => { send<Standing>("GET", "/api/me/faction").then(r => r.ok && setStanding(r.data)); }, []);
  const chosen = factionOf(choice);
  const locked = !!standing && !standing.can_choose;

  async function enlist() {
    if (!choice) return;
    if (choice === standing?.faction) { onKeep(); return; }
    setBusy(true); setError("");
    const result = await send<Standing>("PUT", "/api/me/faction", { faction: choice });
    setBusy(false);
    if (!result.ok) { setError(result.error); return; }
    onJoined(choice);
  }

  return <section className="onboarding-step wide" aria-labelledby="side-title">
    <header className="onboarding-head">
      <h1 id="side-title">Choose your side</h1>
      <p>Every kind of creator belongs here. Your faction isn&apos;t about what you stream; it&apos;s about how you show up. Pick the people whose beliefs feel like yours.</p>
    </header>
    {locked && current && <p className="notice">You&apos;re in {factionOf(current)?.name}. Your free switch has been used or has ended; you can switch between seasons.</p>}
    {error && <p className="notice error" role="alert">{error}</p>}
    <div className="faction-picks" role="radiogroup" aria-label="Faction">
      {FACTIONS.map(f => <button key={f.slug} type="button" role="radio" aria-checked={choice === f.slug} className={`faction-pick ${f.slug}`} disabled={locked && f.slug !== current} onClick={() => setChoice(f.slug)}>
        <Image src={crestSrc(f.slug)} width={120} height={120} alt="" unoptimized />
        <span className="faction-pick-name">{f.name}</span>
        <span className="eyebrow">{f.title}</span>
        <span className="faction-pick-creed">{f.creed}</span>
        <span className="small-text">{f.belief}</span>
        {current === f.slug && <span className="badge verified">Your side</span>}
      </button>)}
    </div>
    {chosen && <article className={`faction-story panel ${chosen.slug}`} aria-label={`About ${chosen.name}`}>
      <p className="faction-story-lore">{chosen.lore}</p>
      <dl>
        <div><dt>Who belongs here</dt><dd>{chosen.people}</dd></div>
        <div><dt>What we reject</dt><dd>{chosen.rejects}</dd></div>
        <div><dt>Home turf</dt><dd>{chosen.turf.join(" · ")}</dd></div>
      </dl>
      <ul className="trait-chips">{chosen.values.map(v => <li key={v}>{v}</li>)}</ul>
    </article>}
    <p className="small-text center">No faction is better than another. You get one free switch in your first 7 days, then only between seasons.</p>
    <div className="onboarding-actions">
      <button type="button" onClick={enlist} disabled={!choice || busy || (locked && choice !== current)} data-faction={choice ?? undefined}>{busy ? "Please wait…" : !chosen ? "Pick a faction" : choice === current ? `Continue with ${chosen.name}` : `Enlist in ${chosen.name}`}</button>
    </div>
  </section>;
}

/** The welcome ceremony (docs/MOTION.md §1), about 2.6 s with overlaps. */
const ceremony = (root: HTMLElement, { stagger }: typeof import("motion")): AnimationSequence => {
  const $ = (selector: string) => root.querySelectorAll(selector);
  const fade = { opacity: [0, 1] };
  const rise = (px: number) => ({ opacity: [0, 1], transform: [`translateY(${px}px)`, "translateY(0px)"] });
  return [
    [$(".ceremony-corner"), { opacity: [0, 1], transform: ["scale(0)", "scale(1)"] }, { at: 0, duration: 0.4 }],
    [$(".joined-crest img"), { opacity: [0, 1], transform: ["translateY(16px) scale(0.92)", "translateY(0px) scale(1)"] }, { at: 0.2, duration: 0.5 }],
    [$(".relic"), { opacity: [0, 0.2] }, { at: 0.5, duration: 0.3 }],
    [$(".relic path"), { strokeDashoffset: [1, 0] }, { at: 0.5, duration: 0.8 }],
    [$(".joined h1 .line"), rise(12), { at: 0.8, duration: 0.4, delay: stagger(0.15) }],
    [$(".joined .eyebrow"), fade, { at: 1.3, duration: 0.3 }],
    [$(".joined-line"), fade, { at: 1.45, duration: 0.3 }],
    [$(".joined-lore, .joined-call"), fade, { at: 1.7, duration: 0.4 }],
    [$(".joined-grid > div"), rise(8), { at: 2.1, duration: 0.25, delay: stagger(0.08) }],
    [$(".joined-cry, .joined .primary"), fade, { at: 2.4, duration: 0.2 }],
  ];
};

function Joined({ slug, play, onPlayed, onNext }: { slug: FactionSlug; play: boolean; onPlayed: () => void; onNext: () => void }) {
  const f = factionOf(slug)!;
  const [until, setUntil] = useState<string | null>(null);
  const [playing, setPlaying] = useState(play);
  const root = useRef<HTMLElement>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const skip = useChoreography(root, playing ? ceremony : null, () => setPlaying(false));
  useEffect(() => { send<Standing>("GET", "/api/me/faction").then(r => r.ok && setUntil(r.data.free_switch_until)); }, []);
  useEffect(() => { heading.current?.focus({ preventScroll: true }); }, []);
  useEffect(() => { if (play) onPlayed(); }, [play, onPlayed]);
  return <section ref={root} className={playing ? "onboarding-step joined frame ceremony" : "onboarding-step joined frame"} aria-labelledby="joined-title">
    {playing && ["tl", "tr", "bl", "br"].map(c => <i key={c} className={`ceremony-corner ${c}`} data-play aria-hidden="true" />)}
    <span className="joined-crest">
      <Relic slug={slug} />
      <Image src={crestSrc(slug)} width={120} height={120} alt={`${f.name} crest`} unoptimized data-play />
    </span>
    <h1 id="joined-title" ref={heading} tabIndex={-1}><span className="line" data-play>Welcome to</span> <span className="line" data-play>{f.name}</span></h1>
    <p className="eyebrow" data-play>{f.title} · {f.creed}</p>
    <p className="joined-line" data-play>{f.line}</p>
    <p className="joined-lore" data-play>{f.lore}</p>
    <p className="joined-call" data-play>You chose how you want to show up. Now make it real.</p>
    <div className="joined-grid">
      <div data-play><strong>Your colors</strong><span>The whole site now wears {f.name}&apos;s colors.</span></div>
      <div data-play><strong>Your crest</strong><span>Shown on your channel, your player card and next to your name.</span></div>
      <div data-play><strong>Your side</strong><span><Link href={`/factions/${slug}`}>The {f.name} hub</Link> and <Link href="/war-map">the war map</Link> show where your side stands this season.</span></div>
      <div data-play><strong>Your switch</strong><span>{until ? `One free switch until ${new Date(until).toLocaleDateString(undefined, { month: "long", day: "numeric" })}, then only between seasons.` : "Switching opens between seasons."}</span></div>
    </div>
    <p className="joined-cry" data-play>{f.battleCry}</p>
    <button type="button" className="primary" onClick={onNext} data-play>Continue</button>
    {playing && <button type="button" className="link-button ceremony-skip" onClick={skip}>Skip</button>}
  </section>;
}

/** Four-pointed star centred on (x, y), for Aetheron's relic. */
const star = (x: number, y: number, r: number) => `M${x} ${y - r}L${x + r / 3} ${y - r / 3}L${x + r} ${y}L${x + r / 3} ${y + r / 3}L${x} ${y + r}L${x - r / 3} ${y + r / 3}L${x - r} ${y}L${x - r / 3} ${y - r / 3}Z`;
const RELICS: Record<FactionSlug, string[]> = {
  // The Phoenix Flame, rising.
  myria: ["M60 108C34 100 28 76 40 58C44 70 50 74 54 72C46 52 54 30 70 14C68 34 82 44 88 62C94 80 84 102 60 108Z", "M60 100C50 94 50 82 58 72C60 82 68 86 68 92C68 97 64 100 60 100Z"],
  // The Pale Moon: a crescent with three stars.
  aetheron: ["M70 18A42 42 0 1 0 98 88A34 34 0 1 1 70 18Z", star(84, 40, 7), star(100, 58, 5), star(86, 70, 4)],
  // The Crown's outline.
  glint: ["M22 88L22 40L42 62L60 30L78 62L98 40L98 88Z", "M22 98L98 98"],
};
/** The faction's relic, a thin line behind the crest. Drawn in during the ceremony, then 20% opacity. */
function Relic({ slug }: { slug: FactionSlug }) {
  return <svg className="relic" viewBox="0 0 120 120" aria-hidden="true" data-play>{RELICS[slug].map(d => <path key={d} d={d} pathLength={1} />)}</svg>;
}

function ProfileStep({ username, onNext, onBack }: { username: string; onNext: () => void; onBack: () => void }) {
  const [profile, setProfile] = useState<Profile | null>(null);
  const [error, setError] = useState<{ message: string; field?: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [bio, setBio] = useState("");
  const load = useCallback(async () => {
    const r = await send<Profile>("GET", "/api/me/profile");
    if (r.ok) { setProfile(r.data); setBio(r.data.bio); }
  }, []);
  useLoad(load);

  async function upload(file: File | undefined) {
    if (!file) return;
    const form = new FormData();
    form.append("file", file);
    setBusy(true);
    const r = await send("POST", "/api/me/avatar", form);
    setBusy(false);
    if (!r.ok) setError({ message: r.error }); else { setError(null); await load(); }
  }
  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!profile) return;
    const form = new FormData(event.currentTarget);
    setBusy(true);
    const r = await send("PATCH", "/api/me/profile", { display_name: form.get("display_name"), bio: form.get("bio"), revision: profile.revisions.profile });
    setBusy(false);
    if (!r.ok) { setError({ message: r.error, field: r.field }); return; }
    onNext();
  }

  return <section className="onboarding-step frame" aria-labelledby="profile-title">
    <h1 id="profile-title" className="auth-title">Set up your profile</h1>
    <p className="intro">How should people know you? You can change all of this later in Settings.</p>
    {error && <p className="notice error" role="alert">{error.message}</p>}
    {!profile ? <p className="loading">Loading your profile…</p> : <form onSubmit={save}>
      <div className="avatar-row">
        <Avatar sizes={profile.avatar} name={profile.display_name || username} size={72} />
        <label className="button quiet small">{profile.avatar ? "Change avatar" : "Upload avatar"}<input className="sr-only" type="file" accept="image/jpeg,image/png,image/webp" disabled={busy} onChange={e => upload(e.target.files?.[0])} /></label>
        <span className="small-text">JPEG, PNG or WebP.</span>
      </div>
      <label className="field" htmlFor="display_name"><span>Display name</span><input id="display_name" name="display_name" defaultValue={profile.display_name} maxLength={32} aria-invalid={error?.field === "display_name" || undefined} /><small>Up to 32 characters. Your @{username} always shows next to it.</small></label>
      <label className="field" htmlFor="bio"><span>Bio</span><textarea id="bio" name="bio" value={bio} onChange={e => setBio(e.target.value)} maxLength={300} rows={4} aria-invalid={error?.field === "bio" || undefined} /><small>{bio.length}/300</small></label>
      <div className="onboarding-actions split">
        <button type="button" className="quiet" onClick={onBack}>Back</button>
        <span className="onboarding-actions">
          <button type="button" className="link-button" onClick={onNext}>I&apos;ll do this later</button>
          <button disabled={busy}>{busy ? "Saving…" : "Continue"}</button>
        </span>
      </div>
    </form>}
  </section>;
}

function FollowStep({ onNext, onBack }: { onNext: (count: number) => void; onBack: () => void }) {
  const [items, setItems] = useState<Chip[] | null>(null);
  const [following, setFollowing] = useState<Set<string>>(new Set());
  const [error, setError] = useState("");
  useEffect(() => { send<{ items: Chip[] }>("GET", "/api/me/suggestions").then(r => setItems(r.ok ? r.data.items : [])); }, []);

  async function toggle(name: string) {
    const on = following.has(name);
    const r = await send(on ? "DELETE" : "PUT", `/api/follows/${encodeURIComponent(name)}`);
    if (!r.ok) { setError(r.error); return; }
    setError("");
    setFollowing(prev => { const next = new Set(prev); if (on) next.delete(name); else next.add(name); return next; });
  }

  return <section className="onboarding-step wide" aria-labelledby="follow-title">
    <header className="onboarding-head">
      <h1 id="follow-title">Follow some creators</h1>
      <p>You&apos;ll get an alert when they go live. Live channels and your own side come first.</p>
    </header>
    {error && <p className="notice error" role="alert">{error}</p>}
    {!items ? <p className="loading center">Finding creators…</p> : items.length === 0
      ? <div className="panel empty-follow"><strong>You&apos;re early.</strong><span>Nobody has streamed here yet. Channels show up here once they go live.</span></div>
      : <ul className="follow-grid">{items.map(c => {
        const name = c.username ?? "";
        const on = following.has(name);
        return <li key={name} className={on ? "follow-card on" : "follow-card"}>
          <Avatar sizes={c.avatar} name={c.display_name} size={56} />
          <span className="follow-text">
            <span className="follow-name">{c.display_name}</span>
            <span className="follow-faction"><Crest faction={c.faction ?? null} initial="" size={18} />{factionOf(c.faction)?.name ?? "No side yet"}{c.live && <span className="live-chip">Live</span>}</span>
          </span>
          <button type="button" className={on ? "small" : "quiet small"} aria-pressed={on} onClick={() => toggle(name)}>{on ? "Following" : "Follow"}</button>
        </li>;
      })}</ul>}
    <div className="onboarding-actions split">
      <button type="button" className="quiet" onClick={onBack}>Back</button>
      <span className="onboarding-actions">
        {following.size === 0 && <button type="button" className="link-button" onClick={() => onNext(0)}>Skip for now</button>}
        <button type="button" onClick={() => onNext(following.size)}>{following.size ? `Continue (${following.size} followed)` : "Continue"}</button>
      </span>
    </div>
  </section>;
}

function Ready({ slug, followed, username }: { slug: FactionSlug | null; followed: number; username: string }) {
  const f = factionOf(slug);
  const [verified, setVerified] = useState<boolean | null>(null);
  const [resent, setResent] = useState(false);
  useEffect(() => { send<{ email_verified: boolean }>("GET", "/api/auth/me").then(r => r.ok && setVerified(r.data.email_verified)); }, []);
  async function resend() {
    const r = await send("POST", "/api/auth/email/resend", {});
    setResent(r.ok);
  }
  return <section className="onboarding-step ready frame" aria-labelledby="ready-title">
    {f && <Image src={crestSrc(f.slug)} width={88} height={88} alt="" unoptimized />}
    <h1 id="ready-title" className="auth-title">You&apos;re in!</h1>
    <ul className="ready-chips">
      {f && <li>{f.name} · {f.title}</li>}
      <li>{followed} {followed === 1 ? "channel" : "channels"} followed</li>
    </ul>
    {verified === false && <div className="ready-email">
      <span className="eyebrow">Confirm your email</span>
      <p>We sent you a confirmation link; it works for 24 hours. You can browse and watch now. Chat and going live unlock once you confirm.</p>
      <button type="button" className="quiet small" onClick={resend} disabled={resent}>{resent ? "Email sent again" : "Resend the email"}</button>
    </div>}
    <nav className="ready-links" aria-label="Where to next">
      <Link href="/">Watch a stream</Link>
      <Link href="/studio/stream">Start streaming</Link>
      <Link href={`/${username}`}>Your channel</Link>
      <Link href="/factions">{f ? `${f.name} and the factions` : "The factions"}</Link>
    </nav>
    {/* A full load so the server renders the new faction theme everywhere. */}
    <button type="button" className="primary" onClick={() => window.location.assign("/")}>Enter S.V.E.R</button>
  </section>;
}
