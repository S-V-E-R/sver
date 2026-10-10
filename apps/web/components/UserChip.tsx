"use client";
import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { send } from "../lib/client-api";
import { joined, platformNames, type Chip, type Sizes } from "../lib/types";
import { Crest } from "./FactionIdentity";
import { Avatar } from "./Avatar";

type Card = { username: string; display_name: string; avatar: Sizes; bio: string; follower_count: number; joined_at: string; viewer: { signed_in: boolean; is_self: boolean; following: boolean; blocked: boolean }; live: boolean; level?: number; title?: string; frame?: number; also_known_as?: { platform: string; handle: string; url: string | null }[] };

/** A user's name and avatar; linked chips open the user card on click (docs/PROFILES.md, "User card"). */
export function UserChip({ user, size = 32 }: { user: Chip; size?: number }) {
  const [open, setOpen] = useState(false);
  const [card, setCard] = useState<Card | null>(null);
  const [error, setError] = useState("");
  const root = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent | KeyboardEvent) => {
      if (event instanceof KeyboardEvent ? event.key === "Escape" : !root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", close);
    return () => { document.removeEventListener("mousedown", close); document.removeEventListener("keydown", close); };
  }, [open]);
  if (!user.linked || !user.username) return <span className="chip"><Avatar sizes={null} name={user.display_name} size={size} /><span><strong>{user.display_name}</strong></span></span>;
  const username = user.username;
  async function toggle() {
    setOpen(!open);
    if (!card) {
      const result = await send<Card>("GET", `/api/users/${encodeURIComponent(username)}/card`);
      if (result.ok) setCard(result.data); else setError(result.error);
    }
  }
  async function follow() {
    if (!card) return;
    const result = await send<{ following: boolean; follower_count: number }>(card.viewer.following ? "DELETE" : "PUT", `/api/follows/${encodeURIComponent(username)}`);
    if (result.ok) setCard({ ...card, follower_count: result.data.follower_count, viewer: { ...card.viewer, following: result.data.following } });
  }
  return <span className="chip" ref={root}>
    <button type="button" className="chip-button" aria-expanded={open} onClick={toggle}><Avatar sizes={user.avatar} name={user.display_name} size={size} />{user.faction && <Crest faction={user.faction} size={18} />}<span><strong className="faction-name" data-faction={user.faction}>{user.display_name}</strong> <span className="handle">@{username}</span>{user.live && <> <span className="live badge">Live</span></>}</span></button>
    {open && <span className="user-card panel" role="dialog" aria-label={`${user.display_name} (@${username})`}>
      {error ? <span className="muted">{error}</span> : !card ? <span className="muted">Loading…</span> : <>
        <span className="card-head"><Avatar sizes={card.avatar} name={card.display_name} size={64} /><span><strong>{card.display_name}</strong><span className="handle">@{card.username}</span></span></span>
        {card.bio && <span className="card-bio">{card.bio}</span>}
        <span className="muted">{card.level ? <><span className="player-level" data-frame={card.frame ?? 0}>Level {card.level}</span>{card.title && ` ${card.title}`} · </> : ""}{card.follower_count.toLocaleString()} followers · Joined {joined(card.joined_at)}</span>
        {!!card.also_known_as?.length && <span className="card-aka"><span className="muted">Also known as</span> {card.also_known_as.map((a, i) => <span key={a.platform}>{i > 0 && " · "}{platformNames[a.platform] || a.platform}: {a.url ? <a href={a.url} rel="nofollow noopener noreferrer" target="_blank">{a.handle}</a> : a.handle}</span>)}</span>}
        <span className="card-actions">{card.live && <Link className="button small" href={`/${card.username}/live`}>Watch live</Link>}<Link className="button small" href={`/${card.username}`}>View channel</Link>{card.viewer.signed_in && !card.viewer.is_self && !card.viewer.blocked && <button type="button" className="small quiet" onClick={follow}>{card.viewer.following ? "Following" : "Follow"}</button>}</span>
      </>}
      <button type="button" className="small quiet card-close" onClick={() => setOpen(false)}>Close</button>
    </span>}
  </span>;
}
