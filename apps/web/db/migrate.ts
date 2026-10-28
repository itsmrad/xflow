// bun --env-file=.env.local db/migrate.ts — applies db/migrations over the direct URL.
import { drizzle } from "drizzle-orm/node-postgres";
import { migrate } from "drizzle-orm/node-postgres/migrator";
import { Pool } from "pg";

const url = process.env.DATABASE_URL_UNPOOLED;
if (!url) throw new Error("DATABASE_URL_UNPOOLED is not set");
const pool = new Pool({ connectionString: url.replace("sslmode=require", "sslmode=verify-full"), max: 1 });
await migrate(drizzle(pool), { migrationsFolder: new URL("./migrations", import.meta.url).pathname });
await pool.end();
console.log("migrations applied");
