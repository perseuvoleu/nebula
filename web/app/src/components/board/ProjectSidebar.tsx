import { ChevronDownIcon, ChevronRightIcon, CircleDotIcon, GitBranchIcon, LayersIcon, SearchIcon } from "lucide-react";
import { cn } from "@/lib/utils";
import type { Dot, SidebarProject, SidebarScope } from "@/lib/board";
import { StatusDot } from "./CardTile";

const item = "flex min-h-7 w-full min-w-0 items-center gap-1.5 rounded-sm px-2 text-left text-[12.5px] outline-none hover:bg-bg2 hover:text-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring";
const on = "bg-sel2 font-semibold text-foreground hover:bg-sel2";
const count = "ml-auto shrink-0 font-mono text-[11px] font-normal text-dim tabular-nums";
const sub = "min-h-6 pl-[26px] font-mono text-[11px]";

function projectDot(p: SidebarProject): Dot {
  if (p.cards === 0) return "off";
  if (p.needsYou) return "needs";
  return p.running ? "running" : "done";
}

function ProjectNode({ project, scope, expanded, onSelect, onToggle }: {
  project: SidebarProject;
  scope: SidebarScope;
  expanded: boolean;
  onSelect?: (scope: SidebarScope) => void;
  onToggle?: (project: string | null) => void;
}) {
  const selected = scope.kind === "project" && scope.project === project.project;
  const decisions = scope.kind === "decisions" && scope.project === project.project;
  const Chevron = expanded ? ChevronDownIcon : ChevronRightIcon;
  return (
    <li>
      <div className={cn("flex items-center rounded-sm", selected && on)}>
        <button type="button" aria-expanded={expanded} aria-label={`Checkouturi ${project.name}`} onClick={() => onToggle?.(project.project)}
          className="grid size-7 shrink-0 place-items-center rounded-sm text-dim outline-none hover:bg-bg3 hover:text-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring">
          <Chevron aria-hidden className="size-3" />
        </button>
        <button type="button" aria-current={selected ? "page" : undefined} onClick={() => onSelect?.({ kind: "project", project: project.project })}
          className={cn(item, "pl-1", !selected && "text-muted-foreground hover:text-foreground")}>
          <StatusDot dot={projectDot(project)} />
          <span className="min-w-0 flex-1 truncate font-mono text-[12.5px]">{project.name}</span>
          {project.host && <span className="shrink-0 font-mono text-[10.5px] text-remote">@{project.host}</span>}
          <span className={count}>{project.cards}</span>
        </button>
      </div>
      {expanded && (
        <ul className="flex flex-col gap-px">
          {project.checkouts.map((co) => {
            const current = scope.kind === "checkout" && scope.project === project.project && scope.branch === co.branch && scope.host === co.host;
            const Icon = co.primary ? CircleDotIcon : GitBranchIcon;
            return (
              <li key={`${co.host ?? ""}:${co.branch}`}>
                <button type="button" aria-current={current ? "page" : undefined}
                  onClick={() => onSelect?.({ kind: "checkout", project: project.project, host: co.host, branch: co.branch })}
                  className={cn(item, sub, current ? on : "text-dim")}>
                  <Icon aria-hidden className="size-3 shrink-0" />
                  <span className="min-w-0 flex-1 truncate">{co.branch}</span>
                  <span className="shrink-0 text-tag text-info">{co.primary ? "main" : "wt"}</span>
                  {co.host && <span className="shrink-0 text-tag text-remote">@{co.host}</span>}
                </button>
              </li>
            );
          })}
          <li>
            <button type="button" aria-current={decisions ? "page" : undefined} onClick={() => onSelect?.({ kind: "decisions", project: project.project })}
              className={cn(item, "min-h-6 pl-[26px] text-meta", decisions ? on : "text-special")}>
              <span aria-hidden className="size-2 shrink-0 rotate-45 rounded-[1px] bg-special" />
              <span className="min-w-0 flex-1 truncate">Decizii</span>
            </button>
          </li>
        </ul>
      )}
    </li>
  );
}

export function ProjectSidebar({ projects, total, needsCount, scope, expanded, showQuiet, query = "", onQueryChange, onSelect, onToggleProject, onToggleQuiet }: {
  projects: SidebarProject[];
  total: number;
  needsCount?: number;
  scope: SidebarScope;
  expanded: (string | null)[];
  showQuiet: boolean;
  query?: string;
  onQueryChange?: (query: string) => void;
  onSelect?: (scope: SidebarScope) => void;
  onToggleProject?: (project: string | null) => void;
  onToggleQuiet?: () => void;
}) {
  const needle = query.trim().toLowerCase();
  const visible = projects.filter((p) => !needle || p.name.toLowerCase().includes(needle));
  const active = visible.filter((p) => p.cards > 0);
  const quiet = visible.filter((p) => p.cards === 0);
  const node = (p: SidebarProject) => (
    <ProjectNode key={JSON.stringify(p.project)} project={p} scope={scope} expanded={expanded.includes(p.project)} onSelect={onSelect} onToggle={onToggleProject} />
  );
  const QuietChevron = showQuiet ? ChevronDownIcon : ChevronRightIcon;
  return (
    <nav aria-label="Proiecte" className="flex min-h-0 w-full flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground md:w-[236px] md:shrink-0">
      <div className="px-2 pt-2 pb-1">
        <label className="relative block">
          <span className="sr-only">Caută proiect</span>
          <SearchIcon aria-hidden className="pointer-events-none absolute top-1/2 left-2.5 size-3 -translate-y-1/2 text-dim" />
          <input type="search" value={query} readOnly={!onQueryChange} onChange={(e) => onQueryChange?.(e.target.value)} placeholder="Caută proiect"
            className="field h-[30px] w-full bg-bg1 pr-2 pl-7 text-xs" />
        </label>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 pb-3">
        <ul className="flex flex-col gap-px">
          <li>
            <button type="button" aria-current={scope.kind === "all" ? "page" : undefined} onClick={() => onSelect?.({ kind: "all" })}
              className={cn(item, scope.kind === "all" ? on : "text-muted-foreground")}>
              <LayersIcon aria-hidden className="size-3 shrink-0 text-dim" />
              <span className="flex-1">Toate proiectele</span>
              <span className={count}>{total}</span>
            </button>
          </li>
          <li>
            <button type="button" aria-current={scope.kind === "all-decisions" ? "page" : undefined} onClick={() => onSelect?.({ kind: "all-decisions" })}
              className={cn(item, scope.kind === "all-decisions" ? on : "text-muted-foreground")}>
              <span aria-hidden className="mx-0.5 size-2 shrink-0 rotate-45 rounded-[1px] bg-special" />
              <span className="flex-1">Toate deciziile</span>
            </button>
          </li>
          {needsCount !== undefined && <li>
            <button type="button" aria-current={scope.kind === "needs" ? "page" : undefined} onClick={() => onSelect?.({ kind: "needs" })}
              className={cn(item, scope.kind === "needs" ? on : "text-muted-foreground")}>
              <StatusDot dot="needs" label="" />
              <span className="flex-1">Te așteaptă</span>
              <span className={cn(count, needsCount > 0 && "font-semibold text-err")}>{needsCount}</span>
            </button>
          </li>}
        </ul>
        <h2 className="label-caps flex items-center gap-1.5 px-2.5 pt-[9px] pb-[3px]">
          Active <span className="font-mono tabular-nums">{active.length}</span>
        </h2>
        <ul className="flex flex-col gap-px">{active.map(node)}</ul>
        <h2 className="pt-2">
          <button type="button" aria-expanded={showQuiet} onClick={onToggleQuiet}
            className="label-caps flex w-full items-center gap-1.5 rounded-sm px-2.5 py-1 text-left outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring">
            <QuietChevron aria-hidden className="size-3" />
            Liniștite <span className="font-mono tabular-nums">{quiet.length}</span>
          </button>
        </h2>
        {showQuiet && <ul className="flex flex-col gap-px">{quiet.map(node)}</ul>}
      </div>
    </nav>
  );
}
