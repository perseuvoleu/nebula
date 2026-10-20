import { useRef, useState } from "react";
import { Trash2Icon } from "lucide-react";
import { api, describeError } from "@/lib/api";
import type { SessionRow } from "@/lib/types";
import { Button } from "./ui/button";

export function DeleteSessionButton({ project, row, onDeleted }: {
  project: string; row: SessionRow; onDeleted: (id: string) => void;
}) {
  const busy = useRef(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const remove = async () => {
    if (busy.current || !window.confirm(`Ștergi sesiunea „${row.name}”?\n${project} · ${row.worktree}\n${row.path}\nID: ${row.id}\nProcesul activ va fi oprit. Sesiunea va fi eliminată din Nebula.`)) return;
    busy.current = true; setPending(true); setError(null);
    try {
      await api("sessions/delete", {}, { project, id: row.id });
      onDeleted(row.id);
    } catch (failure) { setError(describeError(failure)); }
    finally { busy.current = false; setPending(false); }
  };
  return <div className="shrink-0 self-center">
    <Button type="button" variant="ghost" className="size-11" aria-label="Șterge sesiunea" aria-description={`${row.name} · ${row.worktree} · ${row.id}`} disabled={pending} onClick={() => void remove()}><Trash2Icon aria-hidden /></Button>
    {pending && <span role="status" className="sr-only">Se șterge…</span>}
    {error && <p role="alert" className="max-w-40 text-xs">{error}</p>}
  </div>;
}
