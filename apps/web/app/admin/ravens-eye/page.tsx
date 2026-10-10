"use client";
import { useCallback, useState } from "react";
import { send, useLoad } from "../../../lib/client-api";

type Money = Record<string, { count: number; valor: number; usd_cents: number }>;
type Stats = {
  signups: number; verified_signups: number; viewers: number; chatters: number; messages: number; follows: number;
  broadcasts: number; streamers: number; broadcast_hours: number; watch_hours: number; peak_viewers: number;
  faction_joins: Record<string, number>; money: Money;
  top_channels: { username: string; watch_hours: number }[]; top_categories: { category: string; broadcast_hours: number }[];
};
type Day = { day: string; stats: Stats; partial: boolean };
const METRICS: [keyof Stats, string, "sum" | "max"][] = [
  ["signups", "Sign-ups", "sum"], ["viewers", "Signed-in viewers", "max"], ["watch_hours", "Watch hours", "sum"],
  ["peak_viewers", "Peak viewers", "max"], ["streamers", "Streamers live", "max"], ["broadcast_hours", "Broadcast hours", "sum"],
  ["messages", "Chat messages", "sum"], ["follows", "Follows", "sum"],
];
const usd = (cents: number) => `$${(cents / 100).toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
const round = (n: number) => Math.round(n * 10) / 10;

/** A small trend line; the label states the values in words for screen readers. */
function Spark({ values, label }: { values: number[]; label: string }) {
  const max = Math.max(1, ...values);
  const step = values.length > 1 ? 100 / (values.length - 1) : 100;
  const points = values.map((v, i) => `${(i * step).toFixed(1)},${(28 - 26 * v / max).toFixed(1)}`).join(" ");
  return <svg className="spark" viewBox="0 0 100 30" preserveAspectRatio="none" role="img" aria-label={`${label}: from ${values[0] ?? 0} to ${values.at(-1) ?? 0}, highest ${max}`}>
    <polyline points={points} fill="none" stroke="currentColor" strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
  </svg>;
}
/** Adds up a top list across days. */
function combine<T extends Record<string, string | number>>(days: Day[], pick: (s: Stats) => T[], key: keyof T, value: keyof T) {
  const totals = new Map<string, number>();
  for (const d of days) for (const row of pick(d.stats)) totals.set(String(row[key]), (totals.get(String(row[key])) ?? 0) + Number(row[value]));
  return [...totals].sort((a, b) => b[1] - a[1]).slice(0, 10);
}

/** Admin → Raven's Eye: platform analytics from daily rollups (docs/ADMIN.md). Staff only. */
export default function RavensEye() {
  const [range, setRange] = useState(30);
  const [days, setDays] = useState<Day[] | null>(null);
  const [message, setMessage] = useState("");
  const load = useCallback(async () => {
    const r = await send<{ days: Day[] }>("GET", `/api/admin/ravens-eye?days=${range}`);
    if (r.ok) { setDays(r.data.days); setMessage(""); } else setMessage(r.error);
  }, [range]);
  useLoad(load);
  const money: Money = {};
  for (const d of days ?? []) for (const [kind, m] of Object.entries(d.stats.money)) {
    const t = money[kind] ?? { count: 0, valor: 0, usd_cents: 0 };
    money[kind] = { count: t.count + m.count, valor: t.valor + m.valor, usd_cents: t.usd_cents + m.usd_cents };
  }
  const factions: Record<string, number> = {};
  for (const d of days ?? []) for (const [f, n] of Object.entries(d.stats.faction_joins)) factions[f] = (factions[f] ?? 0) + n;
  return <section className="panel section ravens-eye"><h1>Raven&apos;s Eye</h1>
    <p className="muted">Platform numbers by UTC day. Finished days are kept once; today is so far. Watch hours and peaks count real (Counted and Trusted) viewers only. Chat counts exist from the day this started, since chat expires after 7 days.</p>
    <div className="row wrap" role="group" aria-label="Range">{[7, 30, 90].map(n => <button key={n} type="button" className={n === range ? "small" : "small quiet"} aria-pressed={n === range} onClick={() => setRange(n)}>{n} days</button>)}</div>
    {message && <p role="alert" className="form-message error">{message}</p>}
    {!days ? <p className="loading">Loading…</p> : <>
      <div className="stat-grid">{METRICS.map(([key, label, how]) => {
        const values = days.map(d => Number(d.stats[key]) || 0);
        const total = how === "sum" ? values.reduce((a, b) => a + b, 0) : Math.max(0, ...values);
        return <div key={key} className="stat-tile">
          <span className="muted small">{label}{how === "max" ? " (best day)" : ""}</span>
          <strong>{round(total).toLocaleString()}</strong>
          <Spark values={values} label={label} />
        </div>;
      })}</div>
      <h2>Money by kind</h2>
      <div className="table-scroll"><table className="table"><thead><tr><th scope="col">Kind</th><th scope="col">Transactions</th><th scope="col">Valor moved</th><th scope="col">USD moved</th></tr></thead>
        <tbody>{Object.entries(money).sort((a, b) => b[1].usd_cents - a[1].usd_cents).map(([kind, m]) => <tr key={kind}><th scope="row">{kind.replaceAll("_", " ")}</th><td>{m.count}</td><td>{m.valor.toLocaleString()}</td><td>{usd(m.usd_cents)}</td></tr>)}</tbody></table></div>
      <div className="row wrap top-lists">
        <section><h2>Top channels by watch hours</h2><ol>{combine(days, s => s.top_channels, "username", "watch_hours").map(([name, h]) => <li key={name}>{name} · {round(h)} h</li>)}</ol></section>
        <section><h2>Top categories by broadcast hours</h2><ol>{combine(days, s => s.top_categories, "category", "broadcast_hours").map(([name, h]) => <li key={name}>{name} · {round(h)} h</li>)}</ol></section>
        <section><h2>Faction joins</h2><ul>{Object.entries(factions).map(([f, n]) => <li key={f}>{f[0].toUpperCase() + f.slice(1)} · {n}</li>)}</ul></section>
      </div>
    </>}
  </section>;
}
