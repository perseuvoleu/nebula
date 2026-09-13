import { useRef, useState } from "react";
import { api, describeError } from "@/lib/api";
import type { Worktree } from "@/lib/types";
import { Button } from "./ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "./ui/dialog";

export function DeleteCheckoutButton({ project, tree, onDeleted }: {
  project: string; tree: Worktree; onDeleted: (tree: Worktree) => void;
}) {
  const [target, setTarget] = useState<Worktree | null>(null);
  const [phase, setPhase] = useState<"checking" | "ready" | "blocked" | "deleting">("checking");
  const [error, setError] = useState<string | null>(null);
  const busy = useRef(false);
  const protectedTree = tree.is_main || ["main", "master"].includes(tree.branch);
  const body = (w: Worktree) => ({ project, id: w.id, branch: w.branch, path: w.path });
  const inspect = async () => {
    if (busy.current) return;
    busy.current = true; setTarget(tree); setPhase("checking"); setError(null);
    try { await api("worktrees/check-delete", {}, body(tree)); setPhase("ready"); }
    catch (failure) { setError(describeError(failure)); setPhase("blocked"); }
    finally { busy.current = false; }
  };
  const remove = async () => {
    if (!target || phase !== "ready" || busy.current) return;
    busy.current = true; setPhase("deleting"); setError(null);
    try { await api("worktrees/delete", {}, body(target)); setTarget(null); onDeleted(target); }
    catch (failure) { setError(describeError(failure)); setPhase("blocked"); }
    finally { busy.current = false; }
  };
  return <>
    <Button type="button" variant="ghost" className="min-h-11 w-full justify-start" disabled={protectedTree || !tree.id}
      onClick={() => void inspect()}>Elimină worktree{protectedTree ? " · protejat" : !tree.id ? " · necesită CLI nou" : ""}</Button>
    <Dialog open={target !== null} onOpenChange={open => { if (!open && !busy.current) setTarget(null); }}>
      <DialogContent>
        <DialogHeader><DialogTitle>Elimină worktree</DialogTitle>
          <DialogDescription>Se elimină doar directorul checkout-ului și înregistrarea Nebula. Branch-ul se păstrează. Nu se opresc sesiuni automat.</DialogDescription>
        </DialogHeader>
        {target && <div className="space-y-2 break-all text-sm">
          <p>{project} · <strong>{target.branch}</strong></p><p className="font-mono">{target.path}</p><p>ID: {target.id}</p>
          <p>Sesiuni: {target.sessions.length ? target.sessions.map(s => `${s.name} (${s.alive ? "activă" : s.status})`).join(", ") : "niciuna în lista curentă"}.</p>
          <p>Se refuză modificări necomise, fișiere untracked/ignored, commituri neintegrate în main/master și orice sesiune (inclusiv arhivate și terminale).</p>
        </div>}
        {phase === "checking" && <p role="status">Se verifică siguranța…</p>}
        {phase === "ready" && <p role="status">Curat, integrat în main/master, fără sesiuni. Verificarea se repetă la confirmare.</p>}
        {error && <p role="alert">{error}</p>}
        <DialogFooter>
          <Button variant="ghost" disabled={phase === "deleting" || phase === "checking"} onClick={() => setTarget(null)}>Anulează</Button>
          <Button disabled={phase !== "ready"} onClick={() => void remove()}>{phase === "deleting" ? "Se elimină…" : "Confirmă eliminarea worktree-ului"}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  </>;
}
