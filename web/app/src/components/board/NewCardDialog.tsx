import { STAGES, type Stage } from "@/lib/board";
import { Button } from "@/components/ui/button";
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from "@/components/ui/dialog";

const STAGE_LABEL: Record<Stage, string> = {
  backlog: "Backlog", next: "Next", doing: "Doing", review: "Review", done: "Done",
};

export interface CardDraft {
  title: string;
  project: string | null;
  stage: Stage;
}

const HOME = "";

export function NewCardDialog({ open, onOpenChange, projects, draft, onDraftChange, onCreate, pending }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  projects: string[];
  draft: CardDraft;
  onDraftChange: (draft: CardDraft) => void;
  onCreate: () => void;
  pending?: boolean;
}) {
  const fieldCls = "h-11 rounded-lg border border-input bg-card px-3 text-base outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60";
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Card nou</DialogTitle>
          <DialogDescription>Un card este o sarcină; sesiunile pornite din el îi rămân legate.</DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-4">
          <label className="flex flex-col gap-1.5 text-sm">
            <span className="font-medium">Titlu</span>
            <input value={draft.title} onChange={(e) => onDraftChange({ ...draft, title: e.target.value })} required autoComplete="off"
              placeholder="Ce trebuie făcut" className={`${fieldCls} placeholder:text-muted-foreground`} />
          </label>
          <div className="grid gap-4 sm:grid-cols-[1fr_9rem]">
            <label className="flex min-w-0 flex-col gap-1.5 text-sm">
              <span className="font-medium">Proiect</span>
              <select value={draft.project ?? HOME} onChange={(e) => onDraftChange({ ...draft, project: e.target.value || null })}
                className={`${fieldCls} font-mono text-sm`}>
                <option value={HOME}>fără proiect</option>
                {projects.map((p) => <option key={p} value={p}>@{p}</option>)}
              </select>
            </label>
            <label className="flex flex-col gap-1.5 text-sm">
              <span className="font-medium">Etapă</span>
              <select value={draft.stage} onChange={(e) => onDraftChange({ ...draft, stage: e.target.value as Stage })} className={fieldCls}>
                {STAGES.map((s) => <option key={s} value={s}>{STAGE_LABEL[s]}</option>)}
              </select>
            </label>
          </div>
        </div>
        <DialogFooter>
          <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>Renunță</Button>
          <Button type="button" onClick={onCreate} disabled={pending || !draft.title.trim()}>
            {pending ? "Se creează…" : "Creează cardul"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
