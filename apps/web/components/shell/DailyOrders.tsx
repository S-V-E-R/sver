"use client";
import { useCallback, useState } from "react";
import { send, useLoad } from "../../lib/client-api";

type Order = { slot: number; label: string; rarity: string; target: number; progress: number; done: boolean; xp: number; ev?: number; ev_channel?: string | null };
type Orders = { orders: Order[]; streak?: number; week?: number; can_reroll?: boolean; verify?: boolean };

/** Sidebar daily orders (docs/PROGRESSION.md section 3): three a day, reset at midnight UTC. */
export function DailyOrders() {
  const [data, setData] = useState<Orders | null>(null);
  const load = useCallback(async () => {
    const r = await send<Orders>("GET", "/api/me/orders");
    if (r.ok) setData(r.data);
  }, []);
  useLoad(load);
  async function reroll(slot: number) {
    const r = await send<Orders>("POST", `/api/me/orders/${slot}/reroll`);
    if (r.ok) setData(r.data);
  }
  if (!data || (!data.orders.length && !data.verify)) return null;
  return <section className="side-section daily-orders frame" aria-labelledby="side-orders">
    <div className="side-label"><span id="side-orders">Daily orders</span>{!!data.streak && <span title="Days in a row with an order done">{data.streak}-day streak</span>}</div>
    {data.verify ? <p className="side-empty">Verify your email to get daily orders.</p>
      : <ul className="order-list">{data.orders.map(o => <li key={o.slot} className={o.done ? "done" : undefined} data-rarity={o.rarity.toLowerCase()}>
        <span className="order-label">{o.label}</span>
        <span className="order-meta"><span title={o.done && o.ev_channel ? `${o.ev} Engagement Valor in ${o.ev_channel}` : "Engagement Valor goes to the channel you're in when it's done"}>{o.rarity} · {o.xp} XP · {o.ev ?? 0} EV</span><span>{o.done ? "Done" : `${o.progress}/${o.target}`}</span></span>
        <span className="xp-bar" aria-hidden="true"><span style={{ width: `${Math.round(100 * o.progress / o.target)}%` }} /></span>
        {data.can_reroll && !o.done && <button type="button" className="quiet small" onClick={() => void reroll(o.slot)}>Reroll<span className="sr-only"> {o.label}</span></button>}
      </li>)}</ul>}
    {!data.verify && <p className="side-empty">{data.week ?? 0} done this week · milestones at 10, 20, 30</p>}
  </section>;
}
