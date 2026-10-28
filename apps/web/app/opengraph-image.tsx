import { readFile } from "node:fs/promises";
import path from "node:path";
import { ImageResponse } from "next/og";

export const alt = "xflow — voice dictation for developers";
export const size = { width: 1200, height: 630 };
export const contentType = "image/png";

const font = (f: string) => readFile(path.join(process.cwd(), "node_modules/geist/dist/fonts", f));

/** One OG template (§8): dark ground, the grid frame, the wordmark and the line. Built at build time. */
export default async function Image() {
  const [sans, mono] = await Promise.all([font("geist-sans/Geist-SemiBold.ttf"), font("geist-mono/GeistMono-Medium.ttf")]);
  const line = "#262625";
  return new ImageResponse(
    (
      <div style={{ width: "100%", height: "100%", display: "flex", background: "#0A0A0A", color: "#EDEDEC", position: "relative" }}>
        <div style={{ position: "absolute", left: 80, top: 0, bottom: 0, width: 1, background: line }} />
        <div style={{ position: "absolute", right: 80, top: 0, bottom: 0, width: 1, background: line }} />
        <div style={{ position: "absolute", left: 0, right: 0, top: 120, height: 1, background: line }} />
        <div style={{ position: "absolute", left: 0, right: 0, bottom: 120, height: 1, background: line }} />
        <div style={{ display: "flex", flexDirection: "column", justifyContent: "space-between", padding: "52px 120px", width: "100%" }}>
          <div style={{ display: "flex", alignItems: "center", fontFamily: "mono", fontSize: 36 }}>
            xflow
            <div style={{ width: 16, height: 36, background: "#C6F135", marginLeft: 6 }} />
          </div>
          <div style={{ display: "flex", flexDirection: "column", fontFamily: "sans", fontSize: 84, letterSpacing: -3, lineHeight: 1.05 }}>
            <div>Hold Fn and talk.</div>
            <div style={{ display: "flex" }}>
              <span style={{ background: "rgba(198,241,53,0.22)", padding: "0 8px", marginLeft: -8 }}>xflow types it.</span>
            </div>
          </div>
          <div style={{ display: "flex", fontFamily: "mono", fontSize: 22, color: "#85857F", letterSpacing: 2 }}>
            VOICE DICTATION FOR DEVELOPERS · MACOS · LINUX · MIT
          </div>
        </div>
      </div>
    ),
    { ...size, fonts: [{ name: "sans", data: sans, weight: 600 }, { name: "mono", data: mono, weight: 500 }] },
  );
}
