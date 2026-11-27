import type { ComponentProps } from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

const pillVariants = cva(
  "inline-flex max-w-full shrink-0 items-center gap-1 rounded-full px-[7px] py-[1.5px] text-[11px] leading-[1.35] font-medium whitespace-nowrap",
  {
    variants: {
      tone: {
        neutral: "bg-bg2 text-muted-foreground",
        backlog: "bg-tint-backlog text-backlog",
        next: "bg-tint-info text-info",
        doing: "bg-tint-warn text-warn",
        review: "bg-tint-special text-special",
        done: "bg-tint-ok text-ok",
        needs: "bg-tint-err text-err",
        project: "bg-tint-accent font-mono text-[10px] text-primary",
        branch: "bg-tint-info font-mono text-tag text-info",
        remote: "bg-tint-remote font-mono text-tag text-remote",
        blocked: "border border-line-err bg-tint-err text-tag text-err hover:border-err",
        proposed: "border border-dashed border-line-warn bg-tint-warn text-tag text-warn",
        artifact: "bg-tint-accent font-mono text-[10px] text-primary",
      },
    },
    defaultVariants: { tone: "neutral" },
  },
);

export type PillTone = NonNullable<VariantProps<typeof pillVariants>["tone"]>;

export function Pill({ tone, className, ...props }: ComponentProps<"span"> & { tone?: PillTone }) {
  return <span data-tone={tone ?? "neutral"} className={cn(pillVariants({ tone }), className)} {...props} />;
}
