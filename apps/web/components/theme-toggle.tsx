/**
 * System / Light / Dark segmented control (§5.9), with no React on the client: the buttons are server
 * markup and this inline script wires them (and follows the OS setting). The head script in
 * app/layout.tsx applies the saved theme before paint; this one keeps it in sync afterwards.
 */
const WIRE = `(()=>{const g=document.currentScript.previousElementSibling,q=matchMedia("(prefers-color-scheme: dark)");
const get=()=>{try{return localStorage.getItem("theme")||"system"}catch(e){return"system"}};
const sync=()=>{const m=get();for(const b of g.children)b.setAttribute("aria-checked",String(b.dataset.mode===m));
document.documentElement.dataset.theme=(m==="system"?q.matches:m==="dark")?"dark":"light"};
g.addEventListener("click",e=>{const m=e.target.closest("[data-mode]")?.dataset.mode;if(!m)return;
try{m==="system"?localStorage.removeItem("theme"):localStorage.setItem("theme",m)}catch(e){}sync()});
q.addEventListener("change",sync);sync()})()`;

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
