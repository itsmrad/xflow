# apps/web

The xflow website, docs and account backend: Next.js on Vercel, Clerk, Neon Postgres, Dodo Payments.

## Develop

```sh
bun install                 # from the repo root
cp .env.example .env.local  # fill in dev values; never commit it
bun run dev
```

Checks (CI runs the same ones in the `web` job):

```sh
bun run lint && bun run typecheck && bun run test && bun run build
bun run start & bun scripts/axe.ts http://localhost:3000 /sign-in   # every sitemap route, both themes, 375 px and desktop
bun scripts/overflow.ts http://localhost:3000 /sign-in              # no page wider than 390/768/1440
bun scripts/copy-check.ts http://localhost:3000                     # copy buttons with the clipboard refused
bun scripts/budget.ts                                               # first-load JS and font bytes, after a build
TEST_DATABASE_URL=postgres://… bun run test:db                      # throwaway database only
```

## Change the schema

1. Edit `db/schema.ts`.
2. `bun run db:generate` writes the next migration into `db/migrations/`. Commit both.
3. CI fails if `db:generate` would still produce a change, so the schema and the migrations can't drift.

Migrations must stay additive (new tables, nullable or defaulted columns) so that the running deploy keeps working against the migrated database.

## Deploy to production

Production is https://xflowcli.vercel.app. Secrets come from Infisical (prod → Vercel Production).
A production build fails when a required variable is missing (`lib/env.ts`).

Migrate **before** deploying, never after: new code may query tables or functions that only the migration creates.

1. Run the migrations against production over the direct (unpooled) URL:
   ```sh
   DATABASE_URL_UNPOOLED='<production direct URL>' bun db/migrate.ts   # prints "migrations applied"
   ```
   It is idempotent: already applied migrations are skipped.
2. Merge to `main`. Vercel builds and deploys `apps/web`. The ignore step skips deploys that don't touch it.
3. Verify:
   - `curl -sI https://xflowcli.vercel.app` returns 200 with the security headers.
   - `https://xflowcli.vercel.app/__clerk/v1/environment` returns JSON (the Clerk proxy).
   - Sign in, open `/dashboard`: usage reads "live".
   - `curl -s -X POST https://xflowcli.vercel.app/api/v1/device/code` returns a `verification_uri` on the production host.

Rolling back: redeploy the previous deployment in Vercel. Because migrations are additive, the old code runs against the new schema.

## Launch Pro

Set live Dodo keys, `DODO_PRO_PRODUCT_ID`, `DODO_PAYMENTS_ENVIRONMENT=live_mode` and `PRO_LAUNCHED=1` in Production, then
change the "Coming soon" copy on the marketing pages, which is static. Until `PRO_LAUNCHED=1`, checkout answers 503.
