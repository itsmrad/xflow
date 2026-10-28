export type PillState = "listening" | "processing" | "success" | "error" | "hands-free";

const LABEL: Record<PillState, string> = {
  listening: "Recording pill: listening",
  processing: "Recording pill: processing",
  success: "Recording pill: text typed",
  error: "Recording pill: didn't catch anything",
  "hands-free": "Recording pill: hands-free listening",
};

function Bars() {
  return (
    <span className="bars">
      {Array.from({ length: 9 }, (_, i) => (
        <i key={i} />
      ))}
    </span>
  );
}

/** CSS recreation of the real overlay (docs/img/pill-*.png), §7.9. */
export function Pill({ state, timer = "0:04" }: { state: PillState; timer?: string }) {
  return (
    <span role="img" aria-label={LABEL[state]} className="pill" data-state={state}>
      {(state === "listening" || state === "hands-free") && (
        <>
          <Bars />
          <span>{timer}</span>
        </>
      )}
      {state === "processing" && (
        <span className="dots">
          <i />
          <i />
          <i />
        </span>
      )}
      {state === "success" && (
        <>
          <span className="text-[#5BD47A]">✓</span>
          <span>Typed</span>
        </>
      )}
      {state === "error" && <span className="text-[#FF6369]">Didn&apos;t catch anything</span>}
    </span>
  );
}
