/**
 * Prints the missing path (§9.11). The 404 is prerendered once for every URL, so the server writes a
 * placeholder and an inline script fills in location.pathname: no client component on every page.
 */
const FILL = `for(const e of document.currentScript.previousElementSibling.querySelectorAll("[data-path]"))e.textContent=location.pathname`;

export function NotFoundPath() {
  return (
    <>
      <pre className="t-mono-sm overflow-x-auto p-5">
        <code className="grid whitespace-pre-wrap break-all">
          <span>
            <span aria-hidden className="select-none text-syn-muted">$ </span>open{" "}
            <span data-path suppressHydrationWarning>
              …
            </span>
          </span>
          <span className="text-rec">
            zsh: no such file or directory:{" "}
            <span data-path suppressHydrationWarning>
              …
            </span>
          </span>
        </code>
      </pre>
      <script dangerouslySetInnerHTML={{ __html: FILL }} />
    </>
  );
}
