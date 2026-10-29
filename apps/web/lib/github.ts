import { REPO } from "@/content/site";

/** Stars at build time, ISR hourly (§7.1). Hidden below 100 or when GitHub is unreachable. */
export async function getStars(): Promise<number | null> {
  try {
    const res = await fetch(REPO.replace("github.com", "api.github.com/repos"), {
      next: { revalidate: 3600 },
      headers: { accept: "application/vnd.github+json" },
      signal: AbortSignal.timeout(3000),
    });
    if (!res.ok) return null;
    const { stargazers_count: n } = (await res.json()) as { stargazers_count?: number };
    return typeof n === "number" && n >= 100 ? n : null;
  } catch {
    return null;
  }
}
