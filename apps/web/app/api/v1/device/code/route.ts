import { DEVICE_CODE_TTL_SECONDS, DEFAULT_POLL_INTERVAL_SECONDS, cleanClientName } from "@/lib/device";
import { appUrl, clientIp, error, fields, json } from "@/lib/http";
import { createDeviceCode } from "@/lib/server/device";
import { withinLimit } from "@/lib/server/limits";

/** Starts a CLI sign-in (RFC 8628 §3.1). */
export async function POST(request: Request) {
  if (!(await withinLimit(`device-code:${clientIp(request)}`, 10, 600))) {
    return error(429, "too many sign-in attempts; try again in a few minutes", { "retry-after": "600" });
  }
  const body = await fields(request);
  const { deviceCode, userCode } = await createDeviceCode(cleanClientName(body.client_name));
  const verificationUri = `${appUrl()}/device`;
  return json({
    device_code: deviceCode,
    user_code: userCode,
    verification_uri: verificationUri,
    verification_uri_complete: `${verificationUri}?code=${userCode}`,
    expires_in: DEVICE_CODE_TTL_SECONDS,
    interval: DEFAULT_POLL_INTERVAL_SECONDS,
  });
}
