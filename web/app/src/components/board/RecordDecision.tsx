import { type FormEvent, useState } from "react";
import { api, describeError, isUncertain } from "@/lib/api";
import { Button } from "@/components/ui/button";

const fieldCls = "rounded-lg border border-input bg-card px-3 py-2 text-base outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60 sm:text-sm";

export function RecordDecision({ cardRef, onRecorded }: { cardRef: string; onRecorded: () => void }) {
  const [what, setWhat] = useState("");
  const [why, setWhy] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!what.trim() || pending) return;
    setPending(true);
    setError(null);
    try {
      await api("decisions", {}, { card: cardRef, what: what.trim(), ...(why.trim() ? { why: why.trim() } : {}) });
      setWhat("");
      setWhy("");
      onRecorded();
    } catch (failure) {
      setError(isUncertain(failure) ? "Poate s-a notat; verifică artefactele înainte să repeți." : describeError(failure));
    } finally {
      setPending(false);
    }
  };

  return (
    <form aria-label="Notează o decizie" onSubmit={submit} className="flex flex-col gap-2 rounded-lg border bg-background p-3">
      <label className="flex flex-col gap-1 text-xs font-medium text-muted-foreground">
        Decizia
        <input value={what} onChange={(e) => setWhat(e.target.value)} maxLength={500} required autoComplete="off"
          placeholder="Ce ai decis" className={`h-10 ${fieldCls}`} />
      </label>
      <label className="flex flex-col gap-1 text-xs font-medium text-muted-foreground">
        De ce
        <textarea value={why} onChange={(e) => setWhy(e.target.value)} maxLength={4000} rows={2}
          placeholder="Opțional" className={`resize-y ${fieldCls}`} />
      </label>
      <div className="flex items-center gap-2">
        {error && <p role="alert" className="min-w-0 flex-1 text-sm">{error}</p>}
        <Button type="submit" size="sm" className="ml-auto" disabled={pending || !what.trim()}>Notează</Button>
      </div>
    </form>
  );
}
