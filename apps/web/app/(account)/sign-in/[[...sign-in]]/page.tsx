import type { Metadata } from "next";
import { SignIn } from "@clerk/nextjs";
import { CenterPanel } from "@/components/account";

export const metadata: Metadata = { title: "Sign in" };

export default function Page() {
  return (
    <CenterPanel label="Account">
      {/* Clerk mounts client-side; reserving its card height (490 px at 390 and 1440) keeps CLS at 0 (§10.3). */}
      <div className="min-h-[490px]">
        <SignIn />
      </div>
    </CenterPanel>
  );
}
