import { Pill } from "./pill";
import { Terminal, WindowChrome } from "./terminal";

const WATCH = [
  { kind: "cmd" as const, text: "xflow watch --json | jq -r .state" },
  { kind: "idle" as const, text: "idle" },
  { kind: "listening" as const, text: "listening", note: "level update every 40 ms" },
  { kind: "listening" as const, text: "listening" },
  { kind: "processing" as const, text: "processing" },
  { kind: "success" as const, text: "success" },
];
const PHASE = ["tl-0", "tl-0", "tl-1", "tl-1", "tl-2", "tl-3"];

/**
 * The hero is the product (§9.1): an editor receiving dictation, and the daemon's real state stream.
 * Pure HTML/CSS; animation only reveals pre-laid-out content (no CLS). Final frame under reduced motion.
 */
export function HeroWindow() {
  return (
    <div data-loop className="hero-loop mx-auto grid max-w-[1040px] overflow-hidden rounded-md border border-line bg-bg-raised text-left shadow-1 md:grid-cols-12">
      <div className="md:col-span-6 lg:col-span-7">
        <WindowChrome title="deploy.md" />
        <div className="relative px-5 pb-10 pt-6 font-mono text-[13px] leading-6 sm:px-8 sm:text-[15px]">
          <p className="text-syn-keyword">## Rollout</p>
          <p className="mt-6 text-syn-muted">Staging is green as of this morning.</p>
          <p aria-hidden className="mt-6 text-fg">
            <span className="typed typed-a block w-fit whitespace-nowrap">Deploy the API to Kubernetes and bump</span>
            <span className="block w-fit whitespace-nowrap">
              <span className="typed typed-b inline-block">the Postgres pool to 20.</span>
              <span className="cursor ml-0.5" />
            </span>
          </p>
          <div aria-hidden className="mt-6 grid h-10 justify-center [&>*]:col-start-1 [&>*]:row-start-1 [&>*]:justify-self-center">
            <span className="ph ph-listen">
              <Pill state="listening" />
            </span>
            <span className="ph ph-proc">
              <Pill state="processing" />
            </span>
            <span className="ph ph-ok">
              <Pill state="success" />
            </span>
          </div>
          <p className="sr-only">Dictated text appears at the cursor: “Deploy the API to Kubernetes and bump the Postgres pool to 20.”</p>
        </div>
      </div>
      <div className="border-t border-line md:col-span-6 md:border-t-0 md:border-l lg:col-span-5">
        <Terminal
          title="zsh"
          live
          lines={WATCH}
          className="h-full rounded-none border-0 shadow-none"
          lineClass={(i) => PHASE[i]!}
        />
      </div>
    </div>
  );
}
