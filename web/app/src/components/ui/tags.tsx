import { type Stage, STAGE_LABEL, projectName } from "@/lib/board";
import { cn } from "@/lib/utils";
import { Pill } from "./pill";

export function StagePill({ stage }: { stage: Stage }) {
  return <Pill tone={stage}>{STAGE_LABEL[stage]}</Pill>;
}

export function ProjectTag({ project, className }: { project: string | null; className?: string }) {
  return <Pill tone="project" className={cn("max-w-[86px] truncate", className)}>{projectName(project)}</Pill>;
}

export function BranchTag({ branch, className }: { branch: string; className?: string }) {
  return <Pill tone="branch" className={cn("max-w-[124px] truncate", className)} title={branch}>{branch}</Pill>;
}
