import { TriangleAlertIcon } from "lucide-react";
import type { AgentKind } from "@/lib/types";
import { RUN_ROLES, type RunRole } from "@/lib/board";
import { kindLabel } from "@/lib/status";
import { Button } from "@/components/ui/button";
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from "@/components/ui/dialog";

const KINDS: AgentKind[] = ["claude", "codex", "cursor", "pi"];

export interface RunHost {
  host: string;
  writers: number;
  writerSlots: number | null;
}

export interface RunDraft {
  role: RunRole;
  kind: AgentKind;
  host: string;
  worktree: "new" | "current";
}

const slotsFull = (h: RunHost | undefined) => !!h && h.writerSlots !== null && h.writers >= h.writerSlots;
const slotText = (h: RunHost) => h.writerSlots === null ? `${h.writers} scriu` : `${h.writers}/${h.writerSlots}`;

function runCommand(cardRef: string, draft: RunDraft, worktree: string): string {
  return `nebula todo run ${cardRef} --role ${draft.role} --kind ${draft.kind} --host ${draft.host} --worktree ${worktree}`;
}

function Segmented<T extends string>({ legend, name, options, value, onChange, label }: {
  legend: string;
  name: string;
  options: readonly T[];
  value: T;
  onChange: (value: T) => void;
  label?: (value: T) => React.ReactNode;
}) {
  return (
    <fieldset className="flex flex-col gap-1.5 text-sm">
      <legend className="mb-1.5 text-[11px] font-semibold text-muted-foreground">{legend}</legend>
      <div className="flex flex-wrap gap-1">
        {options.map((option) => (
          <label key={option} className={`flex h-9 flex-auto cursor-pointer items-center justify-center gap-1.5 rounded-sm border px-[9px] text-meta whitespace-nowrap has-focus-visible:ring-2 has-focus-visible:ring-ring ${value === option ? "border-primary bg-sel2 font-semibold text-foreground" : "bg-bg0 text-muted-foreground hover:border-edge-hover hover:text-foreground"}`}>
            <input type="radio" name={name} value={option} checked={value === option} onChange={() => onChange(option)} className="sr-only" />
            {label ? label(option) : option}
          </label>
        ))}
      </div>
    </fieldset>
  );
}

export function RunDialog({ open, onOpenChange, cardRef, cardTitle, branch, hosts, draft, onDraftChange, onRun, pending, error }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  cardRef: string;
  cardTitle: string;
  branch: string | null;
  hosts: RunHost[];
  draft: RunDraft;
  onDraftChange: (draft: RunDraft) => void;
  onRun: () => void;
  pending?: boolean;
  error?: string | null;
}) {
  const set = <K extends keyof RunDraft>(key: K) => (value: RunDraft[K]) => onDraftChange({ ...draft, [key]: value });
  const target = hosts.find((h) => h.host === draft.host);
  const writes = draft.role === "worker" || draft.role === "fix";
  const overCap = writes && slotsFull(target);
  const worktrees = branch ? (["current", "new"] as const) : (["new"] as const);
  const current = draft.worktree === "current" ? branch : null;
  const worktree = current ? "current" : "new";

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Pornește un agent pe card</DialogTitle>
          <DialogDescription className="flex min-w-0 gap-2">
            <span className="shrink-0 font-mono">{cardRef}</span>
            <span className="truncate">{cardTitle}</span>
          </DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-4">
          <Segmented legend="Rol" name="run-role" options={RUN_ROLES} value={draft.role} onChange={set("role")} />
          <Segmented legend="Agent" name="run-kind" options={KINDS} value={draft.kind} onChange={set("kind")} label={kindLabel} />
          <Segmented legend="Host" name="run-host" options={hosts.map((h) => h.host)} value={draft.host} onChange={set("host")}
            label={(host) => {
              const h = hosts.find((x) => x.host === host)!;
              return (
                <>
                  <span className="font-mono">@{host}</span>
                  <span className={`font-mono text-xs tabular-nums ${slotsFull(h) ? "font-medium" : "opacity-70"}`}>{slotText(h)}</span>
                </>
              );
            }} />
          <Segmented legend="Checkout" name="run-worktree" options={worktrees} value={worktree} onChange={set("worktree")}
            label={(w) => w === "new" ? "Worktree nou" : <span className="truncate font-mono">{branch}</span>} />
          {overCap && target && (
            <p role="status" className="flex gap-2 rounded-sm border border-line-warn bg-tint-warn px-2.5 py-2 text-xs text-warn">
              <TriangleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
              <span>
                Sloturile de scriere de pe <span className="font-mono">@{target.host}</span> sunt ocupate ({slotText(target)}).
                Numărul e doar informativ: pornirea nu îl verifică; alege alt host ca să rămâi sub el.
              </span>
            </p>
          )}
          {writes && current && (
            <p role="status" className="flex gap-2 rounded-sm border border-line-warn bg-tint-warn px-2.5 py-2 text-xs text-warn">
              <TriangleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
              <span>
                Pornirea pe <span className="font-mono">{current}</span> e refuzată dacă acest checkout are deja un writer; un worktree nou nu are.
              </span>
            </p>
          )}
          <pre className="overflow-x-auto rounded-sm border bg-bg0 px-3 py-2 font-mono text-[11px] leading-relaxed whitespace-pre-wrap wrap-anywhere text-muted-foreground">
            <span aria-hidden>$ </span>{runCommand(cardRef, draft, current ?? "new")}
          </pre>
          {error && <p role="alert" className="text-sm text-err">{error}</p>}
        </div>
        <DialogFooter>
          <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>Renunță</Button>
          <Button type="button" onClick={onRun} disabled={pending}>
            {pending ? "Se pornește…" : "Pornește"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
