import { CenterPanel } from "@/components/account";
import { NotFoundPath } from "@/components/not-found-path";
import { Button } from "@/components/ui";

export default function NotFound() {
  return (
    <main id="content">
      <CenterPanel label="404" wide>
        <h1 className="sr-only">Page not found</h1>
        <figure className="overflow-hidden rounded-md border border-line bg-bg-raised shadow-1">
          <pre className="t-mono-sm overflow-x-auto p-5">
            <NotFoundPath />
          </pre>
        </figure>
        <div className="mt-6 flex gap-3">
          <Button href="/" variant="primary">
            Go home
          </Button>
          <Button href="/docs" variant="secondary">
            Search docs
          </Button>
        </div>
      </CenterPanel>
    </main>
  );
}
