"use client";
import { useCallback, useState } from "react";
import { send } from "../lib/client-api";
import { Turnstile } from "./Turnstile";
export function CopyrightForm({ sitekey, caseId, location = "", onSaved }: { sitekey?: string; caseId?: string; location?: string; onSaved?: () => void }) {
  const [token, setToken] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);
  const onToken = useCallback((token: string) => setToken(token), []);
  if (done) return <p role="status">{message}</p>;
  return <form onSubmit={async e => {
    e.preventDefault(); if (busy) return;
    const form = new FormData(e.currentTarget);
    const body = { ...Object.fromEntries(form.entries()), good_faith: form.has("good_faith"), perjury: form.has("perjury"), jurisdiction: form.has("jurisdiction"), turnstile: token };
    setBusy(true);
    const result = await send<{ id?: string }>("POST", caseId ? `/api/me/copyright/${caseId}/counter` : "/api/copyright", body);
    setBusy(false);
    if (result.ok) { setDone(true); setMessage(caseId ? "Your counter-notice was received for review." : `Your notice was received. Reference: ${result.data.id}. Keep this reference for correspondence with dmca@sver.tv.`); onSaved?.(); }
    else setMessage(result.error);
  }}>
    <p>{caseId ? "Your signed counter-notice, including your contact details, will be forwarded to the original claimant after review." : "Copyright owners and authorized representatives may submit a notice without an account. Valid notices may be shared with the uploader, including contact details."}</p>
    {[['name', 'Full legal name', 'text', 150], ['email', 'Email', 'email', 254], ['phone', 'Phone', 'tel', 80], ['address', 'Mailing address', 'text', 500], ['location', 'Full S.V.E.R video, clip or Beacon link', 'url', 2048]] .map(([name, label, type, limit]) => <label className="field" key={name}><span>{label}</span><input name={String(name)} type={String(type)} maxLength={Number(limit)} defaultValue={name === "location" ? location : ""} required /></label>)}
    <label className="field"><span>{caseId ? "Explain the mistake or misidentification" : "Identify the copyrighted work and the infringing material; explain your ownership or authority"}</span><textarea name="description" required rows={6} maxLength={6000} /></label>
    <label className="checkbox"><input name="good_faith" type="checkbox" required />{caseId ? "I have a good-faith belief that the material was removed or disabled by mistake or misidentification." : "I have a good-faith belief that the use described is not authorized by the copyright owner, their agent or the law."}</label>
    <label className="checkbox"><input name="perjury" type="checkbox" required />{caseId ? "I declare under penalty of perjury that my good-faith statement above is true." : "The information in this notice is accurate, and under penalty of perjury I am the copyright owner or authorized to act for the owner of the exclusive right allegedly infringed."}</label>
    {caseId && <label className="checkbox"><input name="jurisdiction" type="checkbox" required />I consent to the jurisdiction of the Federal District Court for the judicial district where my address is located, or, if outside the United States, any judicial district in which SVER LLC may be found. I will accept service of process from the original claimant or their agent.</label>}
    <label className="field"><span>Electronic signature (full legal name)</span><input name="signature" required maxLength={150} /></label>
    {sitekey && <Turnstile sitekey={sitekey} action="copyright" onToken={onToken} />}
    <button disabled={busy || (!caseId && !token)}>{busy ? "Sending…" : caseId ? "Submit counter-notice" : "Submit copyright notice"}</button>
    {message && <p role="alert">{message}</p>}
  </form>;
}
