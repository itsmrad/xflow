import { Kbd } from "./ui";
import { WindowChrome } from "./terminal";

/** A static, illustrative dictation result. No microphone access or simulated live state. */
export function HeroWindow() {
  return (
    <figure className="home-demo">
      <figcaption className="home-demo-caption">From your voice to your cursor</figcaption>
      <div className="home-editor">
        <WindowChrome title="deploy.md" />
        <div className="home-document">
          <p className="t-mono-sm text-fg-muted">## Rollout</p>
          <p className="text-fg-muted">Staging is green as of this morning.</p>
          <p className="home-transcript text-fg">
            Deploy the API to Kubernetes and bump the Postgres pool to 20.
            <span aria-hidden className="home-caret" />
          </p>
          <span className="home-result text-fg-muted">Dictation inserted</span>
        </div>
        <div className="home-demo-shortcut">
          <Kbd>Fn</Kbd><span>Hold. Talk. Release.</span>
        </div>
      </div>
      <p className="home-demo-footnote">Illustrative example. Your app stays in focus.</p>
    </figure>
  );
}
