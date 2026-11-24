import { ChevronDownIcon, ChevronRightIcon, CircleDotIcon, GitBranchIcon, LayersIcon, SearchIcon } from "lucide-react";
import { cn } from "@/lib/utils";
import { type Dot } from "@/lib/board";
import { StatusDot } from "./CardTile";

export type SidebarCheckout = { branch: string; host: string | null; primary: boolean };

export type SidebarProject = {
  name: string;
  host: string | null;
  cards: number;
  needsYou: boolean;
  running: boolean;
  checkouts: SidebarCheckout[];
};

export type SidebarScope =
  | { kind: "all" }
  | { kind: "needs" }
  | { kind: "project"; project: string }
  | { kind: "checkout"; project: string; branch: string };

const item = "flex min-h-8 w-full min-w-0 items-center gap-2 rounded-md px-2 text-left text-sm outline-none hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-sidebar-ring";
const on = "bg-sidebar-accent font-medium text-sidebar-accent-foreground";
const count = "shrink-0 font-mono text-xs text-muted-foreground tabular-nums";

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
  onToggle?: (project: string) => void;
}) {
  const selected = scope.kind === "project" && scope.project === project.name;
  const Chevron = expanded ? ChevronDownIcon : ChevronRightIcon;
  return (
    <li>
      <div className={cn("flex items-center rounded-md", selected && on)}>
        <button type="button" aria-expanded={expanded} aria-label={`Checkouturi ${project.name}`} onClick={() => onToggle?.(project.name)}
          className="grid size-8 shrink-0 place-items-center rounded-md text-muted-foreground outline-none hover:bg-sidebar-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring">
          <Chevron aria-hidden className="size-3.5" />
        </button>
        <button type="button" aria-current={selected ? "page" : undefined} onClick={() => onSelect?.({ kind: "project", project: project.name })}
          className={cn(item, "pl-1", !selected && "text-muted-foreground hover:text-foreground")}>
          <StatusDot dot={projectDot(project)} />
          <span className="min-w-0 flex-1 truncate font-mono text-xs">{project.name}</span>
          {project.host && <span className="shrink-0 font-mono text-xs text-muted-foreground">@{project.host}</span>}
          <span className={count}>{project.cards}</span>
        </button>
      </div>
      {expanded && project.checkouts.length > 0 && (
        <ul className="mt-0.5 flex flex-col gap-0.5">
          {project.checkouts.map((co) => {
            const current = scope.kind === "checkout" && scope.project === project.name && scope.branch === co.branch;
            const Icon = co.primary ? CircleDotIcon : GitBranchIcon;
            return (
              <li key={`${co.host ?? ""}:${co.branch}`}>
                <button type="button" aria-current={current ? "page" : undefined}
                  onClick={() => onSelect?.({ kind: "checkout", project: project.name, branch: co.branch })}
                  className={cn(item, "min-h-7 pl-9 font-mono text-xs", current ? on : "text-muted-foreground hover:text-foreground")}>
                  <Icon aria-hidden className="size-3 shrink-0" />
                  <span className="min-w-0 flex-1 truncate">{co.branch}</span>
                  <span className="shrink-0">{co.primary ? "main" : "wt"}</span>
                  {co.host && <span className="shrink-0">@{co.host}</span>}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </li>
  );
}

export function ProjectSidebar({ projects, total, needsCount, scope, expanded, showQuiet, query = "", onQueryChange, onSelect, onToggleProject, onToggleQuiet }: {
  projects: SidebarProject[];
  total: number;
  needsCount: number;
  scope: SidebarScope;
  expanded: string[];
  showQuiet: boolean;
  query?: string;
  onQueryChange?: (query: string) => void;
  onSelect?: (scope: SidebarScope) => void;
  onToggleProject?: (project: string) => void;
  onToggleQuiet?: () => void;
}) {
  const needle = query.trim().toLowerCase();
  const visible = projects.filter((p) => !needle || p.name.toLowerCase().includes(needle));
  const active = visible.filter((p) => p.cards > 0);
  const quiet = visible.filter((p) => p.cards === 0);
  const node = (p: SidebarProject) => (
    <ProjectNode key={p.name} project={p} scope={scope} expanded={expanded.includes(p.name)} onSelect={onSelect} onToggle={onToggleProject} />
  );
  const QuietChevron = showQuiet ? ChevronDownIcon : ChevronRightIcon;
  return (
    <nav aria-label="Proiecte" className="flex min-h-0 w-full flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground md:w-60 md:shrink-0">
      <div className="p-2">
        <label className="relative block">
          <span className="sr-only">Caută proiect</span>
          <SearchIcon aria-hidden className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <input type="search" value={query} readOnly={!onQueryChange} onChange={(e) => onQueryChange?.(e.target.value)} placeholder="Caută proiect"
            className="h-8 w-full rounded-md border border-input bg-card pr-2 pl-8 text-sm outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60" />
        </label>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
        <ul className="flex flex-col gap-0.5">
          <li>
            <button type="button" aria-current={scope.kind === "all" ? "page" : undefined} onClick={() => onSelect?.({ kind: "all" })}
              className={cn(item, scope.kind === "all" ? on : "text-muted-foreground hover:text-foreground")}>
              <LayersIcon aria-hidden className="size-3.5 shrink-0" />
              <span className="flex-1">Toate proiectele</span>
              <span className={count}>{total}</span>
            </button>
          </li>
          <li>
            <button type="button" aria-current={scope.kind === "needs" ? "page" : undefined} onClick={() => onSelect?.({ kind: "needs" })}
              className={cn(item, scope.kind === "needs" ? on : "text-muted-foreground hover:text-foreground")}>
              <StatusDot dot="needs" label="" />
              <span className="flex-1">Te așteaptă</span>
              <span className={cn(count, needsCount > 0 && "font-medium text-foreground")}>{needsCount}</span>
            </button>
          </li>
        </ul>
        <h2 className="flex items-center gap-1.5 px-2 pt-4 pb-1 text-xs font-medium text-muted-foreground">
          Active <span className="font-mono tabular-nums">{active.length}</span>
        </h2>
        <ul className="flex flex-col gap-0.5">{active.map(node)}</ul>
        <h2 className="pt-3">
          <button type="button" aria-expanded={showQuiet} onClick={onToggleQuiet}
            className="flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-left text-xs font-medium text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring">
            <QuietChevron aria-hidden className="size-3.5" />
            Liniștite <span className="font-mono tabular-nums">{quiet.length}</span>
          </button>
        </h2>
        {showQuiet && <ul className="flex flex-col gap-0.5">{quiet.map(node)}</ul>}
      </div>
    </nav>
  );
}
