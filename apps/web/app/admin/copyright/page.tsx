"use client";
import Link from "next/link";
import { useCallback, useState } from "react";
import { send, useLoad } from "../../../lib/client-api";
import { RecordedPlayer } from "../../../components/RecordedPlayer";
import { BeaconEvidence } from "../../../components/BeaconEvidence";
type Case = { id: string; video_id: string | null; beacon_id: string | null; owner_id: string | null; status: string; created_at: string; restore_after: string | null; restore_by: string | null; forward_state: string | null; reason: string | null };
type Details = { case: Case; notice: Record<string, string | boolean>; counter: Record<string, string | boolean> | null };
export default function CopyrightAdmin() {
  const [queue, setQueue] = useState<{ cases: Case[]; agent_renew_by: string; agent_renewal_due: boolean } | null>(null);
  const [selected, setSelected] = useState<Details | null>(null);
  const [message, setMessage] = useState("");
  const load = useCallback(async () => { const r = await send<typeof queue>("GET", "/api/admin/copyright"); if (r.ok) setQueue(r.data); else setMessage(r.error); }, []);
  useLoad(load);
  async function open(id: string) { const r = await send<Details>("GET", `/api/admin/copyright/${id}`); if (r.ok) setSelected(r.data); else setMessage(r.error); }
  const c = selected?.case;
  return <><h1>Copyright</h1><p className={queue?.agent_renewal_due ? "notice" : "muted"}>Designated-agent registration DMCA-1081854 must be renewed by {queue?.agent_renew_by ?? "2029-10-04"}.</p><p>Validate each notice and counter-notice before acting. A claimant&apos;s notice of a filed court action stops counter-notice restoration.</p><ul className="list">{queue?.cases.map(c => <li key={c.id}><button className="quiet" onClick={() => void open(c.id)}>{c.status.replaceAll("_", " ")} · {c.id.slice(0, 8)} · {new Date(c.created_at).toLocaleDateString()}</button>{c.restore_by && <span> Restore by {new Date(c.restore_by).toLocaleDateString()} · Forwarding: {c.forward_state}</span>}</li>)}</ul>
    {selected && c && <section className="panel video-manage"><h2>Case {c.id}</h2><p>{c.status}</p>{c.video_id ? <RecordedPlayer id={c.video_id} review /> : c.beacon_id && <BeaconEvidence id={c.beacon_id} />}{([['Notice', selected.notice], ['Counter-notice', selected.counter]] as const).map(([title, notice]) => notice && <details key={title} open><summary>{title}</summary><dl>{Object.entries(notice).map(([key, value]) => <div key={key}><dt>{key}</dt><dd>{String(value)}</dd></div>)}</dl></details>)}
      <form onSubmit={async e => { e.preventDefault(); const f = new FormData(e.currentTarget); const result = await send("POST", `/api/admin/copyright/${c.id}`, { action: f.get("action"), reason: f.get("reason") }); setMessage(result.ok ? "Decision saved." : result.error); if (result.ok) { await load(); await open(c.id); } }}>
        <label className="field"><span>Decision</span><select name="action" required defaultValue=""><option value="" disabled>Choose…</option>{c.status === "OPEN" && <><option value="remove">Valid notice: disable access and notify</option><option value="reject">Reject invalid notice</option></>}{c.status === "COUNTER_PENDING" && <><option value="accept_counter">Valid counter-notice: forward to claimant</option><option value="reject_counter">Counter-notice incomplete or invalid</option></>}{['REMOVED','COUNTER_PENDING','COUNTER'].includes(c.status) && <option value="litigation">Court action filed: retain hold</option>}{c.status === "COUNTER" && <option value="restore">Restore after statutory period</option>}</select></label><label className="field"><span>Reason / court-action reference</span><textarea name="reason" required maxLength={1000} /></label><button>Record decision</button>
      </form><p>Review repeated upheld claims before restricting recording or taking an account action. <Link href="/admin/bans">Account bans</Link></p>
    </section>}{message && <p role="status">{message}</p>}</>;
}
