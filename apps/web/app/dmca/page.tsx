import type { Metadata } from "next";
import { PolicyPage } from "../../components/SitePage";
import { CopyrightForm } from "../../components/CopyrightForm";
import { apiGet } from "../../lib/server-api";

export const metadata: Metadata = {
  title: "Copyright & DMCA | S.V.E.R",
  description: "How to send S.V.E.R a copyright infringement notice or counter-notification.",
  alternates: { canonical: "https://sver.tv/dmca" },
};

export default async function DmcaPage() {
  const config = await apiGet<{ turnstile_site_key: string }>("/api/auth/config");
  return <PolicyPage path="/dmca" title="Copyright & DMCA" intro="Respecting creators’ work and handling copyright concerns." summary={[
    "Share only content you have the right to use.",
    "Copyright owners or authorized representatives can email an infringement notice.",
    "Uploaders can send a counter-notice if removal was a mistake or misidentification.",
    "Notices must meet the legal requirements; read the full process below.",
  ]} sections={[
    { title: "Report copyright infringement", content: <>
      <p>S.V.E.R respects intellectual property rights. Only stream or share content you have the right to use. Copyright owners and their authorized representatives can send notices to <a href="mailto:dmca@sver.tv">dmca@sver.tv</a> without creating an account.</p>
      <p>Identify the copyrighted work and the material on S.V.E.R, with links precise enough for us to find it. Include your signature and contact details, a good-faith statement that the use is unauthorized by the owner, their agent or the law, and a statement of accuracy and authority made under penalty of perjury.</p>
      <p>Consult the <a href="https://www.copyright.gov/512/">U.S. Copyright Office’s notice requirements</a> for the full legal requirements, including the contact information a notice must contain. Consider whether permission or a legal exception applies before sending a notice.</p>
    </> },
    { title: "Submit a copyright notice (videos, clips and Beacons)", content: config.data?.turnstile_site_key ? <CopyrightForm sitekey={config.data.turnstile_site_key} /> : <p>The form could not load. Please email <a href="mailto:dmca@sver.tv">dmca@sver.tv</a> with your notice.</p> },
    { title: "What happens next", content: <>
      <p>We review notices and act expeditiously on valid claims, which may include removing material or disabling access. We may request missing information and notify the uploader of the claim. A notice may be shared with the uploader, including the information necessary to understand and respond to it.</p>
      <p>S.V.E.R terminates accounts of repeat infringers in appropriate circumstances. Copyright notices must not be used to harass people or remove content you do not own.</p>
    </> },
    { title: "Counter-notifications", content: <>
      <p>If your material was removed by mistake or misidentification, you may send a signed counter-notification to <a href="mailto:dmca@sver.tv">dmca@sver.tv</a>. It must identify the removed material and its former location, provide your contact details, and include the required perjury, court-jurisdiction and service-of-process statements. The rules differ for people outside the United States; use the <a href="https://www.copyright.gov/512/">Copyright Office’s counter-notification requirements</a> before submitting.</p>
      <p>We may forward a valid counter-notification to the original claimant. Restoration generally follows the statutory 10–14 business-day process unless we receive notice of a court action seeking to restrain infringement. Other Platform rules can independently prevent restoration.</p>
    </> },
    { title: "Copyright contact", content: <>
      <p>Send notices and counter-notices to our designated agent. Email is fastest.</p>
      <address className="dmca-agent">
        Copyright Agent<br />
        SVER LLC<br />
        4030 Wake Forest Road, Suite 349<br />
        Raleigh, NC 27609<br />
        Phone: <a href="tel:+12526639474">(252) 663-9474</a><br />
        Email: <a href="mailto:dmca@sver.tv">dmca@sver.tv</a>
      </address>
      <p>This agent is registered in the <a href="https://www.copyright.gov/dmca-directory/">Copyright Office directory</a> under registration number DMCA-1081854.</p>
      <p>For safety concerns unrelated to copyright, email <a href="mailto:safety@sver.tv">safety@sver.tv</a>.</p>
    </> },
  ]} />;
}
