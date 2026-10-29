import type { Metadata } from "next";
import { SignUp } from "@clerk/nextjs";
import { CenterPanel } from "@/components/account";

export const metadata: Metadata = { title: "Sign up" };

export default function Page() {
  return (
    <CenterPanel label="Account">
      {/* Clerk mounts client-side; reserving its card height (590 px at 390, 572 at 1440) keeps CLS at 0 (§10.3). */}
      <div className="min-h-[590px] sm:min-h-[572px]">
        <SignUp />
      </div>
    </CenterPanel>
  );
}
