import type { Metadata } from "next";
import { normalizeUserCode } from "@/lib/tokens";
import { pendingDevice } from "@/lib/server/device";

export const metadata: Metadata = { title: "Sign in the xflow CLI" };

// Placeholder markup; the designed page comes from docs/web/DESIGN.md.
export default async function DevicePage({ searchParams }: PageProps<"/device">) {
  const params = await searchParams;
  const one = (v: string | string[] | undefined) => (Array.isArray(v) ? v[0] : v);
  const done = one(params.done);
  if (done === "approve") return <Notice text="CLI signed in. You can close this tab and go back to your terminal." />;
  if (done === "deny") return <Notice text="Sign-in denied. The CLI will not get access." />;

  const code = normalizeUserCode(one(params.code) ?? "");
  const device = code ? await pendingDevice(code) : null;
  const expired = one(params.error) === "expired" || (code && !device);

  if (!device) {
    return (
      <main>
        <h1>Sign in the xflow CLI</h1>
        {expired && <p role="alert">That code is expired or already used. Run `xflow login` again.</p>}
        <form method="get" action="/device">
          <label>
            Code shown in your terminal
            <input name="code" autoComplete="off" autoCapitalize="characters" placeholder="XXXX-XXXX" required />
          </label>
          <button type="submit">Continue</button>
        </form>
      </main>
    );
  }

  return (
    <main>
      <h1>Sign in the xflow CLI?</h1>
      <p>
        Code <code>{device.userCode}</code> from <strong>{device.clientName}</strong>, requested at{" "}
        {device.createdAt.toISOString().slice(11, 16)} UTC.
      </p>
      <p>Only approve if you just ran `xflow login` yourself and the code matches your terminal.</p>
      <form method="post" action="/api/v1/device/decide">
        <input type="hidden" name="user_code" value={device.userCode} />
        <button type="submit" name="decision" value="approve">
          Approve
        </button>
        <button type="submit" name="decision" value="deny">
          Deny
        </button>
      </form>
    </main>
  );
}

function Notice({ text }: { text: string }) {
  return (
    <main>
      <p>{text}</p>
    </main>
  );
}
