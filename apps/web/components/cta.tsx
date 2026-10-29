import { Crossings } from "./frame";
import { buttonClass } from "./ui";

/**
 * OS-aware primary download button (§9.1). Static HTML says macOS; a tiny inline script swaps
 * label and target to Linux before paint. Fixed width, so nothing shifts.
 */
export function OsButton({ id = "os-cta" }: { id?: string }) {
  const swap = `(function(){try{var p=(navigator.userAgentData&&navigator.userAgentData.platform)||navigator.userAgent;if(/linux/i.test(p)&&!/android/i.test(p)){var a=document.getElementById(${JSON.stringify(id)});a.href='/download#linux';a.firstChild.textContent='Install on Linux'}}catch(e){}})()`;
  return (
    <>
      <a id={id} href="/download" className={buttonClass("primary", "lg", "w-full sm:w-[200px]")} suppressHydrationWarning>
        Install on macOS
      </a>
      <script dangerouslySetInnerHTML={{ __html: swap }} />
    </>
  );
}

export function CtaBand() {
  return (
    <section className="frame rule relative overflow-hidden" aria-labelledby="cta-title">
      <Crossings />
      <div aria-hidden className="bg-dots pointer-events-none absolute inset-0" />
      <div className="section-y relative px-[var(--cell-p)] text-center">
        <p className="t-stat text-fg">
          <span aria-hidden className="text-fg-subtle">$ </span>xflow setup<span aria-hidden className="cursor ml-1" />
        </p>
        <h2 id="cta-title" className="t-lede mx-auto mt-6 max-w-[40ch] text-fg-muted">
          From download to first words in one guided setup.
        </h2>
        <div className="mt-10 flex flex-col items-center justify-center gap-3 sm:flex-row">
          <OsButton id="os-cta-band" />
        </div>
      </div>
    </section>
  );
}
