import "server-only";
import { auth } from "@clerk/nextjs/server";
import { sql } from "drizzle-orm";
import { db } from "@/db";
import { users } from "@/db/schema";

/** Our users row for a Clerk user, created on first sight. */
export async function ensureUser(clerkUserId: string, email?: string | null): Promise<string> {
  const [row] = await db
    .insert(users)
    .values({ clerkUserId, email: email ?? null })
    .onConflictDoUpdate({ target: users.clerkUserId, set: { email: sql`coalesce(excluded.email, ${users.email})` } })
    .returning({ id: users.id });
  if (!row) throw new Error("user upsert returned no row");
  return row.id;
}

/** The signed-in user's id, or null. For route handlers and server components. */
export async function sessionUserId(): Promise<string | null> {
  const { userId } = await auth();
  return userId ? ensureUser(userId) : null;
}
