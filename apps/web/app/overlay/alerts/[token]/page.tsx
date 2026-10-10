"use client";
import { useParams, useSearchParams } from "next/navigation";
import { useEffect, useRef, useState } from "react";
import "../../../../styles/overlays.css";

type Alert = { type: "alert"; id: number; kind: string; text: string; message: string | null; seconds: number; sound: "chime" | "horn" | "none"; volume: number };
type Settings = { goal: { kind: "followers" | "subs" | null; target: number; label: string } };
type Frame = { type: "hello"; settings: Settings; goal_count: number | null; recent: Alert[] } | { type: "settings"; settings: Settings; goal_count: number | null } | { type: "goal"; goal_count: number } | Alert;

/** A short tone made in the browser (no sound files to host or review). */
function play(sound: Alert["sound"], volume: number) {
  if (sound === "none" || volume <= 0) return;
  try {
    const audio = new AudioContext();
    const gain = audio.createGain();
    gain.gain.value = volume / 100 * 0.3;
    gain.connect(audio.destination);
    const notes = sound === "chime" ? [[880, 0], [1320, 0.12]] : [[220, 0], [330, 0.18]];
    for (const [frequency, at] of notes) {
      const tone = audio.createOscillator();
      tone.type = sound === "chime" ? "sine" : "sawtooth";
      tone.frequency.value = frequency;
      tone.connect(gain);
      tone.start(audio.currentTime + at);
      tone.stop(audio.currentTime + at + 0.35);
    }
    setTimeout(() => void audio.close(), 1200);
  } catch { /* no audio in this browser source */ }
}

/**
 * Alerts and overlays (docs/OVERLAYS.md): the streamer's OBS browser source. `?widget=goal` shows
 * the goal bar and `?widget=events` the recent list; without it, the alert box. The private token
 * in the URL is the only credential. Reconnects on its own.
 */
export default function AlertsOverlay() {
  const { token } = useParams<{ token: string }>();
  const widget = useSearchParams().get("widget") ?? "alerts";
  const [settings, setSettings] = useState<Settings | null>(null);
  const [count, setCount] = useState<number | null>(null);
  const [recent, setRecent] = useState<Alert[]>([]);
  const [showing, setShowing] = useState<Alert | null>(null);
  const queue = useRef<Alert[]>([]);
  const busy = useRef(false);

  useEffect(() => {
    let ws: WebSocket | null = null;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let stopped = false;
    let wait = 1000;
    const next = () => {
      const alert = queue.current.shift();
      if (!alert) { busy.current = false; setShowing(null); return; }
      busy.current = true;
      setShowing(alert);
      play(alert.sound, alert.volume);
      setTimeout(next, alert.seconds * 1000);
    };
    const connect = () => {
      ws = new WebSocket(`${location.origin.replace(/^http/, "ws")}/api/overlays/ws?token=${encodeURIComponent(token)}`);
      ws.onopen = () => { wait = 1000; };
      ws.onmessage = event => {
        const data = JSON.parse(event.data) as Frame;
        if (data.type === "hello") { setSettings(data.settings); setCount(data.goal_count); setRecent(data.recent); }
        else if (data.type === "settings") { setSettings(data.settings); setCount(data.goal_count); }
        else if (data.type === "goal") setCount(data.goal_count);
        else if (data.type === "alert") {
          setRecent(list => [data, ...list].slice(0, 5));
          queue.current.push(data);
          if (!busy.current) next();
        }
      };
      ws.onclose = () => { if (!stopped) { retry = setTimeout(connect, wait); wait = Math.min(wait * 2, 30000); } };
    };
    connect();
    return () => { stopped = true; clearTimeout(retry); ws?.close(); };
  }, [token]);

  const goal = settings?.goal;
  return <main className="alerts-overlay">
    <style>{"html,body{background:transparent!important}"}</style>
    {widget === "goal" ? goal?.kind && count !== null && <section className="overlay-goal" aria-label="Goal">
      <p><strong>{goal.label || (goal.kind === "followers" ? "Follower goal" : "Subscriber goal")}</strong> <span>{count.toLocaleString()} / {goal.target.toLocaleString()}</span></p>
      <span className="overlay-goal-bar" aria-hidden="true"><span style={{ width: `${Math.min(100, Math.round(100 * count / goal.target))}%` }} /></span>
    </section>
      : widget === "events" ? <ol className="overlay-recent" aria-label="Recent events">{recent.map(a => <li key={`${a.id}-${a.text}`}>{a.text}</li>)}</ol>
        : showing && <div className={`overlay-alert kind-${showing.kind}`} role="status" key={`${showing.id}-${showing.text}`} style={{ animationDuration: `${showing.seconds}s` }}>
          <p className="overlay-alert-text">{showing.text}</p>
          {showing.message && <p className="overlay-alert-message">{showing.message}</p>}
        </div>}
  </main>;
}
