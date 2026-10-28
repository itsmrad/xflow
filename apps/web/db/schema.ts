import { sql } from "drizzle-orm";
import {
  boolean,
  index,
  integer,
  pgEnum,
  pgTable,
  text,
  timestamp,
  uuid,
} from "drizzle-orm/pg-core";

const ts = (name: string) => timestamp(name, { withTimezone: true });
const createdAt = () => ts("created_at").notNull().defaultNow();

export const users = pgTable("users", {
  id: uuid("id").primaryKey().defaultRandom(),
  clerkUserId: text("clerk_user_id").notNull().unique(),
  email: text("email"),
  dodoCustomerId: text("dodo_customer_id").unique(),
  createdAt: createdAt(),
});

export const apiTokens = pgTable(
  "api_tokens",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    userId: uuid("user_id")
      .notNull()
      .references(() => users.id, { onDelete: "cascade" }),
    name: text("name").notNull(),
    // SHA-256 hex of the full token. The token itself is shown once and never stored.
    tokenHash: text("token_hash").notNull().unique(),
    // First characters of the token, so a user can tell tokens apart.
    prefix: text("prefix").notNull(),
    createdAt: createdAt(),
    lastUsedAt: ts("last_used_at"),
    revokedAt: ts("revoked_at"),
  },
  (t) => [index("api_tokens_user_idx").on(t.userId)],
);

export const deviceStatus = pgEnum("device_status", ["pending", "approved", "denied"]);

export const deviceCodes = pgTable(
  "device_codes",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    deviceCodeHash: text("device_code_hash").notNull().unique(),
    userCode: text("user_code").notNull().unique(),
    clientName: text("client_name").notNull(),
    status: deviceStatus("status").notNull().default("pending"),
    userId: uuid("user_id").references(() => users.id, { onDelete: "cascade" }),
    intervalSeconds: integer("interval_seconds").notNull().default(5),
    expiresAt: ts("expires_at").notNull(),
    lastPolledAt: ts("last_polled_at"),
    consumedAt: ts("consumed_at"),
    createdAt: createdAt(),
  },
  (t) => [index("device_codes_expires_idx").on(t.expiresAt)],
);

export const subscriptions = pgTable(
  "subscriptions",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    dodoSubscriptionId: text("dodo_subscription_id").notNull().unique(),
    userId: uuid("user_id")
      .notNull()
      .references(() => users.id, { onDelete: "cascade" }),
    dodoCustomerId: text("dodo_customer_id").notNull(),
    productId: text("product_id").notNull(),
    status: text("status").notNull(),
    currentPeriodEnd: ts("current_period_end"),
    pastDueEndsAt: ts("past_due_ends_at"),
    cancelAtPeriodEnd: boolean("cancel_at_period_end").notNull().default(false),
    // Event time of the last applied webhook; older deliveries are ignored.
    lastEventAt: ts("last_event_at").notNull(),
    createdAt: createdAt(),
    updatedAt: ts("updated_at").notNull().defaultNow(),
  },
  (t) => [index("subscriptions_user_idx").on(t.userId)],
);

export const usageKind = pgEnum("usage_kind", ["transcribe", "cleanup"]);

export const usageEvents = pgTable(
  "usage_events",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    userId: uuid("user_id")
      .notNull()
      .references(() => users.id, { onDelete: "cascade" }),
    tokenId: uuid("token_id").references(() => apiTokens.id, { onDelete: "set null" }),
    kind: usageKind("kind").notNull(),
    model: text("model").notNull(),
    status: integer("status").notNull(),
    audioBytes: integer("audio_bytes").notNull().default(0),
    audioMs: integer("audio_ms").notNull().default(0),
    inputTokens: integer("input_tokens").notNull().default(0),
    outputTokens: integer("output_tokens").notNull().default(0),
    // Provider cost charged against the user's fair-use budget, in micro-dollars.
    costMicros: integer("cost_micros").notNull().default(0),
    latencyMs: integer("latency_ms").notNull(),
    createdAt: createdAt(),
  },
  (t) => [index("usage_events_user_created_idx").on(t.userId, t.createdAt.desc())],
);

// Fixed-window counters and concurrency leases, shared by every serverless instance.
export const rateLimits = pgTable("rate_limits", {
  key: text("key").primaryKey(),
  windowStart: ts("window_start").notNull().default(sql`now()`),
  count: integer("count").notNull(),
});
