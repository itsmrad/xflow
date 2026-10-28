import "server-only";
import { AwsClient } from "aws4fetch";

const BUCKET = "releases";
const URL_TTL_SECONDS = 300;
// Release artifacts only: "v0.4.0/xflow-aarch64-apple-darwin.tar.gz", "latest.json".
const KEY_RE = /^(?:[A-Za-z0-9][A-Za-z0-9._-]{0,127}\/){0,3}[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;

export function isReleaseKey(key: string): boolean {
  return KEY_RE.test(key) && !key.includes("..");
}

/** A short-lived presigned GET for a release object in Neon Object Storage (path-style S3). */
export async function releaseUrl(key: string): Promise<string | null> {
  const { AWS_ACCESS_KEY_ID: id, AWS_SECRET_ACCESS_KEY: secret, AWS_ENDPOINT_URL_S3: endpoint, AWS_REGION: region } = process.env;
  if (!id || !secret || !endpoint || !region) return null;
  const aws = new AwsClient({ accessKeyId: id, secretAccessKey: secret, region, service: "s3" });
  const url = new URL(`${endpoint.replace(/\/+$/, "")}/${BUCKET}/${key.split("/").map(encodeURIComponent).join("/")}`);
  url.searchParams.set("X-Amz-Expires", String(URL_TTL_SECONDS));
  const signed = await aws.sign(url, { method: "GET", aws: { signQuery: true } });
  return signed.url;
}
