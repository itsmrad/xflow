import "server-only";
import { and, desc, eq, isNull } from "drizzle-orm";
import { db } from "@/db";
import { apiTokens, subscriptions } from "@/db/schema";
import { proProductIds } from "@/lib/billing";
import { isEntitled } from "@/lib/entitlement";

export async function userSubscriptions(userId: string) {
  return db
    .select({
      productId: subscriptions.productId,
      status: subscriptions.status,
      currentPeriodEnd: subscriptions.currentPeriodEnd,
      pastDueEndsAt: subscriptions.pastDueEndsAt,
      cancelAtPeriodEnd: subscriptions.cancelAtPeriodEnd,
    })
    .from(subscriptions)
    .where(eq(subscriptions.userId, userId))
    .orderBy(desc(subscriptions.updatedAt));
}

export async function userIsPro(userId: string): Promise<boolean> {
  return isEntitled(await userSubscriptions(userId), new Date(), proProductIds());
}

export async function userTokens(userId: string) {
  return db
    .select({
      id: apiTokens.id,
      name: apiTokens.name,
      prefix: apiTokens.prefix,
      createdAt: apiTokens.createdAt,
      lastUsedAt: apiTokens.lastUsedAt,
    })
    .from(apiTokens)
    .where(and(eq(apiTokens.userId, userId), isNull(apiTokens.revokedAt)))
    .orderBy(desc(apiTokens.createdAt));
}

export async function revokeToken(userId: string, tokenId: string): Promise<boolean> {
  const rows = await db
    .update(apiTokens)
    .set({ revokedAt: new Date() })
    .where(and(eq(apiTokens.id, tokenId), eq(apiTokens.userId, userId), isNull(apiTokens.revokedAt)))
    .returning({ id: apiTokens.id });
  return rows.length === 1;
}
