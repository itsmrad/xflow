import type { Metadata } from "next";
import { currentUser } from "@clerk/nextjs/server";
import { redirect } from "next/navigation";
import { proProductIds } from "@/lib/billing";
import { isEntitled } from "@/lib/entitlement";
import { userSubscriptions, userTokens } from "@/lib/server/account";
import { initialUsage, sealUsage, usageWindowStart } from "@/lib/server/realtime";
import { ensureUser } from "@/lib/server/users";
import { LiveUsage } from "./live-usage";
import { UserButton } from "@clerk/nextjs";
import { Crossings } from "@/components/frame";
import { Badge, buttonClass, Callout } from "@/components/ui";

export const metadata: Metadata = { title: "Dashboard" };

export default async function Dashboard({ searchParams }: PageProps<"/dashboard">) {
  const clerk = await currentUser();
  if (!clerk) redirect("/sign-in");
  const userId = await ensureUser(clerk.id, clerk.primaryEmailAddress?.emailAddress);
  const since = usageWindowStart();
  const [subs, tokens, sealed, usage, params] = await Promise.all([
    userSubscriptions(userId),
    userTokens(userId),
    sealUsage(userId, since),
    initialUsage(userId, since),
    searchParams,
  ]);
  const pro = isEntitled(subs, new Date(), proProductIds());
  const current = subs[0];
  const realtimeUrl = process.env.NEXT_PUBLIC_NEON_REALTIME_URL;

  const email = clerk.primaryEmailAddress?.emailAddress ?? clerk.id;
  const until = current?.currentPeriodEnd?.toISOString().slice(0, 10);

  return (
    <section className="frame rule -mt-px">
      <Crossings />
      <div className="grid lg:grid-cols-12">
        <aside className="border-b border-line lg:col-span-3 lg:border-r lg:border-b-0">
          <div className="p-[var(--cell-p)] lg:sticky lg:top-16">
            <div className="flex items-center gap-3">
              <span className="inline-flex size-8 items-center justify-center">
                <UserButton />
              </span>
              <span className="t-body-sm min-w-0 truncate text-fg-muted">{email}</span>
            </div>
            <nav aria-label="Dashboard" className="mt-6">
              <ul className="flex gap-1 overflow-x-auto lg:flex-col">
                {[
                  ["#plan", "Plan"],
                  ["#usage", "Usage"],
                  ["#tokens", "CLI tokens"],
                ].map(([href, label]) => (
                  <li key={href}>
                    <a href={href} className="t-label block whitespace-nowrap rounded-sm px-3 py-2 text-fg-muted hover:bg-ink-a4 hover:text-fg">
                      {label}
                    </a>
                  </li>
                ))}
              </ul>
            </nav>
          </div>
        </aside>
        <div className="min-w-0 lg:col-span-9">
          <div className="border-b border-line p-[var(--cell-p)]">
            <h1 className="t-h1 text-fg">Dashboard</h1>
          </div>

          <section id="plan" aria-labelledby="plan-title" className="border-b border-line p-[var(--cell-p)]">
            <div className="flex flex-wrap items-center gap-3">
              <h2 id="plan-title" className="t-mono-label text-fg-subtle">
                Plan
              </h2>
              {pro ? <Badge tone="PRO">Pro</Badge> : <Badge>Free · BYOK</Badge>}
            </div>
            {params.checkout === "done" && !pro && (
              <div className="mt-4">
                <Callout type="note" title="Payment received.">
                  Pro turns on as soon as the payment confirms. You don&apos;t need to pay again.
                </Callout>
              </div>
            )}
            {current?.status === "past_due" && (
              <div className="mt-4">
                <Callout type="warn" title="Your last payment failed.">
                  Update your card to keep Pro.
                </Callout>
              </div>
            )}
            <p className="t-body mt-4 max-w-[56ch] text-fg-muted">
              {pro
                ? until
                  ? current?.cancelAtPeriodEnd
                    ? `Pro until ${until}. You won't be charged again.`
                    : `Renews ${until}.`
                  : "Pro is active."
                : "You're using your own key. Pro is $5/month for unlimited hosted transcription and cleanup."}
            </p>
            <div className="mt-6 flex flex-wrap gap-3">
              <form method="post" action={pro || current ? "/api/v1/billing/portal" : "/api/v1/billing/checkout"}>
                <button type="submit" className={buttonClass(pro || current ? "secondary" : "primary", "md")}>
                  {pro || current ? "Manage billing" : "Get Pro — $5/mo"}
                </button>
              </form>
              {!pro && current && (
                <form method="post" action="/api/v1/billing/checkout">
                  <button type="submit" className={buttonClass("primary", "md")}>
                    Subscribe again
                  </button>
                </form>
              )}
            </div>
          </section>

          <section id="usage" aria-labelledby="usage-title" className="border-b border-line p-[var(--cell-p)]">
            <h2 id="usage-title" className="t-mono-label text-fg-subtle">
              Usage
            </h2>
            {sealed && realtimeUrl ? (
              <LiveUsage url={realtimeUrl} sealed={sealed} initialData={usage} userId={userId} since={since.toISOString()} />
            ) : (
              <p className="t-body-sm mt-4 text-fg-muted">
                No hosted usage yet. Free (BYOK) dictation isn&apos;t counted; it never touches our servers.
              </p>
            )}
          </section>

          <section id="tokens" aria-labelledby="tokens-title" className="p-[var(--cell-p)]">
            <h2 id="tokens-title" className="t-mono-label text-fg-subtle">
              CLI tokens
            </h2>
            {tokens.length === 0 ? (
              <p className="t-body-sm mt-4 text-fg-muted">
                No CLI sign-ins. Run <code className="t-mono-sm text-fg">xflow login</code> in your terminal.
              </p>
            ) : (
              <div className="mt-4 overflow-x-auto rounded-md border border-line">
                <table className="w-full border-collapse text-left">
                  <thead className="bg-bg-sunken">
                    <tr className="t-mono-label text-fg-subtle">
                      <th scope="col" className="px-4 py-3 font-medium">Token</th>
                      <th scope="col" className="px-4 py-3 font-medium">Created</th>
                      <th scope="col" className="px-4 py-3 font-medium">Last used</th>
                      <th scope="col" className="px-4 py-3"><span className="sr-only">Actions</span></th>
                    </tr>
                  </thead>
                  <tbody>
                    {tokens.map((t) => (
                      <tr key={t.id} className="t-body-sm h-12 border-t border-line text-fg-muted">
                        <td className="px-4 py-2">
                          <code className="t-mono-sm text-fg">{t.prefix}…</code> <span>{t.name}</span>
                        </td>
                        <td className="t-mono-sm px-4 py-2">{t.createdAt.toISOString().slice(0, 10)}</td>
                        <td className="t-mono-sm px-4 py-2">{t.lastUsedAt ? t.lastUsedAt.toISOString().slice(0, 10) : "never"}</td>
                        <td className="px-4 py-2 text-right">
                          <form method="post" action={`/api/v1/tokens/${t.id}/revoke`}>
                            <button type="submit" className={buttonClass("danger", "sm")} aria-label={`Revoke ${t.name}`}>
                              Revoke
                            </button>
                          </form>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>
        </div>
      </div>
    </section>
  );
}
