import { useEffect, useState } from "react";
import { GitBranchIcon } from "lucide-react";
import { api, describeError, isUncertain } from "@/lib/api";
import type { Worktree } from "@/lib/types";
import { Button } from "./ui/button";
import { DeleteCheckoutButton } from "./DeleteCheckoutButton";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "./ui/dialog";

export function CheckoutPanel({ project, currentPath, onSelect }: {
  project: string; currentPath?: string; onSelect: (tree: Worktree) => void;
}) {
  const [trees, setTrees] = useState<Worktree[]>([]);
  const [open, setOpen] = useState(false);
  const [sourcePath, setSourcePath] = useState("");
  const [name, setName] = useState("");
  const [mode, setMode] = useState<"worktree" | "branch">("worktree");
  const [phase, setPhase] = useState<"idle" | "sending" | "uncertain">("idle");
  const [error, setError] = useState<string | null>(null);
  const primary = trees.find(w => w.is_main);
  const current = trees.find(w => w.path === currentPath) ?? primary;
  const source = trees.find(w => w.path === sourcePath);
  const occupied = primary?.sessions.some(s => s.alive) ?? false;
  const valid = /^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(name) && name.length <= 80 && !["main", "master"].includes(name);

  useEffect(() => {
    const abort = new AbortController();
    api<{ worktrees: Worktree[] }>("worktrees", { project }, undefined, { signal: abort.signal })
      .then(data => { if (!abort.signal.aborted) { setTrees(data.worktrees); setError(null); } })
      .catch(e => { if (!abort.signal.aborted) setError(describeError(e)); });
    return () => abort.abort();
  }, [project, open]);

  const create = async () => {
    if (phase !== "idle" || !valid || !source || (mode === "branch" && occupied)) return;
    setPhase("sending"); setError(null);
    try {
      const result = await api<{ worktree: Worktree }>("worktrees", {}, {
        project, source: source.path, base: source.branch, name, mode,
      }, { timeoutMs: 40000 });
      setTrees(old => [...old.filter(w => w.path !== result.worktree.path), result.worktree]);
      onSelect(result.worktree);
      setPhase("idle"); setOpen(false); setName("");
    } catch (failure) {
      const uncertain = isUncertain(failure);
      setPhase(uncertain ? "uncertain" : "idle");
      setError(uncertain ? "Creare incertă. Poate să fi reușit: închide dialogul și verifică lista înainte de a reîncerca." : describeError(failure));
    }
  };

  return (
    <div className="border-b border-sidebar-border px-3 py-2">
      <label className="block text-xs text-muted-foreground">
        <span>Branch / worktree</span>
        <select aria-label="Branch / worktree" value={current?.path ?? ""} disabled={!trees.length}
          onChange={e => { const w = trees.find(t => t.path === e.target.value); if (w) onSelect(w); }}
          className="mt-1 h-10 w-full min-w-0 rounded-lg border border-input bg-card px-2 font-mono text-sm text-foreground">
          {!trees.length && <option value="">Se încarcă…</option>}
          {trees.map(w => <option key={w.path} value={w.path}>{w.branch}{w.is_main ? " · principal" : " · wt"}</option>)}
        </select>
      </label>
      <Button type="button" variant="ghost" size="sm" className="mt-1 w-full justify-start" disabled={!current}
        onClick={() => { setSourcePath(current!.path); setMode("worktree"); setPhase("idle"); setError(null); setOpen(true); }}>
        <GitBranchIcon aria-hidden /> Branch / worktree nou
      </Button>
      {current && <DeleteCheckoutButton project={project} tree={current} onDeleted={deleted => {
        const remaining = trees.filter(w => w.path !== deleted.path);
        setTrees(remaining);
        const next = remaining.find(w => w.is_main) ?? remaining[0];
        if (next) onSelect(next);
        void api<{ worktrees: Worktree[] }>("worktrees", { project }).then(data => setTrees(data.worktrees))
          .catch(failure => setError(`Worktree eliminat. Reîncărcarea listei a eșuat: ${describeError(failure)}`));
      }} />}
      {!open && error && <p role="alert" className="mt-1 text-xs text-muted-foreground">{error}</p>}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="max-h-[90dvh] overflow-y-auto">
          <DialogHeader>
            <DialogTitle>Branch / worktree nou</DialogTitle>
            <DialogDescription>Pe server, din commitul branch-ului ales. Modificările necomise nu se copiază.</DialogDescription>
          </DialogHeader>
          <form className="flex flex-col gap-4" onSubmit={e => { e.preventDefault(); void create(); }}>
            <label className="flex flex-col gap-1.5 text-sm">Branch de bază
              <select value={sourcePath} onChange={e => setSourcePath(e.target.value)} disabled={phase !== "idle"}
                className="h-11 min-w-0 rounded-lg border border-input bg-card px-3 font-mono text-sm">
                {trees.map(w => <option key={w.path} value={w.path}>{w.branch}{w.is_main ? " · principal" : " · wt"}</option>)}
              </select>
            </label>
            <label className="flex flex-col gap-1.5 text-sm">Nume branch nou
              <input value={name} onChange={e => setName(e.target.value)} required maxLength={80} autoCapitalize="none" autoCorrect="off" spellCheck={false}
                placeholder="fix-login" disabled={phase !== "idle"} className="h-11 rounded-lg border border-input bg-card px-3 text-base" />
              <span className="text-xs text-muted-foreground">Litere mici, cifre și cratime. Numele trebuie să fie nou.</span>
            </label>
            <fieldset className="flex gap-4 text-sm" disabled={phase !== "idle"}>
              <legend className="mb-2 font-medium">Creează ca</legend>
              {(["worktree", "branch"] as const).map(value => <label key={value} className="flex min-h-11 items-center gap-2">
                <input type="radio" name="checkout-mode" checked={mode === value} onChange={() => setMode(value)} />
                {value === "worktree" ? "Worktree" : "Branch"}
              </label>)}
            </fieldset>
            <p className="text-xs text-muted-foreground">{mode === "worktree" ? "Director separat, potrivit pentru lucru în paralel. Sesiunile existente nu sunt mutate." : occupied
              ? "Checkout-ul principal are sesiuni deschise. Alege Worktree pentru a nu le schimba fișierele."
              : "Schimbă branch-ul în directorul principal. Este refuzat dacă există modificări necomise sau sesiuni deschise."}</p>
            {error && <p role="alert" className="text-sm">{error}</p>}
            <DialogFooter>
              <Button type="button" variant="ghost" onClick={() => setOpen(false)}>Închide</Button>
              <Button type="submit" disabled={phase !== "idle" || !valid || !source || (mode === "branch" && occupied)}>
                {phase === "sending" ? "Se creează…" : "Creează"}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </div>
  );
}
