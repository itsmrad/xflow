import { currentUser } from "@clerk/nextjs/server";
import { eq } from "drizzle-orm";
import { db } from "@/db";
import { users } from "@/db/schema";
import { proLaunched, proProductIds, USER_ID_METADATA_KEY } from "@/lib/billing";
import { appUrl, error, json, sameOrigin, seeOther, wantsHtml } from "@/lib/http";
import { dodo } from "@/lib/server/billing";
import { ensureUser } from "@/lib/server/users";
import { userIsPro } from "@/lib/server/account";

/** Starts a Dodo checkout for Pro ($5/month) for the signed-in user. */
export async function POST(request: Request) {
  // Until launch, refuse before touching Clerk or the database.
  if (!proLaunched()) return error(503, "Pro is coming soon");
  if (!sameOrigin(request)) return error(403, "cross-origin request refused");
  const clerk = await currentUser();
  if (!clerk) return error(401, "sign in first");
  const email = clerk.primaryEmailAddress?.emailAddress ?? null;
  const userId = await ensureUser(clerk.id, email);
  const client = dodo();
  const productId = proProductIds()[0];
  if (!client || !productId) return error(503, "billing is not configured");
  if (await userIsPro(userId)) return wantsHtml(request) ? seeOther("/dashboard") : error(409, "already on Pro");
  if (!email) return error(400, "your account needs an email address to subscribe");

  const [user] = await db.select({ customer: users.dodoCustomerId }).from(users).where(eq(users.id, userId));
  const session = await client.checkoutSessions.create({
    product_cart: [{ product_id: productId, quantity: 1 }],
    customer: user?.customer ? { customer_id: user.customer } : { email, name: clerk.fullName ?? undefined },
    // One Dodo customer per xflow account. Matching by email would attach this checkout to another
    // account's customer (a deleted and re-created account, say).
    ...(user?.customer ? {} : { feature_flags: { always_create_new_customer: true } }),
    metadata: { [USER_ID_METADATA_KEY]: userId },
    return_url: `${appUrl()}/dashboard?checkout=done`,
  });
  if (!session.checkout_url) return error(502, "billing provider returned no checkout URL");
  return wantsHtml(request) ? seeOther(session.checkout_url) : json({ url: session.checkout_url });
}
