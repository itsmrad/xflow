import { clientIp, fields, json } from "@/lib/http";
import { pollDevice } from "@/lib/server/device";
import { withinLimit } from "@/lib/server/limits";
import { isDeviceCodeShape, sha256 } from "@/lib/tokens";

const GRANT_TYPE = "urn:ietf:params:oauth:grant-type:device_code";

/** CLI polls here until the user approves (RFC 8628 §3.4–3.5). */
export async function POST(request: Request) {
  if (!(await withinLimit(`device-token:${clientIp(request)}`, 120, 60))) {
    return json({ error: "slow_down", interval: 30 }, 400);
  }
  const body = await fields(request);
  if (body.grant_type !== undefined && body.grant_type !== GRANT_TYPE) {
    return json({ error: "unsupported_grant_type" }, 400);
  }
  const code = body.device_code;
  if (typeof code !== "string" || !isDeviceCodeShape(code)) return json({ error: "invalid_grant" }, 400);

  const { outcome, interval, token } = await pollDevice(sha256(code));
  if (outcome.kind === "issue" && token) return json({ access_token: token, token_type: "Bearer" });
  const errorCode = outcome.kind === "error" ? outcome.error : "invalid_grant";
  return json({ error: errorCode, interval }, 400);
}
