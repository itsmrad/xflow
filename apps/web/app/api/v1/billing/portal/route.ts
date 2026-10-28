import { eq } from "drizzle-orm";
import { db } from "@/db";
import { users } from "@/db/schema";
import { error, json, sameOrigin, seeOther, wantsHtml } from "@/lib/http";
import { dodo } from "@/lib/server/billing";
import { sessionUserId } from "@/lib/server/users";

/** Dodo customer portal (invoices, payment method, cancel) for the signed-in user. */
export async function POST(request: Request) {
  if (!sameOrigin(request)) return error(403, "cross-origin request refused");
  const userId = await sessionUserId();
  if (!userId) return error(401, "sign in first");
  const client = dodo();
  if (!client) return error(503, "billing is not configured");
  const [user] = await db.select({ customer: users.dodoCustomerId }).from(users).where(eq(users.id, userId));
  if (!user?.customer) return error(404, "no billing account yet");
  const { link } = await client.customers.customerPortal.create(user.customer);
  return wantsHtml(request) ? seeOther(link) : json({ url: link });
}
