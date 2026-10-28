import "server-only";
import { attachDatabasePool } from "@vercel/functions";
import { drizzle } from "drizzle-orm/node-postgres";
import { Pool } from "pg";
import * as schema from "./schema";

const url = process.env.DATABASE_URL;
if (!url) throw new Error("DATABASE_URL is not set");

// Neon URLs say sslmode=require; verify-full keeps the same checks without pg's warning.
const pool = new Pool({ connectionString: url.replace("sslmode=require", "sslmode=verify-full"), max: 5 });
// An idle connection dropping (compute scaled to zero) must not crash the process.
pool.on("error", (error) => console.error("postgres pool error", error.message));
attachDatabasePool(pool);

export const db = drizzle(pool, { schema });
export { schema };
