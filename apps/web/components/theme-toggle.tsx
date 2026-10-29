/**
 * System / Light / Dark segmented control (§5.9), with no React on the client: the buttons are server
 * markup and this inline script wires them. The chosen mode lives in memory and applies at once;
 * saving it is best effort (storage may be blocked). The OS setting only matters while the mode is System.
 * The head script in app/layout.tsx applies the saved theme before paint.
 */
const WIRE = `(()=>{const g=document.currentScript.previousElementSibling,q=matchMedia("(prefers-color-scheme: dark)");
let m;try{m=localStorage.getItem("theme")}catch(e){}m=m==="light"||m==="dark"?m:"system";
const apply=()=>{for(const b of g.children)b.setAttribute("aria-checked",String(b.dataset.mode===m));
document.documentElement.dataset.theme=(m==="system"?q.matches:m==="dark")?"dark":"light"};
g.addEventListener("click",e=>{const v=e.target.closest("[data-mode]")?.dataset.mode;if(!v)return;m=v;apply();
try{v==="system"?localStorage.removeItem("theme"):localStorage.setItem("theme",v)}catch(e){}});
q.addEventListener("change",()=>{if(m==="system")apply()});apply()})()`;

export function ThemeToggle() {
  return (
    <>
      <div role="radiogroup" aria-label="Theme" className="inline-flex h-8 rounded-sm border border-line p-0.5">
        {(["system", "light", "dark"] as const).map((m) => (
          <button
            key={m}
            type="button"
            role="radio"
            data-mode={m}
            aria-checked={m === "system"}
            suppressHydrationWarning
            className="t-caption rounded-xs px-2.5 capitalize text-fg-muted transition-colors duration-[200ms] hover:text-fg aria-checked:bg-ink-a8 aria-checked:text-fg"
          >
            {m}
          </button>
        ))}
      </div>
      <script dangerouslySetInnerHTML={{ __html: WIRE }} />
    </>
  );
}
