import type { Metadata } from "next";
import { SignUp } from "@clerk/nextjs";
import { CenterPanel } from "@/components/account";

export const metadata: Metadata = { title: "Sign up" };

export default function Page() {
  return (
    <CenterPanel label="Account">
      <SignUp />
    </CenterPanel>
  );
}
