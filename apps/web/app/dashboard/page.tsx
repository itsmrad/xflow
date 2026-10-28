import type { Metadata } from "next";
import { currentUser } from "@clerk/nextjs/server";
import { redirect } from "next/navigation";
import { proProductIds } from "@/lib/billing";
import { isEntitled } from "@/lib/entitlement";
import { userSubscriptions, userTokens } from "@/lib/server/account";
import { initialUsage, sealUsage, usageWindowStart } from "@/lib/server/realtime";
import { ensureUser } from "@/lib/server/users";
import { LiveUsage } from "./live-usage";

export const metadata: Metadata = { title: "Dashboard" };

// Placeholder markup; the designed page comes from docs/web/DESIGN.md.
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

  return (
    <main>
      <h1>Dashboard</h1>
      <p>Signed in as {clerk.primaryEmailAddress?.emailAddress ?? clerk.id}</p>

      <section>
        <h2>Plan</h2>
        {params.checkout === "done" && !pro && <p>Payment received; Pro turns on as soon as the payment confirms.</p>}
        {pro ? (
          <p>
            Pro{current?.currentPeriodEnd && ` · ${current.cancelAtPeriodEnd ? "ends" : "renews"} ${current.currentPeriodEnd.toISOString().slice(0, 10)}`}
            {current?.status === "past_due" && " · payment failed, update your card"}
          </p>
        ) : (
          <p>Free (bring your own key). Pro is $5/month for unlimited hosted transcription and cleanup.</p>
        )}
        <form method="post" action={pro || current ? "/api/v1/billing/portal" : "/api/v1/billing/checkout"}>
          <button type="submit">{pro || current ? "Manage billing" : "Upgrade to Pro"}</button>
        </form>
        {!pro && current && (
          <form method="post" action="/api/v1/billing/checkout">
            <button type="submit">Subscribe again</button>
          </form>
        )}
      </section>

      {sealed && realtimeUrl ? (
        <LiveUsage url={realtimeUrl} sealed={sealed} initialData={usage} userId={userId} since={since.toISOString()} />
      ) : null}

      <section>
        <h2>CLI tokens</h2>
        {tokens.length === 0 ? (
          <p>None. Run `xflow login` to sign in a computer.</p>
        ) : (
          <ul>
            {tokens.map((t) => (
              <li key={t.id}>
                <code>{t.prefix}…</code> {t.name} · created {t.createdAt.toISOString().slice(0, 10)} · last used{" "}
                {t.lastUsedAt ? t.lastUsedAt.toISOString().slice(0, 10) : "never"}
                <form method="post" action={`/api/v1/tokens/${t.id}/revoke`}>
                  <button type="submit">Revoke</button>
                </form>
              </li>
            ))}
          </ul>
        )}
      </section>
    </main>
  );
}
