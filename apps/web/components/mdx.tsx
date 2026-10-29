import type { MDXComponents } from "mdx/types";
import type { ComponentProps, ReactNode } from "react";
import Link from "next/link";
import { Pill } from "./pill";
import { ProviderTable } from "./providers";
import { Terminal, type Line } from "./terminal";
import { Callout, Kbd } from "./ui";
import { CopyText } from "./copy-text";

function Steps({ children }: { children: ReactNode }) {
  return <ol className="not-prose my-6 border-y border-line [counter-reset:step]">{children}</ol>;
}
function Step({ title, children }: { title: string; children: ReactNode }) {
  return (
    <li className="flex gap-5 border-line py-5 [counter-increment:step] [&+&]:border-t">
      <span aria-hidden className="t-mono-sm pt-0.5 text-fg-subtle before:content-[counter(step,decimal-leading-zero)]" />
      <div className="min-w-0">
        <h3 className="t-h4 text-fg">{title}</h3>
        <div className="t-body-sm mt-1 text-fg-muted [&_code]:t-mono-sm [&_code]:text-fg [&_a]:text-fg [&_a]:underline [&>*+*]:mt-2">{children}</div>
      </div>
    </li>
  );
}

/** Fenced code: Shiki output from rehype-code, wrapped in our CodeBlock chrome with a copy button. */
function Pre({ title, children, ...props }: ComponentProps<"pre"> & { title?: string }) {
  return (
    <figure className="code not-prose relative min-w-0 overflow-hidden rounded-md border border-line bg-bg-sunken">
      <div className="flex h-9 items-center justify-between border-b border-line pl-4 pr-1">
        <figcaption className="t-mono-label text-fg-subtle">[ {title ?? "code"} ]</figcaption>
        <CopyText />
      </div>
      <pre {...props} tabIndex={0} style={undefined}>
        {children}
      </pre>
    </figure>
  );
}

function A({ href = "", ...props }: ComponentProps<"a">) {
  if (href.startsWith("/") || href.startsWith("#")) return <Link href={href} {...props} />;
  return <a href={href} {...props} />;
}

export const mdxComponents: MDXComponents = {
  pre: Pre,
  a: A,
  Callout,
  Kbd,
  Steps,
  Step,
  Pill,
  Terminal: (p: { title: string; lines: Line[] }) => <Terminal {...p} className="my-6" />,
  ProviderTable,
};
