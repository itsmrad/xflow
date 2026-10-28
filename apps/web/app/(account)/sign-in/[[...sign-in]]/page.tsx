import type { Metadata } from "next";
import { SignIn } from "@clerk/nextjs";
import { CenterPanel } from "@/components/account";

export const metadata: Metadata = { title: "Sign in" };

export default function Page() {
  return (
    <CenterPanel label="Account">
      <SignIn />
    </CenterPanel>
  );
}
