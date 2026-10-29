import { REPO } from "@/content/site";

export type Asset = { name: string; size: number; url: string; sha256: string | null };
export type Release = { tag: string; prerelease: boolean; url: string; assets: Asset[] };

/** Newest GitHub release (pre-releases included), at build time with hourly ISR (§9.3). Null if none. */
export async function latestRelease(): Promise<Release | null> {
  try {
    const res = await fetch(`${REPO.replace("github.com", "api.github.com/repos")}/releases?per_page=1`, {
      next: { revalidate: 3600 },
      headers: { accept: "application/vnd.github+json" },
      signal: AbortSignal.timeout(3000),
    });
    if (!res.ok) return null;
    const [r] = (await res.json()) as {
      tag_name: string;
      prerelease: boolean;
      html_url: string;
      assets: { name: string; size: number; browser_download_url: string; digest?: string | null }[];
    }[];
    if (!r) return null;
    return {
      tag: r.tag_name,
      prerelease: r.prerelease,
      url: r.html_url,
      assets: r.assets.map((a) => ({ name: a.name, size: a.size, url: a.browser_download_url, sha256: a.digest?.startsWith("sha256:") ? a.digest.slice(7) : null })),
    };
  } catch {
    return null;
  }
}
