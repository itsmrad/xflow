import type { Metadata } from "next";
import { CenterPanel, Panel } from "@/components/account";
import { buttonClass } from "@/components/ui";
import { normalizeUserCode } from "@/lib/tokens";
import { pendingDevice } from "@/lib/server/device";
import { withinLimit } from "@/lib/server/limits";
import { sessionUserId } from "@/lib/server/users";

export const metadata: Metadata = { title: "Sign in the xflow CLI" };

export default async function DevicePage({ searchParams }: PageProps<"/device">) {
  const params = await searchParams;
  const one = (v: string | string[] | undefined) => (Array.isArray(v) ? v[0] : v);
  const done = one(params.done);
  if (done === "approve") return <Notice ok text="Done. Go back to your terminal — xflow is signed in." />;
  if (done === "deny") return <Notice text="Request denied. Nothing was connected." />;

  const code = normalizeUserCode(one(params.code) ?? "");
  const userId = code ? await sessionUserId() : null;
  // Looking codes up shares the approval limit, so this page can't be used to probe codes.
  const limited = !!userId && !(await withinLimit(`device-decide:${userId}`, 20, 600));
  const device = code && userId && !limited ? await pendingDevice(code) : null;
  const expired = one(params.error) === "expired" || (code && !device && !limited);

  if (!device) {
    return (
      <CenterPanel label="CLI sign-in">
        <Panel>
          <h1 className="t-h2 text-fg">Connect the xflow CLI</h1>
          <p className="t-body mt-3 text-fg-muted">Enter the code shown in your terminal after <code className="t-mono-sm text-fg">xflow login</code>.</p>
          {expired && (
            <p role="alert" className="t-body-sm mt-4 text-rec">
              That code has expired or was already used. Run <code className="t-mono-sm">xflow login</code> again to get a new one.
            </p>
          )}
          {limited && (
            <p role="alert" className="t-body-sm mt-4 text-rec">
              Too many attempts. Wait a few minutes and try again.
            </p>
          )}
          <form method="get" action="/device" className="mt-6">
            <label htmlFor="code" className="t-label text-fg">
              Code
            </label>
            <input
              id="code"
              name="code"
              autoComplete="one-time-code"
              autoCapitalize="characters"
              spellCheck={false}
              inputMode="text"
              maxLength={9}
              placeholder="XXXX-XXXX"
              aria-describedby="code-hint"
              required
              className="mt-2 block h-14 w-full rounded-sm border border-line-input bg-bg-raised px-4 font-mono text-[28px] leading-9 tracking-[0.3em] text-fg uppercase placeholder:text-fg-subtle focus:border-focus focus:outline-2 focus:outline-focus"
            />
            <p id="code-hint" className="t-caption mt-2 text-fg-subtle">
              8 letters, like KXMQ-TRBD. The dash is optional.
            </p>
            <button type="submit" className={buttonClass("primary", "lg", "mt-6 w-full")}>
              Continue
            </button>
          </form>
          <Security />
        </Panel>
      </CenterPanel>
    );
  }

  return (
    <CenterPanel label="CLI sign-in">
      <Panel>
        <h1 className="t-h2 text-fg">Sign in the xflow CLI?</h1>
        <p className="t-body mt-3 text-fg-muted">Check that this code matches the one in your terminal.</p>
        <p className="mt-6 rounded-sm border border-line bg-bg-sunken px-4 py-3 text-center font-mono text-[28px] leading-9 tracking-[0.3em] text-fg">{device.userCode}</p>
        <p className="t-caption mt-3 text-fg-subtle">
          From <span className="text-fg">{device.clientName}</span>, requested {minutesAgo(device.createdAt)}.
        </p>
        <form method="post" action="/api/v1/device/decide" className="mt-6 grid grid-cols-2 gap-3">
          <input type="hidden" name="user_code" value={device.userCode} />
          <button type="submit" name="decision" value="approve" className={buttonClass("primary", "lg")}>
            Approve
          </button>
          <button type="submit" name="decision" value="deny" className={buttonClass("secondary", "lg")}>
            Deny
          </button>
        </form>
        <Security />
      </Panel>
    </CenterPanel>
  );
}

function Security() {
  return (
    <p className="t-caption mt-6 border-t border-line pt-4 text-fg-subtle">
      Only approve codes you started yourself. xflow will never ask you for a code by email or chat.
    </p>
  );
}

function minutesAgo(at: Date): string {
  const minutes = Math.floor((Date.now() - at.getTime()) / 60_000);
  return minutes < 1 ? "just now" : `${minutes} min ago`;
}

function Notice({ text, ok = false }: { text: string; ok?: boolean }) {
  return (
    <CenterPanel label="CLI sign-in">
      <Panel>
        <h1 className="t-h2 text-fg">
          {ok && <span className="mr-2 text-ok">✓</span>}
          {text}
        </h1>
        <p className="t-body mt-3 text-fg-muted">{ok ? "You can close this tab." : "Run xflow login again if this was a mistake."}</p>
      </Panel>
    </CenterPanel>
  );
}
