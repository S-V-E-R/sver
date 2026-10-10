import Link from "next/link";
import { notFound } from "next/navigation";
import { StaffStepUp } from "../../components/StepUp";
import { apiGet } from "../../lib/server-api";
import "../../styles/profiles.css";

// Admin pages render only for staff with MFA; everyone else gets the site 404 (the API decides).
export const metadata = { title: "Admin | S.V.E.R", robots: { index: false, follow: false } };
export default async function AdminLayout({ children }: { children: React.ReactNode }) {
  const { status } = await apiGet("/api/admin/appeals");
  if (status !== 200) notFound();
  return <div className="settings-page">
    <nav className="settings-nav" aria-label="Admin"><Link href="/admin">Home</Link><span className="eyebrow">MODERATION</span><Link href="/admin/take-it-down">Take It Down</Link><Link href="/admin/reports">Reports</Link><Link href="/admin/copyright">Copyright</Link><Link href="/admin/appeals">Appeals</Link><Link href="/admin/bans">Bans</Link><Link href="/admin/streams">Live streams</Link><Link href="/admin/magnet">MAGNet</Link><Link href="/admin/categories">Categories</Link><Link href="/admin/factions">Factions</Link><Link href="/admin/guilds">Guilds</Link><Link href="/admin/integrity">Integrity</Link><Link href="/admin/shine">Good Works</Link><Link href="/admin/media">Media review</Link><Link href="/admin/parts">Setup parts</Link><span className="eyebrow">OPERATIONS</span><Link href="/admin/switches">Switches and banner</Link><Link href="/admin/money">Money</Link><Link href="/admin/ravens-eye">Raven&apos;s Eye</Link><Link href="/admin/jobs">Jobs</Link><Link href="/admin/audit">Audit log</Link></nav>
    <div className="settings-body">{children}</div>
    <StaffStepUp />
  </div>;
}
