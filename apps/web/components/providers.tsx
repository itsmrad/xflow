import Link from "next/link";
import providers from "@/content/providers.json";
import { Badge } from "./ui";

type P = (typeof providers)[number];

/** §7.18 "store" grid from the generated providers.json. Names in mono, no third-party logos. */
export function ProviderGrid() {
  const local = providers.filter((p) => p.local || p.id === "custom");
  const cloud = providers.filter((p) => !local.includes(p));
  const tiles: { key: string; name: string; badge: string; tone?: "PRO"; model: string; meta: string; href: string; pro?: boolean }[] = [
    ...cloud.map((p: P) => ({ key: p.id, name: p.name, badge: p.kinds.join(" · "), model: p.defaultModel ?? "", meta: "BYOK", href: p.href })),
    { key: "local", name: "Local / OpenAI-compatible", badge: "LOCAL", model: "whisper.cpp server, any endpoint", meta: "BYOK · offline", href: "/docs/providers/local-and-offline" },
    { key: "pro", name: "xflow Pro", badge: "PRO", tone: "PRO", model: "Hosted, nothing to configure", meta: "$5/month", href: "/pricing", pro: true },
  ];
  return (
    <ul className="cells cells-4 border-t border-line">
      {tiles.map((t) => (
        <li key={t.key} className={`group relative p-4 sm:p-5 transition-colors duration-[120ms] hover:bg-ink-a4 ${t.pro ? "shadow-[inset_0_2px_0_var(--volt)]" : ""}`}>
          <div className="flex flex-col-reverse items-start justify-between gap-2 sm:flex-row sm:gap-3">
            <Link href={t.href} className="stretch font-mono text-[14px] font-medium leading-[22px] text-fg">
              {t.name}
            </Link>
            <Badge tone={t.tone}>{t.badge}</Badge>
          </div>
          <p className="t-mono-sm mt-2 truncate text-fg-subtle">{t.model}</p>
          <p className="t-mono-label mt-3 text-fg-muted sm:mt-4">{t.meta}</p>
        </li>
      ))}
    </ul>
  );
}

export function ProviderTable() {
  return (
    <table>
      <thead>
        <tr>
          <th>Provider</th>
          <th>Default model</th>
          <th>Key</th>
          <th>Hints</th>
        </tr>
      </thead>
      <tbody>
        {providers.map((p) => (
          <tr key={p.id}>
            <td>
              <Link href={p.href}>{p.name}</Link>
            </td>
            <td>{p.defaultModel ? <code>{p.defaultModel}</code> : "set your own"}</td>
            <td>{p.envVar ? <code>{p.envVar}</code> : p.local ? "none" : "optional"}</td>
            <td>{p.vocabulary ? "Yes" : "No"}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
