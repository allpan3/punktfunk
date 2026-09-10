// The console does not host manuals (design/web-console-overhaul.md §2.2): a hint states the
// consequence in one sentence, and everything longer lives in docs-site behind this link.
import { ArrowUpRight } from "lucide-react";
import type { FC, ReactNode } from "react";
import { cn } from "@/lib/utils";
import { m } from "@/paraglide/messages";

const DOCS_BASE = "https://docs.punktfunk.unom.io/docs";

/** `path` is a docs-site route, optionally with an anchor: `virtual-displays#keep-alive`. */
export const DocsLink: FC<{
	path: string;
	className?: string;
	children?: ReactNode;
}> = ({ path, className, children }) => (
	<a
		href={`${DOCS_BASE}/${path}`}
		target="_blank"
		rel="noreferrer"
		className={cn(
			"inline-flex items-center gap-0.5 underline underline-offset-4 hover:text-foreground",
			className,
		)}
	>
		{children ?? m.docs_link()}
		<ArrowUpRight className="size-3 shrink-0" aria-hidden />
	</a>
);
